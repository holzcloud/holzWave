// The overlay above the canvas: divider hit areas, the focus box and the selection boxes.
//
// Dividers are a keyed set of elements (by split id) that is moved in place on every render. A divider must
// survive every layout response, because the drag gesture lives on it: the gesture itself listens on `window`
// and sends at most one `set_ratio` per animation frame.
import { call } from "./bridge.js";
import { el, setRect } from "./dom.js";
import type { PxRect } from "./frame.js";
import { dividerRect, gridRect, ordered, paneRect, ratioAt } from "./geometry.js";
import { state } from "./state.js";
import type { Divider, View } from "./types.js";

const root = document.getElementById("overlay") as HTMLDivElement;
// Layers keep the paint order (focus, dividers, selection) whatever the creation order of the elements is.
const focusLayer = el("div", "layer", root);
const dividerLayer = el("div", "layer", root);
const selectionLayer = el("div", "layer", root);

const dividers = new Map<number, HTMLElement>();
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

function syncDividers(view: View): void {
  const live = new Set<number>();
  for (const d of view.dividers) {
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

function renderFocus(view: View): void {
  focusLayer.replaceChildren();
  if (view.panes.length < 2) return;
  for (const [id, r] of view.panes) {
    if (id === view.focus) setRect(el("div", "focus", focusLayer), paneRect(state.metrics, r));
  }
}

function renderSelection(view: View): void {
  selectionLayer.replaceChildren();
  const sel = state.selection;
  if (!sel) return;
  const pane = view.panes.find(([id]) => id === sel.pane);
  if (!pane) return;
  const m = state.metrics;
  const g = gridRect(m, pane[1]);
  const [s, e] = ordered(sel);
  const cols = state.sizes.get(sel.pane)?.cols ?? 0;
  for (let row = s[1]; row <= e[1]; row++) {
    const from = row === s[1] ? s[0] : 0;
    const to = row === e[1] ? e[0] : cols - 1;
    setRect(el("div", "sel", selectionLayer), {
      x: g.x + from * m.cellW,
      y: g.y + row * m.cellH,
      w: (to - from + 1) * m.cellW,
      h: m.cellH,
    });
  }
}

/** Bring the overlay in line with `state.view` and `state.selection`. Dividers are moved, never rebuilt. */
export function renderOverlay(): void {
  const view = state.view;
  if (!view) {
    endDrag();
    for (const node of dividers.values()) node.remove();
    dividers.clear();
    focusLayer.replaceChildren();
    selectionLayer.replaceChildren();
    return;
  }
  syncDividers(view);
  renderFocus(view);
  renderSelection(view);
}
