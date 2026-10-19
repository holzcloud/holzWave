// Pointer input on the canvas. A gesture is either the program's or the user's, decided once at its press.
//
// The user's: the selection is the core's. A press sends `select` begin with the viewport cell under the pointer
// (word on the second click, line on the third), moving sends `extend` at most once per animation frame, and a
// click that never moved clears. Nothing about the selection is kept here. When a drag, a double click or a triple
// click ends, the core's selection is copied to the system clipboard (`copy_on_select`, selection.ts).
//
// The program's: when the pane's frame header says the program tracks the mouse (`modes.mouse`) and Shift is not
// held, press, release and motion go to the core as `mouse` commands in device pixels relative to the grid origin
// and nothing is selected. Motion is one command per animation frame, and a pointer that is cancelled, loses its
// capture or leaves with the window sends the release for the button it holds, so no button stays down.
//
// Handlers can be stacked in front of the gesture (`pushPointerHandler`): links, hints and copy mode take clicks
// without this file knowing them. The wheel always goes to the core as a `wheel` command for the pane under the
// pointer: only the core sees the live modes and decides between reports, arrow keys and scrollback. Also owns
// `edit.copy`.
import { registerAction } from "./actions.js";
import { call } from "./bridge.js";
import type { PxRect } from "./frame.js";
import { cellAt, gridRect, paneAt } from "./geometry.js";
import { gridPoint, modifierBits, wheelRows } from "./mouse.js";
import { canvas as sceneCanvas } from "./scene.js";
import { copyOnSelect, copySelection } from "./selection.js";
import { state } from "./state.js";
import type { UnitRect, View } from "./types.js";

// ---- handler stack -------------------------------------------------------------------------------

/** The pane under a pointer event and the viewport cell in it (clamped to the pane's grid). */
export interface PointerHit {
  pane: number;
  cell: [number, number];
}

/**
 * Return true to consume the event: no older handler and not the selection gesture see it. `hit` is null when the
 * pointer is outside every pane. A handler sees pointerdown, pointermove, pointerup and pointercancel.
 */
export type PointerHandler = (e: PointerEvent, hit: PointerHit | null) => boolean;

// One entry per push, so removing one push can never remove another push of the same function.
const handlers: { fn: PointerHandler }[] = [];

/**
 * Put `fn` on top of the handler stack: it is consulted before every older handler and before the selection
 * gesture. The returned function removes exactly this push; calling it again does nothing.
 */
export function pushPointerHandler(fn: PointerHandler): () => void {
  const entry = { fn };
  handlers.push(entry);
  return () => {
    const at = handlers.indexOf(entry);
    if (at >= 0) handlers.splice(at, 1);
  };
}

/** Offer the event to the handlers, newest first; true when one consumed it. */
function offer(e: PointerEvent, canvas: HTMLCanvasElement): boolean {
  if (handlers.length === 0) return false;
  const hit = hitAt(e, canvas);
  // A copy of the stack, so a handler may push or pop while it runs.
  for (const entry of [...handlers].reverse()) {
    if (handlers.includes(entry) && entry.fn(e, hit)) return true;
  }
  return false;
}

// ---- cells ---------------------------------------------------------------------------------------

/** The grid of a pane in cells: the mirror's when there is one, else the size the core was told. */
function gridOf(pane: number): { cols: number; rows: number } | null {
  return state.frames.get(pane) ?? state.sizes.get(pane) ?? null;
}

/** A cell clamped to the pane's grid (to 0 when the grid is not known yet). */
function clamp(pane: number, cell: [number, number]): [number, number] {
  const g = gridOf(pane);
  if (!g) return cell;
  return [Math.max(0, Math.min(cell[0], g.cols - 1)), Math.max(0, Math.min(cell[1], g.rows - 1))];
}

/**
 * Where the cells of a pane are on the canvas in CSS pixels: the grid origin and the size of the cells the core
 * knows (the pane's grid rectangle when no frame or size is known yet).
 */
function gridBox(pane: number, r: UnitRect): PxRect {
  const g = gridRect(state.metrics, r, state.headerPx);
  const cells = gridOf(pane);
  if (!cells) return g;
  return { x: g.x, y: g.y, w: cells.cols * state.metrics.cellW, h: cells.rows * state.metrics.cellH };
}

/** The position of an event inside a pane's grid in whole device pixels, as the `mouse` and `wheel` commands want it. */
function devicePoint(e: { clientX: number; clientY: number }, canvas: HTMLCanvasElement, pane: number, r: UnitRect): [number, number] {
  const rect = canvas.getBoundingClientRect();
  return gridPoint(e.clientX, e.clientY, rect, gridBox(pane, r), state.metrics.dpr);
}

function hitAt(e: PointerEvent, canvas: HTMLCanvasElement): PointerHit | null {
  const rect = canvas.getBoundingClientRect();
  const x = e.clientX - rect.left;
  const y = e.clientY - rect.top;
  const at = paneAt(state.metrics, state.view, x, y);
  if (!at) return null;
  return { pane: at[0], cell: clamp(at[0], cellAt(state.metrics, at[1], state.headerPx, x, y)) };
}

// ---- gestures ------------------------------------------------------------------------------------

type Mode = "cell" | "word" | "line";

/** The window of a multi-click: at most this long and this far from the click before. */
const CLICK_MS = 400;
const CLICK_PX = 4;

interface Gesture {
  pane: number;
  pointer: number;
  mode: Mode;
  /** The cell the core last heard of (begin or extend). */
  last: [number, number];
  /** The newest cell that has not been sent yet. */
  pending: [number, number] | null;
  frame: number;
  /** An extend was sent: the gesture is a drag, not a click. */
  moved: boolean;
  /** The newest selection command sent for this gesture: copy on select waits for it, so it sees the final extend. */
  sent: Promise<void>;
}

let gesture: Gesture | null = null;
let lastClick: { t: number; x: number; y: number; n: number; pane: number } | null = null;
/** The wheel's fractional rows, kept for the pane they were collected over. */
let wheelPane = 0;
let wheelRemainder = 0;

type Op =
  | { kind: "begin"; x: number; y: number; mode: Mode }
  | { kind: "extend"; x: number; y: number }
  | { kind: "clear" };

/**
 * Send one selection op; a command that fails is the core's to report, not an error of the page. The promise settles
 * when the command has been answered and never rejects.
 */
function select(pane: number, op: Op): Promise<void> {
  return call("select", { pane, op }).then(
    () => {},
    () => {},
  );
}

// ---- reporting -------------------------------------------------------------------------------------

type Button = "left" | "middle" | "right";
/** The `mouse` command's names for `PointerEvent.button` 0, 1 and 2; other buttons are not reported. */
const BUTTONS: readonly Button[] = ["left", "middle", "right"];

/** A gesture whose press went to the program: the release must follow, whatever happens to the pointer. */
interface Report {
  pane: number;
  pointer: number;
  button: Button;
  /** Where the pointer was last and with which modifiers: what a release without an event of its own carries. */
  x: number;
  y: number;
  mods: number;
}

let report: Report | null = null;
/** The pointer of a press that only focused its pane: its moves and release belong to nobody. */
let swallowed: number | null = null;
/** The newest motion that has not been sent yet and the animation frame that sends it. */
let motion: { pane: number; button: Button | "none"; x: number; y: number; mods: number } | null = null;
let motionFrame = 0;

/** Whether the program of a pane asked for the mouse, by the header of the pane's last frame. */
const tracking = (pane: number): boolean => state.frames.get(pane)?.modes.mouse === true;

/** Send one mouse event; a command that fails is the core's to report, not an error of the page. */
function sendMouse(pane: number, action: "press" | "release" | "motion", button: Button | "none", x: number, y: number, mods: number): void {
  void call("mouse", { pane, action, button, x, y, mods }).catch(() => {});
}

/** Forget the waiting motion and its animation frame. */
function dropMotion(): void {
  if (motionFrame !== 0) {
    cancelAnimationFrame(motionFrame);
    motionFrame = 0;
  }
  motion = null;
}

/** Send the waiting motion now. */
function flushMotion(): void {
  const m = motion;
  dropMotion();
  if (m) sendMouse(m.pane, "motion", m.button, m.x, m.y, m.mods);
}

/** Keep the newest motion and send it with the next animation frame; a motion for another pane goes out first. */
function queueMotion(m: NonNullable<typeof motion>): void {
  if (motion && motion.pane !== m.pane) flushMotion();
  motion = m;
  if (motionFrame === 0) {
    motionFrame = requestAnimationFrame(() => {
      motionFrame = 0;
      flushMotion();
    });
  }
}

/** Give the pointer capture back; it may be gone with the pointer already. */
function releaseCapture(canvas: HTMLCanvasElement, pointer: number): void {
  try {
    canvas.releasePointerCapture(pointer);
  } catch {
    // the capture ended with the pointer
  }
}

/** Take the pointer capture: a convenience, moves outside the canvas then stop arriving without it. */
function capture(canvas: HTMLCanvasElement, pointer: number): void {
  try {
    canvas.setPointerCapture(pointer);
  } catch {
    // no capture
  }
}

/**
 * Send the release of the held button, once: the gesture is over afterwards, so a second cancel, a lost capture or
 * the release that follows finds nothing held. The position is the last one seen.
 */
function releaseReport(canvas: HTMLCanvasElement): void {
  const r = report;
  if (!r) return;
  report = null;
  dropMotion();
  sendMouse(r.pane, "release", r.button, r.x, r.y, r.mods);
  releaseCapture(canvas, r.pointer);
}

/** Remember where the pointer is for the gesture; false when its pane has left the layout. */
function follow(r: Report, e: PointerEvent, canvas: HTMLCanvasElement): boolean {
  const pane = state.view?.panes.find(([id]) => id === r.pane);
  if (!pane) return false;
  [r.x, r.y] = devicePoint(e, canvas, r.pane, pane[1]);
  r.mods = modifierBits(e);
  return true;
}

const same = (a: [number, number], b: [number, number]) => a[0] === b[0] && a[1] === b[1];

/** The click count for a press: 1, 2 or 3 inside the window of the click before, else 1 (a fourth click starts over). */
function countClick(e: PointerEvent, pane: number): number {
  const prev = lastClick;
  const near =
    prev !== null &&
    prev.pane === pane &&
    e.timeStamp - prev.t <= CLICK_MS &&
    Math.abs(e.clientX - prev.x) <= CLICK_PX &&
    Math.abs(e.clientY - prev.y) <= CLICK_PX;
  const n = near && prev ? (prev.n >= 3 ? 1 : prev.n + 1) : 1;
  lastClick = { t: e.timeStamp, x: e.clientX, y: e.clientY, n, pane };
  return n;
}

/** Send the pending extend, if it differs from what the core last heard. */
function flush(g: Gesture): void {
  if (g.frame !== 0) {
    cancelAnimationFrame(g.frame);
    g.frame = 0;
  }
  const to = g.pending;
  g.pending = null;
  if (!to || same(to, g.last)) return;
  g.last = to;
  g.moved = true;
  g.sent = select(g.pane, { kind: "extend", x: to[0], y: to[1] });
}

/** Follow the pointer: remember the newest cell and send it with the next animation frame. */
function track(g: Gesture, e: PointerEvent, canvas: HTMLCanvasElement): void {
  const pane = state.view?.panes.find(([id]) => id === g.pane);
  if (!pane) return;
  const rect = canvas.getBoundingClientRect();
  const x = e.clientX - rect.left;
  const y = e.clientY - rect.top;
  const cell = clamp(g.pane, cellAt(state.metrics, pane[1], state.headerPx, x, y));
  if (same(cell, g.pending ?? g.last)) return;
  g.pending = cell;
  if (g.frame === 0) {
    g.frame = requestAnimationFrame(() => {
      g.frame = 0;
      if (gesture === g) flush(g);
    });
  }
}

function endGesture(canvas: HTMLCanvasElement): void {
  const g = gesture;
  if (!g) return;
  if (g.frame !== 0) cancelAnimationFrame(g.frame);
  gesture = null;
  releaseCapture(canvas, g.pointer);
}

/**
 * Start listening to the canvas; `applyView` takes the layout answer of a focus change. Call after `initScene`:
 * the scene may have replaced the canvas element while it chose the renderer.
 */
export function initPointer(applyView: (v: View) => void): void {
  const canvas = sceneCanvas();
  registerAction({
    id: "edit.copy",
    title: "Copy",
    run: () => {
      copySelection();
    },
  });

  canvas.addEventListener("pointerdown", (e) => {
    swallowed = null;
    if (report) releaseReport(canvas); // a press whose release never came
    if (offer(e, canvas)) return;
    const hit = hitAt(e, canvas);
    const rect = canvas.getBoundingClientRect();
    const at = paneAt(state.metrics, state.view, e.clientX - rect.left, e.clientY - rect.top);
    if (!hit || !at) return;

    // The program's gesture: it asked for the mouse and the user is not holding Shift.
    if (tracking(hit.pane) && !e.shiftKey) {
      endGesture(canvas);
      lastClick = null;
      if (state.view && hit.pane !== state.view.focus) {
        // The first click on a pane only focuses it: the program there did not see the pointer arrive.
        swallowed = e.pointerId;
        void call("focus_pane", { pane: hit.pane }).then(applyView);
        return;
      }
      const button = BUTTONS[e.button];
      if (!button) return;
      const [x, y] = devicePoint(e, canvas, hit.pane, at[1]);
      const mods = modifierBits(e);
      dropMotion();
      sendMouse(hit.pane, "press", button, x, y, mods);
      report = { pane: hit.pane, pointer: e.pointerId, button, x, y, mods };
      capture(canvas, e.pointerId);
      return;
    }

    // The user's gesture: only the primary button selects.
    if (e.button !== 0) return;
    endGesture(canvas); // a press whose release never came
    const n = countClick(e, hit.pane);
    const mode: Mode = n === 1 ? "cell" : n === 2 ? "word" : "line";
    if (state.view && hit.pane !== state.view.focus) void call("focus_pane", { pane: hit.pane }).then(applyView);
    const sent = select(hit.pane, { kind: "begin", x: hit.cell[0], y: hit.cell[1], mode });
    gesture = { pane: hit.pane, pointer: e.pointerId, mode, last: hit.cell, pending: null, frame: 0, moved: false, sent };
    capture(canvas, e.pointerId);
  });

  canvas.addEventListener("pointermove", (e) => {
    if (offer(e, canvas)) return;
    const r = report;
    if (r) {
      if (e.pointerId !== r.pointer) return;
      // A move with no button down means the release was lost (no capture outside the window): send it now.
      if (e.buttons === 0) {
        follow(r, e, canvas);
        releaseReport(canvas);
      } else if (follow(r, e, canvas)) {
        queueMotion({ pane: r.pane, button: r.button, x: r.x, y: r.y, mods: r.mods });
      } else {
        releaseReport(canvas); // the pane is gone from the layout
      }
      return;
    }
    const g = gesture;
    if (g) {
      if (e.pointerId === g.pointer) track(g, e, canvas);
      return;
    }
    // Hover: the pane under the pointer, when its program asked for the mouse and the user is not selecting.
    if (e.buttons !== 0 || e.shiftKey || swallowed !== null) return;
    const rect = canvas.getBoundingClientRect();
    const at = paneAt(state.metrics, state.view, e.clientX - rect.left, e.clientY - rect.top);
    if (!at || !tracking(at[0])) return;
    const [x, y] = devicePoint(e, canvas, at[0], at[1]);
    queueMotion({ pane: at[0], button: "none", x, y, mods: modifierBits(e) });
  });

  canvas.addEventListener("pointerup", (e) => {
    const consumed = offer(e, canvas);
    if (e.pointerId === swallowed) swallowed = null;
    const r = report;
    if (r && e.pointerId === r.pointer) {
      // The release is sent whether or not a handler took the event: a press the program saw must end.
      follow(r, e, canvas);
      releaseReport(canvas);
      return;
    }
    const g = gesture;
    if (!g || e.pointerId !== g.pointer) return;
    if (!consumed) {
      track(g, e, canvas); // the release position is the final one
      flush(g);
      // A press that never moved is a click, and a click is not a selection. Word and line stay selected.
      if (g.mode === "cell" && !g.moved) {
        void select(g.pane, { kind: "clear" });
      } else {
        // A selection ended: copy it once the core has the last begin or extend. The highlight stays.
        const pane = g.pane;
        void g.sent.then(() => copyOnSelect(pane));
      }
    }
    endGesture(canvas);
  });

  canvas.addEventListener("pointercancel", (e) => {
    offer(e, canvas);
    if (e.pointerId === swallowed) swallowed = null;
    if (report && e.pointerId === report.pointer) releaseReport(canvas);
    if (gesture && e.pointerId === gesture.pointer) endGesture(canvas);
  });

  canvas.addEventListener("lostpointercapture", (e) => {
    if (report && e.pointerId === report.pointer) releaseReport(canvas);
  });

  // The window lost the keyboard focus: its pointer events stop, so a held button would never be released.
  window.addEventListener("blur", () => releaseReport(canvas));

  // The program that tracks the mouse owns the right button: no browser menu over its pane.
  canvas.addEventListener("contextmenu", (e) => {
    const rect = canvas.getBoundingClientRect();
    const at = paneAt(state.metrics, state.view, e.clientX - rect.left, e.clientY - rect.top);
    if (at && tracking(at[0])) e.preventDefault();
  });

  canvas.addEventListener(
    "wheel",
    (e) => {
      const rect = canvas.getBoundingClientRect();
      const hit = paneAt(state.metrics, state.view, e.clientX - rect.left, e.clientY - rect.top);
      if (!hit) return;
      e.preventDefault();
      const [pane, unit] = hit;
      if (pane !== wheelPane) {
        wheelPane = pane;
        wheelRemainder = 0;
      }
      // With Shift the browser turns a vertical wheel into a sideways one: the core's Shift policy wants the rows.
      const delta = e.deltaY === 0 && e.shiftKey ? e.deltaX : e.deltaY;
      const step = wheelRows(wheelRemainder, delta, e.deltaMode, state.metrics.cellH, gridOf(pane)?.rows);
      wheelRemainder = step.remainder;
      if (step.rows === 0) return;
      const [x, y] = devicePoint(e, canvas, pane, unit);
      void call("wheel", { pane, rows: step.rows, x, y, mods: modifierBits(e) }).catch(() => {});
    },
    { passive: false },
  );
}
