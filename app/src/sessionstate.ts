// The pure part of the server sessions in the page (docs/design.md 6.34): the strict reader of the host's list of sessions, the words
// of the record's tables as exported constants that a unit test holds to the record, the words for a time, a size and a directory,
// and the small decisions of the server row (which buttons a state shows, whether the list is drawn). No DOM and no command call
// here, so node tests it on the compiled file. Nothing here carries a secret: the host's rows have none, and the reader copies only
// the members it names, one by one, so a member a newer host adds never reaches the page.
import { shortenHome } from "./labels.js";
import { NOT_SIGNED_IN, SIGNED_IN, SIGNING_IN, SIGN_IN_LINK, SIGN_OUT, UNREACHABLE, plain } from "./serverstate.js";
import type { LinkState, ServerView } from "./serverstate.js";

// ---- the words (docs/design.md 6.34, the table "The words") --------------------------------------------------------------------

/** The legend of the group in the Connections dialog. */
export const SERVER_LEGEND = "Server";
export const NEW_SESSION = "New session";
export const ATTACH = "Attach";
export const END = "End";
export const END_ARMED = "Click again to end";
export const FREE_WORD = "free";
export const HELD_WORD = "held";
/** The row while the server has remote sessions off. */
export const REMOTE_OFF = "Remote sessions are off on this server.";
/** The help line under the row: the idle limit is said in words when the page knows it, else by name. */
export const HELP_START = "A session keeps running on the server with no window open. It ends when the server restarts or after ";
export const HELP_UNKNOWN = `${HELP_START}the idle limit of the server with nobody connected.`;

/** The state word of the server row for each state of the link. */
export const STATE_WORDS: Readonly<Record<LinkState, string>> = {
  signed_in: SIGNED_IN,
  signed_out: NOT_SIGNED_IN,
  signing_in: SIGNING_IN,
  unreachable: UNREACHABLE,
};

/** The dot of the server row for each state: filled, hollow, a dotted ring, hollow with a dash (the shapes of 6.34). */
export const STATE_DOTS: Readonly<Record<LinkState, "in" | "out" | "wait" | "down">> = {
  signed_in: "in",
  signed_out: "out",
  signing_in: "wait",
  unreachable: "down",
};

/** `N attached`. */
export function attachedText(n: number): string {
  return `${n} attached`;
}

/** The title of a session that set none: `Session N`, N its place in the list, oldest first, from 1. */
export function untitled(place: number): string {
  return `Session ${place}`;
}

/** The size of a grid: `120×40`. */
export function sizeText(cols: number, rows: number): string {
  return `${cols}×${rows}`;
}

/** The help line; `idleSecs` is the idle limit of the server when the page knows it, else null. */
export function helpLine(idleSecs: number | null): string {
  const words = idleSecs === null ? null : durationWords(idleSecs);
  return words === null ? HELP_UNKNOWN : `${HELP_START}${words} with nobody connected.`;
}

/** A length of time in words (`a day`, `6 hours`, `30 minutes`), or null for one the page does not say. */
export function durationWords(secs: number): string | null {
  if (!Number.isSafeInteger(secs) || secs < 60) return null;
  const unit = (n: number, one: string, many: string): string => (n === 1 ? one : `${n} ${many}`);
  if (secs % 86_400 === 0) return unit(secs / 86_400, "a day", "days");
  if (secs % 3_600 === 0) return unit(secs / 3_600, "an hour", "hours");
  if (secs % 60 === 0) return unit(secs / 60, "a minute", "minutes");
  return null;
}

// ---- the start of a session -------------------------------------------------------------------------------------------------------

const DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const two = (n: number): string => String(n).padStart(2, "0");

/** When a session started: `14:02` today, `Mon 14:02` within a week, the date (`2026-11-03`) otherwise. Local time. */
export function startText(startedSecs: number, now: Date): string {
  const at = new Date(startedSecs * 1000);
  if (Number.isNaN(at.getTime())) return "";
  const clock = `${two(at.getHours())}:${two(at.getMinutes())}`;
  const day = (d: Date): number => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const days = Math.round((day(now) - day(at)) / 86_400_000);
  if (days === 0) return clock;
  if (days > 0 && days < 7) return `${DAYS[at.getDay()]} ${clock}`;
  return `${at.getFullYear()}-${two(at.getMonth() + 1)}-${two(at.getDate())}`;
}

/** A directory of the server with the home prefix as `~`: the server's platform is not known, so both conventions are tried. */
export function dirText(path: string): string {
  return shortenHome(shortenHome(path, "linux"), "macos");
}

// ---- the list of sessions -------------------------------------------------------------------------------------------------------

/** One session as the page reads it. */
export interface SessionRow {
  id: number;
  title: string;
  cwd: string;
  cols: number;
  rows: number;
  /** Seconds since the Unix epoch. */
  started: number;
  attached: number;
  held: boolean;
}

/** The most sessions the page takes in one list; a longer list is refused as a whole. */
export const MAX_SESSIONS = 64;
export const MAX_TITLE = 128;
export const MAX_DIR = 512;
/** The grid clamps of the host. */
export const MAX_COLS = 1000;
export const MAX_ROWS = 500;

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);
const own = (o: Record<string, unknown>, key: string): unknown => (Object.hasOwn(o, key) ? o[key] : undefined);
const whole = (v: unknown, min: number, max: number): number | null =>
  typeof v === "number" && Number.isSafeInteger(v) && v >= min && v <= max ? v : null;

/** Read one row; null for a row that is not well formed. */
function readRow(raw: unknown): SessionRow | null {
  if (!isObject(raw)) return null;
  const id = whole(own(raw, "id"), 0, 0xffff_ffff);
  const cols = whole(own(raw, "cols"), 1, MAX_COLS);
  const rows = whole(own(raw, "rows"), 1, MAX_ROWS);
  const started = whole(own(raw, "started"), 0, Number.MAX_SAFE_INTEGER);
  const attached = whole(own(raw, "attached"), 0, 0xffff);
  const title = own(raw, "title");
  const cwd = own(raw, "cwd");
  const held = own(raw, "held");
  if (id === null || cols === null || rows === null || started === null || attached === null) return null;
  if (typeof title !== "string" || typeof cwd !== "string" || typeof held !== "boolean") return null;
  return { id, title: plain(title, MAX_TITLE), cwd: plain(cwd, MAX_DIR), cols, rows, started, attached, held };
}

/**
 * Read the answer of `server_sessions`: the well formed rows, oldest first, each id once. Null for an answer that cannot be read:
 * not a list, or a list over the bound (refused as a whole, never cut).
 */
export function readSessions(raw: unknown): SessionRow[] | null {
  if (!Array.isArray(raw) || raw.length > MAX_SESSIONS) return null;
  const seen = new Set<number>();
  const out: SessionRow[] = [];
  for (const item of raw) {
    const row = readRow(item);
    if (row === null || seen.has(row.id)) continue;
    seen.add(row.id);
    out.push(row);
  }
  return out.sort((a, b) => a.started - b.started || a.id - b.id);
}

/** The words of one session row, in the order of the record. */
export interface RowWords {
  title: string;
  dir: string;
  size: string;
  start: string;
  attached: string;
  writer: typeof FREE_WORD | typeof HELD_WORD;
}

/** The words of the row at `place` (from 1) of the list. */
export function rowWords(row: SessionRow, place: number, now: Date): RowWords {
  return {
    title: row.title === "" ? untitled(place) : row.title,
    dir: dirText(row.cwd),
    size: sizeText(row.cols, row.rows),
    start: startText(row.started, now),
    attached: attachedText(row.attached),
    writer: row.held ? HELD_WORD : FREE_WORD,
  };
}

// ---- the server row ---------------------------------------------------------------------------------------------------------------

/** What the server row draws. */
export interface RowPlan {
  state: LinkState;
  word: string;
  dot: "in" | "out" | "wait" | "down";
  /** The buttons in order; empty while signing in and while locked. */
  buttons: string[];
  /** The store of secrets is locked: the dim locked line takes the place of the buttons. */
  locked: boolean;
  /** The list of sessions is drawn. */
  list: boolean;
  /** The dim line that remote sessions are off takes the place of the list. */
  off: boolean;
  /** The help line is drawn. */
  help: boolean;
}

/**
 * What the row shows for `view`; `listFailed` says that the last read of the list failed, which the row says as `unreachable`.
 * A link with no address is drawn as `not signed in` with `Sign in…` only.
 */
export function rowPlan(view: ServerView, listFailed: boolean): RowPlan {
  const state: LinkState = view.url === "" ? "signed_out" : listFailed && view.state === "signed_in" ? "unreachable" : view.state;
  const signedIn = state === "signed_in";
  let buttons: string[];
  switch (state) {
    case "signed_in":
      buttons = view.remoteOn ? [SIGN_OUT, NEW_SESSION] : [SIGN_OUT];
      break;
    case "signed_out":
      buttons = [SIGN_IN_LINK];
      break;
    case "unreachable":
      buttons = [SIGN_OUT];
      break;
    default:
      buttons = [];
  }
  if (view.locked) buttons = [];
  return {
    state,
    word: STATE_WORDS[state],
    dot: STATE_DOTS[state],
    buttons,
    locked: view.locked,
    list: signedIn && view.remoteOn,
    off: signedIn && !view.remoteOn,
    help: signedIn && view.remoteOn,
  };
}

/** The name of the server in the row: its host, from the address. Empty without one. */
export function serverName(url: string): string {
  if (url === "") return "";
  try {
    return new URL(url).host;
  } catch {
    return "";
  }
}
