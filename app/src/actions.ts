// The action registry: everything the user can trigger is an action with a stable id ("pane.split_right").
// A shortcut resolves its chord to an id (chords.ts) and runs it here; the command palette of a later plan lists
// the same registry. Nothing in this module touches the DOM, so node can import it.

export interface Action {
  /** The stable id shared with the core's key binding table: `pane.split_right`. */
  id: string;
  /** A short English label for the command palette. */
  title: string;
  run: () => void;
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

/** Run the action under `id`. An unknown id does nothing and returns false. */
export function runAction(id: string): boolean {
  const action = actions.get(id);
  if (!action) return false;
  action.run();
  return true;
}

/** Every registered action in registration order; the array is a copy. */
export function listActions(): Action[] {
  return [...actions.values()];
}
