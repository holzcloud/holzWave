// The webview's mirror of a pane: `frame` events patch it, the renderers draw from it. A pure module (no DOM, no
// state): node imports it for the fixture-driven test and later plans read it for links and hints.
import { LINK, gridCells } from "./frame.js";
import type { CursorShape, Extra, Frame, Run } from "./frame.js";

// ---- the wire ---------------------------------------------------------------------------------------

/** `[x0, x1, uri]`: a stretch of one row that a program marked as a link (OSC 8), both ends inclusive columns. */
export type LinkMsg = [x0: number, x1: number, uri: string];

/** The most link spans the page keeps for one row; the core keeps at most eight as well. */
export const MAX_ROW_LINKS = 8;
/** The longest address a span may carry, in UTF-16 units: the core cleans to 2048 bytes, so more is not from the core. */
const MAX_LINK_URI = 2048;

/** One changed row: runs of one style each (one code point per cell), the cluster tails by column and the links. */
export interface RowMsg {
  y: number;
  runs: Run[];
  extras?: Extra[];
  /** The link spans of the row; absent means none. The row keeps them until it is sent again. */
  links?: LinkMsg[];
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

/**
 * One picture placed in the pane's grid, as sixteen whole numbers (core/src/wire.rs `PlacementMsg`):
 * `[image, generation, layer, z, col, row, gridCols, gridRows, dx, dy, pxW, pxH, sx, sy, sw, sh]`.
 * `layer` is 0 below the cell backgrounds, 1 below the text, 2 above the text. `col` and `row` are signed (a picture
 * can start above or left of the viewport); `dx` and `dy` are the pixel offsets inside that cell, `pxW` and `pxH`
 * the size the picture covers, all in device pixels of the cell size the core was told; `sx`, `sy`, `sw`, `sh` is the
 * part of the picture's own pixels that is shown.
 */
export type PlacementMsg = [
  image: number, generation: number, layer: number, z: number, col: number, row: number, gridCols: number,
  gridRows: number, dx: number, dy: number, pxW: number, pxH: number, sx: number, sy: number, sw: number, sh: number,
];

/** The most placements a frame is read for; the core cuts its list at the same number. */
export const MAX_PLACEMENTS = 256;
/** The longest side, in pixels, of a picture the page takes: the core refuses larger ones, and so does the page. */
export const MAX_IMAGE_SIDE = 16384;
/** The largest magnitude of a position, an offset or a size on the screen: far past any grid (1 Mi cells). */
const MAX_SCREEN = 1 << 20;

/**
 * A placement the page may use, or null: sixteen safe whole numbers, a layer of 0 to 2, an image id that fits 32 bits,
 * a generation of at least 1, a position, offsets and a size inside `MAX_SCREEN`, a shown part that starts inside
 * the picture and is at most `MAX_IMAGE_SIDE` wide and tall. The numbers decide allocations and rectangles, so
 * nothing else gets in.
 */
export function usablePlacement(p: unknown): PlacementMsg | null {
  if (!Array.isArray(p) || p.length !== 16) return null;
  for (const n of p) if (!Number.isSafeInteger(n)) return null;
  const [image, generation, layer, z, col, row, gridCols, gridRows, dx, dy, pxW, pxH, sx, sy, sw, sh] = p as number[] as
    PlacementMsg;
  const within = (n: number, lo: number, hi: number) => n >= lo && n <= hi;
  if (!within(image, 0, 0xffffffff) || generation < 1 || !within(layer, 0, 2) || !within(z, -0x80000000, 0x7fffffff)) return null;
  if (!within(col, -MAX_SCREEN, MAX_SCREEN) || !within(row, -MAX_SCREEN, MAX_SCREEN)) return null;
  if (!within(gridCols, 0, MAX_SCREEN) || !within(gridRows, 0, MAX_SCREEN)) return null;
  if (!within(dx, -MAX_SCREEN, MAX_SCREEN) || !within(dy, -MAX_SCREEN, MAX_SCREEN)) return null;
  if (!within(pxW, 1, MAX_SCREEN) || !within(pxH, 1, MAX_SCREEN)) return null;
  if (!within(sx, 0, MAX_IMAGE_SIDE) || !within(sy, 0, MAX_IMAGE_SIDE)) return null;
  if (!within(sw, 1, MAX_IMAGE_SIDE) || !within(sh, 1, MAX_IMAGE_SIDE)) return null;
  return p as PlacementMsg;
}

/**
 * The usable placements among the first `MAX_PLACEMENTS` entries of a list, in order; an entry that fails is skipped.
 * Anything that is not a list gives an empty one. At most 256 entries are even looked at.
 */
export function usablePlacements(list: unknown): PlacementMsg[] {
  if (!Array.isArray(list)) return [];
  const out: PlacementMsg[] = [];
  const n = Math.min(list.length, MAX_PLACEMENTS);
  for (let i = 0; i < n; i++) {
    const p = usablePlacement(list[i]);
    if (p) out.push(p);
  }
  return out;
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
  /**
   * The pictures placed in the viewport. Present when the list changed (an empty list means every picture is gone) and
   * on a full frame that has pictures; absent otherwise, and then the mirror keeps the list it has.
   */
  images?: PlacementMsg[];
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
  /** The usable placements of the pictures in the viewport, at most `MAX_PLACEMENTS`; empty for a pane without any. */
  images: PlacementMsg[];
}

const BLANK_MODES: ModesMsg = { alt: false, bracketed_paste: false, mouse: false };

/**
 * The link spans of a row that can be used: `[int, int, string]` with `0 <= x0 <= x1 < cols` and an address of at most
 * 2048 units, in order, at most `MAX_ROW_LINKS`. A span that fails is dropped and does not count; anything that is not
 * a list gives an empty one. The numbers decide a box and a lookup, so nothing else gets in.
 */
export function usableLinks(list: unknown, cols: number): LinkMsg[] {
  if (!Array.isArray(list)) return [];
  const out: LinkMsg[] = [];
  for (const span of list) {
    if (out.length >= MAX_ROW_LINKS) break;
    if (!Array.isArray(span) || span.length !== 3) continue;
    const [x0, x1, uri] = span as unknown[];
    if (!Number.isSafeInteger(x0) || !Number.isSafeInteger(x1) || typeof uri !== "string") continue;
    const a = x0 as number, b = x1 as number;
    if (a < 0 || a > b || b >= cols || uri === "" || uri.length > MAX_LINK_URI) continue;
    out.push([a, b, uri]);
  }
  return out;
}

/** A row message that can be stored: an integer `y` inside the grid and a list of runs. Anything else is dropped. */
function usableRow(row: unknown, rows: number, cols: number): RowMsg | null {
  if (typeof row !== "object" || row === null) return null;
  const { y, runs, extras, links } = row as RowMsg;
  if (!Number.isInteger(y) || y < 0 || y >= rows || !Array.isArray(runs)) return null;
  const out: RowMsg = Array.isArray(extras) && extras.length > 0 ? { y, runs, extras } : { y, runs };
  const spans = usableLinks(links, cols);
  if (spans.length > 0) out.links = spans;
  return out;
}

/** The mirror of `msg` with `rows_` as its grid and `images` as its placements; the header fields come from the message. */
function build(msg: FrameMsg, rows_: (RowMsg | null)[], images: PlacementMsg[]): PaneMirror {
  return {
    seq: msg.seq,
    cols: msg.cols,
    rows: msg.rows,
    fg: msg.fg,
    bg: msg.bg,
    rows_,
    cursor: msg.cursor ?? null,
    scrollbar: Array.isArray(msg.scrollbar) ? msg.scrollbar : [0, msg.rows, msg.rows],
    selection: Array.isArray(msg.selection) ? msg.selection : [],
    matches: Array.isArray(msg.matches) ? msg.matches : [],
    search: msg.search ?? null,
    copyCursor: msg.copy_cursor ?? null,
    modes: msg.modes ?? BLANK_MODES,
    images,
  };
}

/**
 * Apply one frame. A full frame replaces the mirror (also for a pane that has none); any other frame must carry the
 * sequence number after the mirror's and the mirror's size, else nothing is patched and `gap` is true: the caller
 * asks for a full frame and ignores frames until it arrives. The mirror passed in is never changed.
 *
 * A frame whose size is not a sane grid (see `gridCells`) or whose sequence number is not a whole number is ignored
 * as a whole, so nothing is ever allocated from a count the frame provides beyond the grid limit.
 */
export function applyFrame(
  mirror: PaneMirror | undefined,
  msg: FrameMsg,
): { mirror: PaneMirror | undefined; gap: boolean } {
  if (msg.full) {
    if (gridCells(msg) === null || !Number.isSafeInteger(msg.seq)) return { mirror, gap: false };
    const rows_: (RowMsg | null)[] = Array.from({ length: msg.rows }, () => null);
    for (const row of Array.isArray(msg.changed) ? msg.changed : []) {
      const usable = usableRow(row, msg.rows, msg.cols);
      if (usable) rows_[usable.y] = usable;
    }
    // A full frame replaces everything: no list means no pictures.
    return { mirror: build(msg, rows_, usablePlacements(msg.images)), gap: false };
  }
  if (!mirror || msg.seq !== mirror.seq + 1 || msg.cols !== mirror.cols || msg.rows !== mirror.rows) {
    return { mirror, gap: true };
  }
  const rows_ = mirror.rows_.slice();
  for (const row of Array.isArray(msg.changed) ? msg.changed : []) {
    const usable = usableRow(row, mirror.rows, mirror.cols);
    if (usable) rows_[usable.y] = usable;
  }
  // A frame without the field keeps the list; with it (also an empty one) the list is replaced.
  const images = Array.isArray(msg.images) ? usablePlacements(msg.images) : mirror.images;
  return { mirror: build(msg, rows_, images), gap: false };
}

/**
 * What decides which marked links get the resting cue. `key` stands for everything `allow` reads, so equal keys mean equal
 * answers (a row is not checked again under the same key); `allow` says whether the address of a span is a link here.
 */
export interface LinkFilter {
  key: string;
  allow(uri: string): boolean;
}

/** What is known of one row object (a row is replaced when a frame touches it, never changed): whether a cell carries LINK, and the last answer. */
interface RowMask {
  flagged: boolean;
  key: string | null;
  runs: Run[];
}
const masks = new WeakMap<RowMsg, RowMask>();

/** Whether a run carries the flag LINK; anything that is not a run does not. */
const flaggedRun = (run: Run): boolean => Array.isArray(run) && typeof run[4] === "number" && (run[4] & LINK) !== 0;

/**
 * The runs of `row` with the flag LINK cleared on every cell that does not lie inside a span of the row whose address
 * `allow` accepts. A cell keeps every other flag, colour and character; runs are split at the edges of the spans, so the
 * cells of each run and the number of them stay what they were. A wide character and its spacer decide together (the
 * decision of the wide character). Where nothing changes the very array of the row comes back.
 */
function maskedRuns(row: RowMsg, cols: number, allow: (uri: string) => boolean): Run[] {
  const accepted = new Uint8Array(cols);
  for (const [x0, x1, uri] of row.links ?? []) {
    if (!allow(uri)) continue;
    for (let c = x0; c <= x1 && c < cols; c++) accepted[c] = 1;
  }
  const out: Run[] = [];
  let changed = false;
  let col = 0;
  let prev = false; // the decision for the cell before, for the spacer of a wide character
  for (const run of row.runs) {
    if (!Array.isArray(run) || typeof run[1] !== "number" || !Number.isInteger(run[1]) || run[1] < 0) {
      out.push(run); // not something to split; the renderers read it as they always did
      prev = false;
      continue;
    }
    const [text, n, fg, bg, flags] = run;
    if (!flaggedRun(run)) {
      out.push(run);
      prev = false;
      col += n;
      continue;
    }
    const chars = typeof text === "string" ? Array.from(text) : [];
    // The pieces of this run: [from, to) in cells of the run and whether the cue stays on them.
    let from = 0;
    let keep = false;
    const pieces: [number, number, boolean][] = [];
    for (let i = 0; i < n; i++) {
      const spacer = chars[i] === "\0" && i + col > 0;
      const now: boolean = spacer ? prev : col + i < cols && accepted[col + i] === 1;
      if (i > 0 && now !== keep) {
        pieces.push([from, i, keep]);
        from = i;
      }
      keep = now;
      prev = now;
    }
    if (n > 0) pieces.push([from, n, keep]);
    if (pieces.length === 1 && pieces[0]![2]) {
      out.push(run); // every cell of it is inside a link the page treats as one
    } else {
      changed = true;
      for (const [a, b, kept] of pieces) {
        const piece = typeof text === "string" ? (a === 0 && b >= chars.length ? text : chars.slice(a, b).join("")) : text;
        out.push([piece, b - a, fg, bg, kept ? flags : flags & ~LINK]);
      }
    }
    col += n;
  }
  return changed ? out : row.runs;
}

/** The runs of `row` as the renderers draw them under `filter`; rows without a flagged cell and rows already checked under the same key cost a lookup. */
function filteredRuns(row: RowMsg, cols: number, filter: LinkFilter): Run[] {
  let mask = masks.get(row);
  if (!mask) {
    mask = { flagged: row.runs.some(flaggedRun), key: null, runs: row.runs };
    masks.set(row, mask);
  }
  if (!mask.flagged) return row.runs;
  if (mask.key !== filter.key) {
    mask.runs = maskedRuns(row, cols, filter.allow);
    mask.key = filter.key;
  }
  return mask.runs;
}

/**
 * The frame the renderers draw: rows never received are empty, cluster tails are aligned by row. With a `filter` the
 * flag LINK stays only on cells inside a span the filter accepts, so the dotted cue marks what the page treats as a link
 * and nothing else (the core flags every span it keeps; the page decides). Without one the rows are drawn as they came.
 */
export function toRenderFrame(mirror: PaneMirror, focused: boolean, filter?: LinkFilter): Frame {
  const lines: Run[][] = [];
  const extras: Extra[][] = [];
  for (let y = 0; y < mirror.rows; y++) {
    const row = mirror.rows_[y];
    lines.push(row ? (filter ? filteredRuns(row, mirror.cols, filter) : row.runs) : []);
    extras.push(row?.extras ?? []);
  }
  return {
    cols: mirror.cols,
    rows: mirror.rows,
    cursor: mirror.cursor,
    focused,
    fg: mirror.fg,
    bg: mirror.bg,
    lines,
    extras,
  };
}

/**
 * One row as one string per cell column: a wide character is followed by an empty string for its spacer, a blank
 * cell is a space, the tail of a grapheme cluster is appended to its cell. The list holds at most `cols` entries
 * and may be shorter (columns a frame never sent are blank). Anything that is not a row gives an empty list.
 */
function cellsOf(row: RowMsg | null | undefined, cols: number): string[] {
  const cells: string[] = [];
  if (!row) return cells;
  fill: for (const run of row.runs) {
    const text = Array.isArray(run) ? run[0] : undefined;
    if (typeof text !== "string") continue;
    for (const ch of text) {
      if (cells.length >= cols) break fill;
      cells.push(ch === "\0" ? "" : ch);
    }
  }
  for (const extra of row.extras ?? []) {
    if (!Array.isArray(extra)) continue;
    const [col, tail] = extra;
    if (Number.isInteger(col) && col >= 0 && col < cells.length && typeof tail === "string") cells[col] += tail;
  }
  return cells;
}

/** Row `y` of the mirror as one string per cell column (see `cellsOf`); an unknown row is empty. */
export function rowCells(mirror: PaneMirror, y: number): string[] {
  if (!Number.isInteger(y) || y < 0 || y >= mirror.rows) return [];
  return cellsOf(mirror.rows_[y], mirror.cols);
}

/** One row as plain text: spacers dropped, tails appended, trailing blanks trimmed. */
function rowText(row: RowMsg | null | undefined, cols: number): string {
  return cellsOf(row, cols).join("").trimEnd();
}

/** The grid as plain text, one string per row: spacers dropped, tails appended, trailing blanks trimmed. */
export function gridText(mirror: PaneMirror): string[] {
  const out: string[] = [];
  for (let y = 0; y < mirror.rows; y++) out.push(rowText(mirror.rows_[y], mirror.cols));
  return out;
}

/**
 * A cheap signature of a span list: equal lists give equal strings and different lists different ones. The empty
 * list (and anything that is not a list) is the empty string.
 */
export function spansSignature(spans: readonly SpanMsg[] | undefined): string {
  if (!Array.isArray(spans) || spans.length === 0) return "";
  return spans.map((s) => (Array.isArray(s) ? s.join(",") : "?")).join(";");
}

/**
 * The spans of the mirror that can be drawn: whole numbers, a row inside the grid, the ends clamped to the columns
 * of the grid. A span that is empty after clamping, or malformed, is dropped.
 */
export function visibleSpans(mirror: PaneMirror): SpanMsg[] {
  const out: SpanMsg[] = [];
  for (const span of mirror.selection) {
    if (!Array.isArray(span) || span.length < 3) continue;
    const [y, x0, x1] = span;
    if (!Number.isInteger(y) || !Number.isInteger(x0) || !Number.isInteger(x1)) continue;
    if (y < 0 || y >= mirror.rows) continue;
    const from = Math.max(0, x0);
    const to = Math.min(mirror.cols - 1, x1);
    if (from <= to) out.push([y, from, to]);
  }
  return out;
}

/**
 * The search matches of the mirror that can be drawn: whole numbers, a row inside the grid, the ends clamped to the
 * columns of the grid, `current` true only for `true`. A match that is empty after clamping, or malformed, is dropped.
 */
export function visibleMatches(mirror: PaneMirror): MatchMsg[] {
  const out: MatchMsg[] = [];
  for (const match of mirror.matches) {
    if (!Array.isArray(match) || match.length < 3) continue;
    const [y, x0, x1, current] = match;
    if (!Number.isInteger(y) || !Number.isInteger(x0) || !Number.isInteger(x1)) continue;
    if (y < 0 || y >= mirror.rows) continue;
    const from = Math.max(0, x0);
    const to = Math.min(mirror.cols - 1, x1);
    if (from <= to) out.push([y, from, to, current === true]);
  }
  return out;
}

/**
 * A signature of everything in a header that the overlay draws or shows besides the cells: the selection spans, the
 * search matches, the search counter and the copy cursor. Equal headers give equal strings; a frame whose signature
 * differs from the one before it needs an overlay render.
 */
export function decorationSignature(mirror: PaneMirror | undefined): string {
  if (!mirror) return "";
  const matches = Array.isArray(mirror.matches)
    ? mirror.matches.map((m) => (Array.isArray(m) ? m.join(",") : "?")).join(";")
    : "";
  const search = mirror.search ? `${mirror.search.total}/${mirror.search.current ?? "-"}` : "-";
  const cursor = Array.isArray(mirror.copyCursor) ? mirror.copyCursor.join(",") : "-";
  return `${spansSignature(mirror.selection)}|${matches}|${search}|${cursor}`;
}

// ---- pane lifecycle -----------------------------------------------------------------------------------

/** The per-pane maps of the page's state that must not outlive a pane. */
export interface PaneStores {
  frames: { delete(pane: number): boolean };
  sizes: { delete(pane: number): boolean };
  titles: { delete(pane: number): boolean };
  cwds: { delete(pane: number): boolean };
  exited: { add(pane: number): unknown };
}

/**
 * Forget a pane that exited: its mirror, size, title and cwd are deleted and its id is remembered as exited. The
 * layout's own pane list is not the signal for this (it only lists the active tab's panes, and the panes of other
 * tabs keep running), so the `pane-exit` event is. A pane nobody heard of is recorded as exited too.
 */
export function forgetPane(stores: PaneStores, pane: number): void {
  stores.frames.delete(pane);
  stores.sizes.delete(pane);
  stores.titles.delete(pane);
  stores.cwds.delete(pane);
  stores.exited.add(pane);
}

/** The per-pane maps a pane that moved to another window leaves: `exited` loses it, `away` gains it. */
export interface LeaveStores extends Omit<PaneStores, "exited"> {
  exited: { delete(pane: number): boolean };
  away: { add(pane: number): unknown };
}

/**
 * Forget a pane that moved to another window: its mirror, size, title and cwd are deleted, it is not an exited pane
 * (it may come back) and it is remembered as away, so that events that were already on their way to this window are
 * dropped. Its absence of a size is what makes it a first appearance when it returns: the page then starts it (a
 * no-op, it runs) and asks for a full frame.
 */
export function leavePane(stores: LeaveStores, pane: number): void {
  stores.frames.delete(pane);
  stores.sizes.delete(pane);
  stores.titles.delete(pane);
  stores.cwds.delete(pane);
  stores.exited.delete(pane);
  stores.away.add(pane);
}

/** Events per second over the last `windowMs`: the arrival rate of frame events, whatever gets drawn of them. */
export class RateMeter {
  private times: number[] = [];

  constructor(private readonly windowMs = 1000) {}

  /** Record one event at `now` (milliseconds on any clock that only goes forward). */
  note(now: number): void {
    this.times.push(now);
    this.trim(now);
  }

  /** The events in the window ending at `now`, scaled to one second. */
  perSecond(now: number): number {
    this.trim(now);
    return (this.times.length * 1000) / this.windowMs;
  }

  private trim(now: number): void {
    const cutoff = now - this.windowMs;
    let n = 0;
    while (n < this.times.length && (this.times[n] ?? Infinity) < cutoff) n++;
    if (n > 0) this.times.splice(0, n);
  }
}
