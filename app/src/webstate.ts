// The pure rules of the browser build's own page code: sentences, key lists and capability tests. No DOM and no import of
// the page, so node tests import it. The modules that act on the page live in `web.ts` and its neighbours.
import { MARKS, PRIORITY } from "./attentionstate.js";
import type { Kind } from "./attentionstate.js";

/** The status line says this when the browser refused a read of the clipboard. */
export const CLIPBOARD_REFUSED = "The browser did not allow reading the clipboard.";

/**
 * The key codes a browser keeps for itself (close tab, new tab, new window, next and previous tab by `Tab`, `PageUp` and
 * `PageDown`) and `Escape`: asked of the keyboard lock while the page is in full screen. Holding `Escape` still leaves.
 */
export const LOCK_KEYS: readonly string[] = ["KeyW", "KeyT", "KeyN", "Tab", "PageUp", "PageDown", "Escape"];

/** Whether this navigator has a keyboard lock (Chromium): an object with a `keyboard` member that has a `lock` function. */
export function canLockKeys(navigator: unknown): boolean {
  if (typeof navigator !== "object" || navigator === null) return false;
  const keyboard = (navigator as { keyboard?: unknown }).keyboard;
  if (typeof keyboard !== "object" || keyboard === null) return false;
  return typeof (keyboard as { lock?: unknown }).lock === "function";
}

/** The status line says this after the browser granted the keyboard lock. */
export const FULLSCREEN_NOTICE = "Browser shortcuts are off in full screen. Hold Escape to leave.";

// ---- links ----------------------------------------------------------------------------------------------------------

/** A link in the browser build opens in a new tab: the target name and the feature string that cuts the tab off from the page. */
export const LINK_TARGET = "_blank";
export const LINK_FEATURES = "noopener,noreferrer";

/**
 * Whether the modifier of a link click is held in the browser: Ctrl or Cmd. A browser page cannot tell which of the two
 * the person's system means by "open", and both are a deliberate act, so both count.
 */
export function webLinkModifier(e: { metaKey?: boolean; ctrlKey?: boolean }): boolean {
  return e.ctrlKey === true || e.metaKey === true;
}

// ---- controls the browser does not have -----------------------------------------------------------------------------

/** The status line says this when a control that exists only on a desktop is run in the browser. */
export const NOT_AVAILABLE = "Not available in the browser.";

/** The actions that need a window of the desktop shell: the palette does not list them in the browser build. */
export const DESKTOP_ONLY_ACTIONS: readonly string[] = ["pane.move_to_new_window", "tab.move_to_new_window"];

// ---- tab marks --------------------------------------------------------------------------------------------------

/** The two icons of the browser tab, same-origin SVG files in the build: the plain mark, and the same with the rose dot. */
export const ICON_PLAIN = "favicon.svg";
export const ICON_ATTENTION = "favicon-attention.svg";

/** Whether `kind` is one of the four kinds of the bar's marks (6.23). */
const isKind = (kind: string): kind is Kind => (PRIORITY as readonly string[]).includes(kind);

/**
 * What happened while the page was hidden: a set of the four kinds of the bar's marks. It records only while hidden, a repeat of a
 * kind changes nothing, and it lists the kinds in the bar's fixed order (ask, bell, failed, done) whatever order they came in.
 */
export class Notices {
  private kinds = new Set<Kind>();

  /** Record `kind` when the page is `hidden`; true when that changed the set. */
  note(kind: string, hidden: boolean): boolean {
    if (!hidden || !isKind(kind) || this.kinds.has(kind)) return false;
    this.kinds.add(kind);
    return true;
  }

  /** Take `kind` out (a question that was answered); true when it was there. */
  drop(kind: string): boolean {
    return isKind(kind) && this.kinds.delete(kind);
  }

  /** Forget everything (the page is visible again); true when something was there. */
  clear(): boolean {
    const had = this.kinds.size > 0;
    this.kinds.clear();
    return had;
  }

  /** The kinds present, in the bar's order. */
  list(): Kind[] {
    return PRIORITY.filter((k) => this.kinds.has(k));
  }
}

/** The title of the tab: the glyphs of `kinds` in the bar's order, each followed by a space, then `base`; `base` alone for none. */
export function noticeTitle(base: string, kinds: readonly string[]): string {
  const glyphs = PRIORITY.filter((k) => kinds.includes(k)).map((k) => MARKS[k].glyph);
  return glyphs.length === 0 ? base : `${glyphs.join(" ")} ${base}`;
}

/** The icon file of the tab: the one with the dot while anything is present. */
export function noticeIcon(kinds: readonly string[]): string {
  return kinds.length > 0 ? ICON_ATTENTION : ICON_PLAIN;
}

/** The kind of a command that ended: `failed` for a status that is a positive whole number, `done` for 0, none or anything else (as on the bar). */
export function commandKind(code: unknown): "done" | "failed" {
  return typeof code === "number" && Number.isSafeInteger(code) && code > 0 ? "failed" : "done";
}

// ---- notifications ----------------------------------------------------------------------------------------------

/** The one value the page keeps in local storage for the notifications: `"1"` while the person has them on. */
export const NOTIFY_FLAG_KEY = "holzwave.notify";

/** The shortest time between two notifications of one kind, in milliseconds. */
export const NOTIFY_GAP_MS = 5000;

/** The text of a notification: the words of the mark and nothing else. Never a pane's name, a command or output. */
export const NOTIFY_TEXT: Readonly<Record<Kind, string>> = {
  ask: "Waits for you",
  bell: "Rang the bell",
  failed: "Command failed",
  done: "Command finished",
};

/** The state of the notifications as the Settings group says it. */
export type NotifyWord = "off" | "on" | "blocked by the browser";

/**
 * The state from the browser's permission (`default`, `granted` or `denied`) and the person's flag: blocked whenever the browser
 * says `denied`; on only when the browser allows it and the person turned it on; off in every other case.
 */
export function permissionState(permission: unknown, flag: boolean): NotifyWord {
  if (permission === "denied") return "blocked by the browser";
  return permission === "granted" && flag ? "on" : "off";
}

/** The rule that decides whether a notification is made: hidden, on, and at most one of a kind in `NOTIFY_GAP_MS`. */
export class NotifyGate {
  private last = new Map<Kind, number>();

  /**
   * The text to show for `kind` at time `now` (milliseconds on a monotonic clock), or null for none. Only a shown notification
   * starts the window of its kind, so a refusal does not extend it; a clock that went back does not hold a kind back.
   */
  next(kind: string, now: number, hidden: boolean, word: string): string | null {
    if (!hidden || word !== "on" || !isKind(kind)) return null;
    const before = this.last.get(kind);
    if (before !== undefined && now >= before && now - before < NOTIFY_GAP_MS) return null;
    this.last.set(kind, now);
    return NOTIFY_TEXT[kind];
  }
}

/** The group `Notifications` of the Settings dialog (design 6.30): its legend, the row's name and meta, and the row of each state. */
export const NOTIFY_GROUP = {
  legend: "Notifications",
  name: "Notify me",
  meta: "bell and attention",
  /** The line after `Check again`. */
  checked: "Checked.",
  /** The line when the browser did not answer the request. */
  failed: "The browser did not answer. Try again.",
  rows: {
    off: {
      tone: "plain",
      advice: "Bell and attention marks also show in the tab title. Turn this on to get a browser notification as well.",
      button: "Turn on",
    },
    on: {
      tone: "ok",
      advice: "A browser notification appears when a pane out of view rings the bell or needs you. It never contains terminal text.",
      button: "Turn off",
    },
    "blocked by the browser": {
      tone: "warn",
      advice: "The browser blocks notifications for this site. Allow them in the site settings of the browser, then choose Check again.",
      button: "Check again",
    },
  },
} as const;

// ---- upload by drop --------------------------------------------------------------------------------------------------

/** The most files taken from one drop (and waiting at one time): the rest are said, not sent. */
export const MAX_DROP_FILES = 16;

/** The label of the drop target on a pane (design 6.30). */
export const DROP_LABEL = "Drop to upload";

/** The status line says this when a drop held more files than are taken. */
export const ONLY_SIXTEEN = "Only 16 files at a time.";

/** The status line says this when a drop held a folder: only files are sent. */
export const FOLDERS_SKIPPED = "Folders are not uploaded.";

/** The longest file name the address carries, in UTF-8 bytes. */
const NAME_BYTES = 200;

/** The name for a file whose name is empty or blank. */
const FALLBACK_NAME = "upload";

/** The longest name the status line shows, in characters, ellipsis included. */
const SHOWN_CHARS = 48;

/** Wrap `path` in single quotes for a POSIX shell: a single quote inside is closed, escaped and opened again; nothing else is special. */
export function shellQuote(path: string): string {
  return `'${path.replaceAll("'", "'\\''")}'`;
}

/** Whether `text` holds a line break: the core then asks the person before it types the text. */
export function hasNewline(text: string): boolean {
  return /[\r\n]/.test(text);
}

/** The UTF-8 length of one code point. */
const utf8Length = (cp: number): number => (cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4);

/** The characters of `name` as whole code points; a lone surrogate (not text) becomes the replacement character. */
function wholeCharacters(name: string): string[] {
  return Array.from(name, (ch) => {
    const cp = ch.codePointAt(0) ?? 0;
    return ch.length === 1 && cp >= 0xd800 && cp <= 0xdfff ? "\ufffd" : ch;
  });
}

/** The address of an upload: the name in the query, encoded, at most 200 bytes of it cut on a character; `upload` for a blank name. */
export function uploadUrl(name: string): string {
  const characters = wholeCharacters(name);
  if (name.trim() === "") return `/api/upload?name=${FALLBACK_NAME}`;
  let bytes = 0;
  let kept = "";
  for (const ch of characters) {
    bytes += utf8Length(ch.codePointAt(0) ?? 0);
    if (bytes > NAME_BYTES) break;
    kept += ch;
  }
  return `/api/upload?name=${encodeURIComponent(kept)}`;
}

/** A parsed JSON object that is not an array, or null. */
function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>) : null;
}

/** The words for a refused upload, from the status and the member `field` of the body (nothing else of the body is read). */
export function refusalText(status: number, body: unknown): string {
  const field = record(body)?.field;
  if (status === 413) return "too large";
  if (status === 400 && field === "size") return "too large";
  if (status === 400 && field === "name") return "name not allowed";
  if (status === 423) return "secrets are locked";
  if (status === 429 || status === 503) return "try again";
  if (status === 401 || status === 403) return "sign in again";
  return "upload failed";
}

/** The status line for a refusal: `Not uploaded: too large.` */
export const notUploaded = (words: string): string => `Not uploaded: ${words}.`;

/** Control characters other than a line break: a path with one is not typed anywhere. */
// eslint-disable-next-line no-control-regex
const PATH_CONTROL = /[\u0000-\u0009\u000b\u000c\u000e-\u001f\u007f]/;

/** The longest path of an answer, in characters. */
const PATH_MAX = 4096;

/** The answer of a finished upload: an absolute path of at most 4,096 characters without control characters (a line break is allowed, it is quoted) and a size. */
export function readUploaded(json: unknown): { path: string; size: number } | null {
  const o = record(json);
  if (o === null) return null;
  const { path, size } = o;
  if (typeof path !== "string" || path[0] !== "/" || path.length > PATH_MAX || PATH_CONTROL.test(path)) return null;
  if (typeof size !== "number" || !Number.isSafeInteger(size) || size < 0) return null;
  return { path, size };
}

/** The folder an answer's path lies in: everything before the last slash (`/` for a file in the root), empty for a path that is not absolute. */
export function uploadFolder(path: string): string {
  if (path[0] !== "/") return "";
  const at = path.lastIndexOf("/");
  return at === 0 ? "/" : path.slice(0, at);
}

/**
 * The files to send from a drop: at most 16 in the order they came, folders left out. Nothing is dropped silently: `over` counts the
 * files beyond the 16 and `folders` the folders, and the page says both. An item is a folder only when it says so (`directory`).
 */
export function takeFiles<T extends { directory?: boolean }>(list: readonly T[]): { files: T[]; over: number; folders: number } {
  const files: T[] = [];
  let over = 0;
  let folders = 0;
  for (const item of list) {
    if (item.directory === true) folders++;
    else if (files.length < MAX_DROP_FILES) files.push(item);
    else over++;
  }
  return { files, over, folders };
}

/** A file name as the status line shows it: no control or direction character, at most 48 characters. */
function shownName(name: string): string {
  // eslint-disable-next-line no-control-regex
  const clean = name.replace(/[\u0000-\u001f\u007f-\u009f\u200e\u200f\u202a-\u202e\u2066-\u2069]/g, "\ufffd");
  const characters = Array.from(clean);
  return characters.length <= SHOWN_CHARS ? clean : `${characters.slice(0, SHOWN_CHARS - 1).join("")}\u2026`;
}

/** The status line while a file is on its way. */
export const uploadingText = (name: string): string => `Uploading ${shownName(name)}`;

/** The status line for files that arrived. */
export const uploadedText = (count: number): string => (count === 1 ? "Uploaded" : `Uploaded ${count} files`);

/**
 * What the status line says when a run of uploads ends: how many arrived, the words of the last refusal, and the files and folders
 * that were not taken. One part stands alone; several are sentences.
 */
export function batchText(done: { ok: number; failed: readonly string[]; over: number; folders: number }): string {
  const parts: string[] = [];
  if (done.ok > 0) parts.push(uploadedText(done.ok));
  const last = done.failed.at(-1);
  if (last !== undefined) parts.push(notUploaded(last));
  if (done.over > 0) parts.push(ONLY_SIXTEEN);
  if (done.folders > 0) parts.push(FOLDERS_SKIPPED);
  if (parts.length < 2) return parts[0] ?? "";
  return parts.map((p) => (p.endsWith(".") ? p : `${p}.`)).join(" ");
}

// ---- download ---------------------------------------------------------------------------------------------------------

/** The most download grants the page keeps: the oldest is ended when a new one would make 17. */
export const MAX_GRANTS = 16;

/** The status line says this when the core refuses a grant: it grants images, video, audio and PDF files only. */
export const DOWNLOAD_ONLY_SENTENCE = "Download works for images, video, audio and PDF files.";

/** The grants the page made for downloads, oldest first, at most 16. */
export class GrantList {
  private tokens: string[] = [];

  /** Keep `token`; the token that has to go to make room (the oldest) is returned, else null. A token held already is not added again. */
  add(token: string): string | null {
    if (this.tokens.includes(token)) return null;
    this.tokens.push(token);
    return this.tokens.length > MAX_GRANTS ? (this.tokens.shift() ?? null) : null;
  }

  /** The tokens held, oldest first; a copy. */
  all(): string[] {
    return [...this.tokens];
  }

  /** Forget every token and return them, oldest first. */
  clear(): string[] {
    const gone = this.tokens;
    this.tokens = [];
    return gone;
  }
}

/** The longest token of a grant. */
const TOKEN_MAX = 128;

/**
 * The part of an answer of `file_grant` the page uses: the address and the token. The address must be the same-origin path
 * `/files/<token>` of the answer's own token (letters, digits, `-` and `_` only), so no address built by anything else is ever followed.
 */
export function readGrant(json: unknown): { url: string; token: string } | null {
  const o = record(json);
  if (o === null) return null;
  const { url, token } = o;
  if (typeof url !== "string" || typeof token !== "string") return null;
  if (token.length === 0 || token.length > TOKEN_MAX || !/^[A-Za-z0-9_-]+$/.test(token)) return null;
  return url === `/files/${token}` ? { url, token } : null;
}

/** The name a downloaded file is saved under: the last part of its path, without control characters; `download` when there is none. */
export function baseName(path: string): string {
  const last = path.split(/[\\/]/).pop() ?? "";
  // eslint-disable-next-line no-control-regex
  const clean = last.replace(/[\u0000-\u001f\u007f-\u009f]/g, "\ufffd");
  return clean === "" ? "download" : clean;
}
