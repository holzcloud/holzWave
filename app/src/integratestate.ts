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
/** An argument of a verb as the core lists it: its name, the kind of value it takes and whether it is required. */
export interface VerbArg {
  name: string;
  kind: string;
  required: boolean;
}
export interface VerbEntry {
  id: string;
  label: string;
  effect: Effect;
  output: "view" | "terminal" | "done";
  refresh_secs: number | null;
  args: VerbArg[];
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
      const args: VerbArg[] = [];
      for (const a of Array.isArray(e.args) ? e.args : []) {
        const given = obj(a);
        if (!given || typeof given.name !== "string" || typeof given.kind !== "string") continue;
        args.push({ name: cut(given.name, CAPS.argName), kind: cut(given.kind, CAPS.label), required: given.required === true });
        if (args.length >= CAPS.args) break;
      }
      verbs.push({ id: e.id, label: cut(e.label, CAPS.label), effect, output, refresh_secs: secs, args });
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

// ---- the palette entries ----------------------------------------------------------------------------------------------

/** The title of a palette entry: the tool's label and the verb's label, both the core's. */
export function entryTitle(toolLabel: string, verbLabel: string): string {
  return `${toolLabel}: ${verbLabel}`;
}

/** The id of a palette action: `integration.open.<tool>.<verb>` for a view, `integration.run.<tool>.<verb>` for a terminal. */
export function entryId(kind: "open" | "run", tool: string, verb: string): string {
  return `integration.${kind}.${tool}.${verb}`;
}

/** Whether a verb needs a value that the palette cannot supply. */
function needsArgument(verb: VerbEntry): boolean {
  return verb.args.some((a) => a.required);
}

/** The verbs of a tool that read and show a view and need no argument, in the core's order: each can open a block. */
export function viewVerbs(tool: ToolEntry): VerbEntry[] {
  return tool.verbs.filter((v) => v.effect === "read" && v.output === "view" && !needsArgument(v));
}

/**
 * The verbs of a tool that open a terminal and need no argument, in the core's order: a read verb launches at once, an Exec verb asks
 * first. A verb that changes something on this computer or in a cluster (Local, Write) has no entry: it needs a selection and a card.
 */
export function terminalVerbs(tool: ToolEntry): VerbEntry[] {
  return tool.verbs.filter((v) => v.output === "terminal" && (v.effect === "read" || v.effect === "exec") && !needsArgument(v));
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

/** The whole selection a row shows, as the core takes it: only the members its tool selects. */
export function selectionOf(row: ContextRow): Sel {
  const out: Sel = {};
  for (const name of ROW_MEMBERS[row.tool]) {
    const v = row[name];
    if (v !== null) out[name] = v;
  }
  return out;
}

/** One value of a row, or undefined when it has none. */
export function valueOf(row: ContextRow, kind: SelectKind): string | undefined {
  return row[kind] ?? undefined;
}

/** The selections of a tool's row, in the order shown: the members a person can choose. */
export function rowKinds(tool: ToolId): readonly SelectKind[] {
  return ROW_MEMBERS[tool];
}

/**
 * The pane a choice in the Context section is for: the focused pane when it belongs to `tool` (the core lists it as a block of
 * the tool or as a terminal a launch opened for it, or it is an integration block of the tool that has no selection yet), else
 * null, which is the pick that new blocks start from. Read from the lists the core and the block layer keep, never guessed.
 */
export function reachPane(
  tool: ToolId,
  focus: number | null,
  panes: readonly PaneInfo[],
  block: { kind: string; source: string } | null,
): number | null {
  if (focus === null) return null;
  if (panes.some((p) => p.pane === focus && p.tool === tool)) return focus;
  if (block !== null && block.kind === "integration" && block.source === tool) return focus;
  return null;
}

/** The line under the section after a choice: which pane changed, or what new blocks start with. */
export function reachWords(pane: number | null, value: string): string {
  const v = cutCell(value, 60);
  return pane === null ? `New blocks start with ${v}.` : `Pane ${pane} now uses ${v}.`;
}

/** What a status segment says. `production` and `stale` are words of the segment; `label` is its accessible name. */
export interface SegmentText {
  tool: ToolId;
  text: string;
  production: boolean;
  stale: boolean;
  label: string;
}

/**
 * The status segment of a row: `⎈ prod-eu/payments` for Kubernetes and `az holz-prod` for Azure, and nothing for a tool without a
 * value, for a tool that is not there, and for the other two tools (the status line has these two). The core resolved the row for
 * the focused pane, so the segment is the pane's selection, else the pick, else what the tool is set to.
 */
export function segmentText(row: ContextRow): SegmentText | null {
  if (row.state === "not_found") return null;
  const stale = row.stale;
  const production = row.production !== null;
  const tail = `${production ? ", production" : ""}${stale ? ", stale" : ""}`;
  if (row.tool === "kube" && row.context !== null) {
    const ns = row.namespace;
    return {
      tool: "kube",
      text: `${glyphOf("kube")} ${cutCell(row.context, 40)}${ns === null ? "" : `/${cutCell(ns, 40)}`}`,
      production,
      stale,
      label: `Kubernetes context ${cutCell(row.context, 60)}${ns === null ? "" : `, namespace ${cutCell(ns, 60)}`}${tail}`,
    };
  }
  if (row.tool === "azure" && row.subscription !== null) {
    return {
      tool: "azure",
      text: `${glyphOf("azure")} ${cutCell(row.subscription, 40)}`,
      production,
      stale,
      label: `Azure subscription ${cutCell(row.subscription, 60)}${tail}`,
    };
  }
  return null;
}

/** What the chip in the header of a pane says. */
export interface ChipText {
  text: string;
  production: boolean;
  title: string;
}

/** The chip of a pane the core listed: the mark and the context holzWave set for it, for Kubernetes and Talos; null without a context. */
export function chipText(info: PaneInfo): ChipText | null {
  if ((info.tool !== "kube" && info.tool !== "talos") || info.context === null) return null;
  return { text: `${glyphOf(info.tool)} ${cutCell(info.context, 60)}`, production: info.production !== null, title: "Set by holzWave for this pane" };
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

// ---- the card of an action that asks first (docs/design.md section 6.26) -------------------------------------------------

/** The effects that ask first. `read` has no card. */
export type AskingEffect = "local" | "write" | "exec";

/** The most fields, and the most characters in one value, that the page reads from a card. The core's own limit is 8 KiB in all. */
export const CARD_CAPS = { fields: 64, value: 8192, name: 128, expiresMax: 3600 } as const;

/**
 * The card as the core prepared it. Every text is the core's: control, direction, zero-width and tag characters were written as
 * escapes there. The page draws these members and sends back the `ticket` and the `hash` and nothing else.
 */
export interface Card {
  ticket: string;
  tool: string;
  verb: string;
  effect: AskingEffect;
  program: string;
  argv: string[];
  target: [string, string][];
  /** The name of the production context the action touches, or null. */
  production: string | null;
  /** Every field the hash covers, in the core's order. */
  fields: [string, string][];
  hash: string;
  /** Seconds the ticket is good for, as the core said. */
  expires: number;
}

const TICKET_SHAPE = /^[0-9a-f]{32}$/;
const HASH_SHAPE = /^[0-9a-f]{64}$/;

function pairs(raw: unknown, max: number): [string, string][] | null {
  if (!Array.isArray(raw) || raw.length > max) return null;
  const out: [string, string][] = [];
  for (const item of raw) {
    if (!Array.isArray(item) || item.length !== 2) return null;
    const [name, value] = item as [unknown, unknown];
    if (typeof name !== "string" || typeof value !== "string") return null;
    if (name.length > CARD_CAPS.name || value.length > CARD_CAPS.value) return null;
    out.push([name, value]);
  }
  return out;
}

/** The fields the hash covers, rebuilt from the members the card is drawn from, in the order the core makes them. */
function expectedFields(c: Omit<Card, "fields" | "hash" | "expires" | "ticket">): [string, string][] {
  return [
    ["tool", c.tool],
    ["verb", c.verb],
    ["effect", c.effect],
    ["program", c.program],
    ...c.argv.map((a, i): [string, string] => [`arg ${i + 1}`, a]),
    ...c.target,
    ["guard", c.production === null ? "none" : "production"],
    ...(c.production === null ? [] : [["production_name", c.production] as [string, string]]),
  ];
}

/**
 * Read the card the core prepared. Anything that is not the contract is `null`: a missing ticket or hash, a hash that is not
 * 64 lower case hex digits, a field list of the wrong shape, more than 64 fields, a value over 8192 characters, an effect that
 * asks nothing, or members that do not say what the fields the hash covers say. The page draws the members, so they must be the
 * very fields the hash is over.
 */
export function readCard(raw: unknown): Card | null {
  const o = obj(raw);
  if (!o) return null;
  if (typeof o.ticket !== "string" || !TICKET_SHAPE.test(o.ticket)) return null;
  if (typeof o.hash !== "string" || !HASH_SHAPE.test(o.hash)) return null;
  if (typeof o.tool !== "string" || typeof o.verb !== "string" || typeof o.program !== "string") return null;
  if (o.tool.length > CARD_CAPS.name || o.verb.length > CARD_CAPS.name || o.program.length > CARD_CAPS.value) return null;
  if (o.effect !== "local" && o.effect !== "write" && o.effect !== "exec") return null;
  const expires = o.expires_in_secs;
  if (typeof expires !== "number" || !Number.isInteger(expires) || expires < 1 || expires > CARD_CAPS.expiresMax) return null;
  const g = obj(o.guard);
  if (!g) return null;
  let production: string | null = null;
  if (g.production !== null && g.production !== undefined) {
    if (typeof g.production !== "string" || g.production === "" || g.production.length > CARD_CAPS.value) return null;
    production = g.production;
  }
  const fields = pairs(o.fields, CARD_CAPS.fields);
  const target = pairs(o.target, CARD_CAPS.fields);
  if (!fields || !target || !Array.isArray(o.argv) || o.argv.length > CARD_CAPS.fields) return null;
  const argv: string[] = [];
  for (const a of o.argv) {
    if (typeof a !== "string" || a.length > CARD_CAPS.value) return null;
    argv.push(a);
  }
  const card: Card = { ticket: o.ticket, tool: o.tool, verb: o.verb, effect: o.effect, program: o.program, argv, target, production, fields, hash: o.hash, expires };
  const expected = expectedFields(card);
  if (expected.length !== fields.length || expected.some(([n, v], i) => n !== fields[i]?.[0] || v !== fields[i]?.[1])) return null;
  return card;
}

/** The first twelve characters of the hash, as the card shows it. */
export function hashShown(hash: string): string {
  return hash.slice(0, 12);
}

/** The sentence of the core's `changes` pair of a Local card, or null. */
export function changesOf(card: Card): string | null {
  return card.target.find(([name]) => name === "changes")?.[1] ?? null;
}

/** The target rows of a card: the core's pairs in the core's order. The `changes` pair of a Local card is in the effect line, not twice. */
export function targetRows(card: Card): [string, string][] {
  return card.effect === "local" ? card.target.filter(([name]) => name !== "changes") : card.target;
}

/** The rows of the argument box: the program, then each argument as the core spelled it, one element each. */
export function argumentRows(card: Card): [string, string][] {
  return [["program", card.program], ...card.argv.map((a, i): [string, string] => [`arg ${i + 1}`, a])];
}

/** Every word of the card that is not the core's. */
export const CARD_WORDS = {
  head: "Confirm needed",
  cannotTitle: "Cannot show this request",
  count: (n: number, of: number): string => `${n} of ${of}`,
  yes: "Allow once",
  yesArmed: "Press again to allow on production",
  no: "Deny",
  foot: "Allows this one action only. Nothing is kept for later. Esc denies.",
  waiting: "waiting for your decision",
  denied: "Denied. Nothing ran.",
  write: "Changes a cluster or an account. Check the target below.",
  local: "Changes the tool's own configuration.",
  exec: "Opens a session with your credentials. Check the command below.",
  armed: "Armed. The next press allows this on production. Nothing has run yet.",
  sending: "Allowed · running…",
  ended: "That action is no longer waiting.",
  changed: "The request changed. Nothing ran. Read the card again.",
  cannot: "This request could not be read in full. It can only be denied.",
  palette: "Answer the integration request",
  failed: "The request failed.",
} as const;

/** The title: the tool and the verb as the core's tool list names them, else the core's ids. */
export function titleWords(card: Card, toolLabel: string | null, verbLabel: string | null): string {
  return `${toolLabel ?? card.tool} · ${verbLabel ?? card.verb}`;
}

/** The effect line: one sentence for the effect, and for a Local verb the sentence of the core that names what it writes. */
export function effectWords(card: Card): string {
  if (card.effect === "write") return CARD_WORDS.write;
  if (card.effect === "exec") return CARD_WORDS.exec;
  const changes = changesOf(card);
  return changes === null ? CARD_WORDS.local : `${CARD_WORDS.local} It writes: ${changes}`;
}

/** The guard line of a production context, in the core's words for the name. */
export function guardWords(name: string): string {
  return `Production context: ${name}.`;
}

/** The modes of a card. */
export type Mode = "ask" | "armed" | "sending" | "done" | "ended" | "changed" | "cannot";

/** What a confirm that worked said. */
export interface Outcome {
  kind: "ran" | "launched";
  note: string;
  pane: number | null;
}

/** The line a mode shows. `ask` has none. */
export function modeWords(mode: Mode, outcome: Outcome | null = null): string {
  switch (mode) {
    case "armed":
      return CARD_WORDS.armed;
    case "sending":
      return CARD_WORDS.sending;
    case "done":
      if (outcome === null) return "";
      if (outcome.kind === "launched") return outcome.pane === null ? "Opened a pane" : `Opened pane ${outcome.pane}`;
      return outcome.note === "" ? "Done." : `Done: ${cutCell(outcome.note, FAULT_MAX)}`;
    case "ended":
      return CARD_WORDS.ended;
    case "changed":
      return CARD_WORDS.changed;
    case "cannot":
      return CARD_WORDS.cannot;
    default:
      return "";
  }
}

/** The answer of `integration_confirm`: what happened, the new pane and layout of a launch. */
export interface ConfirmAnswer {
  kind: "ran" | "launched";
  note: string;
  pane: number | null;
  view: View | null;
}

export function readConfirm(raw: unknown): ConfirmAnswer | null {
  const o = obj(raw);
  if (!o || (o.kind !== "ran" && o.kind !== "launched")) return null;
  const note = typeof o.note === "string" ? cut(o.note, FAULT_MAX) : "";
  const pane = typeof o.pane === "number" && Number.isSafeInteger(o.pane) && o.pane >= 0 ? o.pane : null;
  if (o.kind === "ran") return { kind: "ran", note, pane: null, view: null };
  const view = obj(o.view);
  if (pane === null || !view || !Array.isArray(view.panes)) return null;
  return { kind: "launched", note, pane, view: o.view as View };
}

/** What a refused confirm means: the ticket is gone, the request is another one, or any other sentence of the core. */
export type Refusal = "ended" | "changed" | "other";

export function refusalOf(fault: Fault): Refusal {
  if (fault.code !== "ticket") return "other";
  if (fault.reason === "unknown" || fault.reason === "expired") return "ended";
  if (fault.reason === "changed" || fault.reason === "mismatch") return "changed";
  return "other";
}

/** A press or key made less than this long (milliseconds) after a card appeared, or after it was armed, does not count. */
export const HALF_SECOND = 500;

/** What happened to a card. `trusted` is the browser's `isTrusted`: a click or key a script made is not. */
export type CardEvent =
  | { type: "allow"; now: number; shownAt: number; armedAt: number | null; trusted: boolean; production: boolean }
  | { type: "deny"; trusted: boolean }
  | { type: "ran" }
  | { type: "refused"; refusal: Refusal }
  | { type: "expired" }
  | { type: "fresh" };

/** What the page does besides drawing the next mode. */
export type CardAct = "none" | "arm" | "confirm" | "cancel";

/**
 * The whole state machine of a card as one pure function. `Allow once` counts only for a press that a person made, at least
 * half a second after the card was drawn, and for an armed card also half a second after it was armed (a double click arms
 * and cannot allow). A production card needs a second press. `Deny` counts for any real press at any time but while a confirm is on
 * its way. Nothing else makes the page call `integration_confirm`.
 */
export function nextMode(mode: Mode, event: CardEvent): { mode: Mode; act: CardAct } {
  const stay = { mode, act: "none" as CardAct };
  switch (event.type) {
    case "allow": {
      if (mode !== "ask" && mode !== "armed") return stay;
      if (!event.trusted || event.now - event.shownAt < HALF_SECOND) return stay;
      if (mode === "armed") {
        if (event.armedAt !== null && event.now - event.armedAt < HALF_SECOND) return stay;
        return { mode: "sending", act: "confirm" };
      }
      return event.production ? { mode: "armed", act: "arm" } : { mode: "sending", act: "confirm" };
    }
    case "deny":
      if (!event.trusted) return stay;
      return mode === "ask" || mode === "armed" || mode === "cannot" || mode === "changed" ? { mode: "ended", act: "cancel" } : stay;
    case "ran":
      return mode === "sending" ? { mode: "done", act: "none" } : stay;
    case "refused":
      if (mode !== "sending") return stay;
      return { mode: event.refusal === "ended" ? "ended" : event.refusal === "changed" ? "changed" : "ask", act: "none" };
    case "expired":
      return mode === "ask" || mode === "armed" || mode === "changed" || mode === "cannot" ? { mode: "ended", act: "none" } : stay;
    case "fresh":
      return mode === "changed" ? { mode: "ask", act: "none" } : stay;
    default:
      return stay;
  }
}

/** The label of a row action: `…` after it when the action asks first, so a person knows a card follows. */
export function actionLabel(action: { label: string; effect: Effect }): string {
  return action.effect === "read" || action.label.endsWith("…") ? action.label : `${action.label}…`;
}

/** Whether an action of this effect asks first. */
export function asksFirst(effect: Effect): boolean {
  return effect !== "read";
}

/** The Local verb that makes a value of `kind` the tool's own setting: the verb whose first argument has that kind. Found from the core's list, by no name. */
export function localVerbFor(tool: ToolEntry | null, kind: SelectKind): { verb: string; arg: string; label: string } | null {
  if (tool === null) return null;
  for (const verb of tool.verbs) {
    if (verb.effect !== "local") continue;
    const first = verb.args[0];
    if (first !== undefined && first.kind === kind && first.required) return { verb: verb.id, arg: first.name, label: verb.label };
  }
  return null;
}

/** The context slot of a held terminal: `exited 1`, or `exited` when the core gave no status. */
export function heldText(status: number | null): string {
  return status === null ? "exited" : `exited ${status}`;
}

// ---- the profiles dialog (docs/design.md 6.27) ------------------------------------------------------------------------

/** The bounds of the dialog's answer and fields. The page cuts what it reads and limits what it types; every rule is the core's. */
export const PROFILE_CAPS = {
  profiles: 64,
  notes: 16,
  note: 400,
  id: 64,
  name: 128,
  nameField: 64,
  selection: 253,
  envs: 64,
  envName: 64,
  envValue: 1024,
  secrets: 16,
  tool: 32,
  shown: 60,
} as const;

/** A secret variable of a profile: its name and whether the keychain holds a value. The value itself is never in the page. */
export interface ProfileSecret {
  name: string;
  has_value: boolean;
}

/** A profile as the core lists it. `tool` is the core's word, kept as text even when the page does not know it. */
export interface ProfileEntry {
  id: string;
  tool: string;
  name: string;
  context: string | null;
  namespace: string | null;
  subscription: string | null;
  section: string | null;
  node: string | null;
  production: boolean;
  isolated: boolean;
  env: { name: string; value: string }[];
  secrets: ProfileSecret[];
}

export interface ProfilesView {
  profiles: ProfileEntry[];
  notes: string[];
}

/** The answer of `integration_profiles` and `integration_profile_save`: `{profiles, notes}`. What is not the contract is dropped. */
export function readProfiles(raw: unknown): ProfilesView {
  const o = obj(raw);
  if (!o) return { profiles: [], notes: [] };
  const profiles: ProfileEntry[] = [];
  const seen = new Set<string>();
  for (const item of Array.isArray(o.profiles) ? o.profiles : []) {
    if (profiles.length >= PROFILE_CAPS.profiles) break;
    const p = obj(item);
    if (!p || typeof p.id !== "string" || p.id === "" || seen.has(p.id)) continue;
    const id = cut(p.id, PROFILE_CAPS.id);
    if (seen.has(id)) continue;
    seen.add(id);
    const env: { name: string; value: string }[] = [];
    const given = obj(p.env);
    for (const name of Object.keys(given ?? {})) {
      const value = given?.[name];
      if (name === "" || typeof value !== "string" || env.length >= PROFILE_CAPS.envs) continue;
      env.push({ name: cut(name, PROFILE_CAPS.envName), value: cut(value, PROFILE_CAPS.envValue) });
    }
    const secrets: ProfileSecret[] = [];
    for (const s of Array.isArray(p.secret_env) ? p.secret_env : []) {
      const e = obj(s);
      if (!e || typeof e.name !== "string" || e.name === "" || secrets.length >= PROFILE_CAPS.secrets) continue;
      secrets.push({ name: cut(e.name, PROFILE_CAPS.envName), has_value: e.has_value === true });
    }
    const name = str(p.name, PROFILE_CAPS.name);
    profiles.push({
      id,
      tool: str(p.tool, PROFILE_CAPS.tool),
      name: name === "" ? id : name,
      context: strOrNull(p.context, PROFILE_CAPS.selection),
      namespace: strOrNull(p.namespace, PROFILE_CAPS.selection),
      subscription: strOrNull(p.subscription, PROFILE_CAPS.selection),
      section: strOrNull(p.section, PROFILE_CAPS.selection),
      node: strOrNull(p.node, PROFILE_CAPS.selection),
      production: p.production === true,
      isolated: p.isolated === true,
      env,
      secrets,
    });
  }
  const notes = (Array.isArray(o.notes) ? o.notes : [])
    .filter((n): n is string => typeof n === "string" && n !== "")
    .slice(0, PROFILE_CAPS.notes)
    .map((n) => cut(n, PROFILE_CAPS.note));
  return { profiles, notes };
}

/** The members a profile pins, in the order the list shows them. */
const PINNED: readonly SelectKind[] = ["context", "namespace", "node", "subscription", "section"];

/** The pinned selection of a profile as the list shows it: `prod-eu · payments`, or a dash when it pins nothing. */
export function profileSelectionText(profile: ProfileEntry): string {
  const values = PINNED.map((kind) => profile[kind]).filter((v): v is string => v !== null && v !== "");
  return values.length === 0 ? "—" : values.map((v) => cutCell(v, PROFILE_CAPS.shown)).join(" · ");
}

/** The label and the example of each field a profile can pin. The example is a placeholder: the page never puts a value in a field. */
export const FIELD_WORDS: Readonly<Record<SelectKind, { label: string; example: string }>> = {
  context: { label: "Context", example: "prod-eu" },
  namespace: { label: "Namespace", example: "payments" },
  subscription: { label: "Subscription", example: "00000000-0000-0000-0000-000000000000" },
  section: { label: "Section", example: "work" },
  node: { label: "Node", example: "node-1" },
};

/**
 * The fields the form shows for a tool: what the core says the tool selects, in the core's order. The page names no tool, so a
 * tool the core adds has its form the day it lists it. A tool the core did not list has none.
 */
export function fieldsFor(tool: { selects: readonly SelectKind[] } | null): SelectKind[] {
  return tool === null ? [] : [...tool.selects];
}

/**
 * Whether the tool keeps a configuration folder of its own that a profile can ask for. The core's list has no member for it; the
 * convention of the core's descriptions is that a tool whose selection is a subscription or a section (the accounts of a cloud
 * or a workspace) signs in inside such a folder, and the core refuses the flag for any other tool in its own words.
 */
export function takesOwnFolder(tool: { selects: readonly SelectKind[] } | null): boolean {
  return tool !== null && (tool.selects.includes("subscription") || tool.selects.includes("section"));
}

const ID_MAX = 32;
const ID_FALLBACK = "profile";

/** A lower-case form of a name for an id: letters, digits, `-` and `_`, at most 32 characters, never starting or ending with a dash. */
export function slug(name: string): string {
  const made = name.toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^-+/, "");
  return made.slice(0, ID_MAX).replace(/-+$/, "");
}

/** `base` if no listed id is that, else `base-2`, `base-3`, and so on; a new profile can never replace another one. */
export function uniqueId(base: string, taken: readonly string[]): string {
  const root = base === "" ? ID_FALLBACK : base;
  if (!taken.includes(root)) return root;
  for (let n = 2; ; n += 1) {
    const tail = `-${n}`;
    const candidate = `${root.slice(0, ID_MAX - tail.length).replace(/-+$/, "")}${tail}`;
    if (!taken.includes(candidate)) return candidate;
  }
}

/** What the form holds, as text and flags. */
export interface FormValues {
  id: string;
  tool: string;
  name: string;
  fields: Partial<Record<SelectKind, string>>;
  production: boolean;
  isolated: boolean;
  env: { name: string; value: string }[];
  secrets: string[];
}

/** The profile a save sends: the shape of the profile file, one object. Rules are the core's; this only shapes what was typed. */
export interface ProfileToSave {
  id: string;
  tool: string;
  name: string;
  context: string | null;
  namespace: string | null;
  subscription: string | null;
  section: string | null;
  node: string | null;
  production: boolean;
  isolated: boolean;
  env: Record<string, string>;
  secret_env: string[];
}

/**
 * The object `integration_profile_save` takes. A selection field the tool does not select is null, and so is an empty one (the ends
 * are trimmed); `isolated` is false for a tool without a folder of its own; a variable row with neither a name nor a value is a row
 * that was never filled in and is left out; a secret variable is its name alone.
 */
export function profileToSave(form: FormValues, selects: readonly SelectKind[], ownFolder: boolean): ProfileToSave {
  const pinned = (kind: SelectKind): string | null => {
    if (!selects.includes(kind)) return null;
    const text = (form.fields[kind] ?? "").trim();
    return text === "" ? null : text;
  };
  const env: Record<string, string> = {};
  for (const row of form.env) {
    const name = row.name.trim();
    if (name === "" && row.value === "") continue;
    env[name] = row.value;
  }
  return {
    id: form.id,
    tool: form.tool,
    name: form.name,
    context: pinned("context"),
    namespace: pinned("namespace"),
    subscription: pinned("subscription"),
    section: pinned("section"),
    node: pinned("node"),
    production: form.production,
    isolated: ownFolder && form.isolated,
    env,
    secret_env: form.secrets.map((n) => n.trim()).filter((n) => n !== ""),
  };
}

/** The first variable name that two rows share (an object cannot hold both), or null. Blank names are not names. */
export function duplicateName(rows: readonly { name: string }[]): string | null {
  const seen = new Set<string>();
  for (const row of rows) {
    const name = row.name.trim();
    if (name === "") continue;
    if (seen.has(name)) return name;
    seen.add(name);
  }
  return null;
}

/** The words of the sentences of the dialog that are the page's own. */
export const PROFILE_WORDS = {
  sync:
    "The profile file is synced between your computers. Secret values are kept in the keychain and synced inside the encrypted secrets. Paths of this computer are never part of a profile.",
  secretsHint: "Saved values are never shown. Type a new one to replace it.",
  saveFirst: "Save the profile first.",
  saved: "saved",
  notSet: "not set",
} as const;

/** The bounds the page keeps for the rows; the core's own limits are the ones that decide. */
export const ROW_CAPS = { env: 32, secrets: 8 } as const;

/**
 * The index of the first name of `names` that the core's sentence carries as a word of its own (`profile prod: env LD_PRELOAD is not
 * allowed`), or -1. The page does not interpret the sentence: it only finds the row to put it on. A longer name is another name, and
 * the case counts.
 */
export function namedRow(text: string, names: readonly string[]): number {
  const words = new Set(text.split(/[^A-Za-z0-9_]+/).filter((w) => w !== ""));
  return names.findIndex((n) => n !== "" && words.has(n));
}
