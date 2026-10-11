// The text of the core's selection, kept where a `copy` event can read it synchronously.
//
// The selection itself (anchor, extent, word and line granularity, following output and scrolling) lives in the
// core; a frame carries its spans for the highlight and the text is asked for with `selection_text`. A `copy`
// DOM event needs its data in the same task, so the answer is cached per pane and refreshed when the spans or the
// rows under them change. An empty span list clears the cache, and so does an exited pane (`forgetSelection`):
// selected text does not linger in the page.
import { call } from "./bridge.js";
import { el } from "./dom.js";
import { spansSignature } from "./frames.js";
import type { SpanMsg } from "./frames.js";
import { state } from "./state.js";

interface Entry {
  /** The signature of the spans the text belongs to. */
  sig: string;
  text: string;
  /** A `selection_text` request is out; another one is wanted when it returns (`again`). */
  inflight: boolean;
  again: boolean;
}

const entries = new Map<number, Entry>();

/** The cached text of the pane's selection; empty when nothing is selected or the answer has not arrived. */
export function selectionText(pane: number): string {
  return entries.get(pane)?.text ?? "";
}

/** Drop everything cached for the pane (it exited). An answer still on its way is ignored. */
export function forgetSelection(pane: number): void {
  entries.delete(pane);
}

/** Whether a changed row lies between the first and the last selected row. */
function touches(spans: readonly SpanMsg[], changed: readonly { y: number }[]): boolean {
  if (changed.length === 0) return false;
  let lo = Infinity;
  let hi = -Infinity;
  for (const span of spans) {
    if (!Array.isArray(span)) continue;
    lo = Math.min(lo, span[0]);
    hi = Math.max(hi, span[0]);
  }
  return changed.some((row) => row !== null && typeof row === "object" && row.y >= lo && row.y <= hi);
}

function request(pane: number, entry: Entry): void {
  if (entry.inflight) {
    entry.again = true;
    return;
  }
  entry.inflight = true;
  call<string | null>("selection_text", { pane }).then(
    (text) => {
      entry.inflight = false;
      if (entries.get(pane) !== entry) return; // the selection ended or the pane exited meanwhile
      if (entry.again) {
        // The spans or the rows moved on while this was on its way: this answer may be for the old selection.
        entry.again = false;
        request(pane, entry);
        return;
      }
      entry.text = typeof text === "string" ? text : "";
    },
    () => {
      entry.inflight = false;
      entry.again = false;
    },
  );
}

/**
 * Called for every frame of a pane with the spans of its header and the rows the frame changed. The text is
 * requested when the spans differ from the ones it was asked for, and again when a changed row lies inside the
 * selected rows (the text may differ although the spans did not). An empty list clears the cache.
 */
export function onSelectionSpans(pane: number, spans: readonly SpanMsg[], changed: readonly { y: number }[] = []): void {
  if (spans.length === 0) {
    entries.delete(pane);
    return;
  }
  const sig = spansSignature(spans);
  const known = entries.get(pane);
  if (!known) {
    const entry: Entry = { sig, text: "", inflight: false, again: false };
    entries.set(pane, entry);
    request(pane, entry);
  } else if (known.sig !== sig) {
    known.sig = sig;
    known.text = ""; // the old text is not the new selection's
    request(pane, known);
  } else if (touches(spans, changed)) {
    request(pane, known);
  }
}

// ---- clipboard -----------------------------------------------------------------------------------

/**
 * Put `text` on the clipboard from inside a user gesture (a key or a click handler); true when a copy was started.
 * Hint mode and copy mode copy text that is not the pointer's selection through this.
 */
export function writeClipboard(text: string): boolean {
  const previous = document.activeElement as HTMLElement | null;
  const box = el("textarea", "clipboard-buffer", document.body);
  box.value = text;
  box.setAttribute("readonly", "");
  box.focus();
  box.select();
  let ok = false;
  try {
    ok = document.execCommand("copy");
  } catch {
    // some engines refuse outside a gesture; the async API below is the second try
  }
  box.remove();
  previous?.focus();
  if (!ok && typeof navigator.clipboard?.writeText === "function") {
    void navigator.clipboard.writeText(text).catch(() => {});
    ok = true;
  }
  return ok;
}

/**
 * Copy the focused pane's selection. With the cache filled this is synchronous (the hidden textarea and
 * `execCommand("copy")`) and returns true. With an empty cache the core is asked and the answer, if there is a
 * selection, is written through the async clipboard API: a selection that is scrolled out of the viewport has no
 * spans in the frame, but it is still the core's selection. That path returns false (nothing was copied yet).
 */
export function copySelection(): boolean {
  const pane = state.view?.focus;
  if (pane === undefined) return false;
  const text = selectionText(pane);
  if (text !== "") return writeClipboard(text);
  void call<string | null>("selection_text", { pane }).then(
    (answer) => {
      if (typeof answer === "string" && answer !== "" && typeof navigator.clipboard?.writeText === "function") {
        return navigator.clipboard.writeText(answer);
      }
    },
    () => {},
  );
  return false;
}

/**
 * Whether a copy inside this element belongs to the element (a text field of the page), not to the terminal. The
 * terminal's own input element (`#ime`, keyboard.ts) is not a field: a copy from the Edit menu fires on it.
 */
function isField(target: EventTarget | null): boolean {
  if (target instanceof HTMLElement && target.id === "ime") return false;
  return (
    target instanceof HTMLInputElement ||
    target instanceof HTMLTextAreaElement ||
    (target instanceof HTMLElement && target.isContentEditable)
  );
}

/** Start answering the document's `copy` event from the cache. Capture phase: a bubbling event is not required. */
export function initSelection(): void {
  document.addEventListener(
    "copy",
    (e: ClipboardEvent) => {
      if (isField(e.target) || !e.clipboardData) return;
      const pane = state.view?.focus;
      const text = pane === undefined ? "" : selectionText(pane);
      if (text === "") return;
      e.clipboardData.setData("text/plain", text);
      e.preventDefault();
    },
    true,
  );
}
