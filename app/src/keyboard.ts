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
import { chordsOf, isComposingKey, isModifierKey, type KeyPayload, keyPayload } from "./keymap.js";
import { CONTROL_SELECTOR, isControlKey, isFunctionKeyChord } from "./keyroute.js";
import {
  anySticky,
  consume,
  idleSticky,
  keyEvents,
  keyForChar,
  keyForInputType,
  modsFor,
  type Modifier,
  type RowKey,
  SENTINEL,
  type SourceEvent,
  type StickyState,
  sourceAfter,
  stripSentinel,
  tap,
} from "./keyrowstate.js";
import { offerKey, pushKeyHandler } from "./keystack.js";
import { confirmPaste, isConfirmingPaste, pasteDecision } from "./paste-confirm.js";
import { inBar, walkBar } from "./regions.js";
import { state } from "./state.js";
import { showNotice } from "./status.js";
import type { View } from "./types.js";
import { CLIPBOARD_REFUSED } from "./webstate.js";

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

/**
 * The control has the focus for the keyboard (`:focus-visible`: it was reached by a key, or by a script after one) and not only
 * because a pointer pressed it, which some engines answer with a focus that nobody asked the keyboard to keep. A control that a
 * click left focused stays out of the keys' way: Enter and Tab still go to the terminal, as they did before the bar had the keyboard.
 * An engine that does not know the selector counts every focus as the keyboard's.
 */
function keyboardFocus(target: HTMLElement): boolean {
  try {
    return target.matches(":focus-visible");
  } catch {
    return true;
  }
}

/**
 * A control of the page other than the terminal input has the keyboard focus (a button of the bar, a link, a tab, a scrollable box):
 * the keys that move between controls and press them are its own, and nothing of them goes to the terminal.
 */
function onControl(target: EventTarget | null): target is HTMLElement {
  if (!(target instanceof HTMLElement) || target === ime || target.id === "term") return false;
  return target.matches(CONTROL_SELECTOR) && keyboardFocus(target);
}

/** A control of the bar has the keyboard focus. */
function onBarControl(target: EventTarget | null): target is HTMLElement {
  return inBar(target) && onControl(target);
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
  noteSource({ type: "compositionstart" });
  placeInput();
}

function onCompositionEnd(e: CompositionEvent): void {
  composing = false;
  const box = ime;
  if (box) resetField(box);
  const text = e.data ?? "";
  if (text === "") return;
  endedAt = performance.now();
  recent = { text, until: endedAt + DUPLICATE_MS };
  sendSoftText(text);
}

/** Text that did not come from a keydown: a dead key's result, the emoji picker, dictation. */
function onInput(e: Event): void {
  if (composing || (e as InputEvent).isComposing) return; // the commit arrives with compositionend
  const box = ime;
  if (!box) return;
  const text = touch ? stripSentinel(box.value) : box.value;
  resetField(box);
  if (text === "") return;
  if (recent && performance.now() < recent.until && text === recent.text) {
    recent = null; // the same commit again: some engines deliver it after compositionend as well
    return;
  }
  sendSoftText(text);
}

// ---- terminal input ------------------------------------------------------------------------------

/** The codes of keys whose press went to the core: only their release is sent, so a shortcut leaves no stray one. */
const pressed = new Set<string>();

/**
 * Send one key event for `pane`: the one path of the keyboard, the key row and a soft keyboard's keys, so the core sees one kind of
 * key event. A command that fails is the core's to report, not an error here.
 */
export function sendKeyPayload(pane: number, p: KeyPayload): void {
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

/** Send one key event of the keyboard; `extra` is the modifier bits of a sticky key that the event itself does not carry. */
function sendKey(pane: number, e: KeyboardEvent, release: boolean, extra = 0): void {
  const p = keyPayload(e, release);
  sendKeyPayload(pane, extra === 0 ? p : { ...p, mods: p.mods | extra });
}

function onKeyDown(e: KeyboardEvent): void {
  noteSource({ type: "keydown", key: e.key, code: e.code, keyCode: e.keyCode });
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
  // Escape on a control of the bar gives the keyboard back to the terminal; the bar has no use for it and the terminal gets nothing.
  if (e.key === "Escape" && onBarControl(e.target) && !inOtherField(e.target)) {
    e.preventDefault();
    focusInput();
    return;
  }
  for (const chord of chordsOf(e)) {
    const id = actionIdFor(chord);
    // A program on the alternate screen (vim, less, htop) reads these keys itself, and a text field of the page (the name
    // editor, the search bar) edits its own line: neither gets the shell's line-editing bytes.
    if (id !== undefined && EDIT_STEPS.has(id) && (onAlternateScreen() || inOtherField(e.target))) continue;
    // Copy and paste in a text field of the page are the field's own, with the browser's default.
    if (id !== undefined && CLIPBOARD_ACTIONS.has(id) && inOtherField(e.target)) continue;
    // A function key alone (or with Shift) is what a program such as htop or mc reads for itself: while one is on the alternate
    // screen the terminal input leaves it to the program. The palette entry of the action is the way then.
    if (id !== undefined && isFunctionKeyChord(chord) && e.target === ime && onAlternateScreen()) continue;
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
  // The bar is walked by the page: Tab and Shift+Tab go to the next and the previous control and, past the last or the first, to the
  // terminal. The engine's own Tab would reach buttons on some computers only (WebKit on macOS needs full keyboard access).
  if (e.key === "Tab" && isControlKey(e) && onBarControl(e.target)) {
    e.preventDefault();
    if (!walkBar(e.target, e.shiftKey)) focusInput();
    return;
  }
  if (isControlKey(e) && onControl(e.target)) return; // a focused control keeps Tab, Enter, Space and the arrows: no default prevented
  const pane = state.view?.focus;
  if (pane === undefined) return;
  e.preventDefault();
  pressed.add(e.code);
  const extra = touch ? takeStickyMods() : 0; // a sticky modifier of the key row belongs to this key, from whatever keyboard
  if (extra !== 0) stuck.set(e.code, extra);
  sendKey(pane, e, false, extra);
}

function onKeyUp(e: KeyboardEvent): void {
  if (!pressed.delete(e.code)) return; // the press never went to the core
  const pane = state.view?.focus;
  if (pane === undefined) return;
  const extra = stuck.get(e.code) ?? 0;
  stuck.delete(e.code);
  sendKey(pane, e, true, extra);
}

// ---- touch layouts: sticky modifiers and the soft keyboard (docs/design.md section 6.6) -----------------------------
//
// Everything below is in force on a phone or a tablet only (`setTouchInput`, called by keyrow.ts from the layout class); on a
// desktop and in a browser `touch` stays false, no listener is added to the terminal input and every function above behaves as before.
// Built to the known behaviour of Android input methods and not run on a real one: a soft keyboard sends key code 229 for text
// and may send nothing for Backspace or Enter in an empty field, so the field holds one sentinel character and a `beforeinput`
// event stands for the key.

/** The page is a phone or a tablet. */
let touch = false;

/** The sticky `ctrl` and `alt` of the key row. */
let sticky: StickyState = idleSticky();

/** The last keys came from a hardware keyboard (the key row is not drawn then). */
let hardware = false;

/** The modifier bits a sticky key added to a press, by key code, so its release carries the same bits. */
const stuck = new Map<string, number>();

const touchWatchers = new Set<() => void>();

function touchChanged(): void {
  for (const fn of [...touchWatchers]) fn();
}

/** Call `fn` when the sticky state or the keyboard source changes. The returned function stops it. */
export function onTouchInputChange(fn: () => void): () => void {
  touchWatchers.add(fn);
  return () => void touchWatchers.delete(fn);
}

/** The sticky state, for the key row to draw. */
export function stickyNow(): StickyState {
  return sticky;
}

/** The last keys came from a hardware keyboard. */
export function hardwareKeyboardNow(): boolean {
  return hardware;
}

/** A tap on a sticky key of the row at time `now` (milliseconds on the event clock). */
export function tapSticky(which: Modifier, now: number): void {
  if (!touch) return;
  sticky = tap(sticky, which, now);
  touchChanged();
}

/**
 * The modifier bits the next key carries: `base` plus every armed or locked modifier. The armed ones are spent by this call,
 * the locked ones stay. Call it once for each key that is sent.
 */
export function takeStickyMods(base = 0): number {
  const mods = modsFor(sticky, base);
  const next = consume(sticky);
  if (next !== sticky) {
    sticky = next;
    touchChanged();
  }
  return mods;
}

/** Say where the last event came from; on a desktop it is not asked. */
function noteSource(event: SourceEvent): void {
  if (!touch) return;
  const next = sourceAfter(hardware, event);
  if (next === hardware) return;
  hardware = next;
  touchChanged();
}

/** Put the terminal input back to its resting content: one sentinel on a touch layout, empty otherwise. */
function resetField(box: HTMLTextAreaElement): void {
  if (!touch) {
    box.value = "";
    return;
  }
  box.value = SENTINEL;
  box.setSelectionRange(SENTINEL.length, SENTINEL.length);
}

/** Give the focused input its sentinel, unless a composition is running (setting the value would end it). */
function keepSentinel(): void {
  const box = ime;
  if (!box || composing || box.value === SENTINEL) return;
  resetField(box);
}

/** The key of `key` for the focused pane, with the sticky modifiers (spent by it). A pane that is gone makes no key. */
function sendRowKeyEvents(key: RowKey): void {
  const pane = state.view?.focus;
  if (pane === undefined) return;
  const { press, release } = keyEvents({ ...key, mods: takeStickyMods(key.mods) });
  sendKeyPayload(pane, press);
  sendKeyPayload(pane, release);
}

/**
 * Text a soft keyboard committed. On a touch layout one printable character with a modifier armed or locked is that key with
 * the modifier (`ctrl` then `c` is Ctrl+C, not the text "c"); any other commit is text, and it spends an armed modifier.
 */
function sendSoftText(text: string): void {
  if (!touch || !anySticky(sticky)) {
    sendText(text);
    return;
  }
  const key = keyForChar(text);
  if (key) {
    sendRowKeyEvents(key);
    return;
  }
  takeStickyMods();
  sendText(text);
}

/** An on-screen keyboard's Backspace, Enter and word delete arrive as `beforeinput` events of the field, never as keys. */
function onBeforeInput(e: Event): void {
  const ev = e as InputEvent;
  noteSource({ type: "beforeinput", isComposing: ev.isComposing });
  if (composing || ev.isComposing) return; // an edit inside a composition belongs to the input method
  const key = keyForInputType(ev.inputType);
  if (!key) return;
  ev.preventDefault(); // the field stays at its sentinel
  sendRowKeyEvents(key);
}

/**
 * Turn the touch input on or off (keyrow.ts follows the layout class). On: the sentinel and the `beforeinput` path. Off: the
 * field is empty again, a modifier left locked is released and the keyboard source is forgotten.
 */
export function setTouchInput(on: boolean): void {
  if (on === touch) return;
  touch = on;
  const box = ime;
  if (on) {
    box?.addEventListener("beforeinput", onBeforeInput);
    box?.addEventListener("focus", keepSentinel);
    if (box && document.activeElement === box) keepSentinel();
  } else {
    box?.removeEventListener("beforeinput", onBeforeInput);
    box?.removeEventListener("focus", keepSentinel);
    if (box && !composing) box.value = stripSentinel(box.value);
    sticky = idleSticky();
    hardware = false;
    stuck.clear();
  }
  touchChanged();
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

/** Paste `text` into `pane` through the one paste path: the core decides, the person is asked where it says so. */
export function pasteText(pane: number, text: string): Promise<void> {
  return pasteInto(pane, text);
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
 *
 * In a browser (the platform `web`) there is no native read: the page reads the browser's clipboard once, from that
 * gesture, and a read the browser refuses is said in the status line and gives the empty text.
 */
async function readClipboardText(): Promise<string> {
  if (state.os !== "web") {
    try {
      const answer = await call<unknown>("clipboard_text");
      if (typeof answer === "string") return answer;
    } catch {
      // no native clipboard here: the web view's is the second try
    }
  }
  try {
    return await navigator.clipboard.readText();
  } catch {
    if (state.os === "web") showNotice(CLIPBOARD_REFUSED);
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
  // The key row and the touch input follow the layout class of an Android window. On a desktop and in a browser the module is
  // never fetched. It is loaded when needed because it imports the scene and the phone chrome, which import this file.
  if (state.os === "android") void import("./keyrow.js").then((m) => m.initKeyRow());
}
