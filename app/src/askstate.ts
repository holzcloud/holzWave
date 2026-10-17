// The pure half of the card of a `wsh ai` request (docs/design.md section 6.20): how the core's card and its events are read,
// which words the card says and which presses count. No DOM, no bridge, no command call and no timer: whoever draws the card
// uses text nodes only, and the controller of `ask.ts` makes the one answer command.
//
// A card is the core's: every member is read by name (a member that is not named here never reaches the page, and an own
// `__proto__` is never copied), a malformed card makes the whole card unreadable, and the page never holds the piped text.
// The only text in a card is the prompt the core cleaned and will send, and it is shown in full with the characters a person
// cannot see written as escapes. Nothing here classifies a host or a profile: the place is the core's word.

import { leavesComputer, localityWord, utf8Bytes } from "./chatview.js";
import { showResult } from "./approvalstate.js";
import { findingWords, sizeWords } from "./consentstate.js";
import type { Finding } from "./consentstate.js";

/** A `Send once` that comes this soon (in milliseconds) after the card was first seen is ignored. `Don't send` is never ignored. */
export const IGNORE_MS = 500;
/** The most requests the page holds: the core lets four wait. */
export const MAX_ASKS = 4;

/** What a press decides: the core's two words. */
export type Verdict = "send" | "refuse";
/** Why a card closed: the core's six names. */
export type ClosedWhy = "sent" | "refused" | "gone" | "timed_out" | "pane_closed" | "shutting_down";

/** The two verdicts. */
export const VERDICTS: readonly Verdict[] = ["send", "refuse"];
/** The six reasons a card closes. */
export const CLOSED: readonly ClosedWhy[] = ["sent", "refused", "gone", "timed_out", "pane_closed", "shutting_down"];
/** The refusals of an answer the core can send, by their stable names. */
export const REASONS: readonly string[] = ["bad_verdict", "seal_mismatch", "unknown_ask"];

/** The longest prompt the core sends, in UTF-8 bytes. */
export const MAX_PROMPT = 16384;
/** The most findings one text reports: one per rule of the redactor. */
const MAX_FINDINGS = 22;
/** The longest rule name, profile, model, host and place kept, in characters. */
const MAX_RULE = 40;
const MAX_NAME = 200;
const MAX_HOST = 253;
const MAX_PLACE = 32;
/** The largest count a number of the core may have. */
const MAX_COUNT = 4294967295;
/** The longest fault text kept, in characters. */
const MAX_FAULT = 300;

/** What the card shows of the piped text: counts and findings, never the text. */
export interface AskInputView {
  lines: number;
  bytes: number;
  sent_bytes: number;
  findings: Finding[];
}

/** A card as the core made it: the ten members, each read by name. */
export interface AskView {
  ask: string;
  seal: string;
  pane: number;
  prompt: string;
  prompt_findings: Finding[];
  input: AskInputView | null;
  profile: string;
  model: string;
  host: string;
  locality: string;
}

/** An event of the core about a request: a card that waits, or a card that is over. */
export type AskEvent = { kind: "waiting"; view: AskView } | { kind: "closed"; ask: string; why: ClosedWhy };

/** A refusal of an answer as the card keeps it: the core's stable reason (or null) and its one line of text. */
export interface Fault {
  reason: string | null;
  text: string;
}

/** One request the page holds. */
export interface AskItem {
  view: AskView;
  /** The time the page first saw this request. */
  since: number;
  /** Whether an answer is on its way. */
  sending: boolean;
  fault: Fault | null;
}

type Obj = Record<string, unknown>;

/** The own member `name` of `o`; never one from the prototype chain. */
function member(o: Obj, name: string): unknown {
  return Object.hasOwn(o, name) ? o[name] : undefined;
}

/** A plain object (not an array), else null. */
function record(raw: unknown): Obj | null {
  return typeof raw === "object" && raw !== null && !Array.isArray(raw) ? (raw as Obj) : null;
}

/** An integer from `low` to `high`, else null. */
function whole(value: unknown, low: number, high: number): number | null {
  return typeof value === "number" && Number.isInteger(value) && value >= low && value <= high ? value : null;
}

/** Control, bidirectional and invisible formatting characters. */
const UNSAFE = /[\p{Cc}\u{202A}-\u{202E}\u{2066}-\u{2069}\u{200B}-\u{200F}\u{2060}-\u{2064}\u{FEFF}\u{E0000}-\u{E007F}]/gu;

/** `text` without unsafe characters, blanks collapsed and trimmed, cut at `max` characters. */
function line(text: string, max: number): string {
  const cleaned = text.replace(/\s+/gu, " ").replace(UNSAFE, "").replace(/ {2,}/g, " ").trim();
  return Array.from(cleaned).slice(0, max).join("");
}

/** A string member as one clean line, or null when it is no string. */
function words(value: unknown, max: number): string | null {
  return typeof value === "string" ? line(value, max) : null;
}

/** One finding; null when it is not `{rule, count}`. */
function readFinding(raw: unknown): Finding | null {
  const o = record(raw);
  if (o === null) return null;
  const rule = words(member(o, "rule"), MAX_RULE);
  const count = whole(member(o, "count"), 0, MAX_COUNT);
  return rule === null || rule === "" || count === null ? null : { rule, count };
}

/** A list of findings, at most one per rule; null when it is no list, too long or holds a malformed finding. */
function readFindings(raw: unknown): Finding[] | null {
  if (!Array.isArray(raw) || raw.length > MAX_FINDINGS) return null;
  const out: Finding[] = [];
  for (const entry of raw) {
    const finding = readFinding(entry);
    if (finding === null) return null;
    out.push(finding);
  }
  return out;
}

/** The counts of the piped text; null when any member is wrong. */
function readInput(raw: unknown): AskInputView | null {
  const o = record(raw);
  if (o === null) return null;
  const lines = whole(member(o, "lines"), 0, Number.MAX_SAFE_INTEGER);
  const bytes = whole(member(o, "bytes"), 0, Number.MAX_SAFE_INTEGER);
  const sent = whole(member(o, "sent_bytes"), 0, Number.MAX_SAFE_INTEGER);
  const findings = readFindings(member(o, "findings"));
  if (lines === null || bytes === null || sent === null || findings === null) return null;
  return { lines, bytes, sent_bytes: sent, findings };
}

/** The id of a request: 32 lower-case hexadecimal characters. */
const ASK_ID = /^[0-9a-f]{32}$/;
/** The seal of a card: 64 lower-case hexadecimal characters. */
const SEAL = /^[0-9a-f]{64}$/;

/**
 * Read one card. Exactly the ten members are kept; the answer is null when it is no object, an id or a seal has the wrong
 * shape, the pane is not a pane, the prompt is over the core's limit or any list or count is out of bounds.
 */
export function readAskView(raw: unknown): AskView | null {
  const o = record(raw);
  if (o === null) return null;
  const ask = member(o, "ask");
  const seal = member(o, "seal");
  const pane = whole(member(o, "pane"), 1, Number.MAX_SAFE_INTEGER);
  const prompt = member(o, "prompt");
  const promptFindings = readFindings(member(o, "prompt_findings"));
  const given = member(o, "input");
  const input = given === null ? null : readInput(given);
  const profile = words(member(o, "profile"), MAX_NAME);
  const model = words(member(o, "model"), MAX_NAME);
  const host = words(member(o, "host"), MAX_HOST);
  const locality = words(member(o, "locality"), MAX_PLACE);
  if (typeof ask !== "string" || !ASK_ID.test(ask) || typeof seal !== "string" || !SEAL.test(seal)) return null;
  if (pane === null || typeof prompt !== "string" || utf8Bytes(prompt) > MAX_PROMPT) return null;
  if (promptFindings === null || (given !== null && input === null)) return null;
  if (profile === null || model === null || host === null || locality === null) return null;
  return { ask, seal, pane, prompt, prompt_findings: promptFindings, input, profile, model, host, locality };
}

/** Read one event: a waiting card, or a closed one with a reason the page knows. Anything else is null. */
export function readAskEvent(raw: unknown): AskEvent | null {
  const o = record(raw);
  if (o === null) return null;
  const kind = member(o, "kind");
  if (kind === "waiting") {
    const view = readAskView(member(o, "view"));
    return view === null ? null : { kind: "waiting", view };
  }
  if (kind === "closed") {
    const ask = member(o, "ask");
    const why = CLOSED.find((name) => name === member(o, "why"));
    return typeof ask === "string" && ASK_ID.test(ask) && why !== undefined ? { kind: "closed", ask, why } : null;
  }
  return null;
}

/** The cards `ai_ask_pending` answers: the readable ones, at most `MAX_ASKS`. An answer that is no list is empty. */
export function readPending(raw: unknown): AskView[] {
  if (!Array.isArray(raw)) return [];
  const out: AskView[] = [];
  for (const entry of raw) {
    const view = readAskView(entry);
    if (view !== null && !out.some((known) => known.ask === view.ask) && out.length < MAX_ASKS) out.push(view);
  }
  return out;
}

// ---- the presses --------------------------------------------------------------------------------------------------------

/**
 * Whether a press on `Send once` counts: a person made it (`trusted`), the request is still there and no answer is on its
 * way, and the card has been on screen for `IGNORE_MS`. A click aimed at what stood there before the card came cannot send.
 */
export function canSend(item: AskItem | undefined, now: number, trusted: boolean): boolean {
  return trusted && item !== undefined && !item.sending && now - item.since >= IGNORE_MS;
}

/** Whether a press on `Don't send` counts: a person made it, the request is still there and no answer is on its way. */
export function canRefuse(item: AskItem | undefined, trusted: boolean): boolean {
  return trusted && item !== undefined && !item.sending;
}

// ---- the queue ------------------------------------------------------------------------------------------------------------

/** `items` with `item` added in arrival order. A request that is held already is ignored, and so is a fifth. */
export function addAsk(items: readonly AskItem[], item: AskItem): AskItem[] {
  if (items.some((known) => known.view.ask === item.view.ask) || items.length >= MAX_ASKS) return [...items];
  return [...items, item];
}

/** `items` without the request `ask`. */
export function removeAsk(items: readonly AskItem[], ask: string): AskItem[] {
  return items.filter((item) => item.view.ask !== ask);
}

// ---- the words ------------------------------------------------------------------------------------------------------------

/** Every fixed sentence of the card. The core's own sentences (a refusal) are shown as the core sent them. */
export const WORDS = {
  title: "Send this to the model?",
  no: "Don't send",
  yes: "Send once",
  sending: "Sending…",
  promptLabel: "Prompt",
  inputLabel: "Piped text",
  toLabel: "To",
  emptyPrompt: "none: the piped text is the question",
  noInput: "none",
  leaves: "leaves this computer",
  notWaiting: "That request is not waiting any more.",
  couldNot: "The answer could not be sent.",
  paletteTitle: "Answer the wsh ai request",
  head: (pane: number): string => `wsh ai · pane ${pane}`,
  lead: (pane: number): string => `A program in pane ${pane} asked this. Nothing has left this computer yet.`,
  foot: (pane: number): string =>
    `The answer is printed in pane ${pane}. holzWave does not keep this request. Esc refuses.`,
  count: (at: number, of: number): string => `${at} of ${of}`,
} as const;

/** The prompt as the card shows it: whole, a line feed and a tab as they are, every other hidden character as an escape. */
export function promptWords(view: AskView): string {
  return view.prompt === "" ? WORDS.emptyPrompt : showResult(view.prompt);
}

/** The piped text as counts: `142 lines · 6.1 KiB, 5.8 KiB after cleaning`; `none` when nothing was piped in. */
export function inputWords(input: AskInputView | null): string {
  if (input === null) return WORDS.noInput;
  const lines = `${input.lines} line${input.lines === 1 ? "" : "s"}`;
  return `${lines} · ${sizeWords(input.bytes)}, ${sizeWords(input.sent_bytes)} after cleaning`;
}

/** The findings of a text, one line of words for each, in the core's order. */
export function findingLines(findings: readonly Finding[]): string[] {
  return findings.map(findingWords);
}

/** Where the request goes in words: the route, the host and the place the core named, and whether the place leaves. */
export interface Route {
  route: string;
  host: string;
  place: string;
  leaves: boolean;
}

/** The destination of a card: `anthropic · example-model`, the host, and the core's place word. */
export function routeWords(view: AskView): Route {
  return {
    route: `${view.profile} · ${view.model}`,
    host: view.host,
    place: localityWord(view.locality),
    leaves: leavesComputer(view.locality),
  };
}

/** The place chip: the place word, followed by `· leaves this computer` when it does. */
export function placeWords(route: Route): string {
  return route.leaves ? `${route.place} · ${WORDS.leaves}` : route.place;
}

/**
 * The sentence a request that ended on its own leaves in the status line; null for an answered one (nothing is left) and for
 * the end of the app.
 */
export function closedNotice(why: ClosedWhy, pane: number): string | null {
  switch (why) {
    case "gone":
      return `The wsh ai request from pane ${pane} ended before it was answered.`;
    case "timed_out":
      return `The wsh ai request from pane ${pane} timed out; nothing was sent.`;
    case "pane_closed":
      return `The wsh ai request from pane ${pane} was withdrawn: the pane closed.`;
    case "sent":
    case "refused":
    case "shutting_down":
      return null;
  }
}

/**
 * A refusal of the answer command. The core answers `{code, reason, text}`: the text is its own sentence, cut and cleaned, and
 * the reason its stable name. Anything that is not such an object is the fixed sentence `The answer could not be sent.`.
 */
export function answerFault(raw: unknown): Fault {
  const o = record(raw);
  const text = o === null ? null : words(member(o, "text"), MAX_FAULT);
  if (o === null || text === null || text === "") return { reason: null, text: WORDS.couldNot };
  const named = member(o, "reason");
  const reason = typeof named === "string" && named !== "" ? line(named, MAX_RULE) : null;
  return { reason, text };
}
