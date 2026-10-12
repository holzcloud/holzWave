// The pure side of keyboard input: what the page tells the core about a key event, and which canonical chords an
// event can stand for. The page never builds an escape sequence: it sends facts (code, key, modifier bits, repeat,
// release, composing) and the core's encoder decides what they mean. No DOM and no state, so node can import it.

/** The fields of a `KeyboardEvent` this module reads; a plain object with these fields is enough. */
export interface KeyLike {
  code: string;
  key: string;
  shiftKey?: boolean;
  ctrlKey?: boolean;
  altKey?: boolean;
  metaKey?: boolean;
  repeat?: boolean;
  isComposing?: boolean;
  keyCode?: number;
  /** The browser's modifier state by name; only `"AltGraph"` is asked. Absent on a plain object. */
  getModifierState?(key: string): boolean;
}

/** What the page tells the core about one key event (the `key` command without the pane). */
export interface KeyPayload {
  code: string;
  key: string;
  /** SHIFT 1, ALT 2, CTRL 4, SUPER 8. */
  mods: number;
  repeat: boolean;
  release: boolean;
  composing: boolean;
}

const SHIFT = 1;
const ALT = 2;
const CTRL = 4;
const SUPER = 8;

/**
 * AltGr is held. A layout with a third level (Swiss, German) types `@ \ | { } [ ] ~ €` with it, and Windows
 * reports it as Ctrl and Alt together; the browser says `AltGraph` for it. That Ctrl and Alt belong to the layout, not
 * to the user, so every reader below ignores them while AltGr is held. A real Ctrl+Alt chord has no AltGr and stays one.
 */
function altGraph(e: KeyLike): boolean {
  return typeof e.getModifierState === "function" && e.getModifierState("AltGraph") === true;
}

/** The modifiers the user holds: Ctrl and Alt are not among them while AltGr is. */
function held(e: KeyLike): { shift: boolean; ctrl: boolean; alt: boolean; meta: boolean; altGr: boolean } {
  const altGr = altGraph(e);
  return {
    shift: e.shiftKey === true,
    ctrl: e.ctrlKey === true && !altGr,
    alt: e.altKey === true && !altGr,
    meta: e.metaKey === true,
    altGr,
  };
}

/** The modifier bit set of an event, in the numbering of the `key` command. */
function modsOf(e: KeyLike): number {
  const m = held(e);
  return (m.shift ? SHIFT : 0) | (m.alt ? ALT : 0) | (m.ctrl ? CTRL : 0) | (m.meta ? SUPER : 0);
}

/**
 * The key event as the core's `key` command takes it. `release` is the caller's: a `keyup` is the same facts with
 * the flag set. `key` is passed through untouched, so an emoji (one code point, two UTF-16 units) arrives whole.
 */
export function keyPayload(e: KeyLike, release = false): KeyPayload {
  return {
    code: e.code,
    key: e.key,
    mods: modsOf(e),
    repeat: e.repeat === true,
    release,
    composing: isComposingKey(e),
  };
}

/**
 * The key belongs to an input method or a dead-key sequence, not to the terminal: the browser reports
 * `isComposing`, the legacy key code 229, or a key named `Process` or `Dead`.
 */
export function isComposingKey(e: KeyLike): boolean {
  return e.isComposing === true || e.keyCode === 229 || e.key === "Process" || e.key === "Dead";
}

const MODIFIER_NAMES: ReadonlySet<string> = new Set([
  "Shift", "Control", "Alt", "Meta", "AltGraph", "CapsLock", "Fn", "FnLock", "NumLock", "ScrollLock", "Hyper",
  "Super", "Symbol", "SymbolLock", "OS",
]);

/** A modifier or lock key on its own. It is not forwarded: the encoder reads the modifier bits of the next key. */
export function isModifierKey(e: KeyLike): boolean {
  return MODIFIER_NAMES.has(e.key);
}

// ---- chords --------------------------------------------------------------------------------------

/** `KeyboardEvent.key` values that have a name in the chord grammar (core/src/keybindings.rs). */
const GRAMMAR_NAMES: ReadonlyMap<string, string> = new Map([
  [" ", "Space"],
  ["Enter", "Enter"],
  ["Tab", "Tab"],
  ["Escape", "Escape"],
  ["Backspace", "Backspace"],
  ["Delete", "Delete"],
  ["Insert", "Insert"],
  ["ArrowUp", "Up"],
  ["ArrowDown", "Down"],
  ["ArrowLeft", "Left"],
  ["ArrowRight", "Right"],
  ["Home", "Home"],
  ["End", "End"],
  ["PageUp", "PageUp"],
  ["PageDown", "PageDown"],
]);

/** `KeyboardEvent.code` values of keys that are not a letter or a digit, by the chord key they stand for. */
const PHYSICAL_NAMES: ReadonlyMap<string, string> = new Map([
  ["BracketLeft", "["],
  ["BracketRight", "]"],
  ["Comma", ","],
  ["Period", "."],
  ["Slash", "/"],
  ["Backslash", "\\"],
  ["Semicolon", ";"],
  ["Quote", "'"],
  ["Backquote", "`"],
  ["Minus", "-"],
  ["Equal", "="],
  ["Space", "Space"],
  ["Enter", "Enter"],
  ["NumpadEnter", "Enter"],
  ["Tab", "Tab"],
  ["Escape", "Escape"],
  ["Backspace", "Backspace"],
  ["Delete", "Delete"],
  ["Insert", "Insert"],
  ["ArrowUp", "Up"],
  ["ArrowDown", "Down"],
  ["ArrowLeft", "Left"],
  ["ArrowRight", "Right"],
  ["Home", "Home"],
  ["End", "End"],
  ["PageUp", "PageUp"],
  ["PageDown", "PageDown"],
]);

const FUNCTION_KEY = /^F([1-9]|1\d|2[0-4])$/;

/** The chord key the typed `key` names, or null when it names none (a modifier, a dead key, a long name). */
function logicalKey(key: string): string | null {
  const named = GRAMMAR_NAMES.get(key);
  if (named !== undefined) return named;
  if (FUNCTION_KEY.test(key)) return key;
  const points = [...key];
  if (points.length !== 1) return null;
  const c = points[0] as string;
  if (c === "+") return null; // the separator is not a key of the grammar: Shift+= is how a layout types it
  if (/[\p{Cc}\p{Z}\p{Cf}\p{M}]/u.test(c)) return null;
  const upper = c.toUpperCase();
  return [...upper].length === 1 ? upper : c;
}

/** The chord key of the physical key, from `KeyboardEvent.code`; null for a key the grammar has no name for. */
function physicalKey(code: string): string | null {
  const letter = /^Key([A-Z])$/.exec(code);
  if (letter) return letter[1] as string;
  const digit = /^Digit(\d)$/.exec(code);
  if (digit) return digit[1] as string;
  if (FUNCTION_KEY.test(code)) return code;
  return PHYSICAL_NAMES.get(code) ?? null;
}

/** The modifier part of a canonical chord: `Cmd`, `Ctrl`, `Alt`, `Shift` in that order. */
function chordPrefix(m: { shift: boolean; ctrl: boolean; alt: boolean; meta: boolean }): string {
  return (m.meta ? "Cmd+" : "") + (m.ctrl ? "Ctrl+" : "") + (m.alt ? "Alt+" : "") + (m.shift ? "Shift+" : "");
}

/**
 * The canonical chords an event can stand for, most likely first: `Cmd`, `Ctrl`, `Alt`, `Shift` in that order and
 * then the key, letters in upper case. A binding matches by the key the layout typed or by the physical key, so
 * `Cmd+[` works on a layout that types something else there (Swiss) and `Cmd+Alt+1` works although Option changes
 * the typed character. With Option held the typed character is a composed one, so the physical key comes first.
 * With AltGr held Ctrl and Alt are not part of the event and the typed character stays first. A modifier on its own
 * has no chord.
 */
export function chordsOf(e: KeyLike): string[] {
  const logical = logicalKey(e.key);
  const physical = physicalKey(e.code);
  const m = held(e);
  const names = m.alt ? [physical, logical] : [logical, physical];
  const prefix = chordPrefix(m);
  const out: string[] = [];
  for (const name of names) {
    if (name === null) continue;
    const chord = prefix + name;
    if (!out.includes(chord)) out.push(chord);
  }
  return out;
}

/**
 * The one chord a key press records in the binding editor, in the core's canonical spelling. It is the candidate of
 * `chordsOf` that is least layout-dependent: the typed key for an ASCII letter or digit (a binding is written with
 * the letter that is on the key cap), the physical key for everything else (punctuation, named keys, a character
 * that Option or a national layout composed). A modifier on its own, a dead key with no physical name and a key
 * the grammar has no name for give null, so the editor keeps waiting. What it returns is always one of the chords
 * `chordsOf` offers for the same event, so a recorded binding matches the press that recorded it.
 */
export function primaryChord(e: KeyLike): string | null {
  const key = typeof e.key === "string" ? e.key : "";
  const m = held(e);
  // With AltGr held the typed character is the layout's own, so it is the name and the physical key only a fallback.
  const name = /^[A-Za-z0-9]$/.test(key)
    ? key.toUpperCase()
    : m.altGr
      ? (logicalKey(key) ?? physicalKey(e.code))
      : (physicalKey(e.code) ?? logicalKey(key));
  if (name === null) return null;
  return chordPrefix(m) + name;
}
