// What the page knows about integration blocks (docs/design.md section 6.25): the reader of the view model the core
// sends, the tool entries and faults, and the words. Pure: no DOM, so node can test it. The text of a table, a list or a
// fault comes from clusters, accounts and domains that other people named; this module only reads it, and the block puts
// it on the screen with `textContent`. The caps and member names are those of docs/integration-view.schema.json, and a
// unit test reads that file and checks them.
import type { View } from "./types.js";

/** The tools the core knows, by the word the wire uses. */
export type ToolId = "kube" | "talos" | "azure" | "gam";
export const TOOLS: readonly ToolId[] = ["kube", "talos", "azure", "gam"];

/** The selections a tool can have. */
export type SelectKind = "context" | "namespace" | "subscription" | "section" | "node";
const SELECTS: readonly SelectKind[] = ["context", "namespace", "subscription", "section", "node"];

/** The members of a selection, the profile included. */
export type SelectionMember = SelectKind | "profile";
const MEMBER_NAMES: readonly SelectionMember[] = ["profile", "context", "namespace", "subscription", "section", "node"];
export type Sel = Partial<Record<SelectionMember, string>>;

/** The verb of the list that a selection is chosen from. The convention of the core's descriptions. */
export const PICK_VERBS: Readonly<Record<SelectKind, string>> = {
  context: "contexts",
  namespace: "namespaces",
  subscription: "subscriptions",
  section: "sections",
  node: "nodes",
};

/** The limits of the contract. `shown` is the page's own: a cell is cut there when it is drawn. */
export const CAPS = {
  columns: 64,
  rows: 2000,
  cells: 64,
  cell: 4096,
  text: 65536,
  title: 128,
  note: 128,
  key: 253,
  label: 128,
  actions: 8,
  args: 8,
  argValue: 253,
  argName: 128,
  truncation: 128,
  shown: 240,
} as const;

/** The members of each object of the contract; the reader keeps these and nothing else. */
export const MEMBERS = {
  table: ["kind", "title", "columns", "rows", "truncated", "note", "fetched"],
  kv: ["kind", "title", "rows", "actions", "note", "fetched"],
  text: ["kind", "title", "text", "format", "truncated", "note", "fetched"],
  row: ["key", "cells", "tone", "actions"],
  action: ["verb", "label", "effect", "args"],
  kvRow: ["label", "value", "tone"],
  tones: ["normal", "dim", "warn", "danger"],
  effects: ["read", "local", "write", "exec"],
} as const;

export type Tone = (typeof MEMBERS.tones)[number];
export type Effect = (typeof MEMBERS.effects)[number];

export interface RowAction {
  verb: string;
  label: string;
  effect: Effect;
  args: Record<string, string>;
}
export interface TableRow {
  key: string;
  cells: string[];
  tone: Tone;
  actions: RowAction[];
}
export interface TableModel {
  kind: "table";
  title: string;
  columns: { id: string; label: string }[];
  rows: TableRow[];
  truncated: { shown: number; note: string } | null;
  note: string | null;
  fetched: number | null;
}
export interface KvModel {
  kind: "kv";
  title: string;
  rows: { label: string; value: string; tone: Tone }[];
  actions: RowAction[];
  note: string | null;
  fetched: number | null;
}
export interface TextModel {
  kind: "text";
  title: string;
  text: string;
  format: "plain" | "json";
  truncated: boolean;
  note: string | null;
  fetched: number | null;
}
export type ViewModel = TableModel | KvModel | TextModel;

// ---- reading ----------------------------------------------------------------------------------------------------------

/** `s` cut to `max` characters (not UTF-16 units), so that a character is never split. */
function cut(s: string, max: number): string {
  if (s.length <= max) return s;
  const chars = Array.from(s);
  return chars.length <= max ? s : chars.slice(0, max).join("");
}

/** A cell as the block draws it: at most `max` characters, the last one an ellipsis when something was left out. */
export function cutCell(text: string, max: number = CAPS.shown): string {
  if (text.length <= max) return text;
  const chars = Array.from(text);
  if (chars.length <= max) return text;
  return `${chars.slice(0, max - 1).join("")}…`;
}

function obj(raw: unknown): Record<string, unknown> | null {
  return typeof raw === "object" && raw !== null && !Array.isArray(raw) ? (raw as Record<string, unknown>) : null;
}

function str(raw: unknown, max: number): string {
  return typeof raw === "string" ? cut(raw, max) : "";
}

function strOrNull(raw: unknown, max: number): string | null {
  return typeof raw === "string" ? cut(raw, max) : null;
}

function tone(raw: unknown): Tone {
  return typeof raw === "string" && (MEMBERS.tones as readonly string[]).includes(raw) ? (raw as Tone) : "normal";
}

function fetched(raw: unknown): number | null {
  return typeof raw === "number" && Number.isInteger(raw) && raw >= 0 && raw <= 4294967295 ? raw : null;
}

/** An action of the contract, or null: the verb, the label, a known effect and at most 8 text arguments. */
function readAction(raw: unknown): RowAction | null {
  const o = obj(raw);
  if (!o || typeof o.verb !== "string" || typeof o.label !== "string") return null;
  if (typeof o.effect !== "string" || !(MEMBERS.effects as readonly string[]).includes(o.effect)) return null;
  const given = obj(o.args);
  if (!given) return null;
  const entries = Object.entries(given);
  if (entries.length > CAPS.args) return null;
  const args: Record<string, string> = {};
  for (const [name, value] of entries) {
    if (name.length > CAPS.argName || typeof value !== "string" || Array.from(value).length > CAPS.argValue) return null;
    args[name] = value;
  }
  return { verb: cut(o.verb, CAPS.label), label: cut(o.label, CAPS.label), effect: o.effect as Effect, args };
}

function readActions(raw: unknown): RowAction[] {
  if (!Array.isArray(raw)) return [];
  const out: RowAction[] = [];
  for (const item of raw) {
    const action = readAction(item);
    if (action) out.push(action);
    if (out.length >= CAPS.actions) break;
  }
  return out;
}

function readTable(o: Record<string, unknown>): TableModel | null {
  if (!Array.isArray(o.columns) || !Array.isArray(o.rows)) return null;
  const columns: { id: string; label: string }[] = [];
  for (const item of o.columns) {
    const c = obj(item);
    if (!c || typeof c.id !== "string" || typeof c.label !== "string") continue;
    columns.push({ id: cut(c.id, CAPS.label), label: cut(c.label, CAPS.label) });
    if (columns.length >= CAPS.columns) break;
  }
  const rows: TableRow[] = [];
  for (const item of o.rows) {
    const r = obj(item);
    if (!r || typeof r.key !== "string" || !Array.isArray(r.cells)) continue;
    const cells: string[] = [];
    for (let i = 0; i < columns.length; i++) {
      const cell = r.cells[i];
      cells.push(typeof cell === "string" ? cut(cell, CAPS.cell) : "");
    }
    rows.push({ key: cut(r.key, CAPS.key), cells, tone: tone(r.tone), actions: readActions(r.actions) });
    if (rows.length >= CAPS.rows) break;
  }
  const t = obj(o.truncated);
  const truncated =
    t && typeof t.shown === "number" && Number.isFinite(t.shown) && t.shown >= 0 && typeof t.note === "string"
      ? { shown: t.shown, note: cut(t.note, CAPS.truncation) }
      : null;
  return {
    kind: "table",
    title: str(o.title, CAPS.title),
    columns,
    rows,
    truncated,
    note: strOrNull(o.note, CAPS.note),
    fetched: fetched(o.fetched),
  };
}

function readKv(o: Record<string, unknown>): KvModel | null {
  if (!Array.isArray(o.rows)) return null;
  const rows: { label: string; value: string; tone: Tone }[] = [];
  for (const item of o.rows) {
    const r = obj(item);
    if (!r || typeof r.label !== "string" || typeof r.value !== "string") continue;
    rows.push({ label: cut(r.label, CAPS.label), value: cut(r.value, CAPS.cell), tone: tone(r.tone) });
    if (rows.length >= CAPS.rows) break;
  }
  return { kind: "kv", title: str(o.title, CAPS.title), rows, actions: readActions(o.actions), note: strOrNull(o.note, CAPS.note), fetched: fetched(o.fetched) };
}

function readText(o: Record<string, unknown>): TextModel | null {
  if (typeof o.text !== "string") return null;
  return {
    kind: "text",
    title: str(o.title, CAPS.title),
    text: cut(o.text, CAPS.text),
    format: o.format === "json" ? "json" : "plain",
    truncated: o.truncated === true,
    note: strOrNull(o.note, CAPS.note),
    fetched: fetched(o.fetched),
  };
}

/**
 * Read a view model of the core. Anything that is not the contract is dropped or made empty, never trusted: a value that is
 * no view is `null`, a row is cut or padded to the columns, a cell that is no text is empty, and a member the contract does
 * not have is not kept.
 */
export function readView(raw: unknown): ViewModel | null {
  const o = obj(raw);
  if (!o) return null;
  switch (o.kind) {
    case "table":
      return readTable(o);
    case "kv":
      return readKv(o);
    case "text":
      return readText(o);
    default:
      return null;
  }
}

/** A fault of an integration command: `{code, reason, text}`. A plain string (Tauri's own error) is the sentence. */
export interface Fault {
  code: string;
  reason: string;
  text: string;
}

const FAULT_MAX = 400;
const FAULT_DEFAULT = "The request failed.";

export function readFault(raw: unknown): Fault {
  if (typeof raw === "string") return { code: "failed", reason: "", text: raw.trim() === "" ? FAULT_DEFAULT : cut(raw, FAULT_MAX) };
  const o = obj(raw);
  const text = o && typeof o.text === "string" && o.text.trim() !== "" ? cut(o.text, FAULT_MAX) : FAULT_DEFAULT;
  return { code: o && typeof o.code === "string" ? cut(o.code, 32) : "failed", reason: o && typeof o.reason === "string" ? cut(o.reason, 32) : "", text };
}

export interface RunAnswer {
  view: ViewModel;
  selection: { selection: Sel; production: boolean };
  fetched: number | null;
}

function readSel(raw: unknown): Sel {
  const o = obj(raw);
  const out: Sel = {};
  if (!o) return out;
  for (const name of MEMBER_NAMES) {
    const v = o[name];
    if (typeof v === "string" && v !== "") out[name] = cut(v, CAPS.argValue);
  }
  return out;
}

/** The answer of `integration_run`. The production flag is the core's: a text (the profile's name) or `true` means yes. */
export function readRunAnswer(raw: unknown): RunAnswer | null {
  const o = obj(raw);
  if (!o) return null;
  const view = readView(o.view);
  if (!view) return null;
  const s = obj(o.selection);
  const production = s !== null && (s.production === true || (typeof s.production === "string" && s.production !== ""));
  return { view, selection: { selection: readSel(s?.selection), production }, fetched: fetched(o.fetched) };
}

/** The answer of `integration_launch`: the new pane and the layout that has it. */
export function readLaunchAnswer(raw: unknown): { pane: number; view: View } | null {
  const o = obj(raw);
  if (!o || typeof o.pane !== "number" || !Number.isInteger(o.pane)) return null;
  const view = obj(o.view);
  if (!view || !Array.isArray(view.panes)) return null;
  return { pane: o.pane, view: o.view as View };
}

export type ToolState = "found" | "not_found" | "unusable" | "unsupported";
export interface VerbEntry {
  id: string;
  label: string;
  effect: Effect;
  output: "view" | "terminal" | "done";
  refresh_secs: number | null;
}
export interface ToolEntry {
  id: ToolId;
  label: string;
  state: ToolState;
  path: string | null;
  binary: string | null;
  reason: string | null;
  version: string | null;
  searched: string[];
  selects: SelectKind[];
  verbs: VerbEntry[];
}

const VERB_ID = /^[a-z][a-z0-9_]{0,63}$/;

/** True for a verb id the page will put into a header or a call: lower case words and underscores. */
export function isVerbId(s: string): boolean {
  return VERB_ID.test(s);
}

/** The answer of `integration_tools`, entry by entry; an entry or a verb that is not well formed is dropped. */
export function readTools(raw: unknown): ToolEntry[] {
  if (!Array.isArray(raw)) return [];
  const out: ToolEntry[] = [];
  for (const item of raw) {
    const o = obj(item);
    if (!o || typeof o.id !== "string" || !(TOOLS as readonly string[]).includes(o.id) || typeof o.label !== "string") continue;
    const state: ToolState =
      o.state === "found" || o.state === "unusable" || o.state === "unsupported" ? o.state : "not_found";
    const verbs: VerbEntry[] = [];
    for (const v of Array.isArray(o.verbs) ? o.verbs : []) {
      const e = obj(v);
      if (!e || typeof e.id !== "string" || !VERB_ID.test(e.id) || typeof e.label !== "string") continue;
      const effect: Effect = (MEMBERS.effects as readonly string[]).includes(String(e.effect)) ? (e.effect as Effect) : "exec";
      const output = e.output === "view" || e.output === "terminal" ? e.output : "done";
      const secs = typeof e.refresh_secs === "number" && Number.isFinite(e.refresh_secs) && e.refresh_secs > 0 ? e.refresh_secs : null;
      verbs.push({ id: e.id, label: cut(e.label, CAPS.label), effect, output, refresh_secs: secs });
    }
    out.push({
      id: o.id as ToolId,
      label: cut(o.label, CAPS.label),
      state,
      path: strOrNull(o.path, 4096),
      binary: strOrNull(o.binary, 4096),
      reason: strOrNull(o.reason, FAULT_MAX),
      version: strOrNull(o.version, 64),
      searched: (Array.isArray(o.searched) ? o.searched : []).filter((s): s is string => typeof s === "string").slice(0, 64).map((s) => cut(s, 4096)),
      selects: (Array.isArray(o.selects) ? o.selects : []).filter((s): s is SelectKind => (SELECTS as readonly unknown[]).includes(s)),
      verbs,
    });
  }
  return out;
}

// ---- the selection ----------------------------------------------------------------------------------------------------

export interface PickerEntry {
  /** The value that is sent back: the key of the row the core listed. */
  key: string;
  /** What the entry is called. */
  label: string;
  /** The other cells of the row, for a person who must tell two entries apart. */
  detail: string;
}

/** The entries of a list the core sent for a picker: one per row of a table, the key as the value. */
export function pickerEntries(view: ViewModel | null): PickerEntry[] {
  if (!view || view.kind !== "table") return [];
  return view.rows.map((row) => ({
    key: row.key,
    label: cutCell(row.key),
    detail: cutCell(row.cells.filter((c) => c !== "" && c !== row.key && c !== "*").join(" · ")),
  }));
}

/**
 * The selection after `key` was chosen for `kind`. The page sends the whole selection of the pane (the core replaces it),
 * and a member that belongs to the old value is dropped: a namespace belongs to its cluster, a node to its Talos context.
 */
export function nextSelection(tool: ToolId, current: Sel, kind: SelectKind, key: string): Sel {
  const next: Sel = { ...current, [kind]: key };
  if (kind === "context") {
    if (tool === "kube") delete next.namespace;
    if (tool === "talos") delete next.node;
  }
  return next;
}

/** The text of a pane header: the tool, what is selected, and the interval of a view that reads again by itself. */
export function contextText(tool: { label: string; selects: readonly SelectKind[] }, selection: Sel, refreshSecs: number | null): string {
  const parts = [tool.label];
  for (const kind of tool.selects) {
    const v = selection[kind];
    if (v) parts.push(cutCell(v, 60));
  }
  if (refreshSecs !== null && refreshSecs > 0) {
    parts.push(refreshSecs >= 60 && refreshSecs % 60 === 0 ? `${refreshSecs / 60} min` : `${refreshSecs} s`);
  }
  return parts.join(" · ");
}

/** The header text and the production flag of each pane, filled by the answers of the core. */
const contexts = new Map<number, { text: string; production: boolean }>();

/** Remember what a pane's header says. True when it changed. */
export function setIntegrationContext(pane: number, text: string, production: boolean): boolean {
  const known = contexts.get(pane);
  if (known && known.text === text && known.production === production) return false;
  contexts.set(pane, { text, production });
  return true;
}

export function clearIntegrationContext(pane: number): void {
  contexts.delete(pane);
  selections.delete(pane);
}

/** The whole selection of each integration block, profile included, as its last read answered it. */
const selections = new Map<number, Sel>();

/** Remember the selection a block's last read used, so that a choice made elsewhere can send the whole selection and keep its profile. */
export function setPaneSelection(pane: number, selection: Sel): void {
  selections.set(pane, { ...selection });
}

/** The selection of an integration block, or null when the pane is none or has not read yet. */
export function paneSelection(pane: number): Sel | null {
  const known = selections.get(pane);
  return known ? { ...known } : null;
}

export function integrationContextText(pane: number): string | null {
  return contexts.get(pane)?.text ?? null;
}

/** Whether the core said that the pane's selection is a production one. */
export function integrationProduction(pane: number): boolean {
  return contexts.get(pane)?.production ?? false;
}

// ---- the context of the tools -----------------------------------------------------------------------------------------

/** The state word of a row of the context answer. */
export type RowState = "found" | "not_found" | "signed_out" | "unknown";
const ROW_STATES: readonly RowState[] = ["found", "not_found", "signed_out", "unknown"];

/** One tool as the sidebar, the status line and the pane chip show it. `production` is the core's flag: the name of the profile (maybe empty) or null. */
export interface ContextRow {
  tool: ToolId;
  state: RowState;
  context: string | null;
  namespace: string | null;
  node: string | null;
  subscription: string | null;
  section: string | null;
  production: string | null;
  stale: boolean;
}

/** A pane that has an integration selection: a block, or a terminal that a launch opened. */
export interface PaneInfo {
  pane: number;
  tool: ToolId;
  /** The view the pane shows, or the verb a launch opened. */
  view: string;
  context: string | null;
  namespace: string | null;
  production: string | null;
  held: boolean;
  status: number | null;
}

export interface ContextSummary {
  rows: ContextRow[];
  panes: PaneInfo[];
}

/** The most rows and panes the page reads; the core sends four rows and at most a few hundred panes. */
const MAX_PANES = 512;

function member(o: Record<string, unknown>, name: string): string | null {
  const v = o[name];
  return typeof v === "string" && v !== "" ? cut(v, CAPS.argValue) : null;
}

/** The core's production flag: a name (maybe empty, when the flag is only `true`) or null. */
function productionOf(raw: unknown): string | null {
  if (raw === true) return "";
  return typeof raw === "string" && raw !== "" ? cut(raw, CAPS.label) : null;
}

/**
 * Read the answer of `integration_context`. A row or a pane that is not the contract is dropped, a member that is no text is
 * empty, and an unknown state is `unknown`: the page draws what it understands and trusts nothing else.
 */
export function readContext(raw: unknown): ContextSummary {
  const o = obj(raw);
  const rows: ContextRow[] = [];
  const panes: PaneInfo[] = [];
  if (!o) return { rows, panes };
  for (const item of Array.isArray(o.rows) ? o.rows : []) {
    const r = obj(item);
    if (!r || typeof r.tool !== "string" || !(TOOLS as readonly string[]).includes(r.tool)) continue;
    rows.push({
      tool: r.tool as ToolId,
      state: (ROW_STATES as readonly unknown[]).includes(r.state) ? (r.state as RowState) : "unknown",
      context: member(r, "context"),
      namespace: member(r, "namespace"),
      node: member(r, "node"),
      subscription: member(r, "subscription"),
      section: member(r, "section"),
      production: productionOf(r.production),
      stale: r.stale === true,
    });
    if (rows.length >= TOOLS.length * 2) break;
  }
  for (const item of Array.isArray(o.panes) ? o.panes : []) {
    const p = obj(item);
    if (!p || typeof p.pane !== "number" || !Number.isSafeInteger(p.pane) || p.pane < 0) continue;
    if (typeof p.tool !== "string" || !(TOOLS as readonly string[]).includes(p.tool) || typeof p.view !== "string") continue;
    panes.push({
      pane: p.pane,
      tool: p.tool as ToolId,
      view: cut(p.view, CAPS.label),
      context: member(p, "context"),
      namespace: member(p, "namespace"),
      production: productionOf(p.production),
      held: p.held === true,
      status: typeof p.status === "number" && Number.isSafeInteger(p.status) && p.status >= 0 ? p.status : null,
    });
    if (panes.length >= MAX_PANES) break;
  }
  return { rows, panes };
}

/** The selections each tool shows in its row, in the order they are shown. */
const ROW_MEMBERS: Readonly<Record<ToolId, readonly ("context" | "namespace" | "node" | "subscription" | "section")[]>> = {
  kube: ["context", "namespace"],
  talos: ["context", "node"],
  azure: ["subscription"],
  gam: ["section"],
};

/** The mark of a tool: a glyph for Kubernetes and Talos, a text chip for the other two. */
export function glyphOf(tool: ToolId): string {
  switch (tool) {
    case "kube":
      return "⎈";
    case "talos":
      return "◇";
    case "azure":
      return "az";
    default:
      return "gam";
  }
}

/** The values of a row that its tool selects, in the order shown; the empty list when nothing is selected. */
export function selectionValues(row: ContextRow): string[] {
  const out: string[] = [];
  for (const name of ROW_MEMBERS[row.tool]) {
    const v = row[name];
    if (v !== null) out.push(v);
  }
  return out;
}

/** The text of the selection of a row: `prod-eu · payments`, or a dash when nothing is selected. Each value is cut for the narrow column. */
export function selectionText(row: ContextRow): string {
  const values = selectionValues(row);
  return values.length === 0 ? "—" : values.map((v) => cutCell(v, 60)).join(" · ");
}

export type WordKind = "production" | "missing" | "signed_out" | "stale";
export interface RowWord {
  kind: WordKind;
  text: string;
}

/**
 * The words a row adds to its selection. Production first (the flag is the core's, never a guess from a name), then the state of the
 * tool, then `stale` when there is a value that can be old. These are the only words the page adds.
 */
export function rowWords(row: ContextRow): RowWord[] {
  const out: RowWord[] = [];
  if (row.production !== null) out.push({ kind: "production", text: "production" });
  if (row.state === "not_found") out.push({ kind: "missing", text: "not found" });
  else if (row.state === "signed_out") out.push({ kind: "signed_out", text: "signed out" });
  if (row.stale && selectionValues(row).length > 0) out.push({ kind: "stale", text: "stale" });
  return out;
}

// ---- words ------------------------------------------------------------------------------------------------------------

export type StateName = "reading" | "empty" | "failed" | "missing" | "unusable" | "ok";

/** The words the page owns. The sentence of a fault, a missing tool or an unusable one is the core's, so its text is empty. */
export function stateWords(state: string): { text: string; tone: "plain" | "warn" } {
  switch (state) {
    case "reading":
      return { text: "Reading …", tone: "plain" };
    case "empty":
      return { text: "Nothing to show.", tone: "plain" };
    case "failed":
    case "missing":
    case "unusable":
      return { text: "", tone: "warn" };
    default:
      return { text: "", tone: "plain" };
  }
}

/** How long ago the data was read: `Updated just now`, `Updated 12 s ago`, `Updated 3 min ago`, `Updated 2 h ago`. */
export function ageWords(seconds: number): string {
  const s = Number.isFinite(seconds) ? Math.floor(seconds) : 0;
  if (s < 5) return "Updated just now";
  if (s < 60) return `Updated ${s} s ago`;
  if (s < 3600) return `Updated ${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `Updated ${Math.floor(s / 3600)} h ago`;
  return `Updated ${Math.floor(s / 86400)} d ago`;
}

/**
 * Whether a view that reads again by itself is due: its verb has an interval, the interval has passed since the last
 * start, the window is visible, the block is shown, no request of this block is on the way, and the block is alive.
 */
export function refreshDue(o: {
  secs: number | null;
  last: number;
  now: number;
  hidden: boolean;
  shown: boolean;
  busy: boolean;
  alive: boolean;
}): boolean {
  if (o.secs === null || !(o.secs > 0) || !o.alive || o.hidden || !o.shown || o.busy) return false;
  return o.now - o.last >= o.secs * 1000;
}
