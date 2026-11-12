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
/** The name of the row in the browser build: the server the page was loaded from (design.md 6.30). */
export const THIS_SERVER = "This server";
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
 * A link with no address is drawn as `not signed in` with `Sign in…` only. `web` is the row of the browser build: the page's own
 * sign-in is the credential, so the row is always signed in (or unreachable when the list cannot be read) and has neither
 * `Sign in…` nor `Sign out`.
 */
export function rowPlan(view: ServerView, listFailed: boolean, web = false): RowPlan {
  const desktop: LinkState = view.url === "" ? "signed_out" : listFailed && view.state === "signed_in" ? "unreachable" : view.state;
  const state: LinkState = web ? (listFailed ? "unreachable" : "signed_in") : desktop;
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
  if (web) buttons = buttons.filter((b) => b !== SIGN_OUT && b !== SIGN_IN_LINK);
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

/** The view the row of the browser build reads: signed in by the page's own sign-in, with remote sessions as the list said. */
export function webView(remoteOn: boolean): ServerView {
  return { state: "signed_in", url: "", name: "", locked: false, syncOn: false, remoteOn, error: null };
}

/** Whether a refusal is the host's sentence for remote sessions that are off: only that exact sentence says so. */
export function isRemoteOff(failure: unknown): boolean {
  const text = failure instanceof Error ? failure.message : typeof failure === "string" ? failure : "";
  return text === REMOTE_OFF;
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

// ---- the states of a pane that shows a session (docs/design.md 6.34, "The states of a pane") ----------------------------------------

/** The transient states of a remote pane. */
export type PaneState = "attaching" | "loading_older" | "resyncing" | "ended" | "reconnecting" | "unreachable";

/** The words of the header's context slot for each state; `ended` with a status is `Session ended · exit N`. */
export const PANE_STATE_WORDS: Readonly<Record<PaneState, string>> = {
  attaching: "Attaching…",
  loading_older: "Loading older lines…",
  resyncing: "Resyncing…",
  ended: "Session ended",
  reconnecting: "Reconnecting…",
  unreachable: "Server unreachable",
};

/** The line the record writes on the screen for a pane whose tries are used. The host writes it; the page holds it for the test. */
export const UNREACHABLE_LINE = "Server unreachable. Press Enter to try again.";

/** `Session ended · exit N`, or `Session ended` when the status could not be read (the host's own notice says the same). */
export function endedText(code: number | null): string {
  return code === null ? PANE_STATE_WORDS.ended : `${PANE_STATE_WORDS.ended} · exit ${code}`;
}

/** What the header of a remote pane shows for a state: the word, the dot, the warn tone (with its `!` and underline), the label. */
export interface PaneStateView {
  state: PaneState;
  text: string;
  /** The dot: a dotted ring (`wait`), hollow with a dash (`failed`), or as before (null). */
  dot: "wait" | "failed" | null;
  warn: boolean;
  /** The writer label stays only where the page knows it. */
  label: boolean;
}

/** The parts of a pane's connection the state is read from (connstate's `PaneConn`, without importing it). */
export interface RemoteSeen {
  state: "connecting" | "connected" | "reconnecting" | "failed" | "closed";
  remote: { phase: "" | "attaching" | "loading_older" | "resyncing" };
}

/**
 * The state of a remote pane, or null when it shows the session as it is. `held` is the status of a pane the host holds on the screen
 * after its session ended (`undefined` when it does not hold it, `null` inside when the status could not be read).
 */
export function paneState(c: RemoteSeen, held?: { status: number | null } | null): PaneStateView | null {
  const view = (state: PaneState, dot: PaneStateView["dot"], label: boolean, text = PANE_STATE_WORDS[state]): PaneStateView => ({
    state,
    text,
    dot,
    warn: state === "ended" || state === "unreachable",
    label,
  });
  if (held) return view("ended", "failed", false, endedText(held.status));
  switch (c.state) {
    case "closed":
      return view("ended", "failed", false);
    case "failed":
      return view("unreachable", "failed", false);
    case "reconnecting":
      return view("reconnecting", "wait", false);
    case "connecting":
      return view("attaching", "wait", false);
    default:
      break;
  }
  switch (c.remote.phase) {
    case "attaching":
      return view("attaching", "wait", false);
    case "loading_older":
      return view("loading_older", null, true);
    case "resyncing":
      return view("resyncing", "wait", true);
    default:
      return null;
  }
}

// ---- older lines ------------------------------------------------------------------------------------------------------------------

/** What the latch reads of a pane after a frame. */
export interface OlderSeen {
  /** The remote member of the pane's connection, or undefined for a pane that does not show a session. */
  remote?: { more: boolean; phase: string } | undefined;
  /** The connection is up. */
  connected: boolean;
  /** The frame's scrollbar: `[offset, total, len]`; offset 0 is the top of what the page holds. */
  scrollbar: readonly [number, number, number];
}

/**
 * Decides when a pane that shows a session asks the server for older lines (`pane_load_older`): when the person has scrolled to the
 * top of what the page holds, the pane holds more history than its viewport, the server has more, and nothing runs. After one ask it
 * stays closed until the phase has come and gone, or the person has left the top, so a held scroll key at the top asks once.
 */
export class OlderLatch {
  /** Panes that asked: `asked` until their phase shows, `seen` while it shows. */
  private readonly closed = new Map<number, "asked" | "seen">();

  /** True when `pane` should ask now; the latch closes for it. */
  want(pane: number, seen: OlderSeen): boolean {
    const r = seen.remote;
    if (!r) {
      this.closed.delete(pane);
      return false;
    }
    const [offset, total, len] = seen.scrollbar;
    const atTop = offset === 0 && total > len;
    const mark = this.closed.get(pane);
    if (mark !== undefined) {
      if (r.phase !== "") this.closed.set(pane, "seen");
      else if (mark === "seen" || offset > 0) this.closed.delete(pane);
      if (this.closed.has(pane)) return false;
    }
    if (!seen.connected || !r.more || r.phase !== "" || !atTop) return false;
    this.closed.set(pane, "asked");
    return true;
  }

  /** Forget a pane that left. */
  forget(pane: number): void {
    this.closed.delete(pane);
  }
}
