// The pure part of the session marks of the browser build (docs/design.md 6.30): what the page knows about its session
// and what it says in the status line about it. No DOM, no network and no clock of its own: every function that depends
// on time takes `now` (milliseconds), so node tests it on the compiled file with the clock in the table.
import { messageFor } from "./loginstate.js";

/** A wrong passphrase and a wrong recovery code say the same (design 6.28). */
export const UNLOCK_FAILED = "That did not unlock your secrets.";

/** The status and the parsed body of a failed request: an `HttpError` has them, a rejection of anything else is the network failing (status 0). */
export function failureOf(e: unknown): { status: number; body?: Record<string, unknown> } {
  const f = e as { status?: unknown; body?: unknown } | null;
  const status = typeof f?.status === "number" ? f.status : 0;
  const body = f?.body;
  return typeof body === "object" && body !== null && !Array.isArray(body) ? { status, body: body as Record<string, unknown> } : { status };
}

/** The sentence for a failed `POST /auth/unlock`. `status` is 0 when nothing arrived. */
export function unlockMessage(status: number, body?: Record<string, unknown>): string {
  if (status === 429 || status === 503 || status === 0 || status >= 500) return messageFor(status, body);
  return UNLOCK_FAILED;
}

/** What the server tells about the account that is signed in. */
export interface Account {
  id: string;
  username: string;
  displayName: string;
  role: "admin" | "member";
}

/** The answer of `GET /api/session`, after checking. */
export interface SessionInfo {
  state: "anonymous" | "signed_in" | "locked";
  csrf: string;
  account: Account | null;
  /** Seconds until the idle expiry, or null when the answer has none. */
  seconds: number | null;
  /** Seconds left of the recent sign-in window (0 when none). */
  stepUp: number;
}

/** Whether the person's saved passwords are open. `unknown` until the server has said. */
export type Store = "unknown" | "locked" | "unlocked";

/** What the page believes about the session, the secret store and the connection. */
export interface Model {
  store: Store;
  /** The session is over: the server said so, or closed the socket with 4401, or the lookup found nobody signed in. */
  ended: boolean;
  /** When the idle expiry comes, in milliseconds of the page's clock, when the server last said it is 300 seconds away or less. */
  expiresAt: number | null;
  /** When the connection was lost, while it is down. */
  lostAt: number | null;
  /** The browser reports that it has no network. */
  offline: boolean;
  /** The server refused this page (close code 4403): nothing reconnects, and the mark stays. */
  refused: boolean;
}

/** One entry of the status line. */
export interface Mark {
  /** The kind. */
  id: "ended" | "offline" | "reconnecting" | "expiring" | "locked";
  /** The shape: a character, or `lock` for the drawn lock. */
  glyph: string;
  word: string;
  /** The text button that belongs to the mark, if any. */
  action?: "unlock";
  tone: "plain" | "warn" | "danger";
}

/** The idle mark is shown for the last five minutes. */
export const EXPIRY_WINDOW_S = 300;
/** A lost connection draws nothing for one second: a quick reconnect shows no mark. */
export const RECONNECT_DELAY_MS = 1000;
/** The close codes of the server (design 6.30): the session is over, or this page is refused. */
export const CODE_ENDED = 4401;
export const CODE_REFUSED = 4403;

const MAX_TEXT = 256;

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);
const text = (v: unknown, max = MAX_TEXT): string | null => (typeof v === "string" && v.length <= max ? v : null);
const whole = (v: unknown): number | null => (typeof v === "number" && Number.isInteger(v) && v >= 0 ? v : null);

/** The account member of an answer, or null when it is not the documented shape. */
function readAccount(raw: unknown): Account | null {
  if (!isObject(raw)) return null;
  const id = typeof raw.id === "number" && Number.isSafeInteger(raw.id) ? String(raw.id) : text(raw.id, 64);
  const username = text(raw.username, 128);
  const displayName = text(raw.display_name, 128);
  const role = raw.role === "admin" || raw.role === "member" ? raw.role : null;
  if (id === null || id === "" || username === null || displayName === null || role === null) return null;
  return { id, username, displayName, role };
}

/** The answer of `GET /api/session` as the page uses it, or null when it is not an answer of the documented shape. */
export function readSessionInfo(raw: unknown): SessionInfo | null {
  if (!isObject(raw)) return null;
  const state = raw.state;
  if (state === "anonymous") return { state, csrf: "", account: null, seconds: null, stepUp: 0 };
  if (state !== "signed_in" && state !== "locked") return null;
  const csrf = text(raw.csrf);
  const account = readAccount(raw.account);
  if (csrf === null || account === null) return null;
  let seconds: number | null = null;
  if (raw.seconds !== undefined) {
    seconds = whole(raw.seconds);
    if (seconds === null) return null;
  }
  let stepUp = 0;
  if (raw.step_up !== undefined) {
    const n = whole(raw.step_up);
    if (n === null) return null;
    stepUp = n;
  }
  return { state, csrf, account, seconds, stepUp };
}

/** The model before the server has said anything. */
export function newModel(): Model {
  return { store: "unknown", ended: false, expiresAt: null, lostAt: null, offline: false, refused: false };
}

/** The expiry time for a report of `seconds` left: set when it is within the window, none beyond it. */
function expiryFor(seconds: number, now: number): number | null {
  return seconds <= EXPIRY_WINDOW_S ? now + seconds * 1000 : null;
}

/** The model after the answer of `GET /api/session`. */
export function fromInfo(model: Model, info: SessionInfo, now: number): Model {
  if (info.state === "anonymous") return { ...model, ended: true };
  const next: Model = { ...model, store: info.state === "locked" ? "locked" : "unlocked" };
  if (info.seconds !== null) next.expiresAt = expiryFor(info.seconds, now);
  return next;
}

/** The model after a `hw-session` event; an event that is not understood changes nothing. */
export function reduce(model: Model, event: unknown, now: number): Model {
  if (!isObject(event)) return model;
  const seconds = whole(event.seconds);
  switch (event.state) {
    case "ok":
      return { ...model, expiresAt: seconds === null ? null : expiryFor(seconds, now) };
    case "expiring":
      return { ...model, expiresAt: expiryFor(seconds ?? EXPIRY_WINDOW_S, now) };
    case "locked":
    case "unlocked": {
      const next: Model = { ...model, store: event.state };
      if (seconds !== null) next.expiresAt = expiryFor(seconds, now);
      return next;
    }
    case "ended":
      return { ...model, ended: true };
    default:
      return model;
  }
}

/** The model after a change of the connection: the loss and the return, and the two codes that end it. */
export function connection(model: Model, info: unknown, now: number): Model {
  if (!isObject(info)) return model;
  switch (info.state) {
    case "reconnecting":
      return model.lostAt === null ? { ...model, lostAt: now } : model;
    case "open":
      return model.lostAt === null ? model : { ...model, lostAt: null };
    case "ended":
      if (info.code === CODE_ENDED) return { ...model, ended: true, lostAt: null };
      if (info.code === CODE_REFUSED) return { ...model, refused: true, lostAt: null };
      return model;
    default:
      return model;
  }
}

/** The model after the browser reported that it went offline or came back. */
export function setOffline(model: Model, offline: boolean): Model {
  return model.offline === offline ? model : { ...model, offline };
}

/** The minutes the idle mark says, rounded up: 300 seconds are 5, 61 are 2, 60 and 1 are 1. */
export function minutesLeft(seconds: number): number {
  return Math.max(1, Math.ceil(seconds / 60));
}

/** The marks of the status line, in their order: ended, then offline or reconnecting, then the idle expiry, then the lock. With the session ended only that mark remains. */
export function marks(model: Model, now: number): Mark[] {
  if (model.ended) return [{ id: "ended", glyph: "!", word: "session ended", tone: "danger" }];
  const out: Mark[] = [];
  if (model.offline || model.refused) {
    out.push({ id: "offline", glyph: "!", word: "offline", tone: "danger" });
  } else if (model.lostAt !== null && now - model.lostAt >= RECONNECT_DELAY_MS) {
    out.push({ id: "reconnecting", glyph: "\u25cc", word: "reconnecting", tone: "warn" });
  }
  if (model.expiresAt !== null) {
    const left = (model.expiresAt - now) / 1000;
    if (left > 0 && left <= EXPIRY_WINDOW_S) {
      out.push({ id: "expiring", glyph: "\u25d4", word: `session ends in ${minutesLeft(left)} min`, tone: "warn" });
    }
  }
  if (model.store === "locked") out.push({ id: "locked", glyph: "lock", word: "secrets locked", action: "unlock", tone: "plain" });
  return out;
}

/** The milliseconds until the marks change by themselves (the end of the one second, the end of the minute), or null when nothing will. */
export function nextChange(model: Model, now: number): number | null {
  if (model.ended) return null;
  let next: number | null = null;
  const soonest = (ms: number): void => {
    if (ms > 0 && (next === null || ms < next)) next = ms;
  };
  if (model.lostAt !== null && !model.offline && !model.refused) soonest(RECONNECT_DELAY_MS - (now - model.lostAt));
  if (model.expiresAt !== null) {
    const left = model.expiresAt - now;
    if (left > 0) {
      if (left > EXPIRY_WINDOW_S * 1000) soonest(left - EXPIRY_WINDOW_S * 1000);
      else soonest(left - (minutesLeft(left / 1000) - 1) * 60_000);
    }
  }
  return next;
}
