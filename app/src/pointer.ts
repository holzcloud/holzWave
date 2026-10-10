// Pointer input on the canvas. The selection is the core's: a press sends `select` begin with the viewport cell
// under the pointer (word on the second click, line on the third), moving sends `extend` at most once per
// animation frame, and a click that never moved clears. Nothing about the selection is kept here.
//
// Handlers can be stacked in front of the gesture (`pushPointerHandler`): links, hints and copy mode take clicks
// without this file knowing them. The wheel scrolls the pane under the pointer. Also owns `edit.copy`.
import { registerAction } from "./actions.js";
import { call } from "./bridge.js";
import { cellAt, paneAt } from "./geometry.js";
import { canvas as sceneCanvas } from "./scene.js";
import { copySelection } from "./selection.js";
import { state } from "./state.js";
import type { View } from "./types.js";

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

function hitAt(e: PointerEvent, canvas: HTMLCanvasElement): PointerHit | null {
  const rect = canvas.getBoundingClientRect();
  const x = e.clientX - rect.left;
  const y = e.clientY - rect.top;
  const at = paneAt(state.metrics, state.view, x, y);
  if (!at) return null;
  return { pane: at[0], cell: clamp(at[0], cellAt(state.metrics, at[1], x, y)) };
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
}

let gesture: Gesture | null = null;
let lastClick: { t: number; x: number; y: number; n: number; pane: number } | null = null;
let wheelRemainder = 0;

type Op =
  | { kind: "begin"; x: number; y: number; mode: Mode }
  | { kind: "extend"; x: number; y: number }
  | { kind: "clear" };

/** Send one selection op; a command that fails is the core's to report, not an error of the page. */
function select(pane: number, op: Op): void {
  void call("select", { pane, op }).catch(() => {});
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
  select(g.pane, { kind: "extend", x: to[0], y: to[1] });
}

/** Follow the pointer: remember the newest cell and send it with the next animation frame. */
function track(g: Gesture, e: PointerEvent, canvas: HTMLCanvasElement): void {
  const pane = state.view?.panes.find(([id]) => id === g.pane);
  if (!pane) return;
  const rect = canvas.getBoundingClientRect();
  const cell = clamp(g.pane, cellAt(state.metrics, pane[1], e.clientX - rect.left, e.clientY - rect.top));
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
  try {
    canvas.releasePointerCapture(g.pointer);
  } catch {
    // the capture ended with the pointer
  }
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
    if (e.button !== 0 || offer(e, canvas)) return;
    const hit = hitAt(e, canvas);
    if (!hit) return;
    endGesture(canvas); // a press whose release never came
    const n = countClick(e, hit.pane);
    const mode: Mode = n === 1 ? "cell" : n === 2 ? "word" : "line";
    if (state.view && hit.pane !== state.view.focus) void call("focus_pane", { pane: hit.pane }).then(applyView);
    select(hit.pane, { kind: "begin", x: hit.cell[0], y: hit.cell[1], mode });
    gesture = { pane: hit.pane, pointer: e.pointerId, mode, last: hit.cell, pending: null, frame: 0, moved: false };
    try {
      canvas.setPointerCapture(e.pointerId);
    } catch {
      // Capture is a convenience: moves outside the canvas then stop arriving.
    }
  });

  canvas.addEventListener("pointermove", (e) => {
    if (offer(e, canvas)) return;
    const g = gesture;
    if (g && e.pointerId === g.pointer) track(g, e, canvas);
  });

  canvas.addEventListener("pointerup", (e) => {
    const consumed = offer(e, canvas);
    const g = gesture;
    if (!g || e.pointerId !== g.pointer) return;
    if (!consumed) {
      track(g, e, canvas); // the release position is the final one
      flush(g);
      // A press that never moved is a click, and a click is not a selection. Word and line stay selected.
      if (g.mode === "cell" && !g.moved) select(g.pane, { kind: "clear" });
    }
    endGesture(canvas);
  });

  canvas.addEventListener("pointercancel", (e) => {
    offer(e, canvas);
    if (gesture && e.pointerId === gesture.pointer) endGesture(canvas);
  });

  canvas.addEventListener(
    "wheel",
    (e) => {
      const rect = canvas.getBoundingClientRect();
      const hit = paneAt(state.metrics, state.view, e.clientX - rect.left, e.clientY - rect.top);
      if (!hit) return;
      e.preventDefault();
      wheelRemainder += e.deltaMode === 0 ? e.deltaY / state.metrics.cellH : e.deltaY * 3;
      const rows = Math.trunc(wheelRemainder);
      if (rows === 0) return;
      wheelRemainder -= rows;
      void call("scroll", { pane: hit[0], rows });
    },
    { passive: false },
  );
}
