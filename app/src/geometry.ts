// Pure geometry: where panes and grids are on the canvas and which pane or cell is under a point.
// No DOM, no state: everything arrives as an argument, so node can import and test this module.
import type { Metrics, PxRect } from "./frame.js";
import type { CellSize, Divider, Selection, UnitRect, View } from "./types.js";

/** CSS px between a pane's edge and its grid. */
export const PAD = 6;

/** CSS px of a divider's hit area across its axis. */
export const DIVIDER_HIT = 8;

/** The pane rectangle in CSS pixels inside the canvas. */
export function paneRect(m: Metrics, r: UnitRect): PxRect {
  return { x: r.x * m.width, y: r.y * m.height, w: r.w * m.width, h: r.h * m.height };
}

/** Where the grid of a pane is drawn: the pane rectangle without padding, never negative in size. */
export function gridRect(m: Metrics, r: UnitRect): PxRect {
  const p = paneRect(m, r);
  return { x: p.x + PAD, y: p.y + PAD, w: Math.max(0, p.w - 2 * PAD), h: Math.max(0, p.h - 2 * PAD) };
}

/** The pane containing the point (CSS pixels inside the canvas), or null outside every pane. */
export function paneAt(m: Metrics, view: View | null, x: number, y: number): [number, UnitRect] | null {
  if (!view) return null;
  for (const [id, r] of view.panes) {
    const p = paneRect(m, r);
    if (x >= p.x && x < p.x + p.w && y >= p.y && y < p.y + p.h) return [id, r];
  }
  return null;
}

/** The cell under the point, relative to the pane's grid; the indices never go below 0. */
export function cellAt(m: Metrics, r: UnitRect, x: number, y: number): [number, number] {
  const g = gridRect(m, r);
  return [
    Math.max(0, Math.floor((x - g.x) / m.cellW)),
    Math.max(0, Math.floor((y - g.y) / m.cellH)),
  ];
}

/** How many cells fit into a pane's grid: at least 2 columns and 1 row. */
export function fitCells(m: Metrics, r: UnitRect): CellSize {
  const g = gridRect(m, r);
  return {
    cols: Math.max(2, Math.floor(g.w / m.cellW)),
    rows: Math.max(1, Math.floor(g.h / m.cellH)),
  };
}

/** The hit area of a divider in CSS pixels: `DIVIDER_HIT` wide across its axis, the whole area along it. */
export function dividerRect(m: Metrics, d: Divider): PxRect {
  const a = paneRect(m, d.area);
  return d.axis === "Row"
    ? { x: a.x + a.w * d.ratio - DIVIDER_HIT / 2, y: a.y, w: DIVIDER_HIT, h: a.h }
    : { x: a.x, y: a.y + a.h * d.ratio - DIVIDER_HIT / 2, w: a.w, h: DIVIDER_HIT };
}

/**
 * The ratio a pointer at (x, y) means inside a split's `area` (all in CSS pixels of the canvas).
 * Not finite when the area has no size along the axis: callers skip those.
 */
export function ratioAt(area: PxRect, axis: Divider["axis"], x: number, y: number): number {
  return axis === "Row" ? (x - area.x) / area.w : (y - area.y) / area.h;
}

/** The selection's ends in reading order: the start first. */
export function ordered(s: Selection): [[number, number], [number, number]] {
  const [a, b] = [s.a, s.b];
  return a[1] < b[1] || (a[1] === b[1] && a[0] <= b[0]) ? [a, b] : [b, a];
}
