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
