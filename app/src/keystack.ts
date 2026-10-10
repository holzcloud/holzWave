// The key handler stack: features that need the keys for themselves (palette, search, copy mode, the paste
// confirmation) push a handler instead of editing keyboard.ts. Kept apart from keyboard.ts so a module that only
// needs the stack does not import the page's command bridge. No DOM at import.

/** Return true to consume the event: no later handler, no shortcut and no terminal input sees it. */
export type KeyHandler = (e: KeyboardEvent) => boolean;

// One entry per push, so removing one push can never remove another push of the same function.
const handlers: { fn: KeyHandler }[] = [];

/**
 * Put `fn` on top of the handler stack: it is consulted before every older handler and before the shortcuts.
 * The returned function removes exactly this push; calling it again does nothing. A consuming handler decides
 * for itself whether to call `preventDefault`: a text field of its own needs the browser's default.
 */
export function pushKeyHandler(fn: KeyHandler): () => void {
  const entry = { fn };
  handlers.push(entry);
  return () => {
    const at = handlers.indexOf(entry);
    if (at >= 0) handlers.splice(at, 1);
  };
}

/** Offer a `keydown` to the handlers, newest first; true when one consumed it. */
export function offerKey(e: KeyboardEvent): boolean {
  // Work on a copy so a handler may push or pop while it runs.
  for (const entry of [...handlers].reverse()) {
    if (handlers.includes(entry) && entry.fn(e)) return true;
  }
  return false;
}
