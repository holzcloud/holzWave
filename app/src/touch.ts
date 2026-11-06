// Touch gestures on the phone and tablet layouts (docs/design.md section 6.6). A touch pointer is not a mouse there: one finger
// scrolls the history of a pane (the same `wheel` command the desktop wheel sends, with a short fling after the finger lifts), a long
// press of half a second selects the word under the finger through the core's word selection and shows two handles and a menu with
// `Copy` and `Paste`, and two fingers pinching change the UI scale of this device, with a label for as long as they move and 800 ms
// after. A drag never selects text; the selection is made by a long press and moved by its handles.
//
// This module takes every `touch` pointer event first (pointer.ts hands it a place at the top of its handler stack while the page is a
// phone or a tablet and takes it away again on a desktop); mouse and pen pointers are not its business and pass to the code that
// handled them before. The core owns the selection: this module sends `select` begin, extend and clear and draws the handles from the
// spans of the frames, so nothing about the selection is kept here but the fact that a long press started one.
//
// The pinch value is kept on this device only (devicescale.ts, the page's own storage) and never goes through the settings commands, because
// `settings.json` travels to every computer. Menu and handles are made with the `el` helper and carry fixed words; no text of the
// terminal ever becomes an element.
import { motionReduced } from "./a11y.js";
import { runAction } from "./actions.js";
import { call } from "./bridge.js";
import { el, loadStyles } from "./dom.js";
import { visibleSpans } from "./frames.js";
import { gridRect } from "./geometry.js";
import { focusInput } from "./keyboard.js";
import { layoutNow } from "./phone.js";
import { refreshScale, scaleInForce, storeDeviceScale } from "./settings.js";
import { onOverlayRender } from "./overlay.js";
import { state } from "./state.js";
import type { UnitRect, View } from "./types.js";
import {
  FLING_MS, HANDLE_PX, LABEL_HOLD_MS, LONG_PRESS_MS, MENU_WORDS, classify, distance, dragRows, flingRows, handleBoxes, handleTarget,
  menuPlacement, orderCells, pinchScale, releaseVelocity, scaleLabel, smoothVelocity,
} from "./touchstate.js";
import type { Cell, CellGeometry, HandleBox } from "./touchstate.js";

// ---- what pointer.ts hands over ---------------------------------------------------------------------

type Mode = "cell" | "word" | "line";
type SelectOp =
  | { kind: "begin"; x: number; y: number; mode: Mode }
  | { kind: "extend"; x: number; y: number }
  | { kind: "clear" };

/** The hit of a pointer event as pointer.ts computes it. */
interface Hit {
  pane: number;
  cell: [number, number];
}

/** What this module needs of pointer.ts. It is handed over, so the two files do not import each other. */
export interface TouchHost {
  /** Put a handler on top of the pointer handler stack; the returned function takes it away again. */
  push(fn: (e: PointerEvent, hit: Hit | null) => boolean): () => void;
  /** Take the layout answer of a focus change. */
  applyView(v: View): void;
  /** Send one selection op; never rejects. */
  select(pane: number, op: SelectOp): Promise<void>;
  /** Whether the program of the pane asked for the mouse. */
  tracking(pane: number): boolean;
  /** Send one mouse report (device pixels in the pane's grid). */
  sendMouse(pane: number, action: "press" | "release" | "motion", button: "left" | "middle" | "right" | "none", x: number, y: number, mods: number): void;
  /** The point of an event in whole device pixels inside the grid of a pane, as `wheel` and `mouse` want it; null when the pane left the layout. */
  devicePoint(e: { clientX: number; clientY: number }, pane: number): [number, number] | null;
}

let host: TouchHost | null = null;
let running = false;
let removeHandler: (() => void) | null = null;
let removeHook: (() => void) | null = null;

/**
 * Start following the layout class: on a phone or a tablet the touch handler is on the stack, on a desktop nothing is registered,
 * created or loaded. Called once by `initPointer`, after `initPhone` has decided the class.
 */
export function initTouch(h: TouchHost): void {
  host = h;
  document.addEventListener("hw:layout", (e) => follow((e as CustomEvent<{ layout?: string }>).detail?.layout));
  follow(layoutNow());
}

function follow(layout: string | undefined): void {
  const touch = layout === "phone" || layout === "tablet";
  if (touch && !running) start();
  else if (!touch && running) stop();
}

/** A browser's own menu on a long press must not cover the one drawn here. */
const noMenu = (e: Event): void => e.preventDefault();

function start(): void {
  if (!host) return;
  running = true;
  void loadStyles("./touch.css").catch(() => {});
  removeHandler = host.push(onPointer);
  removeHook = onOverlayRender(refreshSelection);
  document.getElementById("term")?.addEventListener("contextmenu", noMenu);
}

function stop(): void {
  running = false;
  removeHandler?.();
  removeHandler = null;
  removeHook?.();
  removeHook = null;
  document.getElementById("term")?.removeEventListener("contextmenu", noMenu);
  cancelAll();
  dropSelectionUi();
  dropLabel();
}

// ---- the fingers -------------------------------------------------------------------------------------

interface Press {
  id: number;
  /** The pane under the finger at the start, and the cell; null outside every pane. */
  pane: number | null;
  cell: Cell | null;
  /** Where and when it went down. */
  x0: number;
  y0: number;
  t0: number;
  /** The farthest it has been from where it went down. */
  moved: number;
  /** `pending` until it is a long press, a scroll or over; `gone` when a second finger took it. */
  mode: "pending" | "scroll" | "long" | "gone";
  timer: number;
  /** The pane had the focus when the finger went down (a program that tracks the mouse sees a tap only then). */
  focused: boolean;
  /** Scrolling: the fractional rows carried, the last position and time, the smoothed speed. */
  remainder: number;
  lastX: number;
  lastY: number;
  lastT: number;
  lastMoveT: number;
  velocity: number;
}

const fingers = new Map<number, { x: number; y: number }>();
let press: Press | null = null;
let pinch: { startScale: number; startDist: number; last: number } | null = null;

const now = (): number => performance.now();

function onPointer(e: PointerEvent, hit: Hit | null): boolean {
  if (e.pointerType !== "touch") return false;
  switch (e.type) {
    case "pointerdown":
      down(e, hit);
      break;
    case "pointermove":
      move(e);
      break;
    case "pointerup":
      up(e);
      break;
    case "pointercancel":
      cancel(e);
      break;
  }
  return true; // a touch is never the desktop's selection gesture
}

function down(e: PointerEvent, hit: Hit | null): void {
  stopFling();
  fingers.set(e.pointerId, { x: e.clientX, y: e.clientY });
  if (fingers.size >= 2) {
    startPinch();
    return;
  }
  const p: Press = {
    id: e.pointerId, pane: hit?.pane ?? null, cell: hit?.cell ?? null, x0: e.clientX, y0: e.clientY, t0: now(), moved: 0,
    mode: "pending", timer: 0, focused: true, remainder: 0, lastX: e.clientX, lastY: e.clientY, lastT: now(), lastMoveT: now(), velocity: 0,
  };
  press = p;
  if (!hit || !host) return;
  const view = state.view;
  if (view && hit.pane !== view.focus) {
    p.focused = false;
    void call<View>("focus_pane", { pane: hit.pane }).then((v) => host?.applyView(v), () => {});
  }
  // A program that tracks the mouse owns the press: there is no long press to select with.
  if (!host.tracking(hit.pane)) p.timer = window.setTimeout(() => onLongTimer(p), LONG_PRESS_MS);
}

function move(e: PointerEvent): void {
  const f = fingers.get(e.pointerId);
  if (!f) return;
  f.x = e.clientX;
  f.y = e.clientY;
  if (pinch) {
    updatePinch();
    return;
  }
  const p = press;
  if (!p || p.id !== e.pointerId) return;
  p.moved = Math.max(p.moved, Math.hypot(e.clientX - p.x0, e.clientY - p.y0));
  if (p.mode === "pending") {
    const g = classify({ pointers: 1, movedPx: p.moved, elapsedMs: now() - p.t0, released: false });
    if (g === "drag") startScroll(p);
    else if (g === "longpress") longPress(p);
  }
  if (p.mode === "scroll") scrollBy(p, e);
}

function up(e: PointerEvent): void {
  fingers.delete(e.pointerId);
  if (pinch) {
    if (fingers.size < 2) endPinch();
    return;
  }
  const p = press;
  if (!p || p.id !== e.pointerId) return;
  press = null;
  window.clearTimeout(p.timer);
  if (p.mode === "pending") {
    const g = classify({ pointers: 1, movedPx: p.moved, elapsedMs: now() - p.t0, released: true });
    if (g === "tap") tap(p);
    else if (g === "longpress") longPress(p); // the timer ran late
  } else if (p.mode === "scroll") {
    flushWheel();
    startFling(p);
  }
}

function cancel(e: PointerEvent): void {
  fingers.delete(e.pointerId);
  if (pinch) {
    if (fingers.size < 2) endPinch();
    return;
  }
  if (press && press.id === e.pointerId) {
    window.clearTimeout(press.timer);
    if (press.mode === "scroll") flushWheel();
    press = null;
  }
}

/** Forget every finger and everything in flight (the layout left the touch classes). */
function cancelAll(): void {
  fingers.clear();
  if (press) window.clearTimeout(press.timer);
  press = null;
  pinch = null;
  stopFling();
  flushWheel();
}

// ---- tap and long press -------------------------------------------------------------------------------

/** A tap: the pane was focused at the press; it clears a selection; a program that tracks the mouse sees a click. */
function tap(p: Press): void {
  if (p.pane === null || !host) {
    clearSelection(null);
    return;
  }
  clearSelection(p.pane);
  if (p.focused && host.tracking(p.pane)) {
    const at = host.devicePoint({ clientX: p.x0, clientY: p.y0 }, p.pane);
    if (at) {
      host.sendMouse(p.pane, "press", "left", at[0], at[1], 0);
      host.sendMouse(p.pane, "release", "left", at[0], at[1], 0);
    }
  }
}

function onLongTimer(p: Press): void {
  if (press !== p || p.mode !== "pending") return;
  if (classify({ pointers: 1, movedPx: p.moved, elapsedMs: now() - p.t0, released: false }) === "longpress") longPress(p);
}

/** A long press: the word under the finger is selected by the core; the handles and the menu follow its spans. */
function longPress(p: Press): void {
  window.clearTimeout(p.timer);
  if (p.mode !== "pending" || p.pane === null || p.cell === null || !host) return;
  p.mode = "long";
  active = { pane: p.pane, seen: false };
  coreAnchor = null;
  signature = "";
  void host.select(p.pane, { kind: "begin", x: p.cell[0], y: p.cell[1], mode: "word" });
}

// ---- scrolling and the fling -----------------------------------------------------------------------------

function startScroll(p: Press): void {
  window.clearTimeout(p.timer);
  if (p.pane === null) {
    p.mode = "gone";
    return;
  }
  p.mode = "scroll";
  p.lastY = p.y0; // the first move counts the whole distance from the press
  p.lastT = p.t0;
  p.lastMoveT = now();
}

function scrollBy(p: Press, e: PointerEvent): void {
  if (p.pane === null) return;
  const t = now();
  const dy = e.clientY - p.lastY;
  p.velocity = smoothVelocity(p.velocity, dy, t - p.lastT);
  p.lastX = e.clientX;
  p.lastY = e.clientY;
  p.lastT = t;
  p.lastMoveT = t;
  const step = dragRows(dy, p.remainder, state.metrics.cellH);
  p.remainder = step.remainder;
  queueWheel(p.pane, step.rows, e.clientX, e.clientY);
}

/** The rows that wait for the next animation frame, over one pane. A burst of moves is one command. */
let wheel: { pane: number; rows: number; x: number; y: number; frame: number } | null = null;

function queueWheel(pane: number, rows: number, clientX: number, clientY: number): void {
  if (wheel && wheel.pane !== pane) flushWheel();
  if (!wheel) wheel = { pane, rows: 0, x: clientX, y: clientY, frame: 0 };
  wheel.rows += rows;
  wheel.x = clientX;
  wheel.y = clientY;
  if (wheel.frame === 0) wheel.frame = requestAnimationFrame(flushWheel);
}

function flushWheel(): void {
  const w = wheel;
  wheel = null;
  if (!w) return;
  if (w.frame !== 0) cancelAnimationFrame(w.frame);
  sendWheel(w.pane, w.rows, w.x, w.y);
}

/** One `wheel` command, as the desktop wheel sends it: the core decides between a program's report, arrow keys and the history. */
function sendWheel(pane: number, rows: number, clientX: number, clientY: number): void {
  if (rows === 0 || !host) return;
  const at = host.devicePoint({ clientX, clientY }, pane);
  if (!at) return;
  void call("wheel", { pane, rows, x: at[0], y: at[1], mods: 0 }).catch(() => {});
}

let fling: { pane: number; v: number; t0: number; sent: number; frame: number; x: number; y: number } | null = null;

function startFling(p: Press): void {
  if (p.pane === null) return;
  const v = releaseVelocity(p.velocity, now() - p.lastMoveT);
  if (v === 0 || motionReduced()) return;
  fling = { pane: p.pane, v, t0: now(), sent: 0, frame: 0, x: p.lastX, y: p.lastY };
  fling.frame = requestAnimationFrame(tickFling);
}

function tickFling(): void {
  const f = fling;
  if (!f) return;
  const t = now() - f.t0;
  const total = flingRows(f.v, t, state.metrics.cellH, motionReduced());
  const delta = total - f.sent;
  if (delta !== 0) {
    f.sent = total;
    sendWheel(f.pane, delta, f.x, f.y);
  }
  if (t >= FLING_MS) fling = null;
  else f.frame = requestAnimationFrame(tickFling);
}

/** A new touch, a second finger or a change of layout stops a fling at once. */
function stopFling(): void {
  if (fling && fling.frame !== 0) cancelAnimationFrame(fling.frame);
  fling = null;
}

// ---- pinch and its label -------------------------------------------------------------------------------------

function twoFingers(): [{ x: number; y: number }, { x: number; y: number }] | null {
  const [a, b] = [...fingers.values()];
  return a && b ? [a, b] : null;
}

function startPinch(): void {
  const two = twoFingers();
  if (!two) return;
  if (press) window.clearTimeout(press.timer);
  press = null; // the first finger scrolls and taps no more, and neither does the second
  stopFling();
  flushWheel();
  const startScale = scaleInForce();
  pinch = { startScale, startDist: distance(two[0], two[1]), last: startScale };
  showLabel(startScale);
}

function updatePinch(): void {
  const two = twoFingers();
  const p = pinch;
  if (!two || !p) return;
  const next = pinchScale(p.startScale, p.startDist, distance(two[0], two[1]));
  if (next === p.last) return;
  p.last = next;
  storeDeviceScale(next); // this device only: never a settings command, so settings.json and the sync never see it
  refreshScale();
  showLabel(next);
}

function endPinch(): void {
  pinch = null;
  holdLabel();
}

let label: HTMLElement | null = null;
let labelTimer = 0;

/** The label takes the place of the status chip in the phone's top strip and floats at the top of the stage on a tablet. */
function showLabel(percent: number): void {
  window.clearTimeout(labelTimer);
  const wanted = document.getElementById("phone-chip") ?? document.getElementById("stage");
  if (!wanted) return;
  if (!label || label.parentElement !== wanted) {
    label?.remove();
    label = el("span", wanted.id === "phone-chip" ? "touch-scale" : "touch-scale touch-scale-float", wanted);
    label.id = "touch-scale";
    label.setAttribute("role", "status");
  }
  const text = scaleLabel(percent);
  if (label.textContent !== text) label.textContent = text;
}

function holdLabel(): void {
  window.clearTimeout(labelTimer);
  labelTimer = window.setTimeout(dropLabel, LABEL_HOLD_MS);
}

function dropLabel(): void {
  window.clearTimeout(labelTimer);
  label?.remove();
  label = null;
}

// ---- the selection: handles and menu -----------------------------------------------------------------------------

/**
 * A long press started a selection in `pane`. `seen` is set once a frame has shown its spans: the selection is over when they are gone
 * again (cleared, scrolled out of the viewport, or ended by the program), and the handles and the menu go with it.
 */
let active: { pane: number; seen: boolean } | null = null;

/**
 * The cell the core holds as the anchor of the selection, when this module knows it: the start of the word after a long press, the fixed
 * end after a handle drag. The end handle only extends when the anchor is the start; otherwise it makes the start the anchor first.
 */
let coreAnchor: Cell | null = null;

interface Ui {
  layer: HTMLElement;
  start: HTMLElement;
  end: HTMLElement;
  menu: HTMLElement;
  copy: HTMLButtonElement;
  paste: HTMLButtonElement;
}

let ui: Ui | null = null;
/** What the handles and the menu were last placed for, so a render that changed nothing places nothing. */
let signature = "";
/** Where the handles are now (CSS px of the stage) and the cells they mark. */
let placed: { start: HandleBox; end: HandleBox; cells: [Cell, Cell]; geo: CellGeometry } | null = null;

/** A handle: a 44 px target with the stem and the drop the CSS draws. It is decoration for a pointer; the menu has the real controls. */
function makeHandle(layer: HTMLElement, which: "start" | "end"): HTMLElement {
  const h = el("div", `touch-handle touch-handle-${which}`, layer);
  h.setAttribute("aria-hidden", "true");
  h.dataset.role = which;
  el("span", "touch-stem", h);
  el("span", "touch-drop", h);
  h.addEventListener("pointerdown", (e) => grab(e, h, which));
  return h;
}

function build(): Ui | null {
  if (ui) return ui;
  const overlay = document.getElementById("overlay");
  if (!overlay) return null;
  const layer = el("div", "touch-layer", overlay);
  const start = makeHandle(layer, "start");
  const end = makeHandle(layer, "end");
  const menu = el("div", "touch-menu", layer);
  menu.setAttribute("role", "toolbar");
  menu.setAttribute("aria-label", "Selection");
  const copy = el("button", "touch-button", menu);
  copy.type = "button";
  copy.textContent = MENU_WORDS.copy;
  const paste = el("button", "touch-button", menu);
  paste.type = "button";
  paste.textContent = MENU_WORDS.paste;
  // A press on the menu must not take the keyboard from the terminal.
  menu.addEventListener("pointerdown", (e) => e.preventDefault());
  copy.addEventListener("click", onCopy);
  paste.addEventListener("click", onPaste);
  ui = { layer, start, end, menu, copy, paste };
  return ui;
}

function dropSelectionUi(): void {
  active = null;
  coreAnchor = null;
  drag = null;
  signature = "";
  placed = null;
  ui?.layer.remove();
  ui = null;
}

/** Hide the handles and the menu but keep the elements (a selection that is not in view, or one whose frame has not come yet). */
function hideSelectionUi(): void {
  if (!ui) return;
  ui.start.hidden = true;
  ui.end.hidden = true;
  ui.menu.hidden = true;
  signature = "";
  placed = null;
}

/** End the selection: tell the core to clear it (when `pane` is given and it has one) and take the handles and the menu away. */
function clearSelection(pane: number | null): void {
  const had = active !== null;
  const target = pane ?? active?.pane ?? null;
  const spans = target !== null ? (state.frames.get(target)?.selection.length ?? 0) : 0;
  if (target !== null && host && (had || spans > 0)) void host.select(target, { kind: "clear" });
  dropSelectionUi();
}

function onCopy(): void {
  runAction("edit.copy"); // the desktop's copy: the cached text, or the core's, to the clipboard
  clearSelection(null);
  focusInput();
}

function onPaste(): void {
  runAction("edit.paste"); // the desktop's paste, with its confirmation, size limit and bracketed paste
  clearSelection(null);
  focusInput();
}

/** The first and the last selected cell of a pane's frame, both inclusive; null without spans. */
function endsOf(pane: number): [Cell, Cell] | null {
  const mirror = state.frames.get(pane);
  if (!mirror) return null;
  let lo: Cell | null = null;
  let hi: Cell | null = null;
  for (const [y, x0, x1] of visibleSpans(mirror)) {
    if (!lo || y < lo[1] || (y === lo[1] && x0 < lo[0])) lo = [x0, y];
    if (!hi || y > hi[1] || (y === hi[1] && x1 > hi[0])) hi = [x1, y];
  }
  return lo && hi ? [lo, hi] : null;
}

const unitOf = (pane: number): UnitRect | null => state.view?.panes.find(([id]) => id === pane)?.[1] ?? null;

/** Called at the start of every overlay render: put the handles and the menu where the frame's spans say the selection is. */
function refreshSelection(): void {
  if (!active) return;
  const unit = unitOf(active.pane);
  const ends = unit ? endsOf(active.pane) : null;
  if (!unit || !ends) {
    // The selection is gone again after it was shown (cleared by the program, scrolled out): the touch selection is over too.
    if (active.seen && !drag) dropSelectionUi();
    else hideSelectionUi();
    return;
  }
  if (!active.seen) coreAnchor = ends[0]; // the word's own start: the core's anchor after the long press
  active.seen = true;
  const m = state.metrics;
  const g = gridRect(m, unit, state.headerPx);
  const geo: CellGeometry = { originX: g.x, originY: g.y, cellW: m.cellW, cellH: m.cellH };
  const stage = { w: m.width, h: m.height };
  // While a handle is dragged the one under the finger takes the role its cell has against the fixed end.
  const draggedIsEnd = drag && drag.moved ? !isBefore(drag.extent, drag.anchor) : null;
  const sig = `${ends[0]}|${ends[1]}|${geo.originX},${geo.originY},${geo.cellW},${geo.cellH}|${stage.w},${stage.h}|${draggedIsEnd}`;
  if (sig === signature) return;
  const parts = build();
  if (!parts) return;
  signature = sig;
  const boxes = handleBoxes(ends[0], ends[1], geo, stage);
  placed = { start: boxes.start, end: boxes.end, cells: ends, geo };
  let startNode = parts.start;
  let endNode = parts.end;
  if (drag && draggedIsEnd !== null) {
    const other = drag.node === parts.start ? parts.end : parts.start;
    startNode = draggedIsEnd ? other : drag.node;
    endNode = draggedIsEnd ? drag.node : other;
  }
  put(startNode, boxes.start, "start");
  put(endNode, boxes.end, "end");
  // The menu sits above the selection, or below the handles when there is no room above.
  parts.menu.hidden = false;
  const first = ends[0];
  const last = ends[1];
  const selection = {
    x: geo.originX + (first[1] === last[1] ? first[0] : 0) * geo.cellW,
    y: geo.originY + first[1] * geo.cellH,
    w: (first[1] === last[1] ? last[0] - first[0] + 1 : (state.frames.get(active.pane)?.cols ?? 1)) * geo.cellW,
    h: (last[1] - first[1] + 1) * geo.cellH + HANDLE_PX,
  };
  const spot = menuPlacement(selection, { x: 0, y: 0, w: stage.w, h: stage.h }, { w: parts.menu.offsetWidth, h: parts.menu.offsetHeight });
  parts.menu.style.left = `${spot.x}px`;
  parts.menu.style.top = `${spot.y}px`;
}

/** Place a handle (the layer is the stage's size) and say which end of the selection it marks now: the drop's shape follows the role. */
function put(node: HTMLElement, box: HandleBox, role: "start" | "end"): void {
  node.hidden = false;
  node.dataset.role = role;
  node.style.left = `${box.x}px`;
  node.style.top = `${box.y}px`;
  node.style.width = `${box.w}px`;
  node.style.height = `${box.h}px`;
}

const sameCell = (a: Cell, b: Cell): boolean => a[0] === b[0] && a[1] === b[1];
/** Whether `a` is strictly before `b` in reading order. */
const isBefore = (a: Cell, b: Cell): boolean => a[1] < b[1] || (a[1] === b[1] && a[0] < b[0]);

// ---- dragging a handle ---------------------------------------------------------------------------------------

/**
 * A handle drag. The cell that stays is `anchor`, the one the finger moves is `extent`. The end handle moves the core's own extent (an
 * `extend`); the start handle makes the old end the anchor (`begin` in cell mode there) and then extends to the finger, as the core's
 * anchor is the cell of the last `begin`. Crossing the other end swaps the roles: the one under the finger is then the other handle.
 */
let drag: {
  id: number;
  pane: number;
  node: HTMLElement;
  /** The role of the handle at the grab: moving the end (`true`) or the start. */
  end: boolean;
  anchor: Cell;
  extent: Cell;
  grab: { dx: number; dy: number };
  /** Whether a command was sent: a pure touch on a handle changes nothing. */
  moved: boolean;
  /** The newest cell not sent yet, and the animation frame that sends it. */
  pending: Cell | null;
  frame: number;
} | null = null;

function grab(e: PointerEvent, node: HTMLElement, which: "start" | "end"): void {
  if (e.pointerType !== "touch" || !active || !placed || !host) return;
  e.preventDefault();
  e.stopPropagation();
  stopFling();
  const end = which === "end";
  const box = end ? placed.end : placed.start;
  const [from, to] = placed.cells;
  const rect = document.getElementById("term")?.getBoundingClientRect();
  if (!rect) return;
  try {
    node.setPointerCapture(e.pointerId);
  } catch {
    // no capture: the moves of a finger that leaves the handle are lost, the handle stays where it was
  }
  drag = {
    id: e.pointerId, pane: active.pane, node, end, anchor: end ? from : to, extent: end ? to : from,
    grab: { dx: e.clientX - rect.left - box.tipX, dy: e.clientY - rect.top - box.tipY }, moved: false, pending: null, frame: 0,
  };
  node.addEventListener("pointermove", onHandleMove);
  node.addEventListener("pointerup", onHandleEnd);
  node.addEventListener("pointercancel", onHandleEnd);
}

function onHandleMove(e: PointerEvent): void {
  const d = drag;
  if (!d || e.pointerId !== d.id || !placed) return;
  const rect = document.getElementById("term")?.getBoundingClientRect();
  const size = state.frames.get(d.pane);
  if (!rect || !size) return;
  const to = handleTarget({ x: e.clientX - rect.left, y: e.clientY - rect.top }, d.grab, d.end, placed.geo);
  const cell: Cell = [Math.max(0, Math.min(to[0], size.cols - 1)), Math.max(0, Math.min(to[1], size.rows - 1))];
  if (sameCell(cell, d.pending ?? d.extent)) return;
  d.pending = cell;
  if (d.frame === 0) d.frame = requestAnimationFrame(() => sendHandle(d));
}

/** Send the newest cell the handle names; the start handle first makes the fixed end the anchor. */
function sendHandle(d: NonNullable<typeof drag>): void {
  if (d.frame !== 0) cancelAnimationFrame(d.frame);
  d.frame = 0;
  const to = d.pending;
  d.pending = null;
  if (!to || !host || drag !== d) return;
  if (!d.moved && (!d.end || coreAnchor === null || !sameCell(coreAnchor, d.anchor))) {
    void host.select(d.pane, { kind: "begin", x: d.anchor[0], y: d.anchor[1], mode: "cell" });
  }
  coreAnchor = d.anchor;
  d.moved = true;
  d.extent = to;
  signature = ""; // the role of the handle under the finger may have changed
  void host.select(d.pane, { kind: "extend", x: to[0], y: to[1] });
}

function onHandleEnd(e: PointerEvent): void {
  const d = drag;
  if (!d || e.pointerId !== d.id) return;
  if (d.pending) sendHandle(d);
  d.node.removeEventListener("pointermove", onHandleMove);
  d.node.removeEventListener("pointerup", onHandleEnd);
  d.node.removeEventListener("pointercancel", onHandleEnd);
  try {
    d.node.releasePointerCapture(e.pointerId);
  } catch {
    // the capture ended with the pointer
  }
  drag = null;
  signature = "";
}
