// The static table from a canonical chord to an action id. Canonical form is the core's (core/src/keybindings.rs):
// modifiers in the order Cmd, Ctrl, Alt, Shift, then the key, letters upper case, "+" as the separator.
// `CHORDS` is the macOS table; a chord whose action nobody registered resolves and then does nothing.
// `pane.close` is Cmd+W (D-14): the native macOS menu has no Close Window item, so the page owns the shortcut.
// It is only the fallback until the core's rows arrive. On Windows and Linux it holds just the fixed clipboard chords
// (see FIXED_CLIPBOARD): the Super key (`Cmd`) is the OS's there, so a Win-key chord must never run from this table.

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
  ["Cmd+Shift+H", "history.open"],
  ["Cmd+=", "font.increase"],
  ["Cmd+-", "font.decrease"],
  ["Cmd+0", "font.reset"],
  ["Cmd+,", "settings.open"],
  // Line editing, with the macOS standard: Option by word, Cmd by line.
  ["Alt+Left", "edit.word_back"],
  ["Alt+Right", "edit.word_forward"],
  ["Cmd+Left", "edit.line_start"],
  ["Cmd+Right", "edit.line_end"],
  ["Alt+Backspace", "edit.delete_word_back"],
  ["Cmd+Backspace", "edit.delete_to_line_start"],
]);

// ---- the platform --------------------------------------------------------------------------------

/**
 * The fixed clipboard shortcuts of Windows and Linux: outside the core's rebindable table, like `Cmd+C` on macOS.
 * Plain `Ctrl+C` is never a copy (it is SIGINT and always reaches the program). `Ctrl+V` pastes on Windows only: on
 * Linux a terminal keeps it for the program (Ctrl+V is the literal-next of the shell).
 */
const FIXED_CLIPBOARD: ReadonlyMap<string, string> = new Map([
  ["Ctrl+Shift+C", "edit.copy"],
  ["Ctrl+Insert", "edit.copy"],
  ["Ctrl+Shift+V", "edit.paste"],
  ["Shift+Insert", "edit.paste"],
]);
const WINDOWS_CLIPBOARD: ReadonlyMap<string, string> = new Map([...FIXED_CLIPBOARD, ["Ctrl+V", "edit.paste"]]);

/** The platform the page runs on, as `app_info` names it; empty until it is known, which reads as macOS. */
let os = "";

/** Tell this module the platform (`macos`, `windows`, anything else is Linux). An empty name means not known yet. */
export function setOs(name: string): void {
  os = name;
}

/** The static fallback of the platform: the whole table on macOS (and while the OS is unknown), else clipboard chords. */
function fallback(): ReadonlyMap<string, string> {
  if (os === "" || os === "macos") return CHORDS;
  return os === "windows" ? WINDOWS_CLIPBOARD : FIXED_CLIPBOARD;
}

// ---- the core's resolved table --------------------------------------------------------------------

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

/**
 * The lookup from a canonical chord to an action id for the rows of the core's table. An unbound row (chord null)
 * has no entry, a row that is not a row is skipped, and when two rows carry the same chord the first keeps it and
 * the later one is returned in `conflicts` (the core never sends this: it unbinds the displaced action).
 */
export function bindingIndex(rows: readonly BindingRowLike[]): { index: Map<string, string>; conflicts: BindingConflict[] } {
  const index = new Map<string, string>();
  const conflicts: BindingConflict[] = [];
  for (const row of rows) {
    if (typeof row !== "object" || row === null || typeof row.action !== "string" || typeof row.chord !== "string") continue;
    const kept = index.get(row.chord);
    if (kept === undefined) index.set(row.chord, row.action);
    else conflicts.push({ chord: row.chord, kept, dropped: row.action });
  }
  return { index, conflicts };
}

/** What the core's last table says: chord to action, and the actions the table knows (bound or not). */
let table: { index: Map<string, string>; actions: Set<string>; byAction: Map<string, string> } | null = null;

/**
 * Make the core's rows the source of shortcut dispatch. No rows (a core that sent none, or none yet) leaves the
 * static table in force. Returns the conflicts of the index.
 */
export function setBindingTable(rows: readonly BindingRowLike[]): BindingConflict[] {
  if (rows.length === 0) {
    table = null;
    return [];
  }
  const { index, conflicts } = bindingIndex(rows);
  const actions = new Set<string>();
  for (const row of rows) if (typeof row === "object" && row !== null && typeof row.action === "string") actions.add(row.action);
  const byAction = new Map<string, string>();
  for (const [chord, action] of index) byAction.set(action, chord);
  table = { index, actions, byAction };
  return conflicts;
}

/**
 * The action id of a canonical chord, or undefined. With the core's table set, a chord is whatever the table binds.
 * A chord of the static table whose action the core's table does not know (`edit.copy` and `edit.paste`, which are no
 * rebindable actions) stays a fixed shortcut; one whose action the table knows and left unbound or moved stays unowned.
 */
export function actionIdFor(chord: string): string | undefined {
  if (!table) return fallback().get(chord);
  const id = table.index.get(chord);
  if (id !== undefined) return id;
  const fixed = fallback().get(chord);
  return fixed !== undefined && !table.actions.has(fixed) ? fixed : undefined;
}

/** The chord in force for an action: the core's table when set, else the static one; undefined when unbound. */
export function chordFor(action: string): string | undefined {
  if (table) return table.byAction.get(action);
  for (const [chord, id] of fallback()) if (id === action) return chord;
  return undefined;
}
