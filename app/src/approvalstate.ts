// The pure half of the approval card (docs/design.md section 6.15): how a call that waits for a decision is read from the
// core's card, what the page may show of it and which characters are written as escapes. No DOM, no bridge, no command
// call and no timer. Every text in this file is the core's or a fixed sentence of the page: nothing here classifies a
// path, a pattern or a command, and nothing here summarises, shortens, reorders or translates a field. Whoever draws it
// uses text nodes only.
//
// The core makes the card and its hash from one list of fields; this module keeps that list as it came, in its order, and
// keeps the hash as it was sent. The page never computes or changes a hash.

import type { ChatMessage } from "./chatstate.js";
import { faultText } from "./chatview.js";
import { sizeWords } from "./consentstate.js";

/** What a press decides. The core's two words. */
export type Verdict = "allow" | "deny";

/** Why a request cannot be allowed here: `ok` when it can, else the first reason found. */
export type Why = "ok" | "large" | "tool" | "effect" | "shape";

/** One field of a card: its name, the core's escaped text of its value, and the length of the raw value. */
export interface PayloadRow {
  name: string;
  value: string;
  bytes: number;
}

/** A card as the page shows it: the members of the core's card, each read by name. */
export interface Approval {
  conversation: string;
  call_id: string;
  tool: string;
  effect: string;
  pane: number;
  target: string;
  remote: boolean;
  rows: PayloadRow[];
  hash: string;
  tainted: boolean;
  taint: string[];
  /** The changes of a write, as the core built them; an own member of a write card only. */
  diff?: Diff;
}

/** What kind of change a diff shows: the core's three names. */
export type DiffKind = "replace" | "create" | "edit";

/** One row of a diff, in the five shapes the core sends. A number the core leaves out is `null`. */
export type DiffRow =
  | { op: "ctx"; old: number; new: number; text: string }
  | { op: "del"; old: number; new: null; text: string }
  | { op: "add"; old: null; new: number; text: string }
  | { op: "gap"; skipped: number }
  | { op: "note"; text: string };

/** The changes a write would make: the kind, the counts and sizes the core worked out, and the rows in order. */
export interface Diff {
  kind: DiffKind;
  added: number;
  removed: number;
  before_bytes: number | null;
  after_bytes: number;
  rows: DiffRow[];
}

/** A pending call as the page read it: its ids, a valid hash or null, and the card when it can be shown in full. */
export interface Seen {
  id: string;
  tool: string;
  hash: string | null;
  approval: Approval | null;
  why: Why;
}

/** Where the result of a call goes: the route of the turn in the core's words. */
export interface Destination {
  model: string;
  host: string | null;
  /** The place word of the core's locality (`this computer`, `cloud`); null when it is not known. */
  place: string | null;
  /** Whether the place leaves the computer. */
  leaves: boolean;
}

/** What became of a tool call: it waits for a decision, ran, ended in an error, or has no result (the turn is over). */
export type ToolState = "waiting" | "ran" | "error" | "none";

/** One tool call of an assistant message as the transcript draws it. */
export interface ToolRow {
  id: string;
  name: string;
  summary: string;
  state: ToolState;
  /** The result as text, made visible and cut for display; null while there is none. */
  content: string | null;
  /** The length of the whole result in UTF-8 bytes. */
  bytes: number;
}

/** One piece of the line that says where the result goes. */
export interface DestinationPart {
  kind: "text" | "model" | "host" | "place";
  text: string;
  /** Whether this place leaves the computer; false for every other piece. */
  leaves: boolean;
}

/** The most rows a card is shown with; a longer request can only be denied. */
export const MAX_ROWS = 16;
/** The longest field name, in characters. */
export const MAX_NAME = 40;
/**
 * The longest value of one field, in characters. It is the escaped size of the largest `write_file` the tools allow (128 KiB
 * of soft hyphens, each written in six characters), and is held to the core by `limits` of the write card's golden.
 */
export const MAX_VALUE = 393216;
/** The most characters of all values of one card; held to the core's `limits` as well. */
export const MAX_TOTAL = 524288;
/** The most rows of a diff, and the most characters of its row text; a longer diff can only be denied. */
export const MAX_DIFF_ROWS = 4000;
export const MAX_DIFF_CHARS = 524288;
/** The most taint labels a card shows, and the longest one. */
export const MAX_LABELS = 8;
export const MAX_LABEL = 80;
/** The longest target label, in characters. */
export const MAX_TARGET = 200;
/** The most cards drawn at once; the rest are counted. */
export const MAX_CARDS = 16;
/** The longest summary of a tool row, in characters. */
export const MAX_SUMMARY = 120;
/** The most characters of a tool result that are drawn. */
export const MAX_SHOWN = 20000;
/** How many characters of the hash the card shows. */
export const HASH_SHOWN = 12;

/** The two words of a decision. */
export const VERDICTS: readonly Verdict[] = ["allow", "deny"];

/** The core's hash: 64 lower-case hexadecimal characters. */
const HASH = /^[0-9a-f]{64}$/;
/** A field name: lower-case words joined by underscores. */
const FIELD_NAME = /^[a-z][a-z0-9_]{0,39}$/;
/** The largest length the core counts in a `bytes` member. */
const MAX_BYTES = 4294967295;

/**
 * The characters a request could hide something with: controls (also the C1 range), the soft hyphen, direction marks,
 * line and paragraph separators, zero-width and invisible format characters, the byte order mark and the tag block.
 */
const HIDDEN = /[\u0000-\u001f\u007f-\u009f\u00ad\u061c\u200b-\u200f\u2028-\u202e\u2060-\u2064\u2066-\u2069\ufeff\u{e0000}-\u{e007f}]/gu;

/**
 * `text` as the card shows it: every control, direction, zero-width, format and tag character is written `\u{hex}` in
 * lower case (a line feed, a carriage return and a tab as `\n`, `\r` and `\t`), and nothing else changes. Nothing is
 * dropped, so two different texts never look the same.
 */
export function showText(text: string): string {
  return text.replace(HIDDEN, (c) => {
    if (c === "\n") return "\\n";
    if (c === "\r") return "\\r";
    if (c === "\t") return "\\t";
    return `\\u{${(c.codePointAt(0) ?? 0).toString(16)}}`;
  });
}

type Obj = Record<string, unknown>;

/** An own member of `o`; an inherited one never counts. */
function member(o: Obj, key: string): unknown {
  return Object.hasOwn(o, key) ? o[key] : undefined;
}

/** `text` shown and cut at `max` characters. */
function shown(text: string, max: number): string {
  return Array.from(showText(text)).slice(0, max).join("");
}

/** The taint labels: strings, at most `MAX_LABELS`, each shown and cut. Undefined when the member is not a list. */
function readLabels(raw: unknown): string[] | undefined {
  if (raw === undefined) return [];
  if (!Array.isArray(raw)) return undefined;
  const out: string[] = [];
  for (const item of raw as unknown[]) {
    if (out.length >= MAX_LABELS) break;
    if (typeof item === "string") out.push(shown(item, MAX_LABEL));
  }
  return out;
}

/** The payload rows, each field read by name. Undefined when one of them is not a field. */
function readRows(raw: unknown): PayloadRow[] | undefined {
  if (!Array.isArray(raw)) return undefined;
  const rows: PayloadRow[] = [];
  for (const item of raw as unknown[]) {
    if (typeof item !== "object" || item === null || Array.isArray(item)) return undefined;
    const o = item as Obj;
    const name = member(o, "name");
    const value = member(o, "value");
    const bytes = member(o, "bytes");
    if (typeof name !== "string" || !FIELD_NAME.test(name)) return undefined;
    if (typeof value !== "string") return undefined;
    if (typeof bytes !== "number" || !Number.isSafeInteger(bytes) || bytes < 0 || bytes > MAX_BYTES) return undefined;
    rows.push({ name, value: showText(value), bytes });
  }
  return rows;
}

/** A line number: a positive whole number. */
function lineNumber(raw: unknown): number | null {
  return typeof raw === "number" && Number.isSafeInteger(raw) && raw >= 1 && raw <= MAX_BYTES ? raw : null;
}

/** A count or size: a whole number that is not negative. */
function whole(raw: unknown): number | null {
  return typeof raw === "number" && Number.isSafeInteger(raw) && raw >= 0 && raw <= MAX_BYTES ? raw : null;
}

/** One row of a diff, each member read by name; only the members of its shape are copied. Null when it has no valid shape. */
function readRow(raw: unknown): DiffRow | null {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return null;
  const o = raw as Obj;
  const op = member(o, "op");
  const text = member(o, "text");
  switch (op) {
    case "ctx": {
      const old = lineNumber(member(o, "old"));
      const next = lineNumber(member(o, "new"));
      if (old === null || next === null || typeof text !== "string") return null;
      return { op, old, new: next, text: showText(text) };
    }
    case "del": {
      const old = lineNumber(member(o, "old"));
      if (old === null || member(o, "new") !== null || typeof text !== "string") return null;
      return { op, old, new: null, text: showText(text) };
    }
    case "add": {
      const next = lineNumber(member(o, "new"));
      if (next === null || member(o, "old") !== null || typeof text !== "string") return null;
      return { op, old: null, new: next, text: showText(text) };
    }
    case "gap": {
      const skipped = lineNumber(member(o, "skipped"));
      return skipped === null ? null : { op, skipped };
    }
    case "note":
      return typeof text === "string" ? { op, text: showText(text) } : null;
    default:
      return null;
  }
}

/**
 * The diff of a write card: every member read by name, every row read by its shape, the counts held against the rows. `large`
 * when it has more rows or more characters of row text than the page shows (a diff is shown in full or not at all), null when
 * anything is not as the core's shape says. The page computes nothing from a row's text.
 */
function readDiffResult(raw: unknown): Diff | "large" | null {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return null;
  const o = raw as Obj;
  const kind = member(o, "kind");
  const added = whole(member(o, "added"));
  const removed = whole(member(o, "removed"));
  const afterBytes = whole(member(o, "after_bytes"));
  const sent = member(o, "before_bytes");
  const beforeBytes = sent === null ? null : whole(sent);
  const list = member(o, "rows");
  if (kind !== "replace" && kind !== "create" && kind !== "edit") return null;
  if (added === null || removed === null || afterBytes === null || (sent !== null && beforeBytes === null)) return null;
  if (!Array.isArray(list)) return null;
  if (list.length > MAX_DIFF_ROWS) return "large";
  const rows: DiffRow[] = [];
  let chars = 0;
  let adds = 0;
  let dels = 0;
  for (const item of list as unknown[]) {
    const row = readRow(item);
    if (row === null) return null;
    if (row.op === "add") adds += 1;
    if (row.op === "del") dels += 1;
    if (row.op !== "gap") chars += row.text.length;
    rows.push(row);
  }
  if (adds !== added || dels !== removed) return null;
  if (chars > MAX_DIFF_CHARS) return "large";
  return { kind, added, removed, before_bytes: beforeBytes, after_bytes: afterBytes, rows };
}

/** The diff of a write card as the page shows it, or null when it cannot be shown in full (malformed, or over the caps). */
export function readDiff(raw: unknown): Diff | null {
  const read = readDiffResult(raw);
  return read === "large" ? null : read;
}

/** The card of a call with a valid hash, or the first reason it cannot be allowed, in the order shape, tool, effect, large. */
function readCard(o: Obj, id: string, tool: string, hash: string): Approval | Exclude<Why, "ok"> {
  const conversation = member(o, "conversation");
  const effect = member(o, "effect");
  const pane = member(o, "pane");
  const target = member(o, "target");
  const remote = member(o, "remote");
  const rows = readRows(member(o, "payload"));
  const labels = readLabels(member(o, "taint"));
  if (typeof conversation !== "string" || typeof effect !== "string") return "shape";
  if (typeof pane !== "number" || !Number.isSafeInteger(pane) || pane < 0) return "shape";
  if (typeof target !== "string" || typeof remote !== "boolean" || rows === undefined || labels === undefined) return "shape";
  const first = rows[0];
  const raw = member(o, "payload") as Obj[];
  if (first === undefined || first.name !== "tool" || member(raw[0] as Obj, "value") !== tool) return "shape";
  // A write card carries the changes it would make, and no other card does: what the tool does decides, not what the card says.
  const expected = Object.hasOwn(TOOL_EFFECT, tool) ? TOOL_EFFECT[tool] : undefined;
  const sentDiff = member(o, "diff");
  const diff = expected === "write" ? readDiffResult(sentDiff) : null;
  if (expected === "write" && diff === null) return "shape";
  if (expected !== undefined && expected !== "write" && sentDiff !== undefined) return "shape";
  // The page shows the tools and the effects it knows; every other request can only be denied.
  if (!Object.hasOwn(TOOL_WORDS, tool)) return "tool";
  if (!Object.hasOwn(EFFECT_WORDS, effect) || !HANDLED_EFFECTS.includes(effect) || (expected !== undefined && effect !== expected)) {
    return "effect";
  }
  if (diff === "large") return "large";
  // A request is shown in full or not at all: a card that had to be cut could hide the end of what runs.
  if (rows.length > MAX_ROWS) return "large";
  let total = 0;
  for (const item of raw) {
    const length = (member(item, "value") as string).length;
    if (length > MAX_VALUE) return "large";
    total += length;
  }
  if (total > MAX_TOTAL) return "large";
  const flag = member(o, "tainted");
  const card: Approval = {
    conversation, call_id: id, tool, effect, pane, target: shown(target, MAX_TARGET), remote, rows, hash,
    tainted: flag === true || labels.length > 0, taint: labels,
  };
  if (diff !== null) card.diff = diff;
  return card;
}

/**
 * Read one pending entry of the core. Each member is read by name, so a member the card does not have is never copied and
 * an own `__proto__` never is. An entry without a string call id and tool is nothing (null). The hash is accepted only as
 * 64 lower-case hexadecimal characters; without one the call has no card and cannot be answered. The hash that is kept
 * is the string the core sent.
 */
export function readApproval(raw: unknown): Seen | null {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return null;
  const o = raw as Obj;
  const id = member(o, "call_id");
  const tool = member(o, "tool");
  if (typeof id !== "string" || typeof tool !== "string") return null;
  const sent = member(o, "hash");
  const hash = typeof sent === "string" && HASH.test(sent) ? sent : null;
  const card = hash === null ? "shape" : readCard(o, id, tool, hash);
  if (typeof card === "string") return { id, tool, hash, approval: null, why: card };
  return { id, tool, hash, approval: card, why: "ok" };
}

/** Whether the card can be allowed: the page can show the whole request. */
export function canAllow(seen: Seen): boolean {
  return seen.approval !== null;
}

/** Whether the call can be denied: there is a hash to repeat. */
export function canDeny(seen: Seen): boolean {
  return seen.hash !== null;
}

/** A refusal of the decision command as the card keeps it: the core's stable reason (or null) and its one line of text. */
export interface Fault {
  reason: string | null;
  text: string;
}

/** The longest reason name the page keeps, in characters. */
const MAX_REASON = 40;

/**
 * A refusal of `ai_decide`. The core answers `{code, reason, text}`: the text is the core's own sentence, cut and cleaned
 * by the chat's rule, and the reason is its stable name. Anything that is not such an object is the fixed sentence
 * `The decision could not be sent.`, with no reason.
 */
export function approvalFault(raw: unknown): Fault {
  if (typeof raw !== "object" || raw === null || typeof (raw as { text?: unknown }).text !== "string") {
    return { reason: null, text: WORDS.couldNot };
  }
  const named = (raw as { reason?: unknown }).reason;
  return { reason: typeof named === "string" && named !== "" ? shown(named, MAX_REASON) : null, text: faultText(raw) };
}

/** What a refusal means for the card: the call is no longer waiting, the request changed, or the person may try again. */
export type Outcome = "ended" | "changed" | "refused";

/** The reasons that mean the call has been decided already or is over: the card turns into a quiet line. */
export const ENDED: ReadonlySet<string> = new Set([
  "unknown_call", "not_pending", "already_used", "cancelled", "duplicate_call", "too_many", "not_approved",
]);

/** The reasons that mean the request is no longer the one the card showed: the card is read again from the core. */
export const CHANGED: ReadonlySet<string> = new Set(["hash_mismatch", "payload_changed", "taint_changed", "scope_mismatch"]);

/** The step a refusal moves the card to. An unknown reason, `invalid_decision` and no reason keep the card for another try. */
export function outcomeOf(reason: string | null): Outcome {
  if (reason !== null && ENDED.has(reason)) return "ended";
  if (reason !== null && CHANGED.has(reason)) return "changed";
  return "refused";
}

/** The count after `✦ AI`: ` · 2`; empty when no call waits. */
export function waitingLabel(n: number): string {
  return n > 0 ? ` · ${n}` : "";
}

/** The accessible name of the title-bar button while calls wait; empty when none does. */
export function waitingName(n: number): string {
  if (n <= 0) return "";
  return n === 1 ? "AI chat, 1 call waits for your decision" : `AI chat, ${n} calls wait for your decision`;
}

/** The line that counts the calls that wait beyond the cards that are drawn. */
export function moreWords(n: number): string {
  return n === 1 ? "1 more call waits for a decision." : `${n} more calls wait for a decision.`;
}

/** The sentence under the title of a card that can only be denied, by the reason it cannot be allowed; empty for `ok`. */
export function cannotWords(why: Why): string {
  switch (why) {
    case "large":
      return "This request is too large to show in full. It can only be denied.";
    case "tool":
    case "effect":
      return "This page does not know this kind of request, so it cannot show it. It can only be denied.";
    case "shape":
      return "This request could not be read in full. It can only be denied.";
    case "ok":
      return "";
  }
}

// ---- the words ------------------------------------------------------------------------------------------------------

/** The words of each tool of the read set, keyed by the name the core uses. A tool that is not here cannot be allowed. */
export const TOOL_WORDS: Readonly<Record<string, string>> = Object.freeze({
  read_file: "Read a file",
  list_dir: "List a folder",
  grep: "Search files",
  read_pane: "Read a pane",
  write_file: "Write a file",
  edit_file: "Edit a file",
});

/** What each tool does to the world, by the core's names: the effect a card of that tool has. The core's table, held by a golden. */
export const TOOL_EFFECT: Readonly<Record<string, string>> = Object.freeze({
  read_file: "read",
  list_dir: "read",
  grep: "read",
  read_pane: "read",
  write_file: "write",
  edit_file: "write",
});

/** The effects the page handles: a card of any other effect can only be denied. */
export const HANDLED_EFFECTS: readonly string[] = Object.freeze(["read", "write"]);

/** What each effect does to the world, in words. The page handles the effects named here. */
export const EFFECT_WORDS: Readonly<Record<string, string>> = Object.freeze({
  read: "Reads only. Nothing is changed.",
  write: "Changes one file. Check the changes below.",
});

/** The fixed sentences of the card and of the tool rows. Nothing in them holds a path, a host or a field of a request. */
export const WORDS = Object.freeze({
  title: "Approval needed",
  allow: "Allow once",
  deny: "Deny",
  foot: "Allows this one call only. Nothing is kept for later. Esc denies.",
  sending: "Sending…",
  allowed: "Allowed · running…",
  denied: "Denied",
  ended: "That call is no longer waiting.",
  changed: "The request changed. Nothing ran. Read the card again.",
  remote: "remote host",
  bannerTitle: "Untrusted text in this turn",
  destinationUnknown: "The result is sent to the model of this chat.",
  fieldsLabel: "Fields of the request",
  couldNot: "The decision could not be sent.",
  cannotTitleLarge: "Cannot show in full",
  cannotTitle: "Cannot show this request",
  noHash: "This request cannot be shown or answered here. Stop the reply to end it.",
  resultLabel: "Result · untrusted text",
  hide: "Hide result",
  cut: "[cut for display]",
  destinationLead: "The result is sent to ",
  taintTail: "It can try to steer the assistant. Check the target and every field before you allow.",
  destinationWriteLead: "Only the outcome (saved or not) is sent to ",
  destinationWriteUnknown: "Only the outcome (saved or not) is sent to the model of this chat.",
  taintWrite: "A file will be changed. Check the changes below before you allow it.",
  diffReplace: "Replaces the whole file",
  diffCreate: "Creates a new file",
  diffEdit: "Changes parts of the file",
  diffNew: "new · ",
  stateRan: "ran",
  stateError: "error",
  stateWaiting: "waiting",
  stateNone: "no result",
});

/** `pane 1 · This computer`: the core's pane number and target label. */
export function targetWords(pane: number, target: string): string {
  return `pane ${pane} · ${target}`;
}

/** `hash 9f3a41c2d7be`: the first characters of the hash the decision will repeat. */
export function hashWords(hash: string): string {
  return `hash ${hash.slice(0, HASH_SHOWN)}`;
}

/** The line that says where the result of an approved call goes, as pieces: the place piece is the only one that can leave. */
export function destinationParts(dest: Destination | null, effect = "read"): DestinationPart[] {
  const write = effect === "write";
  if (dest === null) return [{ kind: "text", text: write ? WORDS.destinationWriteUnknown : WORDS.destinationUnknown, leaves: false }];
  const parts: DestinationPart[] = [
    { kind: "text", text: write ? WORDS.destinationWriteLead : WORDS.destinationLead, leaves: false },
    { kind: "model", text: dest.model, leaves: false },
  ];
  if (dest.host !== null && dest.host !== "") {
    parts.push({ kind: "text", text: " at ", leaves: false }, { kind: "host", text: dest.host, leaves: false });
  }
  if (dest.place !== null) {
    parts.push({ kind: "text", text: " (", leaves: false }, { kind: "place", text: dest.place, leaves: dest.leaves });
    parts.push({ kind: "text", text: ").", leaves: false });
  } else {
    parts.push({ kind: "text", text: ".", leaves: false });
  }
  return parts;
}

/** The banner of a turn that holds text the user did not write: its title, and a sentence that names the core's labels. */
export function taintWords(labels: readonly string[], effect = "read"): { title: string; text: string } {
  const named = labels.length === 0 ? "" : ` (${labels.join(", ")})`;
  const more = effect === "write" ? ` ${WORDS.taintWrite}` : "";
  return { title: WORDS.bannerTitle, text: `This turn contains text you did not write${named}. ${WORDS.taintTail}${more}` };
}

// ---- the diff ---------------------------------------------------------------------------------------------------------

/** The line above the rows of a diff, as three pieces: the kind in words, the counts the core worked out, and the sizes. */
export function diffHead(diff: Diff): { kind: string; counts: string; size: string } {
  const kind = diff.kind === "replace" ? WORDS.diffReplace : diff.kind === "create" ? WORDS.diffCreate : WORDS.diffEdit;
  const counts = `${diff.added} ${diff.added === 1 ? "line" : "lines"} added, ${diff.removed} removed`;
  const size = diff.before_bytes === null ? `${WORDS.diffNew}${sizeWords(diff.after_bytes)}` : `${sizeWords(diff.before_bytes)} → ${sizeWords(diff.after_bytes)}`;
  return { kind, counts, size };
}

/** What a gap row says of the run it stands for: `40 unchanged lines`. */
export function gapWords(skipped: number): string {
  return `${skipped} unchanged ${skipped === 1 ? "line" : "lines"}`;
}

// ---- the rows of tool calls -----------------------------------------------------------------------------------------

/** One result of a tool call as it came. */
export interface ToolResult {
  content: string;
  is_error: boolean;
}

/** The results of a chat by the id of the call they answer, made in one pass over the messages. */
export function indexResults(messages: readonly ChatMessage[]): Map<string, ToolResult> {
  const out = new Map<string, ToolResult>();
  for (const message of messages) {
    if (message.role !== "tool") continue;
    for (const part of message.parts) {
      if (part.type === "tool_result") out.set(part.call_id, { content: part.content, is_error: part.is_error });
    }
  }
  return out;
}

/** The length of `text` in UTF-8 bytes. */
function utf8Length(text: string): number {
  return new TextEncoder().encode(text).length;
}

/**
 * `text` made visible and cut at `max` characters of what is shown, never in the middle of a character; `tail` is added when
 * it was cut. Only the first `max` characters of a long text are looked at, so a huge result costs no more than a long one.
 */
function cutShown(text: string, max: number, tail: string): string {
  const visible = showText(text.length > max ? text.slice(0, max) : text);
  if (text.length <= max && visible.length <= max) return visible;
  let end = max;
  const last = visible.charCodeAt(end - 1);
  if (last >= 0xd800 && last <= 0xdbff) end -= 1;
  return `${visible.slice(0, end)}${tail}`;
}

/**
 * The rows of the tool calls of one assistant message, one for each `tool_call` part, in order. A call waits while the core
 * lists it as pending, or has no result while the turn runs; it `ran` or ended in an `error` when it has a result; and has
 * no result when the turn is over without one. A result for a call that no message holds is never looked up.
 */
export function rowsOf(
  message: ChatMessage,
  results: ReadonlyMap<string, ToolResult>,
  pending: ReadonlySet<string>,
  running: boolean,
): ToolRow[] {
  const rows: ToolRow[] = [];
  for (const part of message.parts) {
    if (part.type !== "tool_call") continue;
    const result = results.get(part.id);
    let state: ToolState;
    if (result !== undefined) state = result.is_error ? "error" : "ran";
    else state = pending.has(part.id) || running ? "waiting" : "none";
    rows.push({
      id: part.id,
      name: part.name,
      summary: cutShown(part.summary, MAX_SUMMARY, "…"),
      state,
      content: result === undefined ? null : cutShown(result.content, MAX_SHOWN, `\n${WORDS.cut}`),
      bytes: result === undefined ? 0 : utf8Length(result.content),
    });
  }
  return rows;
}

/** The state word at the right of a tool row; the word is the cue. */
export function stateWord(state: ToolState): string {
  switch (state) {
    case "ran":
      return WORDS.stateRan;
    case "error":
      return WORDS.stateError;
    case "waiting":
      return WORDS.stateWaiting;
    case "none":
      return WORDS.stateNone;
  }
}

/** The mark at the left of a tool row: `✓` ran, `!` error, `…` waiting, `–` no result. */
export function stateMark(state: ToolState): string {
  switch (state) {
    case "ran":
      return "✓";
    case "error":
      return "!";
    case "waiting":
      return "…";
    case "none":
      return "–";
  }
}

/** The text of the toggle of a result: `Show result (1.2 KiB)` while it is closed, `Hide result` while it is open. */
export function toggleWords(open: boolean, bytes: number): string {
  return open ? WORDS.hide : `Show result (${sizeWords(bytes)})`;
}
