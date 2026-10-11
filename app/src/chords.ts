// The static table from a canonical chord to an action id. Canonical form is the core's (core/src/keybindings.rs):
// modifiers in the order Cmd, Ctrl, Alt, Shift, then the key, letters upper case, "+" as the separator.
// These are the macOS defaults; a chord whose action nobody registered resolves and then does nothing.
// `pane.close` is Cmd+W (D-14): the native macOS menu has no Close Window item, so the page owns the shortcut.

export const CHORDS: ReadonlyMap<string, string> = new Map([
  ["Cmd+D", "pane.split_right"],
  ["Cmd+Shift+D", "pane.split_down"],
  ["Cmd+W", "pane.close"],
  ["Cmd+]", "pane.focus_next"],
  ["Cmd+[", "pane.focus_prev"],
  ["Cmd+T", "tab.new"],
  ["Cmd+1", "tab.select_1"],
  ["Cmd+2", "tab.select_2"],
  ["Cmd+3", "tab.select_3"],
  ["Cmd+4", "tab.select_4"],
  ["Cmd+5", "tab.select_5"],
  ["Cmd+6", "tab.select_6"],
  ["Cmd+7", "tab.select_7"],
  ["Cmd+8", "tab.select_8"],
  ["Cmd+9", "tab.select_9"],
  ["Cmd+Shift+N", "workspace.new"],
  ["Cmd+Alt+1", "workspace.select_1"],
  ["Cmd+Alt+2", "workspace.select_2"],
  ["Cmd+Alt+3", "workspace.select_3"],
  ["Cmd+Alt+4", "workspace.select_4"],
  ["Cmd+Alt+5", "workspace.select_5"],
  ["Cmd+Alt+6", "workspace.select_6"],
  ["Cmd+Alt+7", "workspace.select_7"],
  ["Cmd+Alt+8", "workspace.select_8"],
  ["Cmd+Alt+9", "workspace.select_9"],
  ["Cmd+C", "edit.copy"],
  ["Cmd+K", "palette.open"],
  ["Cmd+F", "search.open"],
  ["Cmd+Shift+X", "copy_mode.enter"],
  ["Cmd+Shift+Space", "hints.open"],
  ["Cmd+=", "font.increase"],
  ["Cmd+-", "font.decrease"],
  ["Cmd+0", "font.reset"],
  ["Cmd+,", "settings.open"],
]);

/** The action id of a canonical chord, or undefined. */
export function actionIdFor(chord: string): string | undefined {
  return CHORDS.get(chord);
}

// ---- the core's resolved table (neutral placeholder until the next commit) --------------------------

/** One row of the core's binding table, as far as dispatch needs it. */
export interface BindingRowLike {
  action: string;
  chord: string | null;
}

/** Two rows that carry one chord: the first keeps it, the later one is reported and not indexed. */
export interface BindingConflict {
  chord: string;
  kept: string;
  dropped: string;
}

export function bindingIndex(_rows: readonly BindingRowLike[]): {
  index: Map<string, string>;
  conflicts: BindingConflict[];
} {
  return { index: new Map(), conflicts: [] };
}

export function setBindingTable(_rows: readonly BindingRowLike[]): BindingConflict[] {
  return [];
}
