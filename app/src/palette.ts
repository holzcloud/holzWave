// The command palette (docs/design.md section 6 rule 7): Cmd+K opens a floating dialog with a text field and the
// registered actions below it, fuzzy-matched by title. It lists whatever the action registry holds when it opens, so
// an action another module registers shows up here without a change to this file; nothing is special-cased.
//
// It is fully keyboard-operable: typing narrows the list, Up and Down (and Page Up and Page Down) move the active
// row, Enter runs it, Escape closes. While it is open it takes every key (the key handler stack), so nothing reaches
// the terminal behind it. Closing gives the keyboard back to the terminal input before the chosen action runs, so an
// action that opens a field of its own (search, settings) keeps the focus it asks for.
//
// Every string is put in with `textContent`. The matching, the section of an id and the chord label are pure and
// node imports this file for the unit tests: the DOM and the keyboard are reached from `initPalette` only.
import { listActions, registerAction, runAction } from "./actions.js";
import type { Action } from "./actions.js";
import { chordFor } from "./chords.js";
import { el, loadStyles, setAttr, setText } from "./dom.js";
import { rank } from "./fuzzy.js";
import { chordsOf } from "./keymap.js";
import { pushKeyHandler } from "./keystack.js";

/** The id of the action that opens the palette (the core's table binds Cmd+K to it). */
export const PALETTE_ACTION = "palette.open";

/** The section of an action id: the part before the first dot, with underscores as spaces (`copy_mode.enter`: "copy mode"). */
export function sectionOf(id: string): string {
  const dot = id.indexOf(".");
  return (dot < 0 ? id : id.slice(0, dot)).replace(/_/g, " ");
}

/**
 * The chord to show next to an action, or "" for an action nothing is bound to. This is the one place the palette
 * reads chords from: the core's resolved table when it has sent one, else the static table.
 */
export function chordLabel(id: string): string {
  return chordFor(id) ?? "";
}

// ---- the page side ----------------------------------------------------------------------------------

/** The parts of one row, kept so a render updates the elements in place. */
interface Row {
  root: HTMLLIElement;
  title: HTMLElement;
  section: HTMLElement;
  chord: HTMLElement;
}

interface Ui {
  backdrop: HTMLElement;
  input: HTMLInputElement;
  list: HTMLUListElement;
  empty: HTMLElement;
  rows: Row[];
}

let ui: Ui | null = null;
/** The actions the list shows now, best match first. */
let shown: Action[] = [];
/** The index into `shown` of the active row; -1 when nothing matches. */
let active = -1;
let popKeys: (() => void) | null = null;
/** Hands the keyboard back to the terminal input; set by `initPalette`. */
let giveBack: () => void = () => {};

const rowId = (n: number): string => `palette-opt-${n}`;

/** Shortcuts that edit the text of the field: with the palette open they are the field's, not an action's. */
const FIELD_KEYS = new Set(["a", "c", "v", "x", "z"]);

function makeRow(list: HTMLUListElement, n: number): Row {
  const root = el("li", "palette-row", list);
  root.id = rowId(n);
  root.setAttribute("role", "option");
  root.setAttribute("aria-selected", "false");
  const title = el("span", "palette-title", root);
  const section = el("span", "palette-section label", root);
  const chord = el("kbd", "palette-chord", root);
  return { root, title, section, chord };
}

function build(): Ui {
  const backdrop = el("div", "palette-backdrop", document.body);
  backdrop.hidden = true;
  const box = el("div", "palette", backdrop);
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", "Command palette");

  const input = el("input", "palette-input", box);
  input.type = "text";
  input.setAttribute("role", "combobox");
  input.setAttribute("aria-label", "Command");
  input.setAttribute("aria-expanded", "true");
  input.setAttribute("aria-controls", "palette-list");
  input.setAttribute("aria-autocomplete", "list");
  input.setAttribute("autocomplete", "off");
  input.setAttribute("autocorrect", "off");
  input.setAttribute("autocapitalize", "off");
  input.spellcheck = false;
  input.placeholder = "Type a command";

  const list = el("ul", "palette-list", box);
  list.id = "palette-list";
  list.setAttribute("role", "listbox");
  list.setAttribute("aria-label", "Actions");

  const empty = el("p", "palette-empty", box);
  empty.setAttribute("role", "status");
  empty.hidden = true;

  // Typing is a new query; a composition in progress is not one yet.
  input.addEventListener("input", (e) => {
    if ((e as InputEvent).isComposing) return;
    render();
  });
  input.addEventListener("compositionend", () => render());

  // The pointer never takes the focus out of the field, so the keys stay the palette's.
  list.addEventListener("mousedown", (e) => e.preventDefault());
  list.addEventListener("click", (e) => {
    const at = rowIndexOf(e.target);
    if (at >= 0) choose(at);
  });
  let lastX = Number.NaN;
  let lastY = Number.NaN;
  list.addEventListener("pointermove", (e) => {
    // A list that scrolls under a resting pointer must not take the active row away from the keyboard.
    if (e.clientX === lastX && e.clientY === lastY) return;
    lastX = e.clientX;
    lastY = e.clientY;
    const at = rowIndexOf(e.target);
    if (at >= 0 && at !== active) setActive(at, false);
  });
  backdrop.addEventListener("click", (e) => {
    if (e.target === backdrop) closePalette();
  });

  return { backdrop, input, list, empty, rows: [] };
}

function rowIndexOf(target: EventTarget | null): number {
  if (!ui || !(target instanceof Element)) return -1;
  const row = target.closest(".palette-row");
  return row ? ui.rows.findIndex((r) => r.root === row) : -1;
}

/** Rank the registry against the field and show the result; the best match becomes the active row. */
function render(): void {
  if (!ui) return;
  const query = ui.input.value.trim();
  shown = rank(query, listActions(), (a) => a.title).map((r) => r.item);

  const rows = ui.rows;
  while (rows.length < shown.length) rows.push(makeRow(ui.list, rows.length));
  while (rows.length > shown.length) rows.pop()?.root.remove();
  shown.forEach((action, n) => {
    const row = rows[n] as Row;
    setText(row.title, action.title);
    setText(row.section, sectionOf(action.id));
    setText(row.chord, chordLabel(action.id));
    row.chord.hidden = row.chord.textContent === "";
  });

  const none = shown.length === 0;
  ui.empty.hidden = !none;
  setText(ui.empty, none ? `No action matches "${query}"` : "");
  ui.list.hidden = none;
  setActive(none ? -1 : 0, true);
  ui.list.scrollTop = 0;
}

/** Make row `at` the active one (-1: none) and keep it in view when `reveal` is set. */
function setActive(at: number, reveal: boolean): void {
  if (!ui) return;
  active = at;
  ui.rows.forEach((row, n) => setAttr(row.root, "aria-selected", n === at ? "true" : "false"));
  setAttr(ui.input, "aria-activedescendant", at >= 0 ? rowId(at) : null);
  const row = at >= 0 ? ui.rows[at] : undefined;
  if (reveal && row) {
    const top = row.root.offsetTop;
    const bottom = top + row.root.offsetHeight;
    const list = ui.list;
    if (at === 0) list.scrollTop = 0; // the list's own padding above the first row comes with it
    else if (at === ui.rows.length - 1) list.scrollTop = list.scrollHeight;
    else if (top < list.scrollTop) list.scrollTop = top;
    else if (bottom > list.scrollTop + list.clientHeight) list.scrollTop = bottom - list.clientHeight;
  }
}

/** Move the active row by `by` rows. A single step wraps around at the ends; a page stops at them. */
function move(by: number, wrap: boolean): void {
  if (shown.length === 0) return;
  let next = active + by;
  if (wrap) next = (next + shown.length) % shown.length;
  else next = Math.min(shown.length - 1, Math.max(0, next));
  setActive(next, true);
}

/** Rows that fit the list at once, at least one: the step of Page Up and Page Down. */
function pageSize(): number {
  const first = ui?.rows[0]?.root;
  if (!ui || !first || first.offsetHeight === 0) return 1;
  return Math.max(1, Math.floor(ui.list.clientHeight / first.offsetHeight) - 1);
}

/** Run row `at`: the palette closes first, so the action finds the page as a shortcut would. */
function choose(at: number): void {
  const action = shown[at];
  if (!action) return;
  closePalette();
  runAction(action.id);
}

function onKey(e: KeyboardEvent): boolean {
  if (!ui) return false;
  // The field has the keyboard back after a click elsewhere: the key is consumed, the next one lands in the field.
  if (document.activeElement !== ui.input) ui.input.focus({ preventScroll: true });

  const chord = chordFor(PALETTE_ACTION);
  if (chord !== undefined && chordsOf(e).includes(chord)) {
    e.preventDefault();
    closePalette();
    return true;
  }
  if (e.metaKey) {
    // Cmd+A, C, V, X and Z edit the field; every other Cmd chord is not the terminal's or a shortcut's while open.
    if (!FIELD_KEYS.has(e.key.toLowerCase())) e.preventDefault();
    return true;
  }
  switch (e.key) {
    case "Escape":
      e.preventDefault();
      closePalette();
      return true;
    case "Enter":
      e.preventDefault();
      if (active >= 0) choose(active);
      return true;
    case "ArrowDown":
      e.preventDefault();
      move(1, true);
      return true;
    case "ArrowUp":
      e.preventDefault();
      move(-1, true);
      return true;
    case "PageDown":
      e.preventDefault();
      move(pageSize(), false);
      return true;
    case "PageUp":
      e.preventDefault();
      move(-pageSize(), false);
      return true;
    case "Tab":
      e.preventDefault(); // one control: the focus stays in the field
      return true;
    default:
      return true; // typing: the field takes the character, nothing behind it sees the key
  }
}

/** Whether the palette is showing. */
export function isPaletteOpen(): boolean {
  return popKeys !== null;
}

/** Open the palette with an empty query and every action listed. Opening an open palette only refocuses it. */
export function openPalette(): void {
  ui ??= build();
  if (popKeys === null) {
    ui.input.value = "";
    ui.backdrop.hidden = false;
    render();
    // The backdrop is shown first and the class follows, so a transition (when motion is allowed) has a start.
    void ui.backdrop.offsetWidth;
    ui.backdrop.classList.add("open");
    popKeys = pushKeyHandler(onKey);
  }
  ui.input.focus({ preventScroll: true });
}

/** Close the palette and give the keyboard back to the terminal input. */
export function closePalette(): void {
  if (!ui || popKeys === null) return;
  popKeys();
  popKeys = null;
  ui.backdrop.classList.remove("open");
  ui.backdrop.hidden = true;
  shown = [];
  active = -1;
  giveBack();
}

/** Register the action, the stylesheet and the way back to the terminal input. Call once at start-up. */
export async function initPalette(): Promise<void> {
  const { focusInput } = await import("./keyboard.js");
  giveBack = focusInput;
  registerAction({ id: PALETTE_ACTION, title: "Open command palette", run: openPalette });
  try {
    await loadStyles("./palette.css");
  } catch {
    // the palette still works without its stylesheet
  }
}
