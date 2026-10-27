// The pure part of the people dialog (docs/design.md 6.29): the reading of what `GET /api/admin/accounts` answers, the state
// words with their glyphs, the time words and the invitation. No DOM and no network here, so node tests it on the compiled file.
// The server sends data and codes, never a sentence; every sentence the dialog says is here.

/** The most accounts the page takes from the server, and the longest strings. */
export const MAX_ACCOUNTS = 500;
const MAX_NAME = 128;
const MAX_ID = 128;
/** The longest invitation link the page shows. */
const MAX_LINK = 2048;

/** What the dialog says under the table: it is a statement of what is not there. */
export const LIST_NOTE = "This list shows names, roles and states only. It never shows anyone's secrets, terminals, commands or layout.";

export type Role = "admin" | "member";
export type State = "active" | "disabled" | "locked_out";

/** One account as a row: name, role, state and last sign-in, and nothing else. */
export interface Row {
  id: string;
  username: string;
  displayName: string;
  role: Role;
  state: State;
  /** When the person last signed in, or null when they never did. */
  lastSigninMs: number | null;
}

/** What the list answers, after checking. */
export interface People {
  /** The number of active administrators. */
  admins: number;
  rows: Row[];
}

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);
const str = (v: unknown, max: number): string | null => (typeof v === "string" && v.length <= max ? v : null);

function readRow(raw: unknown): Row | null {
  if (!isObject(raw)) return null;
  const id = str(raw.id, MAX_ID);
  const username = str(raw.username, MAX_NAME);
  const displayName = str(raw.display_name, MAX_NAME);
  if (id === null || id === "" || username === null || displayName === null) return null;
  const role = raw.role === "admin" || raw.role === "member" ? raw.role : null;
  const state = raw.state === "active" || raw.state === "disabled" || raw.state === "locked_out" ? raw.state : null;
  if (role === null || state === null) return null;
  let lastSigninMs: number | null = null;
  if (raw.last_signin !== null) {
    if (typeof raw.last_signin !== "string" || raw.last_signin.length > 64) return null;
    lastSigninMs = Date.parse(raw.last_signin);
    if (!Number.isFinite(lastSigninMs)) return null;
  }
  return { id, username, displayName, role, state, lastSigninMs };
}

/**
 * The answer of `GET /api/admin/accounts` as the page uses it, or null when it is not of the documented shape or has more than the
 * page takes. Only the five members above are kept: nothing else of the answer is carried into the page.
 */
export function readAccounts(raw: unknown): People | null {
  if (!isObject(raw)) return null;
  const admins = raw.admins;
  if (typeof admins !== "number" || !Number.isInteger(admins) || admins < 0 || admins > MAX_ACCOUNTS) return null;
  if (!Array.isArray(raw.accounts) || raw.accounts.length > MAX_ACCOUNTS) return null;
  const rows: Row[] = [];
  for (const a of raw.accounts) {
    const row = readRow(a);
    if (row === null) return null;
    rows.push(row);
  }
  return { admins, rows };
}

/** The state of a row as a word, a glyph (its own cue besides the colour) and a tone. */
export interface StateWord {
  word: string;
  glyph: string;
  tone: "ok" | "dim" | "warn";
}

/** The word and the glyph of a state; null for a value that is none of the three (it is never shown). */
export function stateWord(state: string): StateWord | null {
  switch (state) {
    case "active":
      return { word: "active", glyph: "●", tone: "ok" };
    case "disabled":
      return { word: "disabled", glyph: "○", tone: "dim" };
    case "locked_out":
      return { word: "locked out", glyph: "!", tone: "warn" };
    default:
      return null;
  }
}

/**
 * When the person last signed in, in words: `never`, `just now` (also for a time in the future), minutes, hours, `yesterday`,
 * days, weeks, months and years.
 */
export function lastSigninText(ms: number | null, now: number): string {
  if (ms === null || !Number.isFinite(ms)) return "never";
  const seconds = (now - ms) / 1000;
  if (!(seconds >= 45)) return "just now";
  if (seconds < 3600) return `${Math.max(1, Math.floor(seconds / 60))} min ago`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)} h ago`;
  const days = Math.floor(seconds / 86_400);
  if (days === 1) return "yesterday";
  if (days < 14) return `${days} days ago`;
  if (days < 60) return `${Math.floor(days / 7)} weeks ago`;
  if (days < 365) return `${Math.floor(days / 30)} months ago`;
  const years = Math.floor(days / 365);
  return years === 1 ? "1 year ago" : `${years} years ago`;
}

/** The expiry choices of an invitation in days, and the one that is selected at first. */
export const INVITE_DAYS: readonly number[] = [1, 3, 7, 14, 30];
export const DEFAULT_DAYS = 7;

/** The body of `POST /api/admin/invitations`: the role and the expiry, and nothing else. */
export function inviteBody(role: Role, days: number): { role: Role; days: number } {
  return { role, days: INVITE_DAYS.includes(days) ? days : DEFAULT_DAYS };
}

/** The sentence over a new link: it says that the link is shown once, and for how long and as what it works. */
export function inviteNote(days: number, role: Role): string {
  const long = days === 1 ? "1 day" : `${days} days`;
  return `Shown once. Copy it now and send it to the person. It works for ${long}, as ${role === "admin" ? "an administrator" : "a member"}.`;
}

/** The link of the answer of `POST /api/admin/invitations`, or null when the answer has none the page can show. */
export function readInvitation(raw: unknown): string | null {
  if (!isObject(raw)) return null;
  const link = raw.link;
  if (typeof link !== "string" || link === "" || link.length > MAX_LINK) return null;
  return /^\S+$/u.test(link) ? link : null;
}
