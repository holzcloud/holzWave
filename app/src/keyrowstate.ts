// The pure side of the key row on a phone or a tablet (docs/design.md section 6.6): the sticky `ctrl` and `alt`, what a key of
// the row sends, which key a soft keyboard's character stands for, whether a hardware keyboard is attached, and what the
// events of an on-screen keyboard mean. No DOM, no command bridge and no clock: the time of a tap is an argument, so the
// 350 ms boundary is exact in a test. The browser side (keyrow.ts, and the few lines in keyboard.ts) only moves events in and out.
import { type KeyLike, type KeyPayload, keyPayload } from "./keymap.js";
import type { Layout } from "./layoutclass.js";

// ---- the modifier bits -----------------------------------------------------------------------------

/** The bits of the `key` command, read from keymap.ts instead of copied: the one place that numbers them is `keyPayload`. */
const bitOf = (modifier: Partial<KeyLike>): number => keyPayload({ code: "", key: "", ...modifier }).mods;
const SHIFT = bitOf({ shiftKey: true });
const ALT = bitOf({ altKey: true });
const CTRL = bitOf({ ctrlKey: true });

// ---- sticky modifiers ------------------------------------------------------------------------------

/** A second tap on an armed modifier locks it only within this many milliseconds of the first (the design says 350). */
export const LOCK_WINDOW_MS = 350;

/** One U+200B kept in the terminal input on a touch layout, so that a soft keyboard has something to delete (Backspace). */
export const SENTINEL = "​";

/** `idle` is off; `armed` applies to the next key only; `locked` stays on until it is tapped. */
export type StickyMode = "idle" | "armed" | "locked";

export type Modifier = "ctrl" | "alt";

/** One modifier: its mode and the time of the tap that armed it (the clock of the 350 ms window). */
export interface Sticky {
  readonly mode: StickyMode;
  readonly at: number;
}

export interface StickyState {
  readonly ctrl: Sticky;
  readonly alt: Sticky;
}

const IDLE: Sticky = { mode: "idle", at: Number.NEGATIVE_INFINITY };

/** Both modifiers off. */
export function idleSticky(): StickyState {
  return { ctrl: IDLE, alt: IDLE };
}

/**
 * A tap on `which` at time `now` (milliseconds, any clock the caller keeps): idle arms it; a second tap within the window
 * locks it; a second tap after the window cancels the arm; a tap on a locked modifier releases it.
 */
export function tap(state: StickyState, which: Modifier, now: number): StickyState {
  const current = state[which];
  let next: Sticky;
  if (current.mode === "idle") next = { mode: "armed", at: now };
  else if (current.mode === "armed") next = now - current.at <= LOCK_WINDOW_MS ? { mode: "locked", at: now } : IDLE;
  else next = IDLE;
  return { ...state, [which]: next };
}

/** A key was sent: every armed modifier is spent, a locked one stays. */
export function consume(state: StickyState): StickyState {
  const spend = (s: Sticky): Sticky => (s.mode === "armed" ? IDLE : s);
  const ctrl = spend(state.ctrl);
  const alt = spend(state.alt);
  return ctrl === state.ctrl && alt === state.alt ? state : { ctrl, alt };
}

/** Some modifier is armed or locked, so the next key carries it. */
export function anySticky(state: StickyState): boolean {
  return state.ctrl.mode !== "idle" || state.alt.mode !== "idle";
}

/** The modifier bits of a key: its own, plus those of every armed or locked modifier. */
export function modsFor(state: StickyState, base: number): number {
  return base | (state.ctrl.mode !== "idle" ? CTRL : 0) | (state.alt.mode !== "idle" ? ALT : 0);
}

// ---- the keys the row and a soft keyboard send -------------------------------------------------------

/** A key as the `key` command takes it: the physical key, what it typed and its own modifier bits. */
export interface RowKey {
  readonly code: string;
  readonly key: string;
  readonly mods: number;
}

/** The four plain keys of the row. `ctrl` and `alt` are not keys of their own: they are the modifiers of the next one. */
export type PlainName = "esc" | "tab" | "up" | "down";

const PLAIN: Readonly<Record<PlainName, RowKey>> = {
  esc: { code: "Escape", key: "Escape", mods: 0 },
  tab: { code: "Tab", key: "Tab", mods: 0 },
  up: { code: "ArrowUp", key: "ArrowUp", mods: 0 },
  down: { code: "ArrowDown", key: "ArrowDown", mods: 0 },
};

/** The key a plain key of the row sends. */
export function plainKey(name: PlainName): RowKey {
  return PLAIN[name];
}

/** The press and the release of `k`, in the shape `keyPayload` makes for a real key event. */
export function keyEvents(k: RowKey): { press: KeyPayload; release: KeyPayload } {
  const e: KeyLike = {
    code: k.code,
    key: k.key,
    shiftKey: (k.mods & SHIFT) !== 0,
    altKey: (k.mods & ALT) !== 0,
    ctrlKey: (k.mods & CTRL) !== 0,
  };
  return { press: keyPayload(e, false), release: keyPayload(e, true) };
}

/** The punctuation of a US layout: the character, the physical key, and whether Shift types it. */
const PUNCTUATION: ReadonlyMap<string, { code: string; shift: boolean }> = new Map([
  [" ", { code: "Space", shift: false }],
  ["[", { code: "BracketLeft", shift: false }],
  ["]", { code: "BracketRight", shift: false }],
  ["\\", { code: "Backslash", shift: false }],
  ["/", { code: "Slash", shift: false }],
  ["-", { code: "Minus", shift: false }],
  ["=", { code: "Equal", shift: false }],
  [",", { code: "Comma", shift: false }],
  [".", { code: "Period", shift: false }],
  [";", { code: "Semicolon", shift: false }],
  ["'", { code: "Quote", shift: false }],
  ["`", { code: "Backquote", shift: false }],
  ["{", { code: "BracketLeft", shift: true }],
  ["}", { code: "BracketRight", shift: true }],
  ["|", { code: "Backslash", shift: true }],
  ["?", { code: "Slash", shift: true }],
  ["_", { code: "Minus", shift: true }],
  ["+", { code: "Equal", shift: true }],
  ["<", { code: "Comma", shift: true }],
  [">", { code: "Period", shift: true }],
  [":", { code: "Semicolon", shift: true }],
  ['"', { code: "Quote", shift: true }],
  ["~", { code: "Backquote", shift: true }],
  ["!", { code: "Digit1", shift: true }],
  ["@", { code: "Digit2", shift: true }],
  ["#", { code: "Digit3", shift: true }],
  ["$", { code: "Digit4", shift: true }],
  ["%", { code: "Digit5", shift: true }],
  ["^", { code: "Digit6", shift: true }],
  ["&", { code: "Digit7", shift: true }],
  ["*", { code: "Digit8", shift: true }],
  ["(", { code: "Digit9", shift: true }],
  [")", { code: "Digit0", shift: true }],
]);

/**
 * The key a character stands for on a US layout, or null: a string of more than one character, and a character with no
 * key there (`é`, `€`, an emoji, a control character) are text, not a key. A letter in upper case is the same key with Shift.
 */
export function keyForChar(text: string): RowKey | null {
  if ([...text].length !== 1) return null;
  if (/^[a-z]$/.test(text)) return { code: `Key${text.toUpperCase()}`, key: text, mods: 0 };
  if (/^[A-Z]$/.test(text)) return { code: `Key${text}`, key: text, mods: SHIFT };
  if (/^[0-9]$/.test(text)) return { code: `Digit${text}`, key: text, mods: 0 };
  const p = PUNCTUATION.get(text);
  return p ? { code: p.code, key: text, mods: p.shift ? SHIFT : 0 } : null;
}

// ---- the soft keyboard's own events -------------------------------------------------------------------

/**
 * The key that a `beforeinput` of a soft keyboard stands for: Backspace, Enter, and the word kill of a shell (Ctrl+W) for a
 * deleted word. Everything else is text that arrives through `input`, or an edit the terminal has no use for.
 */
export function keyForInputType(inputType: string): RowKey | null {
  switch (inputType) {
    case "deleteContentBackward":
      return { code: "Backspace", key: "Backspace", mods: 0 };
    case "insertLineBreak":
    case "insertParagraph":
      return { code: "Enter", key: "Enter", mods: 0 };
    case "deleteWordBackward":
      return { code: "KeyW", key: "w", mods: CTRL };
    default:
      return null;
  }
}

/** The text with the sentinel characters taken out, and nothing else changed. */
export function stripSentinel(value: string): string {
  return value.split(SENTINEL).join("");
}

// ---- where the keys come from -------------------------------------------------------------------------

/** What the detector reads. A plain object with these fields is enough. */
export type SourceEvent =
  | { type: "keydown"; key: string; code: string; keyCode?: number }
  | { type: "compositionstart" }
  | { type: "beforeinput"; isComposing: boolean };

/** Keys that an on-screen keyboard does not have, so one of them is a hardware keyboard even though its name is long. */
const HARDWARE_ONLY = /^(Escape|Tab|Arrow(Up|Down|Left|Right)|Home|End|PageUp|PageDown|Insert|Delete|F([1-9]|1\d|2[0-4]))$/;

/**
 * Whether the keys now come from a hardware keyboard, after `event`, given whether they did before. A real key event (one
 * character with a code, or a key an on-screen keyboard lacks) means hardware; the keys of an input method (key code 229,
 * `Unidentified`, a composition) mean the soft keyboard. The last source decides and every other event changes nothing.
 * It starts false: a phone has no hardware keyboard until one types.
 */
export function sourceAfter(hardware: boolean, event: SourceEvent): boolean {
  if (event.type === "compositionstart") return false;
  if (event.type === "beforeinput") return event.isComposing ? false : hardware;
  if (event.keyCode === 229 || event.key === "Unidentified" || event.key === "Process") return false;
  if (event.code === "") return hardware;
  if ([...event.key].length === 1 || HARDWARE_ONLY.test(event.key)) return true;
  return hardware;
}

/** The row is drawn on a touch layout while the terminal input has the focus and no hardware keyboard is the source. */
export function rowVisible(facts: { layout: Layout; imeFocused: boolean; hardware: boolean }): boolean {
  return (facts.layout === "phone" || facts.layout === "tablet") && facts.imeFocused && !facts.hardware;
}
