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

/**
 * One picture to draw in a pane (Kitty graphics): the decoded bitmap, where it goes and which part of it. Everything is
 * already validated and mapped to the cell size now: `dx`, `dy`, `dw`, `dh` are CSS pixels relative to the grid's
 * top-left corner (they may reach past the grid; the renderer clips to the grid's box), `sx`, `sy`, `sw`, `sh` are
 * pixels of the bitmap and lie inside it.
 */
export interface ImageDraw<B = ImageBitmap> {
  /** 0 below the cell backgrounds, 1 below the text, 2 above the text. */
  layer: 0 | 1 | 2;
  z: number;
  /** The picture's id: the tie-break inside a layer and a `z`. */
  id: number;
  bitmap: B;
  dx: number;
  dy: number;
  dw: number;
  dh: number;
  sx: number;
  sy: number;
  sw: number;
  sh: number;
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
  /** The pictures to draw, in drawing order inside each layer; absent means none and the draw path is the old one. */
  images?: ImageDraw[];
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

/**
 * The cursor to draw: null when there is none or it lies outside the grid. A pane without the keyboard shows it as
 * a hollow outline whatever its style (docs/design.md).
 */
export function visibleCursor(f: Frame): CursorShape | null {
  const c = f.cursor;
  if (!c) return null;
  const { x, y } = c;
  if (!Number.isInteger(x) || !Number.isInteger(y) || x < 0 || y < 0 || x >= f.cols || y >= f.rows) return null;
  return f.focused || c.style === "hollow" ? c : { x, y, style: "hollow", blink: c.blink };
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
/**
 * The wire flag of a cell inside a link the program marked (core/src/wire.rs). A renderer draws the resting cue of it, a dotted
 * underline (docs/design.md 6.31), unless the cell already has the program's own underline. The core flags every span it keeps;
 * the page clears the flag in the frame it hands the renderers for a span it does not treat as a link (`toRenderFrame` with a
 * `LinkFilter`), so a renderer draws the cue for the flag it is given and asks nothing.
 */
export const LINK = 16;

/**
 * The size of one dot of the resting cue, and its thickness, in device pixels: one CSS pixel, so `round(dpr)`, at least 1. The
 * dots and the gaps between them are this long, and the line lies in the two rows of this height that end one such height above
 * the bottom of the cell. Both renderers use these numbers, so their dots land on the same device pixels.
 */
export const linkDot = (dpr: number): number => (Number.isFinite(dpr) && dpr > 1 ? Math.max(1, Math.round(dpr)) : 1);

/**
 * The device-pixel spans `[from, to)` of the dots of a dotted line over device pixels `[start, stop)`, counted from the grid's
 * left edge: dots lie on the even multiples of `dot`. A pure function, so a test can compare what the renderers draw with it.
 */
export function dotSpans(start: number, stop: number, dot: number): [number, number][] {
  const out: [number, number][] = [];
  if (!(dot >= 1) || !(stop > start)) return out;
  for (let b = Math.floor(start / (2 * dot)) * 2 * dot; b < stop; b += 2 * dot) {
    const from = Math.max(b, start);
    const to = Math.min(b + dot, stop);
    if (to > from) out.push([from, to]);
  }
  return out;
}
/** The cursor colour of the built-in aubergine theme; a renderer draws another one after `setCursorColor`. */
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

/** An earlier frame of a pane that is drawn under the current one, `offsetY` CSS pixels lower than its place. */
export interface UnderFrame {
  frame: Frame;
  offsetY: number;
}

/** A ghost of the cursor (DESK-28): the cell and the opacity, 0 to 1, of a block in the cursor colour. */
export interface GhostCell {
  x: number;
  y: number;
  alpha: number;
}

/**
 * What a pane draws on top of its frame while an effect runs (DESK-28). Every field is optional and none of them is
 * ever needed for a frame to be right: without them the frame is drawn at its place.
 */
export interface PaneFx {
  /** The grid is drawn this many CSS pixels lower than its place (negative: higher), clipped to the grid's own box. */
  offsetY?: number;
  /** Earlier frames of the same grid size, oldest first, drawn before the frame itself to fill what it leaves bare. */
  under?: UnderFrame[];
  /** The cursor's fading ghosts, drawn last. */
  ghosts?: GhostCell[];
}

export interface Renderer {
  /** Called whenever the canvas or the cell metrics change. */
  configure(m: Metrics): void;
  /** Start a frame: clear the whole canvas to `bg`. */
  begin(bg: number): void;
  /** Draw one pane's grid into `rect`; the grid starts at the rect's top-left corner. `fx` is the effects' share. */
  drawPane(frame: Frame, rect: PxRect, fx?: PaneFx): void;
  /** Draw the cursor in this colour (0xRRGGBB) from the next frame on; the caller schedules that frame. */
  setCursorColor(rgb: number): void;
  /**
   * Draw the dotted line of a link in this colour (0xRRGGBB), the system link colour under forced colours, or in the cell's own
   * foreground colour (null, the default) from the next frame on; the caller schedules that frame. A renderer that is never given
   * one behaves as it always did.
   */
  setLinkColor?(rgb: number | null): void;
  /** Block until the GPU has finished, so timings are real. Only used by the benchmark. */
  sync(): void;
}

/** The terminal font size in CSS pixels until the settings say another: the core's default. */
export const FONT_PX = 14;
let fontPx = FONT_PX;

/** The terminal font size in CSS pixels now: what `font()` and the renderers draw with. */
export const getFontPx = (): number => fontPx;

/**
 * Take a new terminal font size. The caller measures again afterwards (`scene.fit`): a cell size taken before
 * stays the size of the old font. A value that is not a positive finite number is ignored.
 */
export function setFontPx(px: number): void {
  if (Number.isFinite(px) && px > 0) fontPx = px;
}

// The bundled JetBrains Mono comes first on every platform, so the cell metrics are the same everywhere; the rest is
// only what a glyph it lacks falls back to: macOS, then Windows, then Linux, then the generic family.
export const FONT_FAMILY =
  '"JetBrains Mono", "SF Mono", Menlo, "Cascadia Mono", Consolas, "DejaVu Sans Mono", "Noto Sans Mono", monospace';
export const font = (flags: number, px = fontPx) =>
  `${flags & ITALIC ? "italic " : ""}${flags & BOLD ? "bold " : ""}${px}px ${FONT_FAMILY}`;
export const css = (c: number) => `#${c.toString(16).padStart(6, "0")}`;
