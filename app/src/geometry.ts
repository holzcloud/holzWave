// Pure geometry: where panes and grids are on the canvas and which pane or cell is under a point.
// No DOM, no state: everything arrives as an argument, so node can import and test this module.
import type { Metrics, PxRect } from "./frame.js";
import type { CellSize, Divider, UnitRect, View } from "./types.js";

/** CSS px between a pane's edge and its grid on the left, right and bottom (the header covers the top). */
export const PAD = 6;

/** The height of a pane's header strip in root font sizes: 20 px at the base size of 12 px. */
export const HEADER_REM = 20 / 12;

/** The root font size at UI scale 100 %: what `headerHeight` assumes for a size that is not a size. */
const BASE_ROOT_PX = 12;

/**
 * The header height in CSS px for a root font size: `HEADER_REM` root sizes, snapped to whole device pixels so the
 * grid below it starts on a pixel edge. The UI scale changes the root size, so header and chrome scale together.
 */
export function headerHeight(rootFontPx: number, dpr: number): number {
  const root = Number.isFinite(rootFontPx) && rootFontPx > 0 ? rootFontPx : BASE_ROOT_PX;
  const ratio = Number.isFinite(dpr) && dpr > 0 ? dpr : 1;
  return Math.max(1, Math.round(HEADER_REM * root * ratio)) / ratio;
}

/** CSS px of a divider's hit area across its axis. */
export const DIVIDER_HIT = 8;

/** The pane rectangle in CSS pixels inside the canvas. */
export function paneRect(m: Metrics, r: UnitRect): PxRect {
  return { x: r.x * m.width, y: r.y * m.height, w: r.w * m.width, h: r.h * m.height };
}

/**
 * Where the grid of a pane is drawn: it starts `headerPx` below the pane's top edge (the header strip is there),
 * keeps `PAD` on the other three sides and is never negative in size. A header height that is not a length counts
 * as no header.
 */
export function gridRect(m: Metrics, r: UnitRect, headerPx: number): PxRect {
  const p = paneRect(m, r);
  const head = Number.isFinite(headerPx) && headerPx > 0 ? headerPx : 0;
  return { x: p.x + PAD, y: p.y + head, w: Math.max(0, p.w - 2 * PAD), h: Math.max(0, p.h - head - PAD) };
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

/** The cell under the point, relative to the pane's grid; the indices never go below 0 (the header is row 0). */
export function cellAt(m: Metrics, r: UnitRect, headerPx: number, x: number, y: number): [number, number] {
  const g = gridRect(m, r, headerPx);
  return [
    Math.max(0, Math.floor((x - g.x) / m.cellW)),
    Math.max(0, Math.floor((y - g.y) / m.cellH)),
  ];
}

/** How many cells fit into a pane's grid: at least 2 columns and 1 row. */
export function fitCells(m: Metrics, r: UnitRect, headerPx: number): CellSize {
  const g = gridRect(m, r, headerPx);
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
