// Types shared with the Rust side (the layout the core sends) and between the frontend modules.
import type { LegacyFrame } from "./frame.js";

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

/** The layout the core sends: workspaces, tabs and the panes of the active tab. */
export interface View {
  workspaces: { name: string; active: boolean }[];
  tabs: { title: string; active: boolean }[];
  focus: number;
  panes: [number, UnitRect][];
  dividers: Divider[];
}

/** The `screen` event payload of the legacy protocol; plan 17 replaces it with `frame`. */
export interface PaneFrame {
  pane: number;
  frame: LegacyFrame & { scrollbar: [number, number, number] };
}

/** A pane's size in cells. */
export interface CellSize {
  cols: number;
  rows: number;
}

/** The legacy frontend selection: cell coordinates of the anchor `a` and the moving end `b`. */
export interface Selection {
  pane: number;
  a: [number, number];
  b: [number, number];
}
