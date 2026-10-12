// The action registry: everything the user can trigger is an action with a stable id ("pane.split_right").
// A shortcut resolves its chord to an id (chords.ts) and runs it here; the command palette of a later plan lists
// the same registry. Nothing in this module touches the DOM, so node can import it.

export interface Action {
  /** The stable id shared with the core's key binding table: `pane.split_right`. */
  id: string;
  /** A short English label for the command palette. */
  title: string;
  run: () => void;
  /**
   * The key press that ran the action keeps its default: the page does not `preventDefault` it. `edit.paste` uses it so
   * that the browser fires its `paste` event into the terminal input.
   */
  keepDefault?: boolean;
}

// A Map keeps the registration order and has no inherited keys ("constructor" is just an unknown id).
const actions = new Map<string, Action>();

/** Add an action. A second registration of the same id throws: two owners of one id is a bug, not an override. */
export function registerAction(action: Action): void {
  if (actions.has(action.id)) throw new Error(`action "${action.id}" is already registered`);
  actions.set(action.id, { ...action });
}

/** Take over an id: the new behaviour keeps the old position in the list; an unknown id is registered. */
export function replaceAction(action: Action): void {
  actions.set(action.id, { ...action });
}

/** Remove an action; true if there was one. */
export function unregisterAction(id: string): boolean {
  return actions.delete(id);
}

/**
 * The actions that run on every auto-repeat of a held shortcut. Everything else runs once per press (WR-01).
 *
 * These are stepping actions (the four focus and font steps and the six line-editing steps): each step is cheap, creates and destroys nothing, and is bounded (focus wraps
 * over at most 64 panes, the core clamps the font size, and the settings calls are queued in order). A person
 * holding the key wants them to keep going. Create and destroy actions (split, close, new tab, new workspace),
 * toggles and openers (palette, search, copy mode, settings) and jumps (tab and workspace select, font reset, copy and
 * paste: a held paste chord pastes once) must not: a held Cmd+W would close pane after pane and end the app with all its shells, a held Cmd+D or Cmd+T
 * would start shells at the key repeat rate. The list is an allowlist and not a denylist on purpose: a forgotten
 * entry makes a stepping key that does not repeat (annoying, harmless), a forgotten denylist entry makes a
 * destructive action run at 30 Hz. An action added later is safe by default and runs once per press until it is
 * put here on purpose.
 */
const REPEATABLE: ReadonlySet<string> = new Set([
  "pane.focus_next",
  "pane.focus_prev",
  "font.increase",
  "font.decrease",
  // Line editing: a step moves or deletes one word or line, so a held key keeps going like it does in any shell.
  "edit.word_back",
  "edit.word_forward",
  "edit.line_start",
  "edit.line_end",
  "edit.delete_word_back",
  "edit.delete_to_line_start",
]);

/** Whether the action under `id` runs on each auto-repeat of its shortcut. Unknown ids are not. */
export function isRepeatable(id: string): boolean {
  return REPEATABLE.has(id);
}

/** Whether the key press that runs the action under `id` keeps its browser default. Unknown ids do not. */
export function keepsDefault(id: string): boolean {
  return actions.get(id)?.keepDefault === true;
}

/** How a run was triggered. `repeat` is the `repeat` flag of the key event that caused it. */
export interface RunOptions {
  repeat?: boolean;
}

/**
 * Run the action under `id`. An unknown id does nothing and returns false. A repeat of a key whose action is not
 * allowed to repeat is consumed: the call returns true (the key was handled) and the action does not run.
 */
export function runAction(id: string, options: RunOptions = {}): boolean {
  const action = actions.get(id);
  if (!action) return false;
  if (options.repeat === true && !isRepeatable(id)) return true;
  action.run();
  return true;
}

/** Every registered action in registration order; the array is a copy. */
export function listActions(): Action[] {
  return [...actions.values()];
}
