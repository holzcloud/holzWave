// The Context section of the sidebar (docs/design.md 6.11): one row for each tool the core listed, with its mark, its name and
// the selection as text. The core resolves the rows for the focused pane (the pane's own selection, else the pick of the tool,
// else what the tool itself is set to); this module asks for them when it should, keeps the last answer and draws it with
// `textContent`. It decides nothing about a context and builds no name, argument or program. The status line and the pane
// headers read the same answer, so the three places cannot disagree.
import { call } from "./bridge.js";
import { el, setAttr, setText } from "./dom.js";
import { glyphOf, readContext, rowWords, selectionText, selectionValues } from "./integratestate.js";
import type { ContextRow, ContextSummary, ToolId } from "./integratestate.js";
import { loadTools, onHint, onTools, toolOf } from "./integratetools.js";
import { state } from "./state.js";

/** What the module needs from the page, handed over so that it imports neither the sidebar nor the status line. */
export interface ContextDeps {
  /** The answer changed: draw the status line and the pane headers again. */
  changed(): void;
}

let deps: ContextDeps = { changed: () => {} };
let summary: ContextSummary = { rows: [], panes: [] };
/** The pane the answer in hand was asked for. */
let askedFor: number | null = null;
/** The last ask failed: the rows in hand are old. */
let failing = false;
let started = false;
let enabled = true;
let section: HTMLElement | null = null;
let box: HTMLElement | null = null;
const rowEls = new Map<ToolId, HTMLElement>();

/** Whether the surfaces are drawn: not on a phone, which has no sidebar and no status segments. */
function onDesktop(): boolean {
  return state.os !== "ios" && state.os !== "android";
}

export function contextRows(): readonly ContextRow[] {
  return summary.rows;
}

// ---- asking -------------------------------------------------------------------------------------------------------------

let inflight = false;
let again = false;

/** Ask the core for the rows of the focused pane. One ask at a time: one that comes while another is on the way follows it. */
async function ask(refresh: boolean): Promise<void> {
  if (!enabled) return;
  if (inflight) {
    again = true;
    return;
  }
  inflight = true;
  const pane = state.view?.focus ?? null;
  try {
    const next = readContext(await call<unknown>("integration_context", { pane, refresh }));
    summary = next;
    askedFor = pane;
    failing = false;
  } catch {
    // The core could not be reached: the last rows stay, and they are said to be old.
    failing = true;
  } finally {
    inflight = false;
  }
  draw();
  deps.changed();
  if (again) {
    again = false;
    void ask(false);
  }
}

/** The key of the layout the page last asked about. */
let layoutKey = "";

/** The layout was drawn: ask again when the focus or the panes changed. Cheap enough for a render path; it asks the core only on a change. */
export function followLayout(): void {
  if (!enabled || !started) return;
  const view = state.view;
  if (!view) return;
  const key = `${view.focus}:${view.panes.map(([id]) => id).join(",")}`;
  if (key === layoutKey) return;
  layoutKey = key;
  void ask(false);
}

// ---- the section --------------------------------------------------------------------------------------------------------

/** The place in the sidebar the section goes into (between Hosts and History). The section itself exists only while there is something to show. */
let slot: HTMLElement | null = null;

/** Hand over the empty place the sidebar made for the section. Nothing is made and nothing is asked here. */
export function mountContextSection(place: HTMLElement): void {
  slot = place;
}

/** Make the section: the head with `Context` and the button that reads again, and the rows. */
function makeSection(place: HTMLElement): void {
  const sec = el("section", "sb-section cx-section", place);
  const head = el("div", "sb-head label", sec);
  el("span", "", head).textContent = "Context";
  const refresh = el("button", "cx-refresh", head);
  refresh.type = "button";
  refresh.textContent = "↻";
  refresh.title = "Read the context again";
  refresh.setAttribute("aria-label", "Read the context again");
  refresh.addEventListener("click", () => void ask(true));
  box = el("div", "cx-rows", sec);
  section = sec;
}

/** Take the section away, so that a sidebar with nothing to show is exactly the one without this feature. */
function removeSection(): void {
  section?.remove();
  section = null;
  box = null;
  rowEls.clear();
}

function rowFor(tool: ToolId): HTMLElement {
  let node = rowEls.get(tool);
  if (node) return node;
  node = el("div", "cx-row");
  node.dataset.tool = tool;
  const head = el("div", "cx-head", node);
  el("span", "cx-mark", head).setAttribute("aria-hidden", "true");
  el("span", "cx-label", head);
  el("span", "cx-words", head);
  el("div", "cx-sel", node);
  rowEls.set(tool, node);
  return node;
}

function drawRow(row: ContextRow, label: string): void {
  const node = rowFor(row.tool);
  setAttr(node, "data-state", row.state);
  setAttr(node, "data-stale", row.stale || failing ? "true" : null);
  setText(node.querySelector(".cx-mark") as HTMLElement, glyphOf(row.tool));
  setText(node.querySelector(".cx-label") as HTMLElement, label);
  const words = node.querySelector(".cx-words") as HTMLElement;
  const list = rowWords(row);
  words.replaceChildren();
  for (const w of list) {
    const span = el("span", "cx-word", words);
    span.dataset.kind = w.kind;
    span.textContent = w.text;
  }
  const sel = node.querySelector(".cx-sel") as HTMLElement;
  setText(sel, selectionText(row));
  sel.title = selectionValues(row).join(" · ");
}

/** Bring the section in line with the last answer. */
function draw(): void {
  const rows = summary.rows;
  if (!enabled || rows.length === 0) {
    removeSection();
    return;
  }
  if (!slot) return;
  if (!section || !box) makeSection(slot);
  if (!section || !box) return;
  setAttr(section, "data-stale", failing ? "true" : null);
  const live = new Set<ToolId>();
  for (const row of rows) {
    live.add(row.tool);
    drawRow(row, labelOf(row.tool));
    const node = rowFor(row.tool);
    if (node.parentElement !== box) box.append(node);
  }
  for (const [tool, node] of rowEls) {
    if (live.has(tool)) continue;
    node.remove();
    rowEls.delete(tool);
  }
  // The order of the core is kept.
  rows.forEach((row, i) => {
    const node = rowEls.get(row.tool);
    if (node && box && box.children[i] !== node) box.insertBefore(node, box.children[i] ?? null);
  });
}

/** The name of a tool: the one the tool list carries, else a fixed one for the four the core knows. */
function labelOf(tool: ToolId): string {
  return toolOf(tool)?.label ?? NAMES[tool];
}
const NAMES: Readonly<Record<ToolId, string>> = { kube: "Kubernetes", talos: "Talos", azure: "Azure", gam: "GAM" };

// ---- start --------------------------------------------------------------------------------------------------------------

/** Start asking: the core's events and the layout make the page ask; a phone asks nothing. Call once, after the sidebar is made. */
export function initIntegrationContext(handed: ContextDeps): void {
  deps = handed;
  enabled = onDesktop();
  started = true;
  if (!enabled) return;
  onHint((h) => {
    if (h.kind === "context" || h.kind === "tools" || h.kind === "held" || h.gap) void ask(false);
  });
  onTools(() => draw());
  void loadTools();
  followLayout();
}
