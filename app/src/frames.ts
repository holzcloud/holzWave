// The webview's mirror of a pane: `frame` events patch it, the renderers draw from it. A pure module (no DOM, no
// state): node imports it for the fixture-driven test and later plans read it for links and hints.
import { gridCells } from "./frame.js";
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

const BLANK_MODES: ModesMsg = { alt: false, bracketed_paste: false, mouse: false };

/** A row message that can be stored: an integer `y` inside the grid and a list of runs. Anything else is dropped. */
function usableRow(row: unknown, rows: number): RowMsg | null {
  if (typeof row !== "object" || row === null) return null;
  const { y, runs, extras } = row as RowMsg;
  if (!Number.isInteger(y) || y < 0 || y >= rows || !Array.isArray(runs)) return null;
  return Array.isArray(extras) && extras.length > 0 ? { y, runs, extras } : { y, runs };
}

/** The mirror of `msg` with `rows_` as its grid; the header fields come from the message. */
function build(msg: FrameMsg, rows_: (RowMsg | null)[]): PaneMirror {
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
      const usable = usableRow(row, msg.rows);
      if (usable) rows_[usable.y] = usable;
    }
    return { mirror: build(msg, rows_), gap: false };
  }
  if (!mirror || msg.seq !== mirror.seq + 1 || msg.cols !== mirror.cols || msg.rows !== mirror.rows) {
    return { mirror, gap: true };
  }
  const rows_ = mirror.rows_.slice();
  for (const row of Array.isArray(msg.changed) ? msg.changed : []) {
    const usable = usableRow(row, mirror.rows);
    if (usable) rows_[usable.y] = usable;
  }
  return { mirror: build(msg, rows_), gap: false };
}

/** The frame the renderers draw: rows never received are empty, cluster tails are aligned by row. */
export function toRenderFrame(mirror: PaneMirror, focused: boolean): Frame {
  const lines: Run[][] = [];
  const extras: Extra[][] = [];
  for (let y = 0; y < mirror.rows; y++) {
    const row = mirror.rows_[y];
    lines.push(row ? row.runs : []);
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

/** One row as plain text: one string per cell, the spacer of a wide character dropped, tails appended. */
function rowText(row: RowMsg | null | undefined, cols: number): string {
  if (!row) return "";
  const cells: string[] = [];
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
  return cells.join("").trimEnd();
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
