// Keyboard input: window-level shortcuts, keys for the core's encoder and paste. The page sends facts about key
// events (code, key, modifier bits, repeat, release, composing) and never builds an escape sequence; the core's
// encoder decides what they mean for the program. A shortcut is a canonical chord from the event, looked up in
// chords.ts and run through the action registry. Features that need keys for themselves (palette, search, copy
// mode, the paste confirmation) push a handler instead of editing this file.
//
// All keys go to a hidden textarea that keeps the keyboard focus: it is what makes the system's input methods and
// dead keys work, because composition events exist only on editable elements.
import { keepsDefault, registerAction, runAction } from "./actions.js";
import { call, listen } from "./bridge.js";
import { actionIdFor, setBindingTable, setOs } from "./chords.js";
import { isConfirmingClose } from "./confirm.js";
import { gridRect } from "./geometry.js";
import { chordsOf, isComposingKey, isModifierKey, keyPayload } from "./keymap.js";
import { offerKey, pushKeyHandler } from "./keystack.js";
import { confirmPaste, isConfirmingPaste, pasteDecision } from "./paste-confirm.js";
import { state } from "./state.js";
import type { View } from "./types.js";

export { pushKeyHandler };
export type { KeyHandler } from "./keystack.js";

// ---- the input element ---------------------------------------------------------------------------

let ime: HTMLTextAreaElement | null = null;

/** Give the keyboard to the terminal input. The page's other modules call this after a control had the focus. */
export function focusInput(): void {
  ime?.focus({ preventScroll: true });
}

/** A text field of the page other than the terminal input: it keeps its own keys and its own paste. */
function inOtherField(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement) || target === ime) return false;
  return (
    target instanceof HTMLInputElement ||
    target instanceof HTMLTextAreaElement ||
    target instanceof HTMLSelectElement ||
    target.isContentEditable ||
    target.dataset.field !== undefined // the code editor: its content is in a shadow root, so its host is the target
  );
}

function createInput(): HTMLTextAreaElement {
  const box = document.createElement("textarea");
  box.id = "ime";
  box.className = "ime";
  box.rows = 1;
  box.setAttribute("aria-label", "Terminal input");
  box.setAttribute("autocomplete", "off");
  box.setAttribute("autocorrect", "off");
  box.setAttribute("autocapitalize", "off");
  box.spellcheck = false;
  (document.getElementById("stage") ?? document.body).append(box);
  return box;
}

// ---- composition and committed text --------------------------------------------------------------

/** An input method composition is active: its keys are the textarea's and its text is not final. */
let composing = false;

/** When the last composition that delivered text ended (`performance.now()`). */
let endedAt = Number.NEGATIVE_INFINITY;

/** A commit just sent: the `input` event some engines fire after `compositionend` must not send it again. */
let recent: { text: string; until: number } | null = null;

/** The window in which an Enter right after a composition's end is the key that ended it, not new input. */
const ENTER_AFTER_COMPOSITION_MS = 80;

/** The window in which an `input` event with the text just committed is the same commit. */
const DUPLICATE_MS = 100;

/** Send committed text (an input method result, a dead-key result) for the focused pane. It is not a paste. */
function sendText(text: string): void {
  const pane = state.view?.focus;
  if (pane === undefined || text === "") return;
  void call("text", { pane, text }).catch(() => {});
}

/** Put the textarea at the cursor cell of the focused pane so the system's candidate window opens there. */
function placeInput(): void {
  const view = state.view;
  if (!ime || !view) return;
  const entry = view.panes.find(([id]) => id === view.focus);
  if (!entry) return;
  const m = state.metrics;
  const grid = gridRect(m, entry[1], state.headerPx);
  const cursor = state.frames.get(view.focus)?.cursor;
  const x = cursor && Number.isFinite(cursor.x) ? Math.max(0, cursor.x) : 0;
  const y = cursor && Number.isFinite(cursor.y) ? Math.max(0, cursor.y) : 0;
  ime.style.left = `${grid.x + x * m.cellW}px`;
  ime.style.top = `${grid.y + y * m.cellH}px`;
  ime.style.height = `${m.cellH}px`;
}

let placing = false;

/** Move the textarea after the frame that just arrived has been patched into the mirror. */
function placeInputSoon(): void {
  if (placing) return;
  placing = true;
  requestAnimationFrame(() => {
    placing = false;
    if (composing) placeInput();
  });
}

function onCompositionStart(): void {
  composing = true;
  placeInput();
}

function onCompositionEnd(e: CompositionEvent): void {
  composing = false;
  const box = ime;
  if (box) box.value = "";
  const text = e.data ?? "";
  if (text === "") return;
  endedAt = performance.now();
  recent = { text, until: endedAt + DUPLICATE_MS };
  sendText(text);
}

/** Text that did not come from a keydown: a dead key's result, the emoji picker, dictation. */
function onInput(e: Event): void {
  if (composing || (e as InputEvent).isComposing) return; // the commit arrives with compositionend
  const box = ime;
  if (!box) return;
  const text = box.value;
  box.value = "";
  if (text === "") return;
  if (recent && performance.now() < recent.until && text === recent.text) {
    recent = null; // the same commit again: some engines deliver it after compositionend as well
    return;
  }
  sendText(text);
}

// ---- terminal input ------------------------------------------------------------------------------

/** The codes of keys whose press went to the core: only their release is sent, so a shortcut leaves no stray one. */
const pressed = new Set<string>();

/** Send one key event for the focused pane. A command that fails is the core's to report, not an error here. */
function sendKey(pane: number, e: KeyboardEvent, release: boolean): void {
  const p = keyPayload(e, release);
  void call("key", {
    pane,
    code: p.code,
    key: p.key,
    mods: p.mods,
    repeat: p.repeat,
    release: p.release,
    composing: p.composing,
  }).catch(() => {});
}

function onKeyDown(e: KeyboardEvent): void {
  // A key that belongs to an input method or a dead-key sequence is the textarea's: no command, no preventDefault.
  if (isComposingKey(e)) return;
  composing = false; // a key that is not an input method's proves no composition is running: never stay stuck
  // WebKit fires the keydown of the Enter that committed a composition after compositionend: it is not input.
  if (e.key === "Enter" && performance.now() - endedAt < ENTER_AFTER_COMPOSITION_MS) {
    e.preventDefault();
    return;
  }
  if (offerKey(e)) {
    // A handler that took the key leaves the default to itself. In the terminal's own input the default would put
    // the character into the field, where it would be sent as text.
    if (e.target === ime) e.preventDefault();
    return;
  }
  for (const chord of chordsOf(e)) {
    const id = actionIdFor(chord);
    // A program on the alternate screen (vim, less, htop) reads these keys itself, and a text field of the page (the name
    // editor, the search bar) edits its own line: neither gets the shell's line-editing bytes.
    if (id !== undefined && EDIT_STEPS.has(id) && (onAlternateScreen() || inOtherField(e.target))) continue;
    // Copy and paste in a text field of the page are the field's own, with the browser's default.
    if (id !== undefined && CLIPBOARD_ACTIONS.has(id) && inOtherField(e.target)) continue;
    // The registry decides what a held key may repeat. A repeat it swallowed is consumed here as well, so it never
    // reaches the encoder as terminal input. An action that keeps its default (paste) lets the first press through
    // to the browser; its repeats are consumed, so a held chord pastes once.
    if (id !== undefined && runAction(id, { repeat: e.repeat })) {
      if (e.repeat || !keepsDefault(id)) e.preventDefault();
      return;
    }
  }
  if (e.metaKey) return; // a Cmd chord nobody owns is neither a command nor terminal input
  if (isModifierKey(e)) return; // the encoder reads the modifier bits of the next key
  if (inOtherField(e.target)) return; // a text field of the page types for itself
  const pane = state.view?.focus;
  if (pane === undefined) return;
  e.preventDefault();
  pressed.add(e.code);
  sendKey(pane, e, false);
}

function onKeyUp(e: KeyboardEvent): void {
  if (!pressed.delete(e.code)) return; // the press never went to the core
  const pane = state.view?.focus;
  if (pane === undefined) return;
  sendKey(pane, e, true);
}

// ---- the binding table ---------------------------------------------------------------------------

/**
 * Take the core's resolved binding table (the `keybindings` rows of every settings view) as the source of shortcut
 * dispatch: a chord runs the action the table binds it to. Without rows (no settings yet, a core that sent none) the
 * static table of chords.ts is in force. The page never applies a binding the core did not return (T-02-72).
 */
export function setBindings(rows: readonly { action: string; chord: string | null }[]): void {
  setBindingTable(rows);
}

// ---- paste ---------------------------------------------------------------------------------------

/** Whether a multi-line paste to a program that did not ask for bracketed paste is confirmed first (D-15). */
let pasteConfirm = true;

/**
 * Set the preference "confirm a multi-line paste" (default true). The settings plan drives it. With it off a paste
 * the core rejected is sent again without asking, except one that holds the bracketed-paste end marker.
 */
export function setPasteConfirm(enabled: boolean): void {
  pasteConfirm = enabled;
}

/**
 * Paste into `pane` through the core, which decides what is unsafe: bracketed when the program asked, `rejected`
 * for multi-line text it did not ask for. The page only asks the person and, on a yes, sends the text again.
 */
async function pasteInto(pane: number, text: string): Promise<void> {
  try {
    const outcome = await call<unknown>("paste", { pane, text, allow_unsafe: false });
    const step = pasteDecision(outcome, text, pasteConfirm);
    if (step === "done") return;
    if (step === "confirm") {
      let yes = false;
      try {
        yes = await confirmPaste(text);
      } finally {
        focusInput();
      }
      if (!yes) return;
    }
    await call("paste", { pane, text, allow_unsafe: true });
  } catch {
    // The core reports what it refused (a paste over its size limit, a pane that is gone); the page does not fail.
  }
}

/** Whether the clipboard data of a paste event holds an image (a file of an `image/` type), whatever else it holds. */
function holdsImage(data: DataTransfer | null): boolean {
  if (!data) return false;
  for (const file of Array.from(data.files ?? [])) if (file.type.startsWith("image/")) return true;
  for (const item of Array.from(data.items ?? [])) if (item.kind === "file" && item.type.startsWith("image/")) return true;
  return Array.from(data.types ?? []).includes("Files");
}

/**
 * Tell the program of `pane` that something that is not text was pasted: the core writes an empty bracketed paste when
 * the program asked for bracketed paste and nothing otherwise. Claude Code on macOS reads that as "fetch the image from
 * the clipboard"; the terminal never writes the image itself. The answer and a failure do not matter to the page.
 */
function sendPasteCue(pane: number): void {
  void call("paste_cue", { pane }).catch(() => {});
}

function onPaste(e: ClipboardEvent): void {
  if (inOtherField(e.target)) return; // a text field of the page keeps its own paste
  e.preventDefault();
  if (pasteTimer !== null) {
    clearTimeout(pasteTimer); // the event is the route: the fallback must not paste as well
    pasteTimer = null;
  } else if (performance.now() < fallbackUntil) {
    return; // the late event of a paste the fallback already made
  }
  if (isConfirmingPaste() || isConfirmingClose()) return; // one question at a time
  const text = e.clipboardData?.getData("text/plain") ?? "";
  const pane = state.view?.focus;
  if (pane === undefined) return;
  if (text === "") {
    // An image and no text is the cue; an event with neither does nothing.
    if (holdsImage(e.clipboardData)) sendPasteCue(pane);
    return;
  }
  void pasteInto(pane, text);
}

// A paste chord is the page's on every platform (Cmd+V on macOS, where the Edit menu is a second route to the same
// event). It is not prevented: the web view then fires `paste` into the terminal input and `onPaste` above takes it. If
// no event comes, the page reads the clipboard itself. Either way the text goes through `pasteInto`, and the two routes
// can never both paste.

/** How long a paste chord waits for the web view's own `paste` event before the page reads the clipboard. */
const PASTE_EVENT_WAIT_MS = 100;

/** How long after the fallback started a `paste` event is still the same paste and is ignored. */
const LATE_PASTE_MS = 500;

/** The timer of a paste chord that is waiting for its `paste` event; null when none is. */
let pasteTimer: ReturnType<typeof setTimeout> | null = null;

/** A `paste` event before this time (`performance.now()`) belongs to a paste the fallback already made. */
let fallbackUntil = Number.NEGATIVE_INFINITY;

/** The paste chord ran: give the browser a moment to fire `paste`, then read the clipboard once if it did not. */
function awaitPasteEvent(): void {
  if (pasteTimer !== null) clearTimeout(pasteTimer);
  pasteTimer = setTimeout(() => {
    pasteTimer = null;
    fallbackUntil = performance.now() + LATE_PASTE_MS;
    void pasteFromClipboard();
  }, PASTE_EVENT_WAIT_MS);
}

/**
 * The text on the system clipboard, or the empty string. The shell reads it first (`clipboard_text`, native on macOS):
 * the page is served from a custom scheme that WebKit does not treat as a secure context, so `navigator.clipboard` is
 * not there in the app's web view on macOS. A shell without a native clipboard (an error), or an answer that is not
 * text, hands over to the web view's clipboard. Only ever called after the person's own paste chord.
 */
async function readClipboardText(): Promise<string> {
  try {
    const answer = await call<unknown>("clipboard_text");
    if (typeof answer === "string") return answer;
  } catch {
    // no native clipboard here: the web view's is the second try
  }
  try {
    return await navigator.clipboard.readText();
  } catch {
    return ""; // no permission or no clipboard: nothing to paste
  }
}

/** The clipboard text through the one paste path. Only ever called after the person's own paste chord. */
async function pasteFromClipboard(): Promise<void> {
  if (isConfirmingPaste() || isConfirmingClose()) return; // one question at a time
  const text = await readClipboardText();
  const pane = state.view?.focus;
  if (pane === undefined) return;
  if (text === "") {
    // The read cannot tell an image from an empty clipboard, and an empty bracketed paste is harmless to a program that
    // ignores it. Only macOS: Claude Code takes an empty paste as its image cue there (and in WSL) and uses Alt+V or
    // Ctrl+V as keys elsewhere, so another platform sends nothing.
    if (state.os === "macos") sendPasteCue(pane);
    return;
  }
  await pasteInto(pane, text);
}

/** The actions whose chord a text field of the page leaves to the field. */
const CLIPBOARD_ACTIONS: ReadonlySet<string> = new Set(["edit.copy", "edit.paste"]);

// ---- layout actions ------------------------------------------------------------------------------

/** Register the actions that call layout commands; `apply` takes the new view the core answers with. */
function registerLayoutActions(apply: (v: View) => void): void {
  const show = (p: Promise<View>) => void p.then(apply);
  registerAction({ id: "pane.split_right", title: "Split pane right", run: () => show(call("split", { column: false })) });
  registerAction({ id: "pane.split_down", title: "Split pane down", run: () => show(call("split", { column: true })) });
  registerAction({
    id: "pane.close",
    title: "Close pane",
    run: () => {
      if (state.view) show(call("close_pane", { pane: state.view.focus }));
    },
  });
  registerAction({ id: "pane.focus_next", title: "Focus next pane", run: () => show(call("cycle_focus", { step: 1 })) });
  registerAction({ id: "pane.focus_prev", title: "Focus previous pane", run: () => show(call("cycle_focus", { step: -1 })) });
  registerAction({ id: "tab.new", title: "New tab", run: () => show(call("new_tab")) });
  registerAction({
    id: "workspace.new",
    title: "New workspace",
    run: () => show(call("new_workspace", { name: `ws ${(state.view?.workspaces.length ?? 0) + 1}` })),
  });
  for (let n = 1; n <= 9; n++) {
    registerAction({ id: `tab.select_${n}`, title: `Select tab ${n}`, run: () => show(call("select_tab", { index: n - 1 })) });
    registerAction({
      id: `workspace.select_${n}`,
      title: `Select workspace ${n}`,
      run: () => show(call("select_workspace", { index: n - 1 })),
    });
  }
}

/** The line-editing steps the core knows, by action id. */
const EDIT_STEPS: ReadonlyMap<string, string> = new Map([
  ["edit.word_back", "word_back"],
  ["edit.word_forward", "word_forward"],
  ["edit.line_start", "line_start"],
  ["edit.line_end", "line_end"],
  ["edit.delete_word_back", "delete_word_back"],
  ["edit.delete_to_line_start", "delete_to_line_start"],
]);

const EDIT_TITLES: ReadonlyMap<string, string> = new Map([
  ["edit.word_back", "Move one word back"],
  ["edit.word_forward", "Move one word forward"],
  ["edit.line_start", "Move to the start of the line"],
  ["edit.line_end", "Move to the end of the line"],
  ["edit.delete_word_back", "Delete the word before the cursor"],
  ["edit.delete_to_line_start", "Delete to the start of the line"],
]);

/** Whether the focused pane shows a full-screen program (alternate screen): it reads its keys itself. */
function onAlternateScreen(): boolean {
  const pane = state.view?.focus;
  return pane !== undefined && state.frames.get(pane)?.modes.alt === true;
}

/** The line-editing actions: the page names the step, the core owns the bytes. */
function registerEditActions(): void {
  for (const [id, step] of EDIT_STEPS) {
    registerAction({
      id,
      title: EDIT_TITLES.get(id) ?? id,
      run: () => {
        const pane = state.view?.focus;
        if (pane !== undefined) void call("send_edit", { pane, action: step });
      },
    });
  }
}

/**
 * Paste on every platform: the chord keeps its default and the web view's `paste` event is the primary route, with a
 * read of the clipboard as the fallback (`edit.copy` is registered in pointer.ts, on every platform).
 */
function registerPasteAction(): void {
  registerAction({ id: "edit.paste", title: "Paste", keepDefault: true, run: awaitPasteEvent });
}

/** Register the layout actions and start listening to the keyboard and to paste. Call once at start-up. */
export function initKeyboard(applyView: (v: View) => void): void {
  setOs(state.os); // app_info was answered before this runs: the static chords of this platform
  registerLayoutActions(applyView);
  registerEditActions();
  registerPasteAction();
  ime = createInput();
  const box = ime;
  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("keyup", onKeyUp);
  window.addEventListener("paste", onPaste);
  box.addEventListener("compositionstart", onCompositionStart);
  box.addEventListener("compositionupdate", placeInput);
  box.addEventListener("compositionend", onCompositionEnd);
  box.oninput = onInput; // text that is not a keydown: dead-key results, the emoji picker, dictation
  // While a composition is active the textarea follows the cursor: a frame of the focused pane moves it.
  void listen<{ pane?: number }>("frame", (msg) => {
    if (composing && msg?.pane === state.view?.focus) placeInputSoon();
  });

  // The canvas is focusable for the mouse, but the keyboard lives in the textarea.
  document.addEventListener("focusin", (e) => {
    if (e.target instanceof Element && e.target.id === "term") focusInput();
  });
  // A click on nothing focusable would leave the page without a keyboard owner. A control that took the focus
  // (a bar button, a dialog) keeps it until it gives it back.
  box.addEventListener("blur", () => {
    composing = false; // a composition does not outlive the focus, whether or not the engine says compositionend
    setTimeout(() => {
      const now = document.activeElement;
      if (now === null || now === document.body) focusInput();
    }, 0);
  });
  focusInput();
}
