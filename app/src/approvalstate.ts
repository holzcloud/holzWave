// The pure half of the approval card (docs/design.md section 6.15): how a call that waits for a decision is read from the
// core's card, what the page may show of it and which characters are written as escapes. No DOM, no bridge, no command
// call and no timer. Every text in this file is the core's or a fixed sentence of the page: nothing here classifies a
// path, a pattern or a command, and nothing here summarises, shortens, reorders or translates a field. Whoever draws it
// uses text nodes only.
//
// The core makes the card and its hash from one list of fields; this module keeps that list as it came, in its order, and
// keeps the hash as it was sent. The page never computes or changes a hash.

import { faultText } from "./chatview.js";

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

/** The most rows a card is shown with; a longer request can only be denied. */
export const MAX_ROWS = 16;
/** The longest field name, in characters. */
export const MAX_NAME = 40;
/** The longest value of one field, in characters. */
export const MAX_VALUE = 65536;
/** The most characters of all values of one card. */
export const MAX_TOTAL = 131072;
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

/** The card of a call with a valid hash, or null when a member is of the wrong kind. */
function readCard(o: Obj, id: string, tool: string, hash: string): Approval | null {
  const conversation = member(o, "conversation");
  const effect = member(o, "effect");
  const pane = member(o, "pane");
  const target = member(o, "target");
  const remote = member(o, "remote");
  const rows = readRows(member(o, "payload"));
  const labels = readLabels(member(o, "taint"));
  if (typeof conversation !== "string" || typeof effect !== "string") return null;
  if (typeof pane !== "number" || !Number.isSafeInteger(pane) || pane < 0) return null;
  if (typeof target !== "string" || typeof remote !== "boolean" || rows === undefined || labels === undefined) return null;
  const first = rows[0];
  const raw = member(o, "payload") as Obj[];
  if (first === undefined || first.name !== "tool" || member(raw[0] as Obj, "value") !== tool) return null;
  const flag = member(o, "tainted");
  return {
    conversation, call_id: id, tool, effect, pane, target: shown(target, MAX_TARGET), remote, rows, hash,
    tainted: flag === true || labels.length > 0, taint: labels,
  };
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
  const approval = hash === null ? null : readCard(o, id, tool, hash);
  return { id, tool, hash, approval, why: approval === null ? "shape" : "ok" };
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
 * by the chat's rule, and the reason is its stable name. Anything that is not such an object keeps no reason.
 */
export function approvalFault(raw: unknown): Fault {
  let reason: string | null = null;
  if (typeof raw === "object" && raw !== null) {
    const named = (raw as { reason?: unknown }).reason;
    if (typeof named === "string" && named !== "") reason = shown(named, MAX_REASON);
  }
  return { reason, text: faultText(raw) };
}

// ---- the words ------------------------------------------------------------------------------------------------------

/** The words of each tool of the read set, keyed by the name the core uses. A tool that is not here cannot be allowed. */
export const TOOL_WORDS: Readonly<Record<string, string>> = Object.freeze({
  read_file: "Read a file",
  list_dir: "List a folder",
  grep: "Search files",
  read_pane: "Read a pane",
});

/** What each effect does to the world, in words. The page handles the effects named here. */
export const EFFECT_WORDS: Readonly<Record<string, string>> = Object.freeze({
  read: "Reads only. Nothing is changed.",
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
});

/** `pane 1 · This computer`: the core's pane number and target label. */
export function targetWords(pane: number, target: string): string {
  return `pane ${pane} · ${target}`;
}

/** `hash 9f3a41c2d7be`: the first characters of the hash the decision will repeat. */
export function hashWords(hash: string): string {
  return `hash ${hash.slice(0, HASH_SHOWN)}`;
}
