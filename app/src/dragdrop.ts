// Drag and drop of panes and tabs (DESK-20, DESK-29): a pane is dragged by its header onto the side of another pane
// (splits it there) or its middle (swaps the two), or onto a tab (moves into it); a tab is dragged to another place
// in the strip or onto a workspace chip (moves into that workspace). The page only works out where the pointer is and
// sends ids to the core, which checks them and the limits; nothing here moves a pane by itself.
//
// Pointer events, not the HTML5 drag API (WKWebView handles that badly). A press on a header or a tab becomes a drag
// after 4 px; before that it is an ordinary click. Escape and a drop on nothing cancel. The terminal gets nothing
// of it: the header presses on the canvas are stopped here, and the outline is a box of the page.
import { registerAction } from "./actions.js";
import { call } from "./bridge.js";
import { el } from "./dom.js";
import { beyondSlop, dropZone, neighbour, outlineOf, type Zone } from "./dropzone.js";
import { paneAt, paneRect } from "./geometry.js";
import { focusInput } from "./keyboard.js";
import { canvas } from "./scene.js";
import { state } from "./state.js";
import type { View } from "./types.js";

type Source = { kind: "pane"; pane: number } | { kind: "tab"; index: number };

/** What a drop would do, worked out on every move and sent when the pointer is released. */
interface Drop {
  /** The box shown, in CSS pixels of the window. */
  box: { x: number; y: number; w: number; h: number };
  send: () => Promise<View>;
}

interface Press {
  source: Source;
  pointer: number;
  x: number;
  y: number;
  active: boolean;
  drop: Drop | null;
}

let applyView: (v: View) => void = () => {};
let press: Press | null = null;
let outline: HTMLElement | null = null;

function indexOf(node: Element | null, selector: string): number {
  const found = node?.closest<HTMLElement>(selector);
  const n = Number(found?.dataset.index);
  return found && Number.isInteger(n) ? n : -1;
}

/** The pane whose header strip holds the window point, or -1. Only a press on the canvas needs this: the title is a node. */
function headerPaneAt(x: number, y: number): number {
  const box = canvas().getBoundingClientRect();
  const at = paneAt(state.metrics, state.view, x - box.left, y - box.top);
  if (!at) return -1;
  const r = paneRect(state.metrics, at[1]);
  return y - box.top < r.y + state.headerPx ? at[0] : -1;
}

function sourceOf(e: PointerEvent): Source | null {
  const target = e.target instanceof Element ? e.target : null;
  const tab = indexOf(target, ".tab");
  if (tab >= 0) return { kind: "tab", index: tab };
  const head = target?.closest<HTMLElement>(".panehead");
  if (head) {
    const pane = Number(head.dataset.pane);
    if (Number.isInteger(pane)) return { kind: "pane", pane };
  }
  if (target === canvas()) {
    const pane = headerPaneAt(e.clientX, e.clientY);
    if (pane >= 0) return { kind: "pane", pane };
  }
  return null;
}

/** What dropping `source` at the window point would do, or null when nothing. */
function dropAt(source: Source, x: number, y: number): Drop | null {
  const view = state.view;
  if (!view) return null;
  const under = document.elementFromPoint(x, y);
  const tab = indexOf(under, ".tab");
  const chip = indexOf(under, ".ws-chip");
  if (tab >= 0 && under) {
    const node = under.closest<HTMLElement>(".tab") as HTMLElement;
    const r = node.getBoundingClientRect();
    const box = { x: r.left, y: r.top, w: r.width, h: r.height };
    if (source.kind === "tab") {
      return tab === source.index ? null : { box, send: () => call("move_tab", { from: source.index, to: tab }) };
    }
    const into = view.tabs[tab];
    if (!into || into.active) return null;
    return { box, send: () => call("move_pane", { pane: source.pane, target: into.focus, edge: "right" }) };
  }
  if (chip >= 0 && under) {
    if (source.kind !== "tab" || view.workspaces[chip]?.active) return null;
    const r = (under.closest(".ws-chip") as HTMLElement).getBoundingClientRect();
    return {
      box: { x: r.left, y: r.top, w: r.width, h: r.height },
      send: () => call("move_tab_to_workspace", { tab: source.index, workspace: chip }),
    };
  }
  if (source.kind !== "pane") return null;
  const c = canvas().getBoundingClientRect();
  if (x < c.left || x >= c.right || y < c.top || y >= c.bottom) return null;
  const at = paneAt(state.metrics, view, x - c.left, y - c.top);
  if (!at || at[0] === source.pane) return null;
  const rect = paneRect(state.metrics, at[1]);
  const zone: Zone | null = dropZone(rect, x - c.left, y - c.top);
  if (!zone) return null;
  const part = outlineOf(rect, zone);
  const target = at[0];
  return {
    box: { x: c.left + part.x, y: c.top + part.y, w: part.w, h: part.h },
    send: () => call("move_pane", { pane: source.pane, target, edge: zone }),
  };
}

function showOutline(drop: Drop | null): void {
  if (!drop) {
    outline?.remove();
    outline = null;
    return;
  }
  outline ??= el("div", "drop-outline", document.body);
  outline.style.left = `${drop.box.x}px`;
  outline.style.top = `${drop.box.y}px`;
  outline.style.width = `${drop.box.w}px`;
  outline.style.height = `${drop.box.h}px`;
}

function end(): void {
  showOutline(null);
  document.documentElement.classList.remove("dragging");
  window.removeEventListener("pointermove", onMove, true);
  window.removeEventListener("pointerup", onUp, true);
  window.removeEventListener("pointercancel", onCancel, true);
  window.removeEventListener("keydown", onKey, true);
  press = null;
}

/** The click that follows a drag must not select the tab or chip the pointer was released over. */
function swallowNextClick(): void {
  const stop = (e: Event): void => {
    e.stopPropagation();
    e.preventDefault();
  };
  window.addEventListener("click", stop, { capture: true, once: true });
  setTimeout(() => window.removeEventListener("click", stop, true), 0);
}

function onMove(e: PointerEvent): void {
  if (!press || e.pointerId !== press.pointer) return;
  if (!press.active) {
    if (!beyondSlop(press, { x: e.clientX, y: e.clientY })) return;
    press.active = true;
    document.documentElement.classList.add("dragging");
  }
  e.preventDefault();
  press.drop = dropAt(press.source, e.clientX, e.clientY);
  showOutline(press.drop);
}

function onUp(e: PointerEvent): void {
  if (!press || e.pointerId !== press.pointer) return;
  const done = press;
  if (done.active) {
    e.preventDefault();
    done.drop = dropAt(done.source, e.clientX, e.clientY);
    swallowNextClick();
  }
  end();
  if (done.active && done.drop) void done.drop.send().then(applyView);
  if (done.active) focusInput();
}

function onCancel(e: PointerEvent): void {
  if (press && e.pointerId === press.pointer) end();
}

function onKey(e: KeyboardEvent): void {
  if (e.key !== "Escape") return;
  e.preventDefault();
  e.stopPropagation();
  if (press?.active) swallowNextClick();
  end();
}

function onDown(e: PointerEvent): void {
  if (e.button !== 0 || press || !state.view) return;
  const source = sourceOf(e);
  if (!source) return;
  if (e.target === canvas()) {
    // A press on a header the canvas sees: it must not start a selection, but it still focuses the pane.
    e.stopPropagation();
    if (source.kind === "pane" && source.pane !== state.view.focus) void call("focus_pane", { pane: source.pane }).then(applyView);
  }
  press = { source, pointer: e.pointerId, x: e.clientX, y: e.clientY, active: false, drop: null };
  window.addEventListener("pointermove", onMove, true);
  window.addEventListener("pointerup", onUp, true);
  window.addEventListener("pointercancel", onCancel, true);
  window.addEventListener("keydown", onKey, true);
}

// ---- actions without the mouse -------------------------------------------------------------------------------

function swapWithNext(): void {
  const view = state.view;
  if (!view) return;
  const next = neighbour(view.panes.map(([id]) => id), view.focus, 1);
  if (next !== undefined) void call("move_pane", { pane: view.focus, target: next, edge: "center" }).then(applyView);
}

function moveToTab(step: 1 | -1): void {
  const view = state.view;
  if (!view) return;
  const here = view.tabs.findIndex((t) => t.active);
  const next = neighbour(view.tabs.map((_, i) => i), here, step);
  const target = next === undefined ? undefined : view.tabs[next]?.focus;
  if (target !== undefined) void call("move_pane", { pane: view.focus, target, edge: "right" }).then(applyView);
}

/** Listen for drags and register the three actions. `apply` takes the core's answer. Call once, after `initScene`. */
export function initDragDrop(apply: (v: View) => void): void {
  applyView = apply;
  // Capture, so a header press on the canvas is seen before the selection gesture.
  document.addEventListener("pointerdown", onDown, true);
  registerAction({ id: "pane.swap_next", title: "Swap with next pane", run: swapWithNext });
  registerAction({ id: "pane.move_next_tab", title: "Move pane to next tab", run: () => moveToTab(1) });
  registerAction({ id: "pane.move_prev_tab", title: "Move pane to previous tab", run: () => moveToTab(-1) });
}
