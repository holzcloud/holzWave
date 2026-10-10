// The frame the renderers draw: run-length encoded rows. Shared by every renderer.

/**
 * `[text, cols, fg, bg, flags]`. The text has exactly one code point per cell: `\0` for the second cell of a wide
 * character, a space for a blank cell. The code points a grapheme cluster has beyond its first travel in
 * `Frame.extras`, so a run's code point count always equals its `cols`.
 */
export type Run = [text: string, cols: number, fg: number, bg: number, flags: number];

/** `[col, tail]`: code points that join the cell at `col` of a row (a combining mark, a ZWJ sequence, a flag). */
export type Extra = [col: number, tail: string];

export type CursorStyle = "block" | "bar" | "underline" | "hollow";

/** The cursor as the engine reports it. `blink` is presentation: a renderer always draws the cursor it is given. */
export interface CursorShape {
  x: number;
  y: number;
  style: CursorStyle;
  blink: boolean;
}

export interface Frame {
  cols: number;
  rows: number;
  cursor: CursorShape | null;
  /** True for the pane that has the keyboard. An unfocused pane shows its cursor as a hollow outline. */
  focused: boolean;
  fg: number;
  bg: number;
  lines: Run[][];
  /** Cluster tails per row, aligned with `lines`; absent means none. */
  extras?: Extra[][];
}

/** The `screen` payload of the legacy protocol (cursor as `[x, y]`, no extras); `scene.fromLegacyFrame` converts it. */
export interface LegacyFrame {
  cols: number;
  rows: number;
  cursor: [number, number] | null;
  fg: number;
  bg: number;
  lines: Run[][];
}

/** The most cells a renderer builds buffers for: 1 Mi cells is a 1024 x 1024 grid, far past any window. */
export const MAX_CELLS = 1 << 20;

/** The number of cells of a frame's grid, or null when it cannot be drawn: not whole positive numbers, or too many. */
export function gridCells(f: Pick<Frame, "cols" | "rows">): number | null {
  const { cols, rows } = f;
  if (!Number.isInteger(cols) || !Number.isInteger(rows) || cols <= 0 || rows <= 0) return null;
  const cells = cols * rows;
  return cells <= MAX_CELLS ? cells : null;
}

/** The cursor to draw: null when there is none or it lies outside the grid. */
export function visibleCursor(f: Frame): CursorShape | null {
  const c = f.cursor;
  if (!c || !f.focused) return null;
  const { x, y } = c;
  if (!Number.isInteger(x) || !Number.isInteger(y) || x < 0 || y < 0 || x >= f.cols || y >= f.rows) return null;
  return c;
}

/** The longest cluster tail that is drawn, in code points; a longer one is cut. Real clusters stay below 16. */
export const MAX_TAIL = 32;

/** The cluster tails of row `y` by column, or null when the row has none. Malformed entries are ignored. */
export function rowExtras(f: Frame, y: number): Map<number, string> | null {
  const list = f.extras?.[y];
  if (!list || list.length === 0) return null;
  let out: Map<number, string> | null = null;
  for (const e of list) {
    if (!Array.isArray(e)) continue;
    const [col, tail] = e;
    if (!Number.isInteger(col) || col < 0 || typeof tail !== "string" || tail === "") continue;
    const cut = tail.length > MAX_TAIL ? Array.from(tail).slice(0, MAX_TAIL).join("") : tail;
    (out ??= new Map()).set(col, cut);
  }
  return out;
}

export const BOLD = 1, ITALIC = 2, UNDERLINE = 4, INVERSE = 8;
export const CURSOR_COLOR = 0xff8fb8;

export interface Metrics {
  /** Cell size in CSS pixels. */
  cellW: number;
  cellH: number;
  dpr: number;
  /** Canvas size in CSS pixels. */
  width: number;
  height: number;
}

/** A rectangle in CSS pixels, relative to the canvas. */
export interface PxRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Renderer {
  /** Called whenever the canvas or the cell metrics change. */
  configure(m: Metrics): void;
  /** Start a frame: clear the whole canvas to `bg`. */
  begin(bg: number): void;
  /** Draw one pane's grid into `rect`; the grid starts at the rect's top-left corner. */
  drawPane(frame: Frame, rect: PxRect): void;
  /** Block until the GPU has finished, so timings are real. Only used by the benchmark. */
  sync(): void;
}

export const FONT_PX = 14;
export const FONT_FAMILY = '"JetBrains Mono", "SF Mono", Menlo, monospace';
export const font = (flags: number, px = FONT_PX) =>
  `${flags & ITALIC ? "italic " : ""}${flags & BOLD ? "bold " : ""}${px}px ${FONT_FAMILY}`;
export const css = (c: number) => `#${c.toString(16).padStart(6, "0")}`;
