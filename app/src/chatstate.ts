// The page mirror of an AI chat: the state of one conversation, the events that change it and the rule that applies an
// event. Pure: it imports nothing, touches no browser object and sends nothing to the core. Every text in it is model
// or user text, kept as a plain string; whoever draws it uses text nodes only.
//
// The reducer `applyChatEvent` is a copy of the core's `ChatState::apply` (core/src/ai/service/events.rs). The two are
// kept equal by replaying the stream the core recorded (tests/fixtures/ai_stream.json) and comparing every state.

/** The words of `Locality` in the core. */
export type Locality = "on_device" | "loopback" | "lan" | "cloud";
/** Who wrote a message. */
export type Role = "user" | "assistant" | "tool";
/** How a message ended up. */
export type MessageStatus = "complete" | "cancelled" | "failed" | "interrupted";
/** What a context item holds. */
export type ContextKind = "selection" | "screen" | "last_command" | "file" | "preview";

/** Token counts; zero means not reported. */
export interface Usage {
  input_tokens: number;
  output_tokens: number;
}

/** What was trimmed from the history to fit the model. */
export interface Trimmed {
  results: number;
  exchanges: number;
}

/** Where a turn goes. */
export interface Route {
  profile: string;
  model: string;
  /** The host as it is shown; null for a provider that has no address. */
  host: string | null;
  locality: Locality;
}

/** One item of a context, described without its text. */
export interface ChatItem {
  kind: ContextKind;
  label: string;
  bytes: number;
}

/** One piece of a message. */
export type ChatPart =
  | { type: "text"; text: string }
  | { type: "tool_call"; id: string; name: string; summary: string }
  | { type: "tool_result"; call_id: string; content: string; is_error: boolean }
  | { type: "context"; hash: string; items: ChatItem[]; provider: string; locality: Locality };

/** A message as the page keeps it. It has no provider-native turn and no stored context text. */
export interface ChatMessage {
  id: string;
  role: Role;
  ts: number;
  model: string | null;
  status: MessageStatus;
  usage: Usage | null;
  parts: ChatPart[];
}

/** Why a turn failed: the stable class and a scrubbed sentence. */
export interface ErrorView {
  class: string;
  text: string;
}

/** How a turn ended. `end` is a word of the core: `done`, `cancelled`, `failed` or a `limit_` name. */
export interface EndView {
  end: string;
  error: ErrorView | null;
  rounds: number;
  usage: Usage;
  saved: boolean;
}

/**
 * A call that waits for a decision. Only the three named members are checked here; every other member the core sent
 * (the payload and its hash, the target, the taint) is passed through for the approval card, which reads them.
 */
export interface PendingCall {
  conversation: string;
  call_id: string;
  tool: string;
  [member: string]: unknown;
}

/** The answer that is being streamed and has no message yet. */
export interface StreamedText {
  text: string;
  reasoning: string;
}

/** A chat as the page holds it; the same thirteen members as the core's `ChatState`. */
export interface ChatState {
  conversation: string;
  title: string;
  profile: string | null;
  model: string | null;
  /** The latest turn this state has seen; 0 before any. */
  turn: number;
  /** The last `seq` applied in that turn. */
  seq: number;
  running: boolean;
  messages: ChatMessage[];
  partial: StreamedText;
  pending: PendingCall[];
  usage: Usage;
  trimmed: Trimmed;
  ended: EndView | null;
}

interface EventHead {
  conversation: string;
  turn: number;
  /** The position inside the turn, from 1. */
  seq: number;
}

/** One event of a turn: the head, then the members of its kind. */
export type ChatEvent = EventHead &
  (
    | { kind: "turn_started"; message: ChatMessage; route: Route }
    | { kind: "delta"; text: string }
    | { kind: "reasoning"; text: string }
    | ({ kind: "usage" } & Usage)
    | ({ kind: "trimmed" } & Trimmed)
    | { kind: "message"; message: ChatMessage }
    | { kind: "tool_proposed"; pending: PendingCall }
    | { kind: "tool_result"; call_id: string; tool: string; content: string; is_error: boolean; redactions: number }
    | ({ kind: "turn_ended" } & EndView)
  );

/** What `applyChatEvent` did with an event. */
export type ApplyResult = "applied" | "stale" | "gap";

const CHAT_ID = /^[A-Za-z0-9_-]{1,64}$/;

/**
 * True for 1 to 64 characters of letters, digits, underscore and hyphen. This is a syntactic filter that keeps an odd
 * value away from every command; the core's rule (which also refuses the Windows device names) is the authority.
 */
export function isChatId(text: unknown): text is string {
  return typeof text === "string" && CHAT_ID.test(text);
}

/** The empty state of a chat that has no messages and no turn; equal to the core's `ChatState::new`. */
export function newChatState(conversation: string): ChatState {
  return {
    conversation,
    title: "",
    profile: null,
    model: null,
    turn: 0,
    seq: 0,
    running: false,
    messages: [],
    partial: { text: "", reasoning: "" },
    pending: [],
    usage: { input_tokens: 0, output_tokens: 0 },
    trimmed: { results: 0, exchanges: 0 },
    ended: null,
  };
}

/** The text parts of a message, joined without a separator. */
export function messageText(message: ChatMessage): string {
  let out = "";
  for (const part of message.parts) {
    if (part.type === "text") out += part.text;
  }
  return out;
}

// ---- the title ----------------------------------------------------------------------------------------------------

/** The longest scrubbed line, in characters (the core's `MAX_DETAIL`). */
const DETAIL_CHARS = 240;
/** The longest title, in characters (the core's `TITLE_CHARS`). */
const TITLE_CHARS = 60;
const ELLIPSIS = "…";
// Control, bidirectional, zero-width and tag characters: the core replaces each of them with a space.
const UNSAFE = /^[\p{Cc}\u{202A}-\u{202E}\u{2066}-\u{2069}\u{200B}-\u{200F}\u{2060}-\u{2064}\u{FEFF}\u{E0000}-\u{E007F}]$/u;
const SPACE = /^\p{White_Space}$/u;

/** One line made safe to show: unsafe characters become spaces, white space collapses, a long line is cut. */
function scrubLine(line: string): string {
  let out = "";
  let outChars = 0;
  let word = "";
  let wordChars = 0;
  const flush = (): boolean => {
    if (wordChars === 0) return false;
    if (outChars > 0) {
      out += " ";
      outChars += 1;
    }
    out += word;
    outChars += wordChars;
    word = "";
    wordChars = 0;
    return outChars > DETAIL_CHARS;
  };
  let over = false;
  for (const c of line) {
    if (UNSAFE.test(c) || SPACE.test(c)) {
      if (flush()) {
        over = true;
        break;
      }
    } else {
      word += c;
      wordChars += 1;
    }
  }
  if (!over && flush()) over = true;
  if (!over) return out;
  return Array.from(out).slice(0, DETAIL_CHARS).join("").trimEnd() + ELLIPSIS;
}

/** The title of a chat made from its first message: the first line with text, scrubbed, cut at 60 characters. */
function titleFrom(text: string): string {
  for (const line of text.split("\n")) {
    const scrubbed = scrubLine(line);
    if (scrubbed === "") continue;
    const chars = Array.from(scrubbed);
    if (chars.length <= TITLE_CHARS) return scrubbed;
    return chars.slice(0, TITLE_CHARS).join("").trimEnd() + ELLIPSIS;
  }
  return "";
}

// ---- the reducer --------------------------------------------------------------------------------------------------

const KINDS: ReadonlySet<string> = new Set([
  "turn_started",
  "delta",
  "reasoning",
  "usage",
  "trimmed",
  "message",
  "tool_proposed",
  "tool_result",
  "turn_ended",
]);

/** A new object with every member of `state`; the lists and nested values are shared and never changed in place. */
function copyState(state: ChatState): ChatState {
  return {
    conversation: state.conversation,
    title: state.title,
    profile: state.profile,
    model: state.model,
    turn: state.turn,
    seq: state.seq,
    running: state.running,
    messages: state.messages,
    partial: state.partial,
    pending: state.pending,
    usage: state.usage,
    trimmed: state.trimmed,
    ended: state.ended,
  };
}

/** Clears the members that belong to one turn, as the core's `reset_turn` does. */
function resetTurn(s: ChatState): void {
  s.running = false;
  s.partial = { text: "", reasoning: "" };
  s.pending = [];
  s.usage = { input_tokens: 0, output_tokens: 0 };
  s.trimmed = { results: 0, exchanges: 0 };
  s.ended = null;
}

/**
 * Applies one event, by the rules of the core, in this order. An event of another conversation is `stale`. In the same
 * turn a `seq` at or below the state's is `stale`, exactly one higher is applied and anything further ahead is a `gap`.
 * An event of another turn is applied only at `seq` 1 (the per-turn members are reset first) and is a `gap` at any other
 * `seq`. A kind this module does not know is a `gap` too, so the next snapshot tells the truth. A `stale` or `gap`
 * answer holds the very state object that was passed in. Neither the state nor the event is ever changed.
 */
export function applyChatEvent(state: ChatState, event: ChatEvent): { state: ChatState; result: ApplyResult } {
  if (event.conversation !== state.conversation) return { state, result: "stale" };
  const sameTurn = event.turn === state.turn;
  if (sameTurn) {
    if (event.seq <= state.seq) return { state, result: "stale" };
    if (event.seq !== state.seq + 1) return { state, result: "gap" };
  } else if (event.seq !== 1) {
    return { state, result: "gap" };
  }
  if (!KINDS.has(event.kind)) return { state, result: "gap" };
  const next = copyState(state);
  if (!sameTurn) resetTurn(next);
  next.turn = event.turn;
  next.seq = event.seq;
  switch (event.kind) {
    case "turn_started":
      if (next.title === "") next.title = titleFrom(messageText(event.message));
      next.messages = [...next.messages, event.message];
      resetTurn(next);
      next.running = true;
      next.profile = event.route.profile;
      next.model = event.route.model;
      break;
    case "delta":
      next.partial = { text: next.partial.text + event.text, reasoning: next.partial.reasoning };
      break;
    case "reasoning":
      next.partial = { text: next.partial.text, reasoning: next.partial.reasoning + event.text };
      break;
    case "message":
      next.messages = [...next.messages, event.message];
      next.partial = { text: "", reasoning: "" };
      break;
    case "tool_proposed":
      next.pending = [...next.pending, event.pending];
      break;
    case "tool_result": {
      const id = event.call_id;
      next.pending = next.pending.filter((entry) => entry.call_id !== id);
      break;
    }
    case "usage":
      next.usage = { input_tokens: event.input_tokens, output_tokens: event.output_tokens };
      break;
    case "trimmed":
      next.trimmed = { results: event.results, exchanges: event.exchanges };
      break;
    case "turn_ended":
      next.running = false;
      next.partial = { text: "", reasoning: "" };
      next.pending = [];
      next.ended = {
        end: event.end,
        error: event.error,
        rounds: event.rounds,
        usage: event.usage,
        saved: event.saved,
      };
      break;
  }
  return { state: next, result: "applied" };
}

// ---- the readers --------------------------------------------------------------------------------------------------
//
// What reaches the page is untrusted: a corrupt file, a newer core or a hostile value must not become a state. Each
// reader checks every member against the declaration in the core, builds its result by naming the known members one by
// one (so a member the page does not use, such as a provider-native turn or stored context text, is never kept and a
// member named like the prototype cannot reach a result) and answers `null` for anything else.

/** The most messages a state may hold. The core's own limit is lower, so more than this is a corrupt or hostile value. */
export const MAX_MESSAGES = 4096;
/** The most parts one message may hold. The core's own limit is lower. */
export const MAX_PARTS = 1024;
/** The most calls that may wait for a decision at once. The core's own limit is lower. */
export const MAX_PENDING = 64;
/** The most items one context part may list. The core's own limit is lower. */
export const MAX_ITEMS = 64;
/** The most events a feed holds while it waits for a snapshot. */
export const MAX_HELD = 1024;

/**
 * How deep and how wide the members of a pending entry that pass through may be. A write card carries its diff: at most 4000
 * rows of five values each (the page's own cap, `MAX_DIFF_ROWS`), about 20000 values in all, so the budget is a little above that.
 * An entry over the budget is dropped as a whole, and the call would then wait with no card.
 */
const MAX_PASS_DEPTH = 8;
const MAX_PASS_NODES = 24576;

const ROLES: readonly Role[] = ["user", "assistant", "tool"];
const STATUSES: readonly MessageStatus[] = ["complete", "cancelled", "failed", "interrupted"];
const LOCALITIES: readonly Locality[] = ["on_device", "loopback", "lan", "cloud"];
const CONTEXT_KINDS: readonly ContextKind[] = ["selection", "screen", "last_command", "file", "preview"];
/** The longest `end` word the page accepts. */
const MAX_END_CHARS = 64;

type Obj = Record<string, unknown>;

function obj(raw: unknown): Obj | undefined {
  return typeof raw === "object" && raw !== null && !Array.isArray(raw) ? (raw as Obj) : undefined;
}

/** An own member of `o`; an inherited one never counts. */
function member(o: Obj, key: string): unknown {
  return Object.hasOwn(o, key) ? o[key] : undefined;
}

/** A safe integer from `min` to `max`, or undefined. */
function int(v: unknown, min: number, max: number): number | undefined {
  return typeof v === "number" && Number.isSafeInteger(v) && v >= min && v <= max ? v : undefined;
}

/** A count the core keeps in 32 bits. */
function u32(v: unknown): number | undefined {
  return int(v, 0, 0xffffffff);
}

/** A count the core keeps in 64 bits, as far as a number can hold it. */
function u64(v: unknown): number | undefined {
  return int(v, 0, Number.MAX_SAFE_INTEGER);
}

function str(v: unknown): string | undefined {
  return typeof v === "string" ? v : undefined;
}

/** A string or null; undefined when it is neither. */
function strOrNull(v: unknown): string | null | undefined {
  return v === null ? null : str(v);
}

function flag(v: unknown): boolean | undefined {
  return typeof v === "boolean" ? v : undefined;
}

function oneOf<T extends string>(v: unknown, words: readonly T[]): T | undefined {
  return typeof v === "string" ? words.find((w) => w === v) : undefined;
}

/** The elements of a list that is at most `max` long; undefined for anything else. */
function items(v: unknown, max: number): unknown[] | undefined {
  return Array.isArray(v) && v.length <= max ? (v as unknown[]) : undefined;
}

/** Reads every element with `read`; undefined as soon as one is refused. */
function each<T>(list: unknown[], read: (raw: unknown) => T | undefined): T[] | undefined {
  const out: T[] = [];
  for (const raw of list) {
    const one = read(raw);
    if (one === undefined) return undefined;
    out.push(one);
  }
  return out;
}

function readUsage(raw: unknown): Usage | undefined {
  const o = obj(raw);
  if (!o) return undefined;
  const input = u32(member(o, "input_tokens"));
  const output = u32(member(o, "output_tokens"));
  return input === undefined || output === undefined ? undefined : { input_tokens: input, output_tokens: output };
}

function readTrimmed(raw: unknown): Trimmed | undefined {
  const o = obj(raw);
  if (!o) return undefined;
  const results = u32(member(o, "results"));
  const exchanges = u32(member(o, "exchanges"));
  return results === undefined || exchanges === undefined ? undefined : { results, exchanges };
}

function readRoute(raw: unknown): Route | undefined {
  const o = obj(raw);
  if (!o) return undefined;
  const profile = str(member(o, "profile"));
  const model = str(member(o, "model"));
  const host = strOrNull(member(o, "host"));
  const locality = oneOf(member(o, "locality"), LOCALITIES);
  if (profile === undefined || model === undefined || host === undefined || locality === undefined) return undefined;
  return { profile, model, host, locality };
}

function readItem(raw: unknown): ChatItem | undefined {
  const o = obj(raw);
  if (!o) return undefined;
  const kind = oneOf(member(o, "kind"), CONTEXT_KINDS);
  const label = str(member(o, "label"));
  const bytes = u64(member(o, "bytes"));
  return kind === undefined || label === undefined || bytes === undefined ? undefined : { kind, label, bytes };
}

function readPart(raw: unknown): ChatPart | undefined {
  const o = obj(raw);
  if (!o) return undefined;
  switch (member(o, "type")) {
    case "text": {
      const text = str(member(o, "text"));
      return text === undefined ? undefined : { type: "text", text };
    }
    case "tool_call": {
      const id = str(member(o, "id"));
      const name = str(member(o, "name"));
      const summary = str(member(o, "summary"));
      return id === undefined || name === undefined || summary === undefined ? undefined : { type: "tool_call", id, name, summary };
    }
    case "tool_result": {
      const callId = str(member(o, "call_id"));
      const content = str(member(o, "content"));
      const isError = flag(member(o, "is_error"));
      if (callId === undefined || content === undefined || isError === undefined) return undefined;
      return { type: "tool_result", call_id: callId, content, is_error: isError };
    }
    case "context": {
      const hash = str(member(o, "hash"));
      const provider = str(member(o, "provider"));
      const locality = oneOf(member(o, "locality"), LOCALITIES);
      const list = items(member(o, "items"), MAX_ITEMS);
      const read = list === undefined ? undefined : each(list, readItem);
      if (hash === undefined || provider === undefined || locality === undefined || read === undefined) return undefined;
      return { type: "context", hash, items: read, provider, locality };
    }
    default:
      return undefined;
  }
}

function readMessageValue(raw: unknown): ChatMessage | undefined {
  const o = obj(raw);
  if (!o) return undefined;
  const id = str(member(o, "id"));
  const role = oneOf(member(o, "role"), ROLES);
  const ts = u64(member(o, "ts"));
  const model = strOrNull(member(o, "model"));
  const status = oneOf(member(o, "status"), STATUSES);
  const usageRaw = member(o, "usage");
  const usage = usageRaw === null ? null : readUsage(usageRaw);
  const list = items(member(o, "parts"), MAX_PARTS);
  const parts = list === undefined ? undefined : each(list, readPart);
  if (id === undefined || role === undefined || ts === undefined || model === undefined) return undefined;
  if (status === undefined || usage === undefined || parts === undefined) return undefined;
  return { id, role, ts, model, status, usage, parts };
}

/** A copy of a JSON value that keeps no member named like the prototype; undefined when it is too deep, too wide or not JSON. */
function copyData(v: unknown, depth: number, budget: { left: number }): unknown {
  budget.left -= 1;
  if (budget.left < 0) return undefined;
  if (v === null || typeof v === "string" || typeof v === "boolean") return v;
  if (typeof v === "number") return Number.isFinite(v) ? v : undefined;
  if (depth >= MAX_PASS_DEPTH || typeof v !== "object") return undefined;
  if (Array.isArray(v)) {
    const out: unknown[] = [];
    for (const element of v as unknown[]) {
      const copy = copyData(element, depth + 1, budget);
      if (copy === undefined) return undefined;
      out.push(copy);
    }
    return out;
  }
  const out: Obj = {};
  for (const key of Object.keys(v)) {
    if (key === "__proto__") continue;
    const copy = copyData((v as Obj)[key], depth + 1, budget);
    if (copy === undefined) return undefined;
    out[key] = copy;
  }
  return out;
}

/**
 * A pending entry: `conversation`, `call_id` and `tool` are checked (the entry must belong to the chat that carries
 * it), every other member the core sent is passed through for the approval card.
 */
function readPending(raw: unknown, conversation: string): PendingCall | undefined {
  const o = obj(raw);
  if (!o) return undefined;
  const owner = str(member(o, "conversation"));
  const callId = str(member(o, "call_id"));
  const tool = str(member(o, "tool"));
  if (owner !== conversation || callId === undefined || tool === undefined) return undefined;
  const out: PendingCall = { conversation: owner, call_id: callId, tool };
  const budget = { left: MAX_PASS_NODES };
  for (const key of Object.keys(o)) {
    if (key === "__proto__" || key === "conversation" || key === "call_id" || key === "tool") continue;
    const copy = copyData(o[key], 0, budget);
    if (copy === undefined) return undefined;
    out[key] = copy;
  }
  return out;
}

/** The members of an `EndView`; they sit in an object of their own in a state and at the top of a `turn_ended` event. */
function readEnd(o: Obj): EndView | undefined {
  const end = str(member(o, "end"));
  const errorRaw = member(o, "error");
  let error: ErrorView | null | undefined = null;
  if (errorRaw !== null) {
    const e = obj(errorRaw);
    const cls = e === undefined ? undefined : str(member(e, "class"));
    const text = e === undefined ? undefined : str(member(e, "text"));
    error = cls === undefined || text === undefined ? undefined : { class: cls, text };
  }
  const rounds = u32(member(o, "rounds"));
  const usage = readUsage(member(o, "usage"));
  const saved = flag(member(o, "saved"));
  if (end === undefined || end.length === 0 || end.length > MAX_END_CHARS) return undefined;
  if (error === undefined || rounds === undefined || usage === undefined || saved === undefined) return undefined;
  return { end, error, rounds, usage, saved };
}

/** A message, or null when it is not one. Only the members the page uses are kept. */
export function readChatMessage(raw: unknown): ChatMessage | null {
  return readMessageValue(raw) ?? null;
}

/** An event, or null when it is not one. The caller treats null as an event it did not receive. */
export function readChatEvent(raw: unknown): ChatEvent | null {
  const o = obj(raw);
  if (!o) return null;
  const conversation = member(o, "conversation");
  const turn = u64(member(o, "turn"));
  const seq = int(member(o, "seq"), 1, Number.MAX_SAFE_INTEGER);
  if (!isChatId(conversation) || turn === undefined || seq === undefined) return null;
  switch (member(o, "kind")) {
    case "turn_started": {
      const message = readMessageValue(member(o, "message"));
      const route = readRoute(member(o, "route"));
      return message === undefined || route === undefined ? null : { conversation, turn, seq, kind: "turn_started", message, route };
    }
    case "delta": {
      const text = str(member(o, "text"));
      return text === undefined ? null : { conversation, turn, seq, kind: "delta", text };
    }
    case "reasoning": {
      const text = str(member(o, "text"));
      return text === undefined ? null : { conversation, turn, seq, kind: "reasoning", text };
    }
    case "usage": {
      const usage = readUsage(o);
      return usage === undefined ? null : { conversation, turn, seq, kind: "usage", ...usage };
    }
    case "trimmed": {
      const trimmed = readTrimmed(o);
      return trimmed === undefined ? null : { conversation, turn, seq, kind: "trimmed", ...trimmed };
    }
    case "message": {
      const message = readMessageValue(member(o, "message"));
      return message === undefined ? null : { conversation, turn, seq, kind: "message", message };
    }
    case "tool_proposed": {
      const pending = readPending(member(o, "pending"), conversation);
      return pending === undefined ? null : { conversation, turn, seq, kind: "tool_proposed", pending };
    }
    case "tool_result": {
      const callId = str(member(o, "call_id"));
      const tool = str(member(o, "tool"));
      const content = str(member(o, "content"));
      const isError = flag(member(o, "is_error"));
      const redactions = u32(member(o, "redactions"));
      if (callId === undefined || tool === undefined || content === undefined) return null;
      if (isError === undefined || redactions === undefined) return null;
      return { conversation, turn, seq, kind: "tool_result", call_id: callId, tool, content, is_error: isError, redactions };
    }
    case "turn_ended": {
      const end = readEnd(o);
      return end === undefined ? null : { conversation, turn, seq, kind: "turn_ended", ...end };
    }
    default:
      return null;
  }
}

/** A state, or null when any member is missing, has the wrong type or is over its cap. Unknown members are dropped. */
export function readChatState(raw: unknown): ChatState | null {
  const o = obj(raw);
  if (!o) return null;
  const conversation = member(o, "conversation");
  const title = str(member(o, "title"));
  const profile = strOrNull(member(o, "profile"));
  const model = strOrNull(member(o, "model"));
  const turn = u64(member(o, "turn"));
  const seq = u64(member(o, "seq"));
  const running = flag(member(o, "running"));
  if (!isChatId(conversation) || title === undefined || profile === undefined || model === undefined) return null;
  if (turn === undefined || seq === undefined || running === undefined) return null;
  const messageList = items(member(o, "messages"), MAX_MESSAGES);
  const messages = messageList === undefined ? undefined : each(messageList, readMessageValue);
  const partialRaw = obj(member(o, "partial"));
  const partialText = partialRaw === undefined ? undefined : str(member(partialRaw, "text"));
  const partialReasoning = partialRaw === undefined ? undefined : str(member(partialRaw, "reasoning"));
  const pendingList = items(member(o, "pending"), MAX_PENDING);
  const pending = pendingList === undefined ? undefined : each(pendingList, (p) => readPending(p, conversation));
  const usage = readUsage(member(o, "usage"));
  const trimmed = readTrimmed(member(o, "trimmed"));
  const endedRaw = member(o, "ended");
  const endedObj = obj(endedRaw);
  const ended = endedRaw === null ? null : endedObj === undefined ? undefined : readEnd(endedObj);
  if (messages === undefined || partialText === undefined || partialReasoning === undefined) return null;
  if (pending === undefined || usage === undefined || trimmed === undefined || ended === undefined) return null;
  return {
    conversation,
    title,
    profile,
    model,
    turn,
    seq,
    running,
    messages,
    partial: { text: partialText, reasoning: partialReasoning },
    pending,
    usage,
    trimmed,
    ended,
  };
}

// ---- the feed -----------------------------------------------------------------------------------------------------

/** What `ChatFeed.receive` did: applied, nothing to do, kept for later, or the caller must ask for a snapshot now. */
export type FeedResult = "applied" | "stale" | "held" | "snapshot";
/** What `ChatFeed.install` did: the snapshot stands, another snapshot is needed, or it was not asked for. */
export type InstallResult = "installed" | "again" | "refused";

/**
 * One chat's state kept up to date from the events of the core. A sequence gap is never guessed at: the feed answers
 * `snapshot` once, keeps the events that arrive meanwhile (a delta is not self-contained, so dropping it would lose
 * text) and replays them after the snapshot through the same reducer.
 *
 * The feed has no timer, no wait and no link to the core; the chat view does the asking. The order for the caller:
 * construct the feed, ask the core for a snapshot and hand the answer to `install`. When `receive` answers `snapshot`,
 * ask for a snapshot and `install` it. When `install` answers `again`, ask once more. When a request is rejected, call
 * `failed`. Call `awaitSnapshot` before asking for a snapshot at any other time (a window gaining focus, a send that
 * returned, a reload): a last event that was lost cannot be seen from the events, and this is how it is repaired.
 */
export class ChatFeed {
  private readonly id: string;
  private current: ChatState;
  private isWaiting = false;
  private held: ChatEvent[] = [];

  /** A feed that waits for its first snapshot. */
  constructor(conversation: string) {
    this.id = conversation;
    this.current = newChatState(conversation);
    this.awaitSnapshot();
  }

  get conversation(): string {
    return this.id;
  }

  /** The state so far; replaced, never changed in place. */
  get state(): ChatState {
    return this.current;
  }

  /** Whether a snapshot is awaited. */
  get waiting(): boolean {
    return this.isWaiting;
  }

  /** Gives the feed one event. Another chat's event is `stale` and touches nothing. */
  receive(event: ChatEvent): FeedResult {
    if (event.conversation !== this.id) return "stale";
    if (this.isWaiting) {
      if (this.held.length >= MAX_HELD) {
        // Not an expected path: drop what was held and ask again, a fresher snapshot holds it all.
        this.held = [];
        return "snapshot";
      }
      this.held.push(event);
      return "held";
    }
    const out = applyChatEvent(this.current, event);
    if (out.result === "applied") {
      this.current = out.state;
      return "applied";
    }
    if (out.result === "stale") return "stale";
    this.isWaiting = true;
    this.held = [event];
    return "snapshot";
  }

  /**
   * Hands over the answer to a snapshot request. The state becomes the snapshot; the held events it already contains
   * (the last one of its turn at or below its `seq`, and everything before it) are dropped, and the others are replayed.
   * A replay that meets a gap stops: the snapshot was older than the hole, and the answer is `again`.
   */
  install(snapshot: ChatState): InstallResult {
    if (!this.isWaiting || snapshot.conversation !== this.id) return "refused";
    const held = this.held;
    this.held = [];
    this.isWaiting = false;
    let from = 0;
    for (let i = held.length - 1; i >= 0; i--) {
      const e = held[i];
      if (e !== undefined && e.turn === snapshot.turn && e.seq <= snapshot.seq) {
        from = i + 1;
        break;
      }
    }
    let state = snapshot;
    for (let i = from; i < held.length; i++) {
      const e = held[i];
      if (e === undefined) continue;
      const out = applyChatEvent(state, e);
      if (out.result === "applied") {
        state = out.state;
      } else if (out.result === "gap") {
        this.current = state;
        this.isWaiting = true;
        return "again";
      }
    }
    this.current = state;
    return "installed";
  }

  /** The snapshot request was rejected: stop waiting and forget the held events; the next event that does not fit asks again. */
  failed(): void {
    this.isWaiting = false;
    this.held = [];
  }

  /** A snapshot is about to be asked for: hold the events until it arrives. */
  awaitSnapshot(): void {
    this.isWaiting = true;
  }
}
