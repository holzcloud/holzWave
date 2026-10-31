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
