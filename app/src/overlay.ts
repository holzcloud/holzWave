// The overlay above the canvas: the pane headers (with the focus line), divider hit areas, the search match boxes
// and the selection boxes (from the frame spans), and the cell boxes other features ask for (link underline, hint
// labels, copy cursor).
//
// Every element is keyed (headers by pane id, dividers by split id, selection boxes by row) and moved in place on
// every render. A divider must survive every layout response, because the drag gesture lives on it:
// the gesture itself listens on `window` and sends at most one `set_ratio` per animation frame.
import { call } from "./bridge.js";
import { el, setAttr, setRect, setText } from "./dom.js";
import type { PxRect } from "./frame.js";
import { visibleMatches, visibleSpans } from "./frames.js";
import { dividerRect, gridRect, paneRect, ratioAt } from "./geometry.js";
import { initPaneHeads, renderPaneHeads } from "./panehead.js";
import { state } from "./state.js";
import type { Divider, View } from "./types.js";

const root = document.getElementById("overlay") as HTMLDivElement;
// Layers keep the paint order (headers, dividers, selection) whatever the creation order of the elements is.
const headLayer = el("div", "layer", root);
const dividerLayer = el("div", "layer", root);
const matchLayer = el("div", "layer", root);
const selectionLayer = el("div", "layer", root);
const markLayer = el("div", "layer", root);

const dividers = new Map<number, HTMLElement>(); // split id -> divider element
const boxes = new Map<string, HTMLElement>(); // "sel-<row>", "match-<n>", "<owner>:<key>" -> box
let applyView: (v: View) => void = () => {};

/** Hand over the function that takes a layout answer of the core (`set_ratio` answers with the new view). */
export function initOverlay(apply: (v: View) => void): void {
  applyView = apply;
}

initPaneHeads(headLayer);

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
function place(keep: Set<string>, key: string, cls: string, layer: HTMLElement, r: PxRect): HTMLElement {
  keep.add(key);
  let node = boxes.get(key);
  if (!node) {
    node = el("div", cls, layer);
    boxes.set(key, node);
  } else if (node.className !== cls) {
    node.className = cls; // a match becomes the current one in place
  }
  setRect(node, r);
  return node;
}

/** Remove the boxes whose key was not placed in this render. */
function sweep(keep: Set<string>): void {
  for (const [key, node] of boxes) {
    if (keep.has(key)) continue;
    node.remove();
    boxes.delete(key);
  }
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
  const g = gridRect(m, pane[1], state.headerPx);
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
 * One box per search match of the focused pane, as its latest frame carries them. The current match has class
 * `current` besides `match`; the stylesheet gives it an outline so it does not stand out by colour alone.
 */
function placeMatches(keep: Set<string>, view: View): void {
  const pane = view.panes.find(([id]) => id === view.focus);
  const mirror = state.frames.get(view.focus);
  if (!pane || !mirror) return;
  const m = state.metrics;
  const g = gridRect(m, pane[1], state.headerPx);
  visibleMatches(mirror).forEach(([y, x0, x1, current], i) => {
    place(keep, `match-${i}`, current ? "match current" : "match", matchLayer, {
      x: g.x + x0 * m.cellW,
      y: g.y + y * m.cellH,
      w: (x1 - x0 + 1) * m.cellW,
      h: m.cellH,
    });
  });
}

/**
 * The copy cursor of the focused pane: the cell the core's copy mode is on, from the frame header. The box has a
 * double border, so it differs from the selection under it by more than colour. It is null in the header while the
 * cursor is scrolled out of sight, and then nothing is drawn.
 */
function placeCopyCursor(keep: Set<string>, view: View): void {
  const pane = view.panes.find(([id]) => id === view.focus);
  const mirror = state.frames.get(view.focus);
  const c = mirror?.copyCursor;
  if (!pane || !mirror || !Array.isArray(c)) return;
  const [x, y] = c;
  if (!Number.isInteger(x) || !Number.isInteger(y) || x < 0 || y < 0 || x >= mirror.cols || y >= mirror.rows) return;
  const m = state.metrics;
  const g = gridRect(m, pane[1], state.headerPx);
  place(keep, "copycursor", "copycursor", markLayer, {
    x: g.x + x * m.cellW,
    y: g.y + y * m.cellH,
    w: m.cellW,
    h: m.cellH,
  });
}

// ---- boxes of other features ----------------------------------------------------------------------------

/** A box over viewport cells of a pane (both ends inclusive) that a feature asks the overlay to draw. */
export interface CellBox {
  /** Unique within the owner; the same key keeps the same element from one render to the next. */
  key: string;
  /** Class names of the box (the owner's stylesheet styles them). */
  cls: string;
  pane: number;
  y: number;
  x0: number;
  x1: number;
  /** Text inside the box, as `textContent`. */
  text?: string;
  /** Value of the `data-hint` attribute, for the hint labels. */
  hint?: string;
}

const owned = new Map<string, CellBox[]>();
const hooks: (() => void)[] = [];
let rendering = false;

/** Replace the boxes of `owner` (an empty list removes them) and redraw. */
export function setCellBoxes(owner: string, list: CellBox[]): void {
  if (list.length === 0) owned.delete(owner);
  else owned.set(owner, list);
  renderOverlay();
}

/**
 * Call `fn` at the start of every overlay render, before anything is placed: a feature that follows the view (the
 * search bar, copy mode) updates itself there, and a `setCellBoxes` it makes in the hook is drawn by the same render.
 * The returned function removes the hook.
 */
export function onOverlayRender(fn: () => void): () => void {
  hooks.push(fn);
  return () => {
    const at = hooks.indexOf(fn);
    if (at >= 0) hooks.splice(at, 1);
  };
}

/** Place the boxes the features asked for, over the grid of their pane when it is visible. */
function placeOwned(keep: Set<string>, view: View): void {
  const m = state.metrics;
  for (const [owner, list] of owned) {
    for (const b of list) {
      const pane = view.panes.find(([id]) => id === b.pane);
      if (!pane || !Number.isInteger(b.y) || !Number.isInteger(b.x0) || !Number.isInteger(b.x1)) continue;
      const g = gridRect(m, pane[1], state.headerPx);
      const node = place(keep, `${owner}:${b.key}`, `cellbox ${b.cls}`, markLayer, {
        x: g.x + b.x0 * m.cellW,
        y: g.y + b.y * m.cellH,
        w: (b.x1 - b.x0 + 1) * m.cellW,
        h: m.cellH,
      });
      setText(node, b.text ?? "");
      setAttr(node, "data-hint", b.hint ?? null);
    }
  }
}

/**
 * Bring the overlay in line with `state.view` and the focused pane's header (selection spans, search matches). Every
 * element is looked up by a stable key and moved in place; one is created or removed only when its key appears or
 * vanishes. A render a hook asks for while one is running is the running one.
 */
export function renderOverlay(): void {
  if (rendering) return;
  rendering = true;
  try {
    for (const hook of [...hooks]) hook();
    const view = state.view;
    const keep = new Set<string>();
    if (view) {
      placeMatches(keep, view);
      placeSelection(keep, view);
      placeCopyCursor(keep, view);
      placeOwned(keep, view);
    }
    sweep(keep);
    syncDividers(view);
    renderPaneHeads();
  } finally {
    rendering = false;
  }
}
