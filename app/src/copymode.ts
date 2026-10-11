// Copy mode (DESK-21): vi-style keys over the screen and the scrollback. The state is the core's (D-06): the page
// maps keys to `copy_mode` operations and shows what comes back (the mode in the status line, the copy cursor and the
// one-cell or growing selection from the frame header). It sends no scroll command: the core scrolls to the cursor.
//
// While the mode is active a key handler on the stack takes every key, so nothing reaches the shell. Two things stay
// out of it: a Cmd chord (a shortcut, which never reaches the shell either) and a key typed into another text
// field of the page (the search bar that `/` opens has its own handler, newer than this one).
import { registerAction, runAction } from "./actions.js";
import { call } from "./bridge.js";
import { focusInput } from "./keyboard.js";
import { pushKeyHandler } from "./keystack.js";
import { onOverlayRender } from "./overlay.js";
import { copySelection, selectionText, writeClipboard } from "./selection.js";
import { state } from "./state.js";

export type CopyModeName = "none" | "copy" | "select";

/** The answer of `copy_mode` (core/src/wire.rs `CopyStateMsg`). */
interface CopyState {
  active: boolean;
  selecting: boolean;
  yanked: string | null;
}

let mode: CopyModeName = "none";
/** The pane copy mode runs in, or null. */
let pane: number | null = null;
/** Bumped on every enter and end: an answer that belongs to an earlier run is ignored. */
let epoch = 0;
let popKeys: (() => void) | null = null;
let render: () => void = () => {};

/** The mode for the status line: `none`, `copy` or `select` (a selection is being extended). */
export function getCopyMode(): CopyModeName {
  return mode;
}

/** Read the answer of the core; null for anything that is not one. */
function readState(answer: unknown): CopyState | null {
  if (typeof answer !== "object" || answer === null) return null;
  const a = answer as Record<string, unknown>;
  if (typeof a.active !== "boolean") return null;
  return {
    active: a.active,
    selecting: a.selecting === true,
    yanked: typeof a.yanked === "string" ? a.yanked : null,
  };
}

/** Leave copy mode on the page; `sendExit` tells the core as well (not when it already said so, or the pane is gone). */
function end(sendExit: boolean): void {
  if (pane === null) return;
  const gone = pane;
  pane = null;
  epoch++;
  mode = "none";
  popKeys?.();
  popKeys = null;
  if (sendExit) void call("copy_mode", { pane: gone, op: { kind: "exit" } }).catch(() => {});
  render();
  focusInput();
}

/** Send one operation for the pane and take the answer's state, unless the mode ended or restarted meanwhile. */
function send(op: Record<string, unknown>): Promise<CopyState | null> {
  const target = pane;
  if (target === null) return Promise.resolve(null);
  const mine = epoch;
  return call<unknown>("copy_mode", { pane: target, op }).then(
    (answer) => {
      const s = readState(answer);
      if (s && mine === epoch && pane === target) {
        if (!s.active) {
          end(false);
        } else {
          const next: CopyModeName = s.selecting ? "select" : "copy";
          if (next !== mode) {
            mode = next;
            render();
          }
        }
      }
      return s;
    },
    () => {
      if (mine === epoch && op.kind === "enter") end(false); // the core refused: the keys are the program's again
      return null;
    },
  );
}

function enter(): void {
  const view = state.view;
  if (!view) return;
  if (pane !== null && pane !== view.focus) end(true); // the previous pane's copy mode ends first
  pane = view.focus;
  popKeys ??= pushKeyHandler(onKey);
  if (mode === "none") {
    mode = "copy"; // the keys are the mode's from this moment; the answer corrects it
    render();
  }
  void send({ kind: "enter" });
}

/** The motion a key stands for, or null. */
function motionOf(e: KeyboardEvent): string | null {
  if (e.ctrlKey) {
    const k = e.key.toLowerCase();
    return k === "u" ? "page_up" : k === "d" ? "page_down" : null;
  }
  switch (e.key) {
    case "h": case "ArrowLeft": return "left";
    case "j": case "ArrowDown": return "down";
    case "k": case "ArrowUp": return "up";
    case "l": case "ArrowRight": return "right";
    case "0": case "Home": return "line_start";
    case "$": case "End": return "line_end";
    case "g": return "top";
    case "G": return "bottom";
    case "w": return "word_forward";
    case "b": return "word_back";
    case "PageUp": return "page_up";
    case "PageDown": return "page_down";
    default: return null;
  }
}

/** y or Enter: copy what is selected the way a mouse selection is copied, then end the mode with `yank`. */
function yank(): void {
  const target = pane;
  if (target === null) return;
  const cached = selectionText(target) !== "";
  if (cached) copySelection(); // synchronous, inside this key's task, like Cmd+C
  const answer = send({ kind: "yank" });
  end(false); // a yank ends the mode: the keys are the program's from the next one on
  void answer.then((s) => {
    if (!cached && s?.yanked) writeClipboard(s.yanked);
  });
}

function onKey(e: KeyboardEvent): boolean {
  if (pane === null) return false;
  // Another text field of the page has the keyboard (the search bar): its keys are its own.
  const t = e.target;
  if ((t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement) && t.id !== "ime") return false;
  if (e.metaKey) return false; // a shortcut; it does not reach the shell either
  if (e.altKey) return true;

  const motion = motionOf(e);
  if (motion !== null) {
    void send({ kind: "move", motion });
    return true;
  }
  if (e.ctrlKey) return true;
  switch (e.key) {
    case "v":
      void send({ kind: "toggle" });
      break;
    case "Escape":
    case "q":
      end(true);
      break;
    case "y":
    case "Enter":
      yank();
      break;
    case "/":
      runAction("search.open");
      break;
    case "n":
      runAction("search.next");
      break;
    case "N":
      runAction("search.prev");
      break;
    default:
      break; // every other key is consumed and does nothing: none of them is the program's now
  }
  return true;
}

/**
 * Register `copy_mode.enter` and `copy_mode.exit`. `renderStatus` is called when the mode changes (the status line
 * shows it). Call once at start-up.
 */
export function initCopyMode(renderStatus: () => void): void {
  render = renderStatus;
  registerAction({ id: "copy_mode.enter", title: "Enter copy mode", run: enter });
  registerAction({ id: "copy_mode.exit", title: "Leave copy mode", run: () => end(true) });
  // Copy mode belongs to the focused pane: it ends when the focus moves on or the pane leaves the layout.
  onOverlayRender(() => {
    if (pane === null) return;
    const view = state.view;
    if (view && view.focus === pane) return;
    end(view?.panes.some(([id]) => id === pane) === true);
  });
}
