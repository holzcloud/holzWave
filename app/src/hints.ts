// Hint mode: every URL, path and hash in the focused pane gets a short label drawn over its first cells. Typing a
// label copies the target's text, Shift plus the label opens it (a URL) or reveals it (a path), Escape or any other
// key leaves. While the mode is active it takes every key (the key handler stack): nothing reaches the program.
//
// The labels and the key reading are pure, so node imports this file for the unit tests; the DOM, the core and the
// overlay are only reached from `initHints`, by dynamic import. The targets are found once, when the mode opens;
// a target whose cells have changed since is not drawn and not offered.
import { registerAction } from "./actions.js";
import { rowCells } from "./frames.js";
import { isModifierKey } from "./keymap.js";
import { pushKeyHandler } from "./keystack.js";
import { findTargets, linkAction } from "./links.js";
import type { Target } from "./links.js";
import { state } from "./state.js";

export const HINT_ALPHABET = "abcdefghijklmnopqrstuvwxyz";
/** The most targets that are labelled: every one-letter and two-letter label of the alphabet. */
export const MAX_HINTS = HINT_ALPHABET.length * HINT_ALPHABET.length;

export type HintMatch = { state: "none" } | { state: "partial" } | { state: "hit"; index: number };

/**
 * `n` unique, prefix-free labels, in the order the targets are read. Up to the alphabet length each label is one
 * letter. Beyond it the last letters of the alphabet become the first letter of two-letter labels, just as many as
 * the count needs, so no label is the start of another and the first targets keep their single letter. More than
 * `MAX_HINTS` targets get `MAX_HINTS` labels; a count that is not a number gets none.
 */
export function hintLabels(n: number): string[] {
  if (Number.isNaN(n)) return [];
  const count = Math.max(0, Math.min(MAX_HINTS, Math.floor(n)));
  const letters = [...HINT_ALPHABET];
  if (count <= letters.length) return letters.slice(0, count);
  const prefixes = Math.ceil((count - letters.length) / (letters.length - 1));
  const labels = letters.slice(0, letters.length - prefixes);
  for (const first of letters.slice(letters.length - prefixes)) {
    for (const second of letters) {
      if (labels.length >= count) return labels;
      labels.push(first + second);
    }
  }
  return labels;
}

/** What `typed` means for `labels`: a whole label, the start of one or nothing. */
export function resolveHint(labels: readonly string[], typed: string): HintMatch {
  const index = labels.indexOf(typed);
  if (typed !== "" && index >= 0) return { state: "hit", index };
  return labels.some((l) => l.startsWith(typed)) ? { state: "partial" } : { state: "none" };
}

/** The label letter of a key (lower case; Shift only asks to open), or null for any other key. */
export function hintLetter(e: { key: string; ctrlKey?: boolean; metaKey?: boolean; altKey?: boolean }): string | null {
  if (e.ctrlKey === true || e.metaKey === true || e.altKey === true) return null;
  return /^[a-zA-Z]$/.test(e.key) ? e.key.toLowerCase() : null;
}

// ---- the page side ----------------------------------------------------------------------------------

interface Item {
  y: number;
  target: Target;
  /** The cells of the target when the mode opened, to tell whether they are still there. */
  raw: string;
}

/**
 * Register `hints.open`. Detection runs for the visible rows of the focused pane when the mode opens, not per
 * frame. Call once at start-up.
 */
export async function initHints(): Promise<void> {
  const [{ call }, { onOverlayRender, renderOverlay, setCellBoxes }, { writeClipboard }, { focusInput }] = await Promise.all([
    import("./bridge.js"),
    import("./overlay.js"),
    import("./selection.js"),
    import("./keyboard.js"),
  ]);

  let mode: { pane: number; items: Item[]; labels: string[]; typed: string; open: boolean } | null = null;
  let popKeys: (() => void) | null = null;

  const cellsOf = (pane: number, y: number, t: Target): string => {
    const mirror = state.frames.get(pane);
    return mirror ? rowCells(mirror, y).slice(t.startCol, t.endCol + 1).join("") : "";
  };

  const end = (): void => {
    if (!mode) return;
    mode = null;
    popKeys?.();
    popKeys = null;
    setCellBoxes("hints", []);
    focusInput();
  };

  /** Draw the labels that continue what was typed and whose cells are still there; leave when the pane lost the focus. */
  const draw = (): void => {
    const m = mode;
    if (!m) return;
    if (state.view?.focus !== m.pane) {
      end();
      return;
    }
    const boxes = [];
    for (let i = 0; i < m.items.length; i++) {
      const item = m.items[i] as Item;
      const label = m.labels[i];
      if (label === undefined || !label.startsWith(m.typed)) continue;
      if (cellsOf(m.pane, item.y, item.target) !== item.raw) continue;
      boxes.push({
        key: String(i), cls: "hint", pane: m.pane, y: item.y, x0: item.target.startCol,
        x1: Math.min(item.target.endCol, item.target.startCol + label.length - 1), text: label, hint: label,
      });
    }
    setCellBoxes("hints", boxes);
  };

  const onKey = (e: KeyboardEvent): boolean => {
    const m = mode;
    if (!m) return false;
    // Shift (or another modifier) on its own is the start of Shift+letter, not an unknown key.
    if (isModifierKey(e)) return true;
    const letter = hintLetter(e);
    if (e.key === "Escape" || letter === null) {
      end();
      return true;
    }
    m.typed += letter;
    if (e.shiftKey) m.open = true;
    const r = resolveHint(m.labels, m.typed);
    if (r.state === "none") {
      end();
    } else if (r.state === "partial") {
      renderOverlay();
    } else {
      const item = m.items[r.index];
      const { pane, open } = m;
      end();
      if (item) {
        const action = open && item.target.kind !== "hash" ? linkAction(item.target, state.cwds.get(pane) ?? null) : null;
        if (action) void call(action.command, action.args).catch(() => {});
        else writeClipboard(item.target.text);
      }
    }
    return true;
  };

  onOverlayRender(draw);

  const start = (): void => {
    if (mode) {
      end();
      return;
    }
    const pane = state.view?.focus;
    const mirror = pane === undefined ? undefined : state.frames.get(pane);
    if (pane === undefined || !mirror) return;
    const items: Item[] = [];
    for (let y = 0; y < mirror.rows && items.length < MAX_HINTS; y++) {
      const cells = rowCells(mirror, y);
      if (cells.length === 0) continue;
      for (const target of findTargets(cells)) {
        if (items.length >= MAX_HINTS) break;
        items.push({ y, target, raw: cells.slice(target.startCol, target.endCol + 1).join("") });
      }
    }
    if (items.length === 0) return;
    mode = { pane, items, labels: hintLabels(items.length), typed: "", open: false };
    popKeys = pushKeyHandler(onKey);
    renderOverlay();
  };

  registerAction({ id: "hints.open", title: "Show hints for links and paths", run: start });
}
