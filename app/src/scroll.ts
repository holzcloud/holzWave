// Smooth scrolling and the cursor trail (DESK-28): the maths and the per-pane state of two optional effects of the
// drawing. The core stays the only owner of the scroll position (D-06): it scrolls by whole rows, and the frame that
// says so arrives and is drawn at once. What this module adds is only what is drawn after it, a slide that starts
// where the old rows were and ends where the new ones are, and a few fading ghosts of the cursor's old cells.
//
// Everything is pure: the clock is injected, nothing touches the DOM or the state, so node imports the file. The
// scene asks `Motion.active()` after each draw and asks for another animation frame only while it says true, so an
// idle page runs no loop.
import type { GhostCell as Ghost } from "./frame.js";
import type { PaneMirror } from "./frames.js";

/** The longest a scroll takes to settle, in milliseconds. */
export const SCROLL_MS = 120;

/** How long a cursor trail takes to fade, in milliseconds. */
export const TRAIL_MS = 160;

/** The most ghost cells a trail draws. */
export const MAX_GHOSTS = 6;

/** The most earlier frames kept under a slide, so a burst of scrolls never grows without bound. */
export const MAX_LAYERS = 4;

/** The opacity of the brightest ghost at the start of a trail. */
const GHOST_ALPHA = 0.55;

/** A cell of a pane's grid. */
export interface Cell {
  x: number;
  y: number;
}

// ---- the maths --------------------------------------------------------------------------------------

/** Cubic ease-out: fast at first, settling at the end. 0 at 0 and 1 at 1; an argument outside 0..1 is clamped. */
export function easeOutCubic(t: number): number {
  if (!(t > 0)) return 0; // also NaN
  if (t >= 1) return 1;
  const u = 1 - t;
  return 1 - u * u * u;
}

/**
 * What is left of an offset of `distance` after `elapsedMs` of an ease-out that lasts `durationMs`: `distance` at
 * the start, decreasing monotonically and exactly 0 from `durationMs` on. The result is never negative and never
 * larger than `distance`; a distance that is not a positive finite number, and a duration that is not positive, are
 * no offset at all, so a caller has nothing to animate and no work to do.
 */
export function easeOffset(distance: number, elapsedMs: number, durationMs = SCROLL_MS): number {
  if (!(distance > 0) || !Number.isFinite(distance) || !(durationMs > 0)) return 0;
  if (!(elapsedMs < durationMs)) return 0; // also NaN
  const t = elapsedMs > 0 ? elapsedMs / durationMs : 0; // a clock that went backwards starts over
  return distance * (1 - easeOutCubic(t));
}

/** `easeOffset` for a signed distance: the sign is kept. */
export function slideOffset(distance: number, elapsedMs: number, durationMs = SCROLL_MS): number {
  return Math.sign(distance) * easeOffset(Math.abs(distance), elapsedMs, durationMs) || 0;
}

/**
 * The rows the viewport moved between two scrollbars (`[offset, total, len]`, the frame's header), positive towards
 * newer output; 0 when it did not move or the change is not a scroll of the history. A change of `total` is output
 * (or the history being cut), a change of `len` a resize: neither is a jump of the viewport, and animating output
 * would be exactly the delay this module promises not to add.
 */
export function scrolledRows(
  before: readonly number[] | undefined, after: readonly number[] | undefined,
): number {
  if (!before || !after) return 0;
  const [offset0, total0, len0] = before;
  const [offset1, total1, len1] = after;
  if (![offset0, total0, len0, offset1, total1, len1].every(Number.isSafeInteger)) return 0;
  if (total0 !== total1 || len0 !== len1) return 0;
  return offset1! - offset0!;
}

/**
 * The cells a cursor that jumped from `from` to `to` passed, along the straight line: `from` itself, then on, up to
 * `max` of them, evenly spread when the line is longer. `to` is not one of them (the cursor is drawn there), and a
 * move of one cell or none has no trail.
 */
export function trailCells(from: Cell, to: Cell, max = MAX_GHOSTS): Cell[] {
  const dx = to.x - from.x, dy = to.y - from.y;
  const n = Math.max(Math.abs(dx), Math.abs(dy));
  if (!Number.isFinite(n) || n <= 1) return [];
  const count = Math.min(n, Math.max(0, Math.floor(max)));
  const out: Cell[] = [];
  for (let i = 0; i < count; i++) {
    const t = i / count; // i / n when the line is short, so the cells are the ones the line crosses
    out.push({ x: Math.round(from.x + dx * t), y: Math.round(from.y + dy * t) });
  }
  return out;
}

/**
 * The ghosts of a trail `elapsedMs` after the jump: the cells of `trailCells`, the one next to the cursor brightest
 * and the one it left faintest, all fading with an ease-out until `TRAIL_MS`, when none is left.
 */
export function trailGhosts(from: Cell, to: Cell, elapsedMs: number, max = MAX_GHOSTS): Ghost[] {
  if (!(elapsedMs < TRAIL_MS)) return [];
  const fade = 1 - easeOutCubic(elapsedMs > 0 ? elapsedMs / TRAIL_MS : 0);
  const cells = trailCells(from, to, max);
  return cells.map((c, i) => ({ ...c, alpha: (GHOST_ALPHA * fade * (i + 1)) / cells.length }));
}

// ---- the state of one pane --------------------------------------------------------------------------

/** One earlier frame still showing under a slide, and how far the rows scrolled after it was current. */
interface Layer {
  mirror: PaneMirror;
  back: number;
}

/** A frame of the pane that is drawn under the slide to fill what the newer one does not cover, at this row offset. */
export interface UnderLayer {
  mirror: PaneMirror;
  rows: number;
}

/**
 * The slide of one pane. The offset (in rows, signed: positive means the rows are drawn lower than their place) starts
 * at the rows just scrolled plus what is left of an earlier slide, so a second scroll in the middle of the first one
 * continues without a jump, and decays to 0 over `SCROLL_MS`.
 *
 * The rows the new frame does not have (the top ones when the content moved down) are the old frame's: the frames
 * before are kept as layers and drawn under, each at the offset that keeps them where they were on screen. Without
 * them the gap would be the pane background for the length of the slide.
 */
class Slide {
  private from = 0;
  private t0 = 0;
  private layers: Layer[] = [];

  constructor(private readonly now: () => number) {}

  /** The offset left now, in rows. */
  offset(): number {
    return slideOffset(this.from, this.now() - this.t0);
  }

  /** Whether the slide still moves: an offset is left. */
  active(): boolean {
    return this.offset() !== 0;
  }

  /** A scroll of `delta` rows; `before` is the frame that was on screen, `max` the rows of the viewport. */
  push(delta: number, before: PaneMirror, max: number): void {
    const left = this.offset();
    for (const l of this.layers) l.back += delta;
    this.layers.push({ mirror: before, back: delta });
    if (this.layers.length > MAX_LAYERS) this.layers.shift();
    // A jump of more than a viewport (a search hit far away) slides in from one viewport away, not from the far end.
    this.from = Math.max(-max, Math.min(max, left + delta));
    this.t0 = this.now();
  }

  /** The earlier frames that still show at least a row, oldest first, with their offset in rows. */
  under(rows: number): UnderLayer[] {
    const now = this.offset();
    const out: UnderLayer[] = [];
    for (const l of this.layers) {
      const at = now - l.back;
      if (Math.abs(at) < rows) out.push({ mirror: l.mirror, rows: at });
    }
    return out;
  }
}

/** What to draw extra for one pane now. Rows are in grid rows; the scene turns them into pixels. */
export interface PaneMotion {
  /** The grid is drawn this many rows lower than its place (negative: higher); 0 when it is not sliding. */
  rows: number;
  /** Earlier frames to draw under the grid, oldest first. */
  under: UnderLayer[];
  /** The fading ghosts of the cursor's old cells. */
  ghosts: Ghost[];
}

/** The trail of one pane: where the cursor was, where it went, when. */
interface Trail {
  from: Cell;
  to: Cell;
  t0: number;
}

/**
 * The two effects for all panes. `setEnabled` says which are allowed (the setting and not reduced motion, resolved
 * by the caller; turning one off drops what it holds at once), `frame` is told about every frame that was applied to
 * a pane's mirror, and `of` is what the scene asks when it draws.
 */
export class Motion {
  private scroll = false;
  private trail = false;
  private slides = new Map<number, Slide>();
  private trails = new Map<number, Trail>();

  constructor(private readonly now: () => number = () => performance.now()) {}

  /** Allow or forbid each effect. Forbidding one cancels what it was doing, so the next draw is the plain one. */
  setEnabled(scroll: boolean, trail: boolean): void {
    if (!scroll) this.slides.clear();
    if (!trail) this.trails.clear();
    this.scroll = scroll;
    this.trail = trail;
  }

  /**
   * A frame was applied to pane `pane`: `before` is the mirror it replaced (undefined for the first), `after` the
   * mirror now. `full` says it replaced everything (a resize, a new start, a request after a gap) and
   * `rowsChanged` that it changed rows of the grid.
   *
   * A frame that changes the grid and is not a scroll cancels the slide of its pane: what slides is no longer
   * what is shown. A frame that moves the cursor by more than one cell starts the trail; one that changes the
   * grid without such a jump ends it.
   */
  frame(pane: number, before: PaneMirror | undefined, after: PaneMirror, full: boolean, rowsChanged: boolean): void {
    if (!this.scroll && !this.trail) return;
    const same = !!before && !full && before.cols === after.cols && before.rows === after.rows;
    // The alternate screen has no history; a program there that moves the viewport redraws it, which is output.
    const delta = same && !before.modes.alt && !after.modes.alt ? scrolledRows(before.scrollbar, after.scrollbar) : 0;
    if (this.scroll) {
      if (delta !== 0) {
        let slide = this.slides.get(pane);
        if (!slide) this.slides.set(pane, (slide = new Slide(this.now)));
        slide.push(delta, before!, after.rows);
      } else if (full || rowsChanged || !same) {
        this.slides.delete(pane);
      }
    }
    if (this.trail) {
      const a = before?.cursor, b = after.cursor;
      if (b && a && same && delta === 0 && Math.max(Math.abs(b.x - a.x), Math.abs(b.y - a.y)) > 1) {
        this.trails.set(pane, { from: { x: a.x, y: a.y }, to: { x: b.x, y: b.y }, t0: this.now() });
      } else if (!b || full || !same || delta !== 0 || rowsChanged) {
        this.trails.delete(pane);
      }
    }
  }

  /**
   * What to draw extra for `pane` now, or null when nothing: no slide with an offset left and no ghost left. A
   * finished slide or trail is forgotten here.
   */
  of(pane: number, rows: number): PaneMotion | null {
    let offset = 0;
    let under: UnderLayer[] = [];
    const slide = this.slides.get(pane);
    if (slide) {
      offset = slide.offset();
      if (offset === 0) this.slides.delete(pane);
      else under = slide.under(rows);
    }
    let ghosts: Ghost[] = [];
    const trail = this.trails.get(pane);
    if (trail) {
      ghosts = trailGhosts(trail.from, trail.to, this.now() - trail.t0);
      if (ghosts.length === 0) this.trails.delete(pane);
    }
    return offset === 0 && ghosts.length === 0 ? null : { rows: offset, under, ghosts };
  }

  /** Whether anything still animates: the scene asks for another animation frame exactly while this is true. */
  active(): boolean {
    for (const slide of this.slides.values()) if (slide.active()) return true;
    const now = this.now();
    for (const t of this.trails.values()) if (now - t.t0 < TRAIL_MS) return true;
    return false;
  }

  /** A pane is gone. */
  forget(pane: number): void {
    this.slides.delete(pane);
    this.trails.delete(pane);
  }
}
