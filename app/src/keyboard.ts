// Keyboard input: window-level shortcuts, keys for the core's encoder and paste. The page sends facts about key
// events (code, key, modifier bits, repeat, release, composing) and never builds an escape sequence; the core's
// encoder decides what they mean for the program. A shortcut is a canonical chord from the event, looked up in
// chords.ts and run through the action registry. Features that need keys for themselves (palette, search, copy
// mode, the paste confirmation) push a handler instead of editing this file.
//
// All keys go to a hidden textarea that keeps the keyboard focus: it is what makes the system's input methods and
// dead keys work, because composition events exist only on editable elements.
import { registerAction, runAction } from "./actions.js";
import { call } from "./bridge.js";
import { actionIdFor } from "./chords.js";
import { chordsOf, isComposingKey, isModifierKey, keyPayload } from "./keymap.js";
import { offerKey, pushKeyHandler } from "./keystack.js";
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
    target.isContentEditable
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
  if (offerKey(e)) return;
  for (const chord of chordsOf(e)) {
    const id = actionIdFor(chord);
    if (id !== undefined && runAction(id)) {
      e.preventDefault();
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

// ---- paste ---------------------------------------------------------------------------------------

function onPaste(e: ClipboardEvent): void {
  if (inOtherField(e.target)) return; // a text field of the page keeps its own paste
  e.preventDefault();
  const text = e.clipboardData?.getData("text/plain") ?? "";
  const pane = state.view?.focus;
  if (text === "" || pane === undefined) return;
  void call("paste", { pane, text, allow_unsafe: false }).catch(() => {});
}

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

/** Register the layout actions and start listening to the keyboard and to paste. Call once at start-up. */
export function initKeyboard(applyView: (v: View) => void): void {
  registerLayoutActions(applyView);
  ime = createInput();
  const box = ime;
  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("keyup", onKeyUp);
  window.addEventListener("paste", onPaste);

  // The canvas is focusable for the mouse, but the keyboard lives in the textarea.
  document.addEventListener("focusin", (e) => {
    if (e.target instanceof Element && e.target.id === "term") focusInput();
  });
  // A click on nothing focusable would leave the page without a keyboard owner. A control that took the focus
  // (a bar button, a dialog) keeps it until it gives it back.
  box.addEventListener("blur", () => {
    setTimeout(() => {
      const now = document.activeElement;
      if (now === null || now === document.body) focusInput();
    }, 0);
  });
  focusInput();
}
