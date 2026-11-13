// The pure part of signing in to a holzWave server from the desktop or mobile app (docs/design.md 6.35 and 6.9): the strict readers of
// what the host answers (the link, the start of a sign-in, one poll), the check of the address a person types, the closed set of the
// states of the wait, the pacing of the polls, and every word of the dialog as an exported constant that a unit test holds to the
// record. No DOM and no command call here, so node tests it on the compiled file.
//
// The host never sends a secret: its views have no member for a token or a device code, and these readers copy only the members they
// name, one by one, so a member a newer host adds (or a canary a test adds) never reaches the page. Every string is bounded and is
// shown with `textContent` only.
import { STRINGS } from "./vaultstate.js";

// ---- the words of the dialog (docs/design.md 6.35, the tables `Screen` and `State`) -----------------------------------------------

export const SIGN_IN_TITLE = "Sign in to the server";
export const ADDRESS_LABEL = "Server address";
export const ADDRESS_HELP = "The address of your holzWave server, starting with https://.";
export const ADDRESS_REFUSED = "Use an address that starts with https://.";
export const DEVICE_LABEL = "Device";
export const CONTINUE = "Continue";
export const CANCEL = "Cancel";
export const OPENING = "Opening your browser…";
export const COMPARE = "Enter this code only if you started this here.";
export const OPEN_AGAIN = "Open again";
export const START_AGAIN = "Start again";
export const CLOSE = "Close";
/** What the dialog says in place of its buttons that need the store of secrets while it is locked (design.md 6.6 and 6.34). */
export const LOCKED_LINE = STRINGS.answerConnect;

/** The links and the words of the connection row and of the popover (design.md 6.9 and 6.34). */
export const SIGN_IN_LINK = "Sign in…";
export const SIGN_OUT = "Sign out";
export const SIGN_OUT_ARMED = "Click again to sign out";
export const NOT_SIGNED_IN = "not signed in";
export const SIGNED_IN = "signed in";
export const SIGNING_IN = "signing in…";
export const UNREACHABLE = "unreachable";

// ---- the states of the second screen ----------------------------------------------------------------------------------------------

/** The states of the wait, in the order of the record's table. `failed` is a refusal of the host that the record has no word for. */
export type WaitState = "waiting" | "approved" | "denied" | "expired" | "too_many" | "unreachable" | "off" | "failed";

/** How a state is shown: the dot (a shape), whether it is a failure (the mark `!` and an underline), and the words. */
export interface WaitWords {
  dot: "ring" | "filled" | "mark";
  bad: boolean;
  word: string;
}

/** One entry per state; the host's word only selects among these and is never shown as it is. */
export const WAIT_WORDS: Readonly<Record<Exclude<WaitState, "failed">, WaitWords>> = {
  waiting: { dot: "ring", bad: false, word: "Waiting for approval…" },
  approved: { dot: "filled", bad: false, word: "Signed in" },
  denied: { dot: "mark", bad: true, word: "The request was denied." },
  expired: { dot: "mark", bad: true, word: "The code expired. Start again." },
  too_many: { dot: "mark", bad: true, word: "Too many devices or requests. Try again later." },
  unreachable: { dot: "mark", bad: true, word: "The server cannot be reached." },
  off: { dot: "mark", bad: true, word: "Device sign-in is off on this server." },
};

/** The words of a state; a `failed` state shows the host's own fixed sentence, bounded. */
export function waitWords(state: WaitState, sentence = ""): WaitWords {
  if (state === "failed") return { dot: "mark", bad: true, word: plain(sentence, MAX_SENTENCE) || "The sign-in failed." };
  return WAIT_WORDS[state];
}

/** The states after which the polling stops and the person can start again. */
export function isFailure(state: WaitState): boolean {
  return state !== "waiting" && state !== "approved";
}

// ---- reading what the host answers -----------------------------------------------------------------------------------------------

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);

/** An own member of `object`, never an inherited one. */
function own(object: Record<string, unknown>, key: string): unknown {
  return Object.hasOwn(object, key) ? object[key] : undefined;
}

// Control characters, line and paragraph separators and the characters that change the direction of text. Written as a string: the
// compiler turns a separator in a regular expression literal into the raw character, which a literal cannot hold.
const UNSEEN = new RegExp("[\\u0000-\\u001f\\u007f-\\u009f\\u2028\\u2029\\u200e\\u200f\\u202a-\\u202e\\u2066-\\u2069]", "gu");

/** The longest sentence of the host that is shown, in characters. */
export const MAX_SENTENCE = 200;
/** The longest device name that is shown, in characters (the host keeps no more). */
export const MAX_NAME = 64;
/** The longest address the page takes. */
export const MAX_ADDRESS = 253;
/** The longest address the host may ask the system browser to open. */
const MAX_OPEN_URL = 2048;

/** `text` as one line of plain text: unseen characters are blanks, the ends are trimmed and the length is bounded. */
export function plain(text: unknown, max: number): string {
  return typeof text === "string" ? text.replace(UNSEEN, " ").trim().slice(0, max) : "";
}

/** The states of the link as the host spells them. */
export type LinkState = "signed_out" | "signing_in" | "signed_in" | "unreachable";
const LINK_STATES: readonly LinkState[] = ["signed_out", "signing_in", "signed_in", "unreachable"];

/** The link to the server as the page reads it. No member holds a secret. */
export interface ServerView {
  state: LinkState;
  /** The address of the server (an `https` origin), or empty when this device never linked one. */
  url: string;
  /** The name of this device on the server. */
  name: string;
  /** The store of secrets is locked: the token cannot be read until the unlock. */
  locked: boolean;
  syncOn: boolean;
  remoteOn: boolean;
  /** The fixed sentence of the last failure, if there is one. */
  error: string | null;
}

/** What a view that is not well formed reads as: signed out, no address, nothing on. */
export const SIGNED_OUT: ServerView = { state: "signed_out", url: "", name: "", locked: false, syncOn: false, remoteOn: false, error: null };

/** The result of the page's own check of an address. */
export type AddressCheck = { ok: true; address: string } | { ok: false };

const LOOPBACK = new Set(["localhost", "127.0.0.1", "[::1]"]);

/**
 * The address check of the page, run before any command: an `https` origin, with no user name, path, query or fragment (one trailing
 * slash is dropped) and a bounded length. `http` is accepted for a loopback host only, so that the tests can run against a server on
 * this computer; the host refuses it again for the app. On success `address` is the origin as the browser writes it.
 */
export function checkAddress(raw: unknown): AddressCheck {
  if (typeof raw !== "string") return { ok: false };
  const text = raw.trim();
  if (text === "" || text.length > MAX_ADDRESS || /[\u0000- \u007f-\u009f\\?#@]/u.test(text)) return { ok: false };
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return { ok: false };
  }
  const secure = url.protocol === "https:";
  if (!secure && !(url.protocol === "http:" && LOOPBACK.has(url.hostname))) return { ok: false };
  if (url.username !== "" || url.password !== "" || url.search !== "" || url.hash !== "" || url.pathname !== "/") return { ok: false };
  if (url.hostname === "") return { ok: false };
  return { ok: true, address: url.origin };
}

/**
 * Read the answer of `server_status`, `server_refresh`, `server_sign_in_cancel` and `server_sign_out`: a plain object. The members are
 * named one by one and nothing is copied by the name it came with. A state word that is not exactly one of the four, or an address
 * that does not pass the address check, reads as the safe value (`signed_out`, no address).
 */
export function readServerView(raw: unknown): ServerView {
  if (!isObject(raw)) return SIGNED_OUT;
  const word = own(raw, "state");
  const state = typeof word === "string" ? (LINK_STATES.find((s) => s === word) ?? "signed_out") : "signed_out";
  const url = own(raw, "url");
  const checked = typeof url === "string" ? checkAddress(url) : { ok: false as const };
  const error = plain(own(raw, "error"), MAX_SENTENCE);
  return {
    state,
    url: checked.ok ? checked.address : "",
    name: plain(own(raw, "name"), MAX_NAME),
    locked: own(raw, "locked") === true,
    syncOn: own(raw, "sync_on") === true,
    remoteOn: own(raw, "remote_on") === true,
    error: error === "" ? null : error,
  };
}

/**
 * What the page needs to show a sign-in that started. The host's `expires_in` is not read: the server decides when a code expires and
 * the host's poll says `expired`, so the page keeps no clock of its own for it.
 */
export interface SignInView {
  /** The user code to compare, `XXXX-XXXX`. */
  code: string;
  /** The address the host asks the system browser to open. */
  openUrl: string;
  /** Seconds between two polls. */
  interval: number;
}

/** The letters of a user code; the host draws from the same twenty. */
const CODE = /^[BCDFGHJKLMNPQRSTVWXZ]{4}-[BCDFGHJKLMNPQRSTVWXZ]{4}$/u;
export const MIN_INTERVAL = 1;
export const MAX_INTERVAL = 60;
const DEFAULT_INTERVAL = 5;

/** A number of seconds from the host, whole and inside the bound; a value outside reads as `fallback`. */
function seconds(raw: unknown, fallback: number): number {
  return typeof raw === "number" && Number.isFinite(raw) && raw >= MIN_INTERVAL && raw <= MAX_INTERVAL ? Math.floor(raw) : fallback;
}

/**
 * Read the answer of `server_sign_in_start`: a code of the right shape, an address to open that is a web address of a bounded length,
 * and the interval. Anything else is null (no code, nothing to show); a member that is not one of these three is not read.
 */
export function readSignIn(raw: unknown): SignInView | null {
  if (!isObject(raw)) return null;
  const code = own(raw, "code");
  const open = own(raw, "url");
  if (typeof code !== "string" || !CODE.test(code)) return null;
  if (typeof open !== "string" || open.length > MAX_OPEN_URL || /[\u0000- \u007f-\u009f]/u.test(open)) return null;
  let url: URL;
  try {
    url = new URL(open);
  } catch {
    return null;
  }
  if (url.protocol !== "https:" && !(url.protocol === "http:" && LOOPBACK.has(url.hostname))) return null;
  return {
    code,
    openUrl: open,
    interval: seconds(own(raw, "interval"), DEFAULT_INTERVAL),
  };
}

/** What one poll of the host says: the state of the wait and the seconds to leave before the next poll. */
export interface SignInPoll {
  state: WaitState;
  interval: number;
}

/**
 * Read the answer of `server_sign_in_poll`. The host's words are `pending`, `approved`, `denied`, `expired`, `too_many`, `off` and
 * `unreachable`; any other word, or an answer that is no object, reads as `unreachable`, which stops the wait and shows a state of
 * the record instead of waiting on something the page does not understand.
 */
export function readPoll(raw: unknown): SignInPoll {
  if (!isObject(raw)) return { state: "unreachable", interval: DEFAULT_INTERVAL };
  const word = own(raw, "state");
  const interval = seconds(own(raw, "interval"), DEFAULT_INTERVAL);
  switch (word) {
    case "pending":
      return { state: "waiting", interval };
    case "approved":
    case "denied":
    case "expired":
    case "too_many":
    case "off":
    case "unreachable":
      return { state: word, interval };
    default:
      return { state: "unreachable", interval };
  }
}

// ---- the failure of a command ---------------------------------------------------------------------------------------------------

/** The fixed sentences of the host that name a state of the record. */
const HOST_UNREACHABLE = "The server cannot be reached.";
const HOST_OFF = "This feature is off on the server.";
/** The text of a store of secrets that is locked (the host's and the page's own constant of design.md 6.6). */
const HOST_LOCKED = "secrets are locked";

/** What a rejected command comes to on the first screen: the state of the record that its sentence names, or the sentence itself. */
export type Refusal =
  | { kind: "locked" }
  | { kind: "state"; state: "unreachable" | "off" }
  | { kind: "sentence"; text: string };

/** The refusal of a command: the host's fixed sentence is read as a state when it is one of the two that the record words. */
export function refusalOf(failure: unknown): Refusal {
  const raw = typeof failure === "string" ? failure : failure instanceof Error ? failure.message : "";
  const text = plain(raw, MAX_SENTENCE);
  if (text === HOST_LOCKED || text === STRINGS.answerConnect) return { kind: "locked" };
  if (text === HOST_UNREACHABLE) return { kind: "state", state: "unreachable" };
  if (text === HOST_OFF) return { kind: "state", state: "off" };
  return { kind: "sentence", text: text === "" ? "The sign-in failed." : text };
}

// ---- the pace of the polls --------------------------------------------------------------------------------------------------------

/**
 * Decides when the next poll is due, from the interval the host states and a clock the caller passes in (milliseconds), so a test
 * drives it with numbers and the page with `performance.now()`. The first poll is due one interval after the start, so the browser has
 * time to open; each answer sets the interval for the next one, bounded to 1 to 60 seconds. It never says a poll is due earlier than
 * the host asked.
 */
export class Pacer {
  private interval: number;
  private last: number;

  constructor(startedAt: number, intervalSeconds: number) {
    this.interval = seconds(intervalSeconds, DEFAULT_INTERVAL);
    this.last = startedAt;
  }

  /** The seconds the pacer is set to. */
  get seconds(): number {
    return this.interval;
  }

  /** Milliseconds until the next poll is due at `now`; zero when it is due. */
  wait(now: number): number {
    return Math.max(0, this.last + this.interval * 1000 - now);
  }

  /** Whether a poll may be sent at `now`. */
  due(now: number): boolean {
    return this.wait(now) === 0;
  }

  /** A poll was sent at `now` and the host asked for `intervalSeconds` before the next one. */
  polled(now: number, intervalSeconds: number): void {
    this.last = now;
    this.interval = seconds(intervalSeconds, this.interval);
  }
}
