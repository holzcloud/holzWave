// The paste confirmation. The core decides what is unsafe (it answers `rejected` for a paste the program did not
// ask for in bracketed form); the page only asks the person. This module holds the decision, the preview and the
// overlay. The pure functions have no DOM at import, so node can test them; `confirmPaste` builds the dialog from
// `dom.ts` helpers and puts every pasted character in with `textContent`, never as markup.
import { el, loadStyles } from "./dom.js";
import { pushKeyHandler } from "./keystack.js";

/** What the core answered to a paste (`paste` command, golden `protocol.json`). */
export type PasteOutcome = "written" | "empty" | "rejected";

/** What the page does next: nothing, ask the person, or send the same text again without asking. */
export type PasteStep = "done" | "confirm" | "resend";

/** The bracketed-paste end marker in its 7-bit and 8-bit spellings: text that holds it is trying to leave the paste. */
const END_MARKERS = ["\u001b[201~", "\u009b201~"];

/**
 * What to do with the core's answer. Only `rejected` goes on: with the preference on the person is asked; with it
 * off the same text is sent again with `allow_unsafe`, except when it holds the bracket end marker, which always
 * asks. An answer the page does not know is `done`: it never turns into a send.
 */
export function pasteDecision(outcome: unknown, text: string, confirmSetting: boolean): PasteStep {
  if (outcome !== "rejected") return "done";
  if (confirmSetting || END_MARKERS.some((m) => text.includes(m))) return "confirm";
  return "resend";
}

// ---- the preview ---------------------------------------------------------------------------------

/** The lines the confirmation shows and how many it left out. */
export interface Preview {
  /** The number of lines in the text; one trailing line break does not add a line. */
  lines: number;
  /** The first lines, cut, with control characters replaced by visible markers. */
  shown: string[];
  /** `lines` minus `shown.length`. */
  hidden: number;
  /** The text holds a control character other than tab and the line breaks: a terminal control sequence. */
  control: boolean;
}

export const PREVIEW_LINES = 5;
export const PREVIEW_COLUMNS = 80;

const BREAK = /\r\n|\n|\r/g;
/** C0 without tab, LF, CR; DEL; C1. */
const CONTROL_SEQUENCE = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/;
/** Everything that is replaced in the preview: C0, DEL, C1, zero-width, bidirectional and invisible format characters. */
const INVISIBLE = /[\u0000-\u001f\u007f-\u009f\u200b-\u200f\u2028-\u202e\u2060-\u206f\ufeff]/g;

/** A control character as its picture (U+2400 block), anything else invisible as `<U+XXXX>`. */
function mark(line: string): string {
  return line.replace(INVISIBLE, (c) => {
    const n = c.charCodeAt(0);
    if (n <= 0x1f) return String.fromCharCode(0x2400 + n);
    if (n === 0x7f) return "\u2421";
    return `<U+${n.toString(16).toUpperCase().padStart(4, "0")}>`;
  });
}

/** The first `PREVIEW_COLUMNS` code points of a line, with an ellipsis when something was left out. */
function cut(line: string): string {
  const points = Array.from(line.slice(0, PREVIEW_COLUMNS * 4));
  return points.length > PREVIEW_COLUMNS ? `${points.slice(0, PREVIEW_COLUMNS).join("")}\u2026` : line;
}

/** Count the lines and take the preview of a pasted text. The work is bounded by the text's length, once. */
export function previewOf(text: string): Preview {
  const shown: string[] = [];
  let lines = 0;
  let start = 0;
  const take = (end: number): void => {
    lines += 1;
    if (shown.length < PREVIEW_LINES) shown.push(mark(cut(text.slice(start, Math.min(end, start + PREVIEW_COLUMNS * 4)))));
  };
  BREAK.lastIndex = 0;
  for (let m = BREAK.exec(text); m !== null; m = BREAK.exec(text)) {
    take(m.index);
    start = m.index + m[0].length;
  }
  if (start < text.length) take(text.length);
  return { lines, shown, hidden: lines - shown.length, control: CONTROL_SEQUENCE.test(text) };
}

// ---- the overlay ---------------------------------------------------------------------------------

let open = false;

/** A confirmation is on screen: the keyboard's paste listener ignores further pastes meanwhile. */
export function isConfirmingPaste(): boolean {
  return open;
}

/**
 * Ask whether `text` may be pasted. A modal dialog with the line count, a bounded preview and Cancel and Paste;
 * Enter or Paste resolve true, Escape, Cancel or a click outside resolve false. While it is open every key goes to
 * it (the key handler stack), so nothing reaches the terminal. Focus is not restored here: the caller does that.
 */
export async function confirmPaste(text: string): Promise<boolean> {
  if (open) return false; // one question at a time; the second paste is declined
  open = true;
  try {
    await loadStyles("./dialogs.css");
  } catch {
    // the dialog still works without its style sheet
  }
  const preview = previewOf(text);

  return new Promise<boolean>((resolve) => {
    const backdrop = el("div", "dlg-backdrop", document.body);
    const box = el("div", "dlg", backdrop);
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-modal", "true");
    box.setAttribute("aria-labelledby", "dlg-title");

    const title = el("h2", "dlg-title", box);
    title.id = "dlg-title";
    title.textContent = preview.lines === 1 ? "Paste 1 line?" : `Paste ${preview.lines} lines?`;

    const why = el("p", "dlg-why", box);
    why.textContent = "The program did not ask for bracketed paste, so every line is typed as if you had entered it.";
    if (preview.control) {
      const note = el("p", "dlg-note", box);
      note.textContent = "Warning: this text contains terminal control sequences.";
    }

    const list = el("div", "dlg-preview", box);
    for (const line of preview.shown) el("div", "dlg-line", list).textContent = line;
    if (preview.hidden > 0) {
      const more = el("p", "dlg-more", box);
      more.textContent = `${preview.hidden} more line${preview.hidden === 1 ? "" : "s"} not shown`;
    }

    const actions = el("div", "dlg-actions", box);
    const cancel = el("button", "dlg-cancel", actions);
    cancel.type = "button";
    cancel.textContent = "Cancel";
    const ok = el("button", "dlg-ok", actions);
    ok.type = "button";
    ok.textContent = "Paste";

    let pop = (): void => {};
    const finish = (result: boolean): void => {
      if (!open) return;
      open = false;
      pop();
      backdrop.remove();
      resolve(result);
    };
    pop = pushKeyHandler((e) => {
      // A Cmd chord belongs to the system menu (quit, hide): not the terminal's, so it is consumed, and its
      // default is left alone.
      if (e.metaKey) return true;
      const focused = document.activeElement;
      if (e.key === "Escape") finish(false);
      else if (e.key === "Enter") finish(focused !== cancel);
      else if (e.key === " ") {
        if (focused === ok) finish(true);
        else if (focused === cancel) finish(false);
      } else if (e.key === "Tab") (focused === ok ? cancel : ok).focus();
      e.preventDefault();
      return true;
    });
    cancel.addEventListener("click", () => finish(false));
    ok.addEventListener("click", () => finish(true));
    backdrop.addEventListener("pointerdown", (e) => {
      if (e.target === backdrop) finish(false);
    });
    ok.focus();
  });
}
