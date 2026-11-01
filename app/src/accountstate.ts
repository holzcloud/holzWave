// The pure part of the account dialog (docs/design.md 6.29): the reading of what `GET /api/me` answers, the state words of
// the groups, the rows of the sessions and the passkeys, the time words, and the sentence for each refusal of a change. No DOM and
// no network here, so node tests it on the compiled file. The server sends data and codes, never a sentence.
import { CODE_MISMATCH, FAILED, messageFor, SECRET_RULE } from "./loginstate.js";
import type { Account } from "./websessionstate.js";

/** The line of the dialog after a change was made. */
export const SAVED = "Saved.";
/** The refusal that a person must hear in these words: the last way to sign in cannot go. */
export const LAST_WAY = "This is your last way to sign in. Add another first.";
/** A change that the server refused for a reason this page has no better words for. */
export const CHANGE_FAILED = "That change was not made. Try again.";
/** A change that needs the saved passwords open while they are locked. */
export const LOCKED_FIRST = "Your saved passwords are locked. Unlock them first.";
/** Said in the form of a new recovery code, before it is made. */
export const REPLACES_OLD = "The new code replaces the old one.";

/** One passkey of the account. */
export interface Passkey {
  id: string;
  name: string;
  createdMs: number;
  /** When it was last used, or null when it never was. */
  lastUsedMs: number | null;
}

/** One session of the account, as a row. */
export interface SessionRow {
  id: string;
  device: string;
  addr: string;
  createdMs: number;
  lastSeenMs: number;
  current: boolean;
}

/** What `GET /api/me` says, after checking. */
export interface Me {
  account: Account;
  oidc: { id: string; name: string } | null;
  password: boolean;
  totp: boolean;
  passkeys: Passkey[];
  secrets: { state: "unlocked" | "locked"; passphrase: boolean; recovery: boolean };
  sessions: SessionRow[];
}

/** The most entries of a list the page takes from the server, and the longest strings. */
export const MAX_ENTRIES = 64;
const MAX_NAME = 128;
const MAX_DEVICE = 256;
const MAX_ADDR = 64;
const MAX_ID = 128;

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);
const str = (v: unknown, max: number): string | null => (typeof v === "string" && v.length <= max ? v : null);
/**
 * An id the way the server sends it: the server numbers its accounts and passkeys, so a whole non-negative number arrives and is kept
 * as its digits (the form it has in a path); a bounded string is kept as it is. Anything else (a fraction, a negative number, a number
 * past the safe range) is not an id.
 */
export const wholeId = (v: unknown, max: number): string | null =>
  typeof v === "number" ? (Number.isSafeInteger(v) && v >= 0 ? String(v) : null) : str(v, max);
const time = (v: unknown): number | null => {
  if (typeof v !== "string" || v.length > 64) return null;
  const ms = Date.parse(v);
  return Number.isFinite(ms) ? ms : null;
};

function readPasskey(raw: unknown): Passkey | null {
  if (!isObject(raw)) return null;
  const id = wholeId(raw.id, MAX_ID);
  const name = str(raw.name, MAX_NAME);
  const createdMs = time(raw.created_at);
  if (id === null || id === "" || name === null || createdMs === null) return null;
  if (raw.last_used === null || raw.last_used === undefined) return { id, name, createdMs, lastUsedMs: null };
  const used = time(raw.last_used);
  return used === null ? null : { id, name, createdMs, lastUsedMs: used };
}

function readSessionRow(raw: unknown): SessionRow | null {
  if (!isObject(raw) || typeof raw.current !== "boolean") return null;
  const id = str(raw.id, MAX_ID);
  const device = str(raw.device, MAX_DEVICE);
  const addr = str(raw.addr, MAX_ADDR);
  const createdMs = time(raw.created_at);
  const lastSeenMs = time(raw.last_seen);
  if (id === null || id === "" || device === null || addr === null || createdMs === null || lastSeenMs === null) return null;
  return { id, device, addr, createdMs, lastSeenMs, current: raw.current };
}

/** The answer of `GET /api/me` as the page uses it, or null when it is not of the documented shape (or has more than the page takes). */
export function readMe(raw: unknown): Me | null {
  if (!isObject(raw) || !isObject(raw.account)) return null;
  const a = raw.account;
  const id = wholeId(a.id, MAX_ID);
  const username = str(a.username, MAX_NAME);
  const displayName = str(a.display_name, MAX_NAME);
  const role = a.role === "admin" || a.role === "member" ? a.role : null;
  if (id === null || id === "" || username === null || displayName === null || role === null) return null;

  let oidc: Me["oidc"] = null;
  if (raw.oidc !== null && raw.oidc !== undefined) {
    if (!isObject(raw.oidc)) return null;
    const oid = str(raw.oidc.id, 64);
    const oname = str(raw.oidc.name, MAX_NAME);
    if (oid === null || oname === null) return null;
    oidc = { id: oid, name: oname };
  }
  if (typeof raw.password !== "boolean" || typeof raw.totp !== "boolean") return null;
  if (!Array.isArray(raw.passkeys) || raw.passkeys.length > MAX_ENTRIES) return null;
  if (!Array.isArray(raw.sessions) || raw.sessions.length > MAX_ENTRIES) return null;
  const passkeys: Passkey[] = [];
  for (const p of raw.passkeys) {
    const k = readPasskey(p);
    if (k === null) return null;
    passkeys.push(k);
  }
  const sessions: SessionRow[] = [];
  for (const s of raw.sessions) {
    const row = readSessionRow(s);
    if (row === null) return null;
    sessions.push(row);
  }
  const secrets = raw.secrets;
  if (!isObject(secrets) || (secrets.state !== "locked" && secrets.state !== "unlocked")) return null;
  if (typeof secrets.passphrase !== "boolean" || typeof secrets.recovery !== "boolean") return null;
  return {
    account: { id, username, displayName, role },
    oidc,
    password: raw.password,
    totp: raw.totp,
    passkeys,
    secrets: { state: secrets.state, passphrase: secrets.passphrase, recovery: secrets.recovery },
    sessions,
  };
}

/** How long ago, in words: `just now` under 45 seconds, then minutes, hours and days. A time in the future is `just now`. */
export function agoText(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 45) return "just now";
  if (seconds < 3600) return `${Math.max(1, Math.floor(seconds / 60))} min ago`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)} h ago`;
  const days = Math.floor(seconds / 86_400);
  return days === 1 ? "1 day ago" : `${days} days ago`;
}

/** When a passkey was last used: `not used yet`, `used today`, `used 3 days ago`. */
export function usedText(lastUsedMs: number | null, nowMs: number): string {
  if (lastUsedMs === null) return "not used yet";
  const seconds = (nowMs - lastUsedMs) / 1000;
  return seconds < 86_400 ? "used today" : `used ${agoText(seconds)}`;
}

/** One group of the dialog: its legend, the name and the state of its row, and what its button does. */
export interface Group {
  id: "password" | "totp" | "passkeys" | "passphrase" | "recovery";
  legend: string;
  name: string;
  /** The state as a word at the right of the row: its own cue next to the colour. */
  state: string;
  tone: "ok" | "dim";
  button: { label: string; action: "change" | "add" | "unlock" };
}

/**
 * The groups that apply to this account, in the order of the dialog. `Password` and `Authenticator` only for an account that has
 * them. While the saved passwords are locked the passphrase and the recovery code cannot be changed: the button is `Unlock`.
 */
export function groupStates(me: Me): Group[] {
  const out: Group[] = [];
  if (me.password) out.push({ id: "password", legend: "Password", name: "Password", state: "set", tone: "ok", button: { label: "Change", action: "change" } });
  if (me.totp) out.push({ id: "totp", legend: "Authenticator", name: "Authenticator app", state: "set", tone: "ok", button: { label: "Replace", action: "change" } });
  const n = me.passkeys.length;
  out.push({
    id: "passkeys",
    legend: "Passkeys",
    name: n === 0 ? "No passkeys" : n === 1 ? "1 passkey" : `${n} passkeys`,
    state: n === 0 ? "none" : "saved",
    tone: n === 0 ? "dim" : "ok",
    button: { label: "Add passkey", action: "add" },
  });
  const locked = me.secrets.state === "locked";
  out.push({
    id: "passphrase",
    legend: "Unlock passphrase",
    name: "Passphrase",
    state: me.secrets.passphrase ? "set" : "not set",
    tone: me.secrets.passphrase ? "ok" : "dim",
    button: locked ? { label: "Unlock", action: "unlock" } : { label: me.secrets.passphrase ? "Change" : "Set", action: "change" },
  });
  out.push({
    id: "recovery",
    legend: "Recovery code",
    name: "Recovery code",
    state: me.secrets.recovery ? "saved" : "none",
    tone: me.secrets.recovery ? "ok" : "dim",
    button: locked ? { label: "Unlock", action: "unlock" } : { label: "Make a new code", action: "change" },
  });
  return out;
}

/** The sessions as rows: the one that shows the dialog first, then the others by last seen, newest first. */
export function sessionRows(me: Me): SessionRow[] {
  return [...me.sessions].sort((a, b) => Number(b.current) - Number(a.current) || b.lastSeenMs - a.lastSeenMs);
}

/** Which change a refusal belongs to. */
export type ChangeKind = "password" | "totp" | "passkey-add" | "passkey-remove" | "passphrase" | "recovery" | "session";

/** The sentence for a failed change and, for a field, which one it concerns. `status` is 0 when nothing arrived. */
export function changeMessage(kind: ChangeKind, status: number, body?: Record<string, unknown>): { text: string; field?: string } {
  const field = typeof body?.field === "string" ? body.field : undefined;
  if (status === 0 || status === 429 || status === 503 || status >= 500) return { text: messageFor(status, body) };
  if (status === 400 && (field === "new" || field === "passphrase")) return { text: SECRET_RULE, field };
  if (status === 401) {
    if (kind === "totp") return { text: CODE_MISMATCH, field: "code" };
    if (kind === "password") return { text: FAILED, field: "current" };
    return { text: FAILED };
  }
  if (status === 409 && field === "method") return { text: LAST_WAY };
  if (status === 423) return { text: LOCKED_FIRST };
  return { text: CHANGE_FAILED };
}
