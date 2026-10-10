// Pure helpers for mouse reporting and the wheel (D-24). No DOM at import time, no state: node imports this module.

/** The modifier bits of the `mouse` and `wheel` commands (same set as the key commands). */
export const MOD_SHIFT = 1;
export const MOD_ALT = 2;
export const MOD_CTRL = 4;
export const MOD_SUPER = 8;

/** The modifier bit set of an input event. */
export function modifierBits(e: { shiftKey: boolean; altKey: boolean; ctrlKey: boolean; metaKey: boolean }): number {
  return (e.shiftKey ? MOD_SHIFT : 0) | (e.altKey ? MOD_ALT : 0) | (e.ctrlKey ? MOD_CTRL : 0) | (e.metaKey ? MOD_SUPER : 0);
}

/** A whole number in 0..max: +Infinity becomes max, NaN and -Infinity 0, and a max that is negative or not a number 0. */
function wholeWithin(v: number, max: number): number {
  const top = Number.isNaN(max) ? 0 : Math.max(0, Math.round(max));
  if (Number.isNaN(v)) return 0;
  return Math.min(top, Math.max(0, Math.round(v)));
}

/**
 * A point of the page as the core wants it: whole device pixels relative to the origin of a pane's grid. `canvas` is
 * the canvas rectangle on the page, `grid` the grid rectangle inside the canvas (both in CSS pixels), `dpr` the device
 * pixel ratio. The result is clamped into the grid: 0 up to its size minus one pixel, so a point on the far edge still
 * names a cell of the grid.
 */
export function gridPoint(
  clientX: number,
  clientY: number,
  canvas: { left: number; top: number },
  grid: { x: number; y: number; w: number; h: number },
  dpr: number,
): [number, number] {
  const ratio = Number.isFinite(dpr) && dpr > 0 ? dpr : 1;
  const x = (clientX - canvas.left - grid.x) * ratio;
  const y = (clientY - canvas.top - grid.y) * ratio;
  return [wholeWithin(x, grid.w * ratio - 1), wholeWithin(y, grid.h * ratio - 1)];
}

/** Rows a wheel line is worth when the browser reports lines (`deltaMode` 1). */
export const ROWS_PER_LINE = 3;

/**
 * Whole rows of a wheel delta. `remainder` is what earlier events left over (in rows); the result holds the whole
 * rows to send and the new remainder, so a slow trackpad scroll adds up instead of being lost. The sign is that of
 * `deltaY`: positive is towards newer output. `deltaMode` is the DOM's: 0 pixels, 1 lines (three rows each), 2 pages
 * (`pageRows` rows each). A delta that is not a number, or no cell height, leaves the remainder as it was.
 */
export function wheelRows(
  remainder: number,
  deltaY: number,
  deltaMode: number,
  cellH: number,
  pageRows = 24,
): { rows: number; remainder: number } {
  const carried = Number.isFinite(remainder) ? remainder : 0;
  if (!Number.isFinite(deltaY) || !(cellH > 0)) return { rows: 0, remainder: carried };
  const delta = deltaMode === 1 ? deltaY * ROWS_PER_LINE : deltaMode === 2 ? deltaY * pageRows : deltaY / cellH;
  const total = carried + delta;
  // `|| 0` turns -0 into 0, so a half row backwards is `rows: 0` and not a negative zero.
  const rows = Math.trunc(total) || 0;
  return { rows, remainder: total - rows || 0 };
}
