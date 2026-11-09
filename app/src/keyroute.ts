// Who a key belongs to when it is not a shortcut: the terminal or the control that has the focus. Pure facts about a key,
// no DOM, so node can test the rules. keyboard.ts applies them to the target of a `keydown`.

/** The facts of a key event that the rules read. */
export interface KeyFacts {
  key: string;
  ctrlKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}

/** The keys a focused control uses itself: they move between controls, press a button or move inside a widget. */
const CONTROL_KEYS: ReadonlySet<string> = new Set(["Tab", "Enter", " ", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"]);

/**
 * Whether the key is one that a control of the page keeps for itself while it has the focus: `Tab` and `Shift+Tab`, `Enter`,
 * `Space` and the four arrows, with no Ctrl, Alt or Cmd (those are chords, and the shortcuts and the terminal see them first).
 * In the terminal's own input none of them is ever the page's: `Tab` is the terminal's `Tab` there.
 */
export function isControlKey(e: KeyFacts): boolean {
  return CONTROL_KEYS.has(e.key) && !e.ctrlKey && !e.altKey && !e.metaKey;
}

/**
 * The elements that are controls of the page when they have the focus: buttons, links, disclosure summaries, anything with a role
 * (a tab, a menu item, a switch) and anything that was made focusable with a `tabindex` (a scrollable box that the arrows scroll).
 * The terminal's canvas has a `tabindex` and is excluded by the caller: it hands the focus to the terminal input.
 */
export const CONTROL_SELECTOR = "button, a[href], summary, [role], [tabindex]";

/** A chord that is a function key, alone or with Shift: `F6`, `Shift+F6`, `F12`. */
export function isFunctionKeyChord(chord: string): boolean {
  return /^(Shift\+)?F([1-9]|1\d|2[0-4])$/.test(chord);
}
