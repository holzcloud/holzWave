// The pure rules of the browser build's own page code: sentences, key lists and capability tests. No DOM and no import of
// the page, so node tests import it. The modules that act on the page live in `web.ts` and its neighbours.

/** The status line says this when the browser refused a read of the clipboard. */
export const CLIPBOARD_REFUSED = "";
