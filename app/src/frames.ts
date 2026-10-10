// The webview's mirror of a pane: `frame` events patch it, the renderers draw from it. A pure module (no DOM, no
// state): node imports it for the fixture-driven test and later plans read it for links and hints.
import type { CursorShape, Extra, Frame, Run } from "./frame.js";

// ---- the wire ---------------------------------------------------------------------------------------

/** One changed row: runs of one style each (one code point per cell) and the cluster tails by column. */
export interface RowMsg {
  y: number;
  runs: Run[];
  extras?: Extra[];
}

/** `[y, x0, x1]`: the selected cells of viewport row `y`, both ends inclusive. */
export type SpanMsg = [y: number, x0: number, x1: number];

/** `[y, x0, x1, current]`: one search match in the viewport. */
export type MatchMsg = [y: number, x0: number, x1: number, current: boolean];

export interface SearchMsg {
  total: number;
  /** The place of the current match in reading order, or null. */
  current: number | null;
}

export interface ModesMsg {
  alt: boolean;
  bracketed_paste: boolean;
  mouse: boolean;
}

/** The `frame` event payload (core/src/wire.rs `FrameMsg`). `full` means: replace the whole mirror. */
export interface FrameMsg {
  pane: number;
  seq: number;
  full: boolean;
  cols: number;
  rows: number;
  fg: number;
  bg: number;
  cursor: CursorShape | null;
  /** `[offset, total, len]`. */
  scrollbar: [number, number, number];
  selection: SpanMsg[];
  matches: MatchMsg[];
  search: SearchMsg | null;
  copy_cursor: [number, number] | null;
  modes: ModesMsg;
  changed: RowMsg[];
}

// ---- the mirror -------------------------------------------------------------------------------------

/** What the webview knows of one pane: the grid as the core last described it and the header of that frame. */
export interface PaneMirror {
  /** The sequence number of the frame this mirror is in step with. */
  seq: number;
  cols: number;
  rows: number;
  fg: number;
  bg: number;
  /** One entry per row of the grid; null for a row no frame has sent (it is blank). */
  rows_: (RowMsg | null)[];
  cursor: CursorShape | null;
  scrollbar: [number, number, number];
  selection: SpanMsg[];
  matches: MatchMsg[];
  search: SearchMsg | null;
  copyCursor: [number, number] | null;
  modes: ModesMsg;
}

/**
 * Apply one frame. A full frame replaces the mirror (also for a pane that has none); any other frame must carry the
 * sequence number after the mirror's, else nothing is patched and `gap` is true: the caller asks for a full frame.
 */
export function applyFrame(
  mirror: PaneMirror | undefined,
  _msg: FrameMsg,
): { mirror: PaneMirror | undefined; gap: boolean } {
  return { mirror, gap: false };
}

/** The frame the renderers draw: rows never received are empty, cluster tails are aligned by row. */
export function toRenderFrame(mirror: PaneMirror, focused: boolean): Frame {
  return { cols: mirror.cols, rows: mirror.rows, cursor: null, focused, fg: 0, bg: 0, lines: [] };
}

/** The grid as plain text, one string per row: spacers dropped, tails appended, trailing blanks trimmed. */
export function gridText(_mirror: PaneMirror): string[] {
  return [];
}
