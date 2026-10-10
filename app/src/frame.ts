// The frame the core sends: run-length encoded rows. Shared by every renderer.

/** `[text, cols, fg, bg, flags]`. A wide character is followed by `\0` for its spacer cell. */
export type Run = [text: string, cols: number, fg: number, bg: number, flags: number];

export interface Frame {
  cols: number;
  rows: number;
  cursor: [number, number] | null;
  fg: number;
  bg: number;
  lines: Run[][];
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
