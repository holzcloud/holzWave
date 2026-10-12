// Pure maths for dropping a pane on a pane (DESK-20): which part of the target the pointer is over and what the pane
// would take there. No DOM, no state: node imports and tests this module.
import type { PxRect } from "./frame.js";

/** Where on a pane a drop lands; the names are the core's (`Edge`). The middle swaps, a side splits there. */
export type Zone = "left" | "right" | "top" | "bottom" | "center";

/** The share of the pane's width or height, from each edge, in which a drop splits instead of swapping. */
export const EDGE_BAND = 0.25;

/**
 * The zone of `rect` at the point (x, y): inside the outer quarter on a side that side, nearest side first at a
 * corner (by share of the pane, so a wide pane and a narrow one feel alike), else the middle. A point outside the
 * rectangle, a rectangle without area and a point that is not a number have no zone.
 */
export function dropZone(rect: PxRect, x: number, y: number): Zone | null {
  if (![rect.x, rect.y, rect.w, rect.h, x, y].every(Number.isFinite) || rect.w <= 0 || rect.h <= 0) return null;
  const fx = (x - rect.x) / rect.w;
  const fy = (y - rect.y) / rect.h;
  if (fx < 0 || fx > 1 || fy < 0 || fy > 1) return null;
  const sides: [Zone, number][] = [
    ["left", fx],
    ["right", 1 - fx],
    ["top", fy],
    ["bottom", 1 - fy],
  ];
  let best: [Zone, number] = sides[0] as [Zone, number];
  for (const s of sides) if (s[1] < best[1]) best = s;
  return best[1] < EDGE_BAND ? best[0] : "center";
}

/** The part of `rect` the dragged pane would take: half of it on a side, all of it for a swap. */
export function outlineOf(rect: PxRect, zone: Zone): PxRect {
  switch (zone) {
    case "left":
      return { ...rect, w: rect.w / 2 };
    case "right":
      return { ...rect, x: rect.x + rect.w / 2, w: rect.w / 2 };
    case "top":
      return { ...rect, h: rect.h / 2 };
    case "bottom":
      return { ...rect, y: rect.y + rect.h / 2, h: rect.h / 2 };
    default:
      return { ...rect };
  }
}

/** Whether a pointer that went from `from` to `to` has moved far enough to be a drag and not a click. */
export function beyondSlop(from: { x: number; y: number }, to: { x: number; y: number }, slop = 4): boolean {
  return Math.hypot(to.x - from.x, to.y - from.y) >= slop;
}

/** The item after (`step` 1) or before (`step` -1) `current` in `items`, wrapping; undefined when there is no other one. */
export function neighbour<T>(items: readonly T[], current: T, step: 1 | -1): T | undefined {
  const at = items.indexOf(current);
  if (at < 0 || items.length < 2) return undefined;
  return items[(at + step + items.length) % items.length];
}
