// Drag and drop of panes and tabs (DESK-20, DESK-29): a pane is dragged by its header onto the side of another pane
// (splits it there) or its middle (swaps the two), or onto a tab or a workspace chip (moves into it); a tab is dragged
// to another place in the strip or onto a workspace chip (moves into that workspace). The page only works out where the pointer is and
// sends ids to the core, which checks them and the limits; nothing here moves a pane by itself.
//
// Pointer events, not the HTML5 drag API (WKWebView handles that badly). A press on a header or a tab becomes a drag
// after 4 px; before that it is an ordinary click. Escape and a drop on nothing cancel. The terminal gets nothing
// of it: the header presses on the canvas are stopped here, and the outline is a box of the page.
//
// Between windows (DESK-29 part 2): a drag released at least 24 px outside the window sends only the pane's id
// (`release_outside`); the shell reads the pointer and tears the pane or tab off into a window of its own, or hands the
// drop to the window under the pointer, which gets `incoming-drop`, works out the zone here and answers with `attach_*`.
// The page that let a pane go forgets it when the layout without it arrives (scene.ts).
//
// While such a drag is outside its window, the page asks the shell every 60 ms to show the drop outline in the window
// under the pointer (`drag_over`; the shell reads the pointer). That window gets `drag-hover` with a pane id and the
// pointer in its own coordinates, works out the zone and shows the outline; `drag-end` (sent when the pointer leaves it,
// on Escape, on any release, or when a window closes) takes it away, and so does a second without news. Nothing moves
// before the release.
import { registerAction } from "./actions.js";
import { call, listen } from "./bridge.js";
import { el } from "./dom.js";
import {
  attachFor,
  beyondSlop,
  dropZone,
  HOVER_POLL_MS,
  HOVER_TTL_MS,
  leavingPanes,
  neighbour,
  outlineOf,
  outsideBeyond,
  type Under,
  type Zone,
} from "./dropzone.js";
import { paneAt, paneRect } from "./geometry.js";
import { focusInput } from "./keyboard.js";
import { refusalText } from "./labels.js";
import { canvas, markLeaving, unmarkLeaving } from "./scene.js";
import { state } from "./state.js";
import { showNotice } from "./status.js";
import type { View } from "./types.js";

type Source = { kind: "pane"; pane: number } | { kind: "tab"; index: number };

/** A box on the window, in CSS pixels. */
interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** What a drop would do, worked out on every move and sent when the pointer is released. */
interface Drop {
  /** The box shown. */
  box: Box;
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
  if (target?.closest(".close")) return null; // a × is a button, not a handle: a press on it drags nothing
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
  // The items (a tab or chip with its ×) are the targets: a drop over the × is a drop on its tab or chip.
  const tab = indexOf(under, ".tab-item");
  const chip = indexOf(under, ".ws-item");
  if (tab >= 0 && under) {
    const node = under.closest<HTMLElement>(".tab-item") as HTMLElement;
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
    if (view.workspaces[chip]?.active) return null;
    const r = (under.closest(".ws-item") as HTMLElement).getBoundingClientRect();
    const box = { x: r.left, y: r.top, w: r.width, h: r.height };
    // A tab goes to the end of that workspace; a pane next to the focus pane of its shown tab.
    return source.kind === "tab"
      ? { box, send: () => call("move_tab_to_workspace", { tab: source.index, workspace: chip }) }
      : { box, send: () => call("move_pane_to_workspace", { pane: source.pane, workspace: chip }) };
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

function showOutline(box: Box | null): void {
  if (!box) {
    outline?.remove();
    outline = null;
    return;
  }
  outline ??= el("div", "drop-outline", document.body);
  outline.style.left = `${box.x}px`;
  outline.style.top = `${box.y}px`;
  outline.style.width = `${box.w}px`;
  outline.style.height = `${box.h}px`;
}

// ---- the outline in another window while the drag is outside this one -----------------------------------------------

let hoverTimer: ReturnType<typeof setInterval> | undefined;
/** A `drag_over` went out, so a `drag_end` is owed to the window it was shown in. */
let hoverSent = false;
let hoverPending = false;
/** The calls about the drag, one after the other: an end never overtakes a hover, and a release comes after both. */
let hoverChain: Promise<unknown> = Promise.resolve();

function pollHover(): void {
  const view = state.view;
  const gone = press?.active && view ? leavingPanes(press.source, view) : null;
  if (!gone || hoverPending) return; // a slow shell is not asked again before it answered
  hoverSent = true;
  hoverPending = true;
  hoverChain = hoverChain
    .then(() => call("drag_over", { pane: gone.pane, kind: gone.kind }))
    .catch(() => {}) // nothing to show is not an error
    .finally(() => {
      hoverPending = false;
    });
}

/** The pointer is outside the window (past the margin) or not: start or stop asking the shell to show the outline. */
function hoverOutside(outside: boolean): void {
  if (outside && hoverTimer === undefined) {
    pollHover();
    hoverTimer = setInterval(pollHover, HOVER_POLL_MS);
  } else if (!outside) stopHover();
}

/** Stop asking; if the outline was shown anywhere, tell the shell the drag is over. */
function stopHover(): void {
  clearInterval(hoverTimer);
  hoverTimer = undefined;
  if (!hoverSent) return;
  hoverSent = false;
  hoverChain = hoverChain.then(() => call("drag_end")).catch(() => {});
}

function end(): void {
  showOutline(null);
  stopHover();
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
  showOutline(press.drop?.box ?? null);
  hoverOutside(outsideBeyond(e.clientX, e.clientY, window.innerWidth, window.innerHeight));
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
  else if (done.active && outsideBeyond(e.clientX, e.clientY, window.innerWidth, window.innerHeight)) releaseOutside(done.source);
  if (done.active) focusInput();
}

/** The shell refused or failed to give a pane or tab a window: it stays, and the status line says so for a moment. */
function refused(kind: "pane" | "tab", ids: number[], reason: unknown): void {
  unmarkLeaving(ids);
  if (!String(reason).includes("no such window")) showNotice(refusalText(kind, reason)); // that window is going itself
}

/**
 * The pointer was released well outside the window: tell the shell which pane (or which tab, by one of its panes).
 * Panes that go are marked first, so that the layout without them, which can arrive before the answer, forgets them.
 * Nothing moves when the shell cancels (a lone pane or tab, no other place to put it) or fails.
 */
function releaseOutside(source: Source): void {
  const view = state.view;
  const gone = view && leavingPanes(source, view);
  if (!gone) return;
  markLeaving(gone.ids);
  // After the calls of the outline (`end` queued the last one): the shell takes the outline away itself, but a late
  // `drag_over` must not show it again.
  hoverChain
    .then(() => call<{ outcome: string }>("release_outside", { pane: gone.pane, kind: gone.kind }))
    .then((r) => {
      if (r?.outcome !== "cancelled") return;
      unmarkLeaving(gone.ids);
      showNotice(refusalText(gone.kind));
    })
    .catch((e) => refused(gone.kind, gone.ids, e));
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

// ---- tear off without the mouse ----------------------------------------------------------------------------

/** Give the focused pane, or the shown tab, a window of its own, beside this one. A lone pane or tab is refused. */
function moveToNewWindow(kind: "pane" | "tab"): void {
  const view = state.view;
  if (!view) return;
  const ids = kind === "pane" ? [view.focus] : view.panes.map(([id]) => id);
  markLeaving(ids);
  call(kind === "pane" ? "tear_off_pane" : "tear_off_tab", { pane: view.focus, at_cursor: false })
    .then(applyView)
    .catch((e) => refused(kind, ids, e)); // refused: it stays where it is
}

// ---- a drop that comes from another window -------------------------------------------------------------------

const boxOf = (node: Element): Box => {
  const r = node.getBoundingClientRect();
  return { x: r.left, y: r.top, w: r.width, h: r.height };
};

/**
 * What is under the window point (x, y) of this window, for the zone of a drop that comes from another window, and the
 * box of the thing it is over (the part of a pane a drop would take, a tab, a chip or the strip).
 */
function probe(x: number, y: number): { under: Under; box: Box | null } {
  const view = state.view;
  const node = document.elementFromPoint(x, y);
  if (!view || !node) return { under: { part: "none" }, box: null };
  const tab = indexOf(node, ".tab-item");
  if (tab >= 0) return { under: { part: "tab", index: tab }, box: boxOf(node.closest(".tab-item") as Element) };
  const chip = indexOf(node, ".ws-item");
  if (chip >= 0) return { under: { part: "chip", index: chip }, box: boxOf(node.closest(".ws-item") as Element) };
  const bar = node.closest("#bar");
  if (bar) return { under: { part: "bar" }, box: boxOf(bar) };
  const c = canvas().getBoundingClientRect();
  if (x < c.left || x >= c.right || y < c.top || y >= c.bottom) return { under: { part: "none" }, box: null };
  const at = paneAt(state.metrics, view, x - c.left, y - c.top);
  if (!at) return { under: { part: "none" }, box: null };
  const rect = paneRect(state.metrics, at[1]);
  const zone = dropZone(rect, x - c.left, y - c.top);
  const part = zone && outlineOf(rect, zone);
  return {
    under: { part: "pane", pane: at[0], zone },
    box: part && { x: c.left + part.x, y: c.top + part.y, w: part.w, h: part.h },
  };
}

const underAt = (x: number, y: number): Under => probe(x, y).under;

/** The outline for another window's drag, or none; it goes by itself when nothing renews it. */
let hoverTtl: ReturnType<typeof setTimeout> | undefined;
function showHover(box: Box | null): void {
  clearTimeout(hoverTtl);
  hoverTtl = box ? setTimeout(() => showOutline(null), HOVER_TTL_MS) : undefined;
  showOutline(box);
}

/** `drag-hover`: a pane or tab of another window is dragged over this one: show where it would land, move nothing. */
function onDragHover(p: { kind: string; pane: number; x: number; y: number }): void {
  const view = state.view;
  if (typeof p !== "object" || p === null || !view) return;
  if (p.kind !== "pane" && p.kind !== "tab") return;
  if (!Number.isSafeInteger(p.pane) || !Number.isFinite(p.x) || !Number.isFinite(p.y)) return;
  const workspace = Math.max(0, view.workspaces.findIndex((w) => w.active));
  const { under, box } = probe(p.x, p.y);
  if (!attachFor(p.kind, p.pane, under, view.tabs, workspace)) return showHover(null); // over no zone: no outline
  // A tab becomes a tab of the workspace of the chip it is over, else of the shown one: the strip is where it lands.
  const bar = document.getElementById("bar");
  showHover(p.kind === "tab" && under.part !== "chip" && bar ? boxOf(bar) : box);
}

/** `incoming-drop`: a pane or tab of another window was released over this one. Answer where it goes, or say nothing. */
function onIncomingDrop(p: { kind: string; pane: number; x: number; y: number }): void {
  showHover(null); // the drop is made: the outline of the preview has done its job
  const view = state.view;
  if (typeof p !== "object" || p === null || !view) return;
  if (p.kind !== "pane" && p.kind !== "tab") return;
  if (!Number.isSafeInteger(p.pane) || !Number.isFinite(p.x) || !Number.isFinite(p.y)) return;
  const workspace = Math.max(0, view.workspaces.findIndex((w) => w.active));
  const answer = attachFor(p.kind, p.pane, underAt(p.x, p.y), view.tabs, workspace);
  if (!answer) return; // over no zone: nothing is sent and the drop lapses in the shell
  // Dropped on the middle of a pane the two panes trade places: the one that was here goes to the other window.
  const swapped = answer.cmd === "attach_pane" && "edge" in answer.args && answer.args.edge === "center" ? [answer.args.target] : [];
  markLeaving(swapped);
  call(answer.cmd, answer.args)
    .then((v) => {
      applyView(v);
      if (swapped.some((id) => v.panes.some(([pane]) => pane === id))) unmarkLeaving(swapped); // refused: it stays
    })
    .catch(() => unmarkLeaving(swapped));
}

/** Listen for drops and drags that come from other windows. Call once, with the other listeners, before `window_ready`. */
export async function initDragDropEvents(): Promise<void> {
  await listen("incoming-drop", onIncomingDrop);
  await listen("drag-hover", onDragHover);
  await listen("drag-end", () => showHover(null));
}

/** Listen for drags and register the five actions. `apply` takes the core's answer. Call once, after `initScene`. */
export function initDragDrop(apply: (v: View) => void): void {
  applyView = apply;
  // Capture, so a header press on the canvas is seen before the selection gesture.
  document.addEventListener("pointerdown", onDown, true);
  registerAction({ id: "pane.swap_next", title: "Swap with next pane", run: swapWithNext });
  registerAction({ id: "pane.move_next_tab", title: "Move pane to next tab", run: () => moveToTab(1) });
  registerAction({ id: "pane.move_prev_tab", title: "Move pane to previous tab", run: () => moveToTab(-1) });
  registerAction({ id: "pane.move_to_new_window", title: "Move pane to new window", run: () => moveToNewWindow("pane") });
  registerAction({ id: "tab.move_to_new_window", title: "Move tab to new window", run: () => moveToNewWindow("tab") });
}
