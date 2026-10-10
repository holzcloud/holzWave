// Keyboard input: window-level shortcuts, the transitional key table that turns keys into terminal input, and
// paste. A shortcut is a canonical chord built from the event, looked up in chords.ts and run through the action
// registry. Features that need keys for themselves (palette, search, copy mode) push a handler instead of
// editing this file.
import { registerAction, runAction } from "./actions.js";
import { call } from "./bridge.js";
import { actionIdFor } from "./chords.js";
import { renderOverlay } from "./overlay.js";
import { state } from "./state.js";
import type { View } from "./types.js";

// ---- handler stack -------------------------------------------------------------------------------

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

// ---- chords --------------------------------------------------------------------------------------

/** The key part of a chord, or null for keys the table has no name for yet (named keys are the keyboard plan's). */
function keyName(e: KeyboardEvent): string | null {
  // Option changes the typed character on macOS (Option+1 gives "¡"): the physical key names the chord instead.
  const physical = e.altKey ? /^(?:Digit(\d)|Key([A-Z]))$/.exec(e.code) : null;
  if (physical) return physical[1] ?? physical[2] ?? null;
  if (e.key === " ") return "Space";
  if (e.key.length === 1) return e.key.toUpperCase();
  return null;
}

/** The canonical chord of a key event: Cmd, Ctrl, Alt, Shift, then the key. Null when the key has no name. */
function chordOf(e: KeyboardEvent): string | null {
  const key = keyName(e);
  if (key === null) return null;
  const parts: string[] = [];
  if (e.metaKey) parts.push("Cmd");
  if (e.ctrlKey) parts.push("Ctrl");
  if (e.altKey) parts.push("Alt");
  if (e.shiftKey) parts.push("Shift");
  parts.push(key);
  return parts.join("+");
}

// ---- terminal input ------------------------------------------------------------------------------

const KEYS: Record<string, string> = {
  Enter: "\r", Backspace: "\x7f", Tab: "\t", Escape: "\x1b",
  ArrowUp: "\x1b[A", ArrowDown: "\x1b[B", ArrowRight: "\x1b[C", ArrowLeft: "\x1b[D",
  Home: "\x1b[H", End: "\x1b[F", Delete: "\x1b[3~", PageUp: "\x1b[5~", PageDown: "\x1b[6~",
};

function send(data: string): void {
  if (!state.view) return;
  if (state.selection) { state.selection = null; renderOverlay(); }
  void call("input", { pane: state.view.focus, data });
}

/** The bytes a key event stands for, or undefined if it is not terminal input. */
function terminalInput(e: KeyboardEvent): string | undefined {
  let data = KEYS[e.key];
  if (data === undefined && e.ctrlKey && e.key.length === 1) {
    const c = e.key.toLowerCase().charCodeAt(0);
    if (c >= 97 && c <= 122) data = String.fromCharCode(c - 96);
  }
  if (data === undefined && e.key.length === 1 && !e.ctrlKey) data = e.altKey ? "\x1b" + e.key : e.key;
  return data;
}

function onKeyDown(e: KeyboardEvent): void {
  // Newest handler first. Work on a copy so a handler may push or pop while it runs.
  for (const entry of [...handlers].reverse()) {
    if (handlers.includes(entry) && entry.fn(e)) return;
  }
  const chord = chordOf(e);
  const id = chord === null ? undefined : actionIdFor(chord);
  if (id !== undefined && runAction(id)) {
    e.preventDefault();
    return;
  }
  if (e.metaKey) return; // a Cmd chord nobody owns is neither a command nor terminal input
  const data = terminalInput(e);
  if (data === undefined) return;
  e.preventDefault();
  send(data);
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
  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("paste", (e) => {
    const text = e.clipboardData?.getData("text");
    if (text) send(text);
  });
}
