// The pure rules of the browser build's own page code: sentences, key lists and capability tests. No DOM and no import of
// the page, so node tests import it. The modules that act on the page live in `web.ts` and its neighbours.

/** The status line says this when the browser refused a read of the clipboard. */
export const CLIPBOARD_REFUSED = "The browser did not allow reading the clipboard.";

/** The key codes a browser keeps for itself, and `Escape`: asked of the keyboard lock while the page is in full screen. */
export const LOCK_KEYS: readonly string[] = [];

/** Whether this navigator has a keyboard lock (Chromium): an object with a `keyboard` member that has a `lock` function. */
export function canLockKeys(_navigator: unknown): boolean {
  return false;
}

/** The status line says this after the browser granted the keyboard lock. */
export const FULLSCREEN_NOTICE = "";
