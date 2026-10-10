// The overlay above the canvas: divider hit areas, the focus box and the selection boxes (from the frame spans).
//
// Every element is keyed (dividers by split id, the focus box as "focus", selection boxes by row) and moved in
// place on every render. A divider must survive every layout response, because the drag gesture lives on it:
// the gesture itself listens on `window` and sends at most one `set_ratio` per animation frame.
import { call } from "./bridge.js";
import { el, setRect } from "./dom.js";
import type { PxRect } from "./frame.js";
import { visibleSpans } from "./frames.js";
import { dividerRect, gridRect, paneRect, ratioAt } from "./geometry.js";
import { state } from "./state.js";
import type { Divider, View } from "./types.js";

const root = document.getElementById("overlay") as HTMLDivElement;
// Layers keep the paint order (focus, dividers, selection) whatever the creation order of the elements is.
const focusLayer = el("div", "layer", root);
const dividerLayer = el("div", "layer", root);
const selectionLayer = el("div", "layer", root);

const dividers = new Map<number, HTMLElement>(); // split id -> divider element
const boxes = new Map<string, HTMLElement>(); // "focus" and "sel-<row>" -> box
let applyView: (v: View) => void = () => {};

/** Hand over the function that takes a layout answer of the core (`set_ratio` answers with the new view). */
export function initOverlay(apply: (v: View) => void): void {
  applyView = apply;
}

// ---- divider drag -------------------------------------------------------------------------------

interface Drag {
  split: number;
  axis: Divider["axis"];
  /** The split's area in CSS px of the canvas, captured once at pointerdown. */
  area: PxRect;
  /** Client position of the canvas origin at pointerdown. */
  left: number;
  top: number;
  pointer: number;
  /** The newest ratio that has not been sent yet. */
  pending: number | null;
  frame: number;
}

let drag: Drag | null = null;
let sent = 0;
let applied = 0;

/** Send the pending ratio; an answer is applied only if no newer answer was applied before it. */
function flush(d: Drag): void {
  if (d.pending === null) return;
  const ratio = d.pending;
  d.pending = null;
  const mine = ++sent;
  void call("set_ratio", { split: d.split, ratio }).then((v) => {
    if (mine > applied) {
      applied = mine;
      applyView(v);
    }
  });
}

function onMove(e: PointerEvent): void {
  const d = drag;
  if (!d || e.pointerId !== d.pointer) return;
  const ratio = ratioAt(d.area, d.axis, e.clientX - d.left, e.clientY - d.top);
  if (!Number.isFinite(ratio)) return;
  d.pending = ratio;
  if (d.frame === 0) {
    d.frame = requestAnimationFrame(() => {
      d.frame = 0;
      if (drag === d) flush(d);
    });
  }
}

function onUp(e: PointerEvent): void {
  const d = drag;
  if (!d || e.pointerId !== d.pointer) return;
  flush(d); // the release position is the final one
  endDrag();
}

function onCancel(e: PointerEvent): void {
  if (drag && e.pointerId === drag.pointer) endDrag();
}

function endDrag(): void {
  if (!drag) return;
  if (drag.frame !== 0) cancelAnimationFrame(drag.frame);
  drag = null;
  window.removeEventListener("pointermove", onMove);
  window.removeEventListener("pointerup", onUp);
  window.removeEventListener("pointercancel", onCancel);
}

function beginDrag(split: number, node: HTMLElement, e: PointerEvent): void {
  if (e.button !== 0 || drag) return;
  const d = state.view?.dividers.find((x) => x.split === split);
  if (!d) return;
  const origin = root.getBoundingClientRect();
  drag = {
    split,
    axis: d.axis,
    area: paneRect(state.metrics, d.area),
    left: origin.left,
    top: origin.top,
    pointer: e.pointerId,
    pending: null,
    frame: 0,
  };
  try {
    node.setPointerCapture(e.pointerId);
  } catch {
    // Capture is a convenience: the window listeners carry the gesture.
  }
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
  window.addEventListener("pointercancel", onCancel);
}

// ---- rendering ----------------------------------------------------------------------------------

function syncDividers(view: View | null): void {
  const live = new Set<number>();
  for (const d of view?.dividers ?? []) {
    live.add(d.split);
    let node = dividers.get(d.split);
    if (!node) {
      const created = el("div", "", dividerLayer);
      created.addEventListener("pointerdown", (e) => beginDrag(d.split, created, e));
      dividers.set(d.split, created);
      node = created;
    }
    const cls = `divider ${d.axis === "Row" ? "row" : "column"}`;
    if (node.className !== cls) node.className = cls;
    setRect(node, dividerRect(state.metrics, d));
  }
  for (const [split, node] of dividers) {
    if (live.has(split)) continue;
    node.remove();
    dividers.delete(split);
  }
  if (drag && !live.has(drag.split)) endDrag();
}

/** Place the box under `key` in `layer`, creating it on first use; the same element is moved on every render. */
function place(keep: Set<string>, key: string, cls: string, layer: HTMLElement, r: PxRect): void {
  keep.add(key);
  let node = boxes.get(key);
  if (!node) {
    node = el("div", cls, layer);
    boxes.set(key, node);
  }
  setRect(node, r);
}

/** Remove the boxes whose key was not placed in this render. */
function sweep(keep: Set<string>): void {
  for (const [key, node] of boxes) {
    if (keep.has(key)) continue;
    node.remove();
    boxes.delete(key);
  }
}

/** The focus box: one element, only while more than one pane is visible. */
function placeFocus(keep: Set<string>, view: View): void {
  if (view.panes.length < 2) return;
  const pane = view.panes.find(([id]) => id === view.focus);
  if (pane) place(keep, "focus", "focus", focusLayer, paneRect(state.metrics, pane[1]));
}

/**
 * One box per selection span of the focused pane, as its latest frame carries them (viewport cells, both ends
 * inclusive). The core owns the selection: nothing is kept here, so a span that scrolls or ends is simply not in
 * the next frame. Boxes are keyed by row and reused.
 */
function placeSelection(keep: Set<string>, view: View): void {
  const pane = view.panes.find(([id]) => id === view.focus);
  const mirror = state.frames.get(view.focus);
  if (!pane || !mirror) return;
  const m = state.metrics;
  const g = gridRect(m, pane[1]);
  for (const [y, x0, x1] of visibleSpans(mirror)) {
    let key = `sel-${y}`;
    for (let i = 1; keep.has(key); i++) key = `sel-${y}-${i}`; // two spans on one row are legal on the wire
    place(keep, key, "sel", selectionLayer, {
      x: g.x + x0 * m.cellW,
      y: g.y + y * m.cellH,
      w: (x1 - x0 + 1) * m.cellW,
      h: m.cellH,
    });
  }
}

/**
 * Bring the overlay in line with `state.view` and the focused pane's selection spans. Every element is looked up by a stable key
 * and moved in place; one is created or removed only when its key appears or vanishes.
 */
export function renderOverlay(): void {
  const view = state.view;
  const keep = new Set<string>();
  if (view) {
    placeFocus(keep, view);
    placeSelection(keep, view);
  }
  sweep(keep);
  syncDividers(view);
}
