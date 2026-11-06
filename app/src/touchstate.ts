// The pure model of the touch gestures (docs/design.md section 6.6): when a press is a tap, a long press, a drag or a pinch,
// how many rows a finger scrolls, how a fling fades, the steps of a pinch, and where the selection handles and the menu sit.
// No DOM, no clock, no state: every time and every size arrives as an argument, so node imports this module and the
// boundaries (499 and 500 ms, 10 px) are pinned exactly. touch.ts feeds it from pointer events.
import { UI_SCALE_RANGE } from "./settings-model.js";
import { wheelRows } from "./mouse.js";

// ---- constants ------------------------------------------------------------------------------------

/** A finger held this long without leaving the slop is a long press. */
export const LONG_PRESS_MS = 500;
/** A finger that stays within this many CSS pixels of where it went down has not moved. */
export const SLOP_PX = 10;
/** The side of a touch target in CSS pixels: a selection handle and a menu button are at least this big. */
export const HANDLE_PX = 44;
/** A pinch changes the UI scale in steps of this many percent. */
export const PINCH_STEP = 5;
/**
 * The range of a pinch: the spec's 75 to 200 percent, held inside the range the page's UI scale has (the core's), so a pinch
 * can never name a scale the preferences would refuse.
 */
export const PINCH_RANGE: readonly [number, number] = [Math.max(75, UI_SCALE_RANGE[0]), Math.min(200, UI_SCALE_RANGE[1])];
/** The time a fling lasts at most, and the time constant of its decay. */
export const FLING_MS = 600;
export const FLING_TAU_MS = 150;
/** A finger slower than this (CSS px per ms) leaves no fling; one faster than `MAX_VELOCITY` counts as that fast. */
export const FLING_MIN_VELOCITY = 0.05;
export const MAX_VELOCITY = 4;
/** A finger that rested this long before it lifted leaves no fling. */
export const FLING_REST_MS = 100;
/** The space between the selection and the menu in CSS pixels. */
export const MENU_GAP_PX = 8;
/** How far the stem of a handle reaches up into the row of the text, in CSS pixels. */
export const STEM_REACH_PX = 2;
/** How long the pinch label stays after the last finger lifted. */
export const LABEL_HOLD_MS = 800;

/** The words of the menu and of the label, exactly the spec's (a unit test holds them to docs/design.md). */
export const MENU_WORDS = { copy: "Copy", paste: "Paste" } as const;
export const scaleLabel = (percent: number): string => `UI scale ${percent} %`;

// ---- tap, long press, drag, pinch -----------------------------------------------------------------

export type Gesture = "pending" | "tap" | "longpress" | "drag" | "pinch";

export interface PressFacts {
  /** The fingers down now, counting one that was just lifted when `released` is set. */
  pointers: number;
  /** The farthest the first finger has been from where it went down, in CSS pixels (the caller keeps the maximum). */
  movedPx: number;
  /** How long the first finger has been down. */
  elapsedMs: number;
  /** The first finger has been lifted. */
  released: boolean;
}

/**
 * What a press is, from the facts. Two fingers are a pinch, whatever else was going on (the caller cancels its pending
 * long press and drag). Moving farther than the slop before the long press is a drag, and stays one. A finger down for
 * `LONG_PRESS_MS` within the slop is a long press (the caller's timer asks with the elapsed time; a release asks too,
 * for a timer that ran late). A lift before that, within the slop, is a tap. Everything else is still pending.
 */
export function classify(f: PressFacts): Gesture {
  if (f.pointers >= 2) return "pinch";
  if (!(f.movedPx <= SLOP_PX)) return "drag"; // also true for a distance that is not a number: never a tap
  if (f.elapsedMs >= LONG_PRESS_MS) return "longpress";
  return f.released ? "tap" : "pending";
}

// ---- scrolling --------------------------------------------------------------------------------------

/**
 * Whole rows for a vertical finger movement of `dyPx` (positive: the finger moved down) and the new remainder. The sign
 * is the wheel's: a finger moving down brings older output into view, which is negative. Fractions are carried in the
 * remainder, never lost and never counted twice. No cell height gives no rows.
 */
export function dragRows(dyPx: number, remainder: number, cellH: number): { rows: number; remainder: number } {
  return wheelRows(remainder, -dyPx, 0, cellH);
}

/** The speed of the finger in CSS px per ms, signed (positive: down), smoothed against the speed before (0 for none). */
export function smoothVelocity(previous: number, dyPx: number, dtMs: number): number {
  if (!Number.isFinite(dyPx) || !Number.isFinite(dtMs)) return Number.isFinite(previous) ? previous : 0;
  const now = dyPx / Math.max(1, dtMs);
  const prev = Number.isFinite(previous) ? previous : 0;
  const mixed = prev === 0 ? now : 0.7 * now + 0.3 * prev;
  return Math.max(-MAX_VELOCITY, Math.min(MAX_VELOCITY, mixed));
}

/** The speed the fling starts with: the last speed, or none when the finger rested before it lifted. */
export function releaseVelocity(velocity: number, sinceLastMoveMs: number): number {
  if (!Number.isFinite(velocity) || !Number.isFinite(sinceLastMoveMs) || sinceLastMoveMs > FLING_REST_MS) return 0;
  return velocity;
}

/**
 * The wheel rows a fling has produced `elapsedMs` after the finger lifted, in total (the caller sends the difference from
 * what it sent before). `velocityPxPerMs` is the finger's (positive: down). The distance follows an exponential decay and
 * stops for good at `FLING_MS`; the rows are whole and have the wheel's sign, which never flips. A finger slower than
 * `FLING_MIN_VELOCITY`, reduced motion and a cell height that is not a length give 0.
 */
export function flingRows(velocityPxPerMs: number, elapsedMs: number, cellH: number, reducedMotion = false): number {
  if (reducedMotion || !(cellH > 0) || !Number.isFinite(velocityPxPerMs) || !Number.isFinite(elapsedMs)) return 0;
  const speed = Math.min(MAX_VELOCITY, Math.abs(velocityPxPerMs));
  if (speed < FLING_MIN_VELOCITY || elapsedMs <= 0) return 0;
  const t = Math.min(elapsedMs, FLING_MS);
  const distance = speed * FLING_TAU_MS * (1 - Math.exp(-t / FLING_TAU_MS));
  const rows = Math.trunc(distance / cellH);
  if (rows === 0) return 0;
  return velocityPxPerMs > 0 ? -rows : rows;
}

// ---- pinch -----------------------------------------------------------------------------------------

/**
 * The UI scale a pinch gives: the scale at the start times the ratio of the finger distances, rounded to a multiple of
 * `PINCH_STEP` and held to `PINCH_RANGE`. A start distance that is not a length, or a distance that is not a number,
 * leaves the start scale.
 */
export function pinchScale(startScale: number, startDist: number, dist: number): number {
  if (!(startDist > 0) || !Number.isFinite(startDist) || !Number.isFinite(dist) || !Number.isFinite(startScale)) return startScale;
  const raw = (startScale * dist) / startDist;
  const stepped = Math.round(raw / PINCH_STEP) * PINCH_STEP;
  return Math.max(PINCH_RANGE[0], Math.min(PINCH_RANGE[1], stepped));
}

/** The distance between two points. */
export const distance = (a: { x: number; y: number }, b: { x: number; y: number }): number => Math.hypot(a.x - b.x, a.y - b.y);

// ---- handles and menu ---------------------------------------------------------------------------------

export interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** A cell as `[column, row]` of the viewport, the way the core's spans and commands name it. */
export type Cell = [number, number];

/** Where the cells of the shown pane are on the stage in CSS pixels: the origin of the grid and the size of one cell. */
export interface CellGeometry {
  originX: number;
  originY: number;
  cellW: number;
  cellH: number;
}

export interface HandleBox extends Box {
  /** The point of the selection the handle marks: the left edge of the start cell or the right edge of the end cell, at the bottom of its row. */
  tipX: number;
  tipY: number;
}

/**
 * Put `v` inside `[lo, hi]`; when the range is empty or not a range, `lo`. A NaN is `lo`.
 */
function within(v: number, lo: number, hi: number): number {
  if (!Number.isFinite(v)) return lo;
  return Math.max(lo, Math.min(v, Math.max(lo, hi)));
}

/**
 * The two handle boxes of a selection from `start` to `end` (the first and the last selected cell, both inclusive). A box
 * is `HANDLE_PX` square, hangs from the bottom of its row with its stem reaching `STEM_REACH_PX` into the row, is centred
 * on the edge it marks, and is moved to stay inside the stage. `tipX` and `tipY` are the unclamped edge point. An end that
 * lies before the start is swapped, so the start is never after the end.
 */
export function handleBoxes(start: Cell, end: Cell, g: CellGeometry, stage: { w: number; h: number }): { start: HandleBox; end: HandleBox } {
  const [from, to] = orderCells(start, end);
  const make = (cell: Cell, right: boolean): HandleBox => {
    const tipX = g.originX + (cell[0] + (right ? 1 : 0)) * g.cellW;
    const tipY = g.originY + (cell[1] + 1) * g.cellH;
    return {
      x: within(tipX - HANDLE_PX / 2, 0, stage.w - HANDLE_PX),
      y: within(tipY - STEM_REACH_PX, 0, stage.h - HANDLE_PX),
      w: HANDLE_PX,
      h: HANDLE_PX,
      tipX,
      tipY,
    };
  };
  return { start: make(from, false), end: make(to, true) };
}

/** `a` and `b` in reading order: by row, then by column. */
export function orderCells(a: Cell, b: Cell): [Cell, Cell] {
  const before = a[1] < b[1] || (a[1] === b[1] && a[0] <= b[0]);
  return before ? [a, b] : [b, a];
}

/**
 * The cell a handle names while it is dragged: the finger, less the distance it held the handle by at the grab, gives the
 * tip; the cell is the one beside the tip inside the row above it, on the side of the selection (right of the left edge of
 * the start, left of the right edge of the end). The cell is not clamped to the grid: the page does that.
 */
export function handleTarget(finger: { x: number; y: number }, grab: { dx: number; dy: number }, end: boolean, g: CellGeometry): Cell {
  const tipX = finger.x - grab.dx;
  const tipY = finger.y - grab.dy;
  const col = Math.floor((tipX + (end ? -1 : 1) * (g.cellW / 2) - g.originX) / g.cellW);
  const row = Math.floor((tipY - g.cellH / 2 - g.originY) / g.cellH);
  return [Number.isFinite(col) ? col : 0, Number.isFinite(row) ? row : 0];
}

/**
 * Where the menu goes: centred over the selection, `MENU_GAP_PX` above it, below it when the stage has no room above, and
 * always inside the stage horizontally. `selection` is the box of the selected rows (give it the handles' height when the
 * menu may land below them).
 */
export function menuPlacement(selection: Box, stage: Box, menu: { w: number; h: number }): { x: number; y: number } {
  const above = selection.y - MENU_GAP_PX - menu.h;
  const y = above >= stage.y ? above : selection.y + selection.h + MENU_GAP_PX;
  const centred = selection.x + selection.w / 2 - menu.w / 2;
  const x = within(centred, stage.x, stage.x + stage.w - menu.w);
  return { x, y };
}
