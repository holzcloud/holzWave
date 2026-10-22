// The Context section of the sidebar (docs/design.md 6.11): one row for each tool the core listed, with its mark, its name and
// the selection as text. The core resolves the rows for the focused pane (the pane's own selection, else the pick of the tool,
// else what the tool itself is set to); this module asks for them when it should, keeps the last answer and draws it with
// `textContent`. It decides nothing about a context and builds no name, argument or program: a choice is a key of a list the
// core sent, and the reach of a choice (a pane, or the pick that new blocks start from) is read from the core's lists. The
// status line and the pane headers read the same answer, so the three places cannot disagree.
import { call } from "./bridge.js";
import { paneBlocks } from "./blockstate.js";
import { el, setAttr, setText } from "./dom.js";
import {
  cutCell, glyphOf, nextSelection, paneSelection, reachPane, reachWords, readContext, readFault, readLaunchAnswer, rowKinds, rowWords,
  selectionOf, selectionText, selectionValues, valueOf,
} from "./integratestate.js";
import type { ContextRow, ContextSummary, SelectKind, ToolEntry, ToolId } from "./integratestate.js";
import { openPicker, pathRow } from "./integratepick.js";
import type { Picker } from "./integratepick.js";
import { loadTools, onHint, onTools, setTools, toolOf } from "./integratetools.js";
import { focusInput } from "./keyboard.js";
import { state } from "./state.js";
import type { View } from "./types.js";

/** What the module needs from the page, handed over so that it imports neither the sidebar nor the status line. */
export interface ContextDeps {
  /** The answer changed: draw the status line and the pane headers again. */
  changed(): void;
  /** Show a layout (the answer of a launch). */
  applyView(v: View): void;
}

/** The verb of a Kubernetes row's `Shell` button: a read verb that opens a terminal, which the core lists. */
const SHELL_VERB = "shell";

let deps: ContextDeps = { changed: () => {}, applyView: () => {} };
let summary: ContextSummary = { rows: [], panes: [] };
/** The pane the answer in hand was asked for. */
let askedFor: number | null = null;
/** The last ask failed: the rows in hand are old. */
let failing = false;
let started = false;
let enabled = true;
let slot: HTMLElement | null = null;
let section: HTMLElement | null = null;
let box: HTMLElement | null = null;
let line: HTMLElement | null = null;
const rowEls = new Map<ToolId, HTMLElement>();

/** Whether the surfaces are drawn: not on a phone, which has no sidebar and no status segments. */
function onDesktop(): boolean {
  return state.os !== "ios" && state.os !== "android";
}

export function contextRows(): readonly ContextRow[] {
  return summary.rows;
}

export function contextPanes(): ContextSummary["panes"] {
  return summary.panes;
}

/** The pane the rows in hand were asked for, and whether the page has moved on since. */
export function contextAskedFor(): number | null {
  return askedFor;
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
  // What a choice said was about the pane that had the focus; an open list was read for it.
  closePicker();
  say("", false);
  draw();
  void ask(false);
}

// ---- the line under the section -----------------------------------------------------------------------------------------

let lineText = "";
let lineWarn = false;

/** Say what a choice did, or why it did not. An empty text clears the line. */
function say(text: string, warn: boolean): void {
  lineText = text;
  lineWarn = warn && text !== "";
  if (line) {
    setText(line, lineText);
    setAttr(line, "data-tone", lineWarn ? "warn" : null);
  }
}

// ---- the section --------------------------------------------------------------------------------------------------------

/** Hand over the empty place the sidebar made for the section. Nothing is made and nothing is asked here. */
export function mountContextSection(place: HTMLElement): void {
  slot = place;
}

/** Make the section: the head with `Context` and the button that reads again, the rows and the line under them. */
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
  line = el("p", "cx-line", sec);
  line.setAttribute("role", "status");
  setText(line, lineText);
  setAttr(line, "data-tone", lineWarn ? "warn" : null);
  section = sec;
}

/** Take the section away, so that a sidebar with nothing to show is exactly the one without this feature. */
function removeSection(): void {
  closePicker();
  section?.remove();
  section = null;
  box = null;
  line = null;
  rowEls.clear();
}

/** The values of a row the person can choose: what the tool selects, at least the first. */
function chooseable(row: ContextRow, entry: ToolEntry | null): readonly SelectKind[] {
  const all = rowKinds(row.tool);
  const listed = entry ? all.filter((k) => entry.selects.includes(k)) : all.filter((k) => valueOf(row, k) !== undefined);
  return listed.length > 0 ? listed : all.slice(0, 1);
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
  el("span", "cx-acts", head);
  el("div", "cx-sel", node);
  rowEls.set(tool, node);
  return node;
}

function button(parent: HTMLElement, cls: string, text: string): HTMLButtonElement {
  const b = el("button", cls, parent);
  b.type = "button";
  b.dataset.field = "";
  b.textContent = text;
  return b;
}

/** The tool is missing or cannot be used: the row offers the path of the program. */
function needsPath(row: ContextRow, entry: ToolEntry | null): boolean {
  return row.state === "not_found" || entry?.state === "unusable";
}

/** The values of a row as buttons; they are rebuilt only when their shape changes, so the keyboard stays where it was. */
function drawSel(node: HTMLElement, row: ContextRow, entry: ToolEntry | null): void {
  const sel = node.querySelector(".cx-sel") as HTMLElement;
  sel.title = selectionValues(row).join(" · ");
  if (row.state === "not_found") {
    delete sel.dataset.shape;
    setText(sel, selectionText(row));
    return;
  }
  const kinds = chooseable(row, entry);
  const shape = kinds.join(",");
  if (sel.dataset.shape !== shape) {
    const active = document.activeElement instanceof HTMLElement && sel.contains(document.activeElement) ? document.activeElement.dataset.select : undefined;
    sel.replaceChildren();
    sel.dataset.shape = shape;
    kinds.forEach((kind, i) => {
      if (i > 0) el("span", "cx-sep", sel).textContent = " · ";
      const b = button(sel, i === 0 ? "cx-value" : "cx-value cx-small", "");
      b.dataset.select = kind;
      b.setAttribute("aria-haspopup", "listbox");
      b.setAttribute("aria-expanded", "false");
      b.addEventListener("click", () => togglePicker(row.tool, kind, b));
      if (active === kind) b.focus({ preventScroll: true });
    });
  }
  for (const b of Array.from(sel.querySelectorAll<HTMLButtonElement>(".cx-value"))) {
    const kind = b.dataset.select as SelectKind;
    const v = valueOf(row, kind);
    setText(b, v === undefined ? "—" : cutCell(v, 60));
    b.title = v ?? "";
    b.setAttribute("aria-label", `${kind}: ${v ?? "none"}`);
  }
}

/** The buttons at the end of the first line: `Shell` for a Kubernetes row, `Set path` for a tool that is not there. */
function drawActs(node: HTMLElement, row: ContextRow, entry: ToolEntry | null): void {
  const acts = node.querySelector(".cx-acts") as HTMLElement;
  const wantShell =
    row.tool === "kube" && row.state === "found" && (entry?.verbs.some((v) => v.id === SHELL_VERB && v.effect === "read" && v.output === "terminal") ?? false);
  const wantPath = needsPath(row, entry);
  const shape = `${wantShell ? "s" : ""}${wantPath ? "p" : ""}`;
  if (acts.dataset.shape === shape) return;
  acts.dataset.shape = shape;
  acts.replaceChildren();
  if (wantShell) {
    const b = button(acts, "cx-act", "Shell");
    b.dataset.act = "shell";
    b.addEventListener("click", () => void shell(row.tool));
  }
  if (wantPath) {
    const b = button(acts, "cx-act", "Set path");
    b.dataset.act = "path";
    b.setAttribute("aria-expanded", String(pathOpen.has(row.tool)));
    b.addEventListener("click", () => togglePath(row.tool));
  }
}

/** The path field under a row, while the person has it open and the tool still needs a path. */
function drawPath(node: HTMLElement, row: ContextRow, entry: ToolEntry | null): void {
  const want = pathOpen.has(row.tool) && needsPath(row, entry);
  let holder = node.querySelector<HTMLElement>(".cx-path");
  if (!want) {
    holder?.remove();
    pathOpen.delete(row.tool);
    return;
  }
  if (holder) return;
  holder = el("div", "cx-path", node);
  pathRow(holder, {
    value: pathText.get(row.tool) ?? "",
    input: (v) => pathText.set(row.tool, v),
    set: () => void setPath(row.tool),
    again: () => void checkAgain(),
    escape: () => focusInput(),
  });
  node.querySelector(".cx-acts [data-act=path]")?.setAttribute("aria-expanded", "true");
}

/** The note under the Kubernetes row while a terminal that a row opened is the focused pane. */
function drawNote(node: HTMLElement, row: ContextRow): void {
  const focus = state.view?.focus;
  const shellFocused = summary.panes.some((p) => p.pane === focus && p.tool === row.tool && p.view === SHELL_VERB);
  let note = node.querySelector<HTMLElement>(".cx-note");
  if (!shellFocused) {
    note?.remove();
    return;
  }
  if (!note) note = el("p", "cx-note", node);
  setText(note, "A shell's own startup file can replace this.");
}

/** The core's reason for a tool that cannot be used, under its row. */
function drawReason(node: HTMLElement, entry: ToolEntry | null): void {
  const text = entry?.state === "unusable" ? (entry.reason ?? "") : "";
  let reason = node.querySelector<HTMLElement>(".cx-reason");
  if (text === "") {
    reason?.remove();
    return;
  }
  if (!reason) reason = el("p", "cx-reason", node);
  setText(reason, text);
}

function drawRow(row: ContextRow): void {
  const node = rowFor(row.tool);
  const entry = toolOf(row.tool);
  setAttr(node, "data-state", row.state);
  setAttr(node, "data-stale", row.stale || failing ? "true" : null);
  setText(node.querySelector(".cx-mark") as HTMLElement, glyphOf(row.tool));
  setText(node.querySelector(".cx-label") as HTMLElement, labelOf(row.tool));
  const words = node.querySelector(".cx-words") as HTMLElement;
  // A tool that cannot be used says it with the core's reason under the row, not with a word of the page's own.
  const list = rowWords(row).filter((w) => !(w.kind === "missing" && entry?.state === "unusable"));
  const shape = list.map((w) => w.kind).join(",");
  if (words.dataset.shape !== shape) {
    words.dataset.shape = shape;
    words.replaceChildren();
    for (const w of list) {
      const span = el("span", "cx-word", words);
      span.dataset.kind = w.kind;
      span.textContent = w.text;
    }
  }
  const production = words.querySelector<HTMLElement>('.cx-word[data-kind="production"]');
  if (production) production.title = row.production ? `Profile: ${row.production}` : "";
  drawSel(node, row, entry);
  drawActs(node, row, entry);
  drawReason(node, entry);
  drawPath(node, row, entry);
  drawNote(node, row);
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
  const parent = box;
  if (!section || !parent) return;
  setAttr(section, "data-stale", failing ? "true" : null);
  const live = new Set<ToolId>();
  for (const row of rows) {
    live.add(row.tool);
    drawRow(row);
  }
  for (const [tool, node] of rowEls) {
    if (live.has(tool)) continue;
    if (picking?.tool === tool) closePicker();
    node.remove();
    rowEls.delete(tool);
  }
  // The order of the core is kept.
  rows.forEach((row, i) => {
    const node = rowEls.get(row.tool);
    if (node && parent.children[i] !== node) parent.insertBefore(node, parent.children[i] ?? null);
  });
}

/** The name of a tool: the one the tool list carries, else a fixed one for the four the core knows. */
function labelOf(tool: ToolId): string {
  return toolOf(tool)?.label ?? NAMES[tool];
}
const NAMES: Readonly<Record<ToolId, string>> = { kube: "Kubernetes", talos: "Talos", azure: "Azure", gam: "GAM" };

// ---- a choice -----------------------------------------------------------------------------------------------------------

let picking: { tool: ToolId; kind: SelectKind; list: Picker; opener: HTMLElement } | null = null;

function closePicker(back?: "value"): void {
  if (picking === null) return;
  const { list, opener } = picking;
  picking = null;
  list.close();
  opener.setAttribute("aria-expanded", "false");
  if (back === "value" && opener.isConnected) opener.focus({ preventScroll: true });
}

/** The pane a choice for `tool` is for: the focused pane when it belongs to the tool, else null (the pick). */
function reachOf(tool: ToolId): number | null {
  const focus = state.view?.focus ?? null;
  const block = focus === null ? null : (paneBlocks.get(focus) ?? null);
  return reachPane(tool, focus, summary.panes, block ? { kind: block.kind, source: block.source } : null);
}

function togglePicker(tool: ToolId, kind: SelectKind, opener: HTMLElement): void {
  if (picking !== null && picking.tool === tool && picking.kind === kind) {
    closePicker("value");
    return;
  }
  closePicker();
  const row = summary.rows.find((r) => r.tool === tool);
  const node = rowEls.get(tool);
  if (!row || !node) return;
  opener.setAttribute("aria-expanded", "true");
  const sel = node.querySelector(".cx-sel");
  const list = openPicker({
    tool, kind, pane: reachOf(tool), current: valueOf(row, kind), host: node, before: sel?.nextSibling ?? null,
    choose: (key) => void choose(tool, kind, key),
    escape: () => {
      closePicker();
      focusInput();
    },
  });
  picking = { tool, kind, list, opener };
}

/** The person chose an entry of a list the core sent: it becomes the selection of the pane the choice reaches, or the pick of the tool. */
async function choose(tool: ToolId, kind: SelectKind, key: string): Promise<void> {
  const row = summary.rows.find((r) => r.tool === tool);
  if (!row) return;
  const reach = reachOf(tool);
  // A block keeps its whole selection, profile included, so that a choice made here does not drop the profile of the pane.
  const base = (reach === null ? null : paneSelection(reach)) ?? selectionOf(row);
  closePicker("value");
  try {
    await call("integration_select", { tool, pane: reach, selection: nextSelection(tool, base, kind, key) });
  } catch (e) {
    say(readFault(e).text, true);
    return;
  }
  say(reachWords(reach, key), false);
  void ask(false);
}

// ---- the buttons of a row -----------------------------------------------------------------------------------------------

/** A terminal in a new tab with the person's shell and the context of the row; the core sets the context for that terminal only. */
async function shell(tool: ToolId): Promise<void> {
  const pane = state.view?.focus ?? null;
  try {
    const answer = readLaunchAnswer(await call<unknown>("integration_launch", { tool, verb: SHELL_VERB, pane, args: {}, placement: "tab" }));
    if (answer === null) {
      say(readFault(null).text, true);
      return;
    }
    say("", false);
    deps.applyView(answer.view);
  } catch (e) {
    say(readFault(e).text, true);
  }
}

const pathOpen = new Set<ToolId>();
const pathText = new Map<ToolId, string>();
let working = false;

function togglePath(tool: ToolId): void {
  if (pathOpen.has(tool)) pathOpen.delete(tool);
  else pathOpen.add(tool);
  const node = rowEls.get(tool);
  const row = summary.rows.find((r) => r.tool === tool);
  if (!node || !row) return;
  drawPath(node, row, toolOf(tool));
  node.querySelector(".cx-acts [data-act=path]")?.setAttribute("aria-expanded", String(pathOpen.has(tool)));
  if (pathOpen.has(tool)) node.querySelector<HTMLElement>(".ig-path")?.focus({ preventScroll: true });
}

/** The person typed a path: the core checks it, the page only sends it. */
async function setPath(tool: ToolId): Promise<void> {
  const path = (pathText.get(tool) ?? "").trim();
  if (path === "" || working) return;
  working = true;
  try {
    setTools(await call<unknown>("integration_binary_set", { tool, path }));
    pathText.delete(tool);
    say("", false);
    void ask(true);
  } catch (e) {
    say(readFault(e).text, true);
  } finally {
    working = false;
  }
}

async function checkAgain(): Promise<void> {
  if (working) return;
  working = true;
  try {
    await loadTools(true);
    await ask(true);
  } finally {
    working = false;
  }
}

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
