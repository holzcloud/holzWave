// The pure part of the session marks of the browser build (docs/design.md 6.30): what the page knows about its session
// and what it says in the status line about it. No DOM, no network and no clock of its own: every function that depends
// on time takes `now` (milliseconds), so node tests it on the compiled file with the clock in the table.
import { messageFor } from "./loginstate.js";

/** A wrong passphrase and a wrong recovery code say the same (design 6.28). */
export const UNLOCK_FAILED = "That did not unlock your secrets.";

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
  ended: boolean;
}

/** One entry of the status line. */
export interface Mark {
  /** The kind, also the order of the marks. */
  id: "locked";
  glyph: string;
  word: string;
  /** The text button that belongs to the mark, if any. */
  action?: "unlock";
  tone: "plain" | "warn" | "danger";
}

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
  return { store: "unknown", ended: false };
}

/** The model after the answer of `GET /api/session`. */
export function fromInfo(model: Model, info: SessionInfo): Model {
  if (info.state === "anonymous") return { ...model, ended: true };
  return { ...model, store: info.state === "locked" ? "locked" : "unlocked" };
}

/** The model after a `hw-session` event; an event that is not understood changes nothing. */
export function reduce(model: Model, event: unknown): Model {
  if (!isObject(event)) return model;
  switch (event.state) {
    case "locked":
      return { ...model, store: "locked" };
    case "unlocked":
      return { ...model, store: "unlocked" };
    default:
      return model;
  }
}

/** The marks of the status line, in their order. */
export function marks(model: Model): Mark[] {
  const out: Mark[] = [];
  if (model.store === "locked") out.push({ id: "locked", glyph: "lock", word: "secrets locked", action: "unlock", tone: "plain" });
  return out;
}
