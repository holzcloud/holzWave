// Types shared with the Rust side (the layout the core sends) and between the frontend modules.
/** A rectangle in unit coordinates: 0..1 of the canvas in both directions. */
export interface UnitRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** One divider between two panes: `ratio` is the position of the divider inside `area`. */
export interface Divider {
  split: number;
  axis: "Row" | "Column";
  area: UnitRect;
  ratio: number;
}

/**
 * The layout the core sends: workspaces, tabs (with the pane that has each one's focus) and the panes of the active tab.
 * `rev` is the place of the view in the order the core built them: the answer of a command and the `layout` event
 * travel on different paths, so the page keeps the newest one and drops a view that is older than the one it shows.
 */
export interface View {
  rev: number;
  workspaces: { name: string; active: boolean }[];
  tabs: { title: string; active: boolean; focus: number }[];
  focus: number;
  panes: [number, UnitRect][];
  dividers: Divider[];
  /** The names the user gave to the panes of the shown tab. */
  pane_names: [number, string][];
}

/** A pane's size in cells. */
export interface CellSize {
  cols: number;
  rows: number;
}

/** What the core was told about a pane: its grid and the cell size in whole device pixels. */
export interface PaneSize extends CellSize {
  cellW: number;
  cellH: number;
}
