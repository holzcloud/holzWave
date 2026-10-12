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

// ---- between windows (DESK-29 part 2) ---------------------------------------------------------------------------
// A drag that ends outside the window tears a pane or a tab off, and one that ends over another window is handed to
// that window, which works out where in it the drop lands. The maths of both sides is here, without the DOM.

/** How far outside the window, in CSS pixels, a release has to be to tear off; closer than that it is a slip. */
export const TEAR_MARGIN = 24;

/**
 * Whether a pointer released at the client point (x, y) of a window of `width` by `height` CSS pixels is outside it by
 * at least `margin` in some direction. A release just over the edge, or in the window, is not.
 */
export function outsideBeyond(x: number, y: number, width: number, height: number, margin = TEAR_MARGIN): boolean {
  if (![x, y, width, height, margin].every(Number.isFinite)) return false;
  return Math.max(-x, x - width, -y, y - height) >= margin;
}

/** What is under the pointer in the window that receives a drop, as the page worked it out from its DOM and layout. */
export type Under =
  | { part: "tab"; index: number }
  | { part: "chip"; index: number }
  | { part: "bar" }
  | { part: "pane"; pane: number; zone: Zone | null }
  | { part: "none" };

/** The answer to an incoming drop: the command and its arguments. */
export type Attach =
  | { cmd: "attach_pane"; args: { pane: number; target: number; edge: Zone } | { pane: number; workspace: number } }
  | { cmd: "attach_tab"; args: { pane: number; workspace: number } };

/**
 * What a drop of `kind` (the pane `pane`, or the tab it is in) does where the pointer is, or null when it lands
 * nowhere. `tabs` are the tabs of the receiving window (each with the pane that has its focus) and `workspace` its shown
 * workspace.
 *
 * A pane lands on the side or the middle of a pane (splits or swaps there), on a tab (next to that tab's focus pane, as
 * in one window), or on the free part of the bar or a workspace chip as a tab of its own. A tab lands in the workspace
 * of the chip it is over and anywhere else in the shown one. A pane that is over no zone (outside the grid of a pane)
 * and a pointer over nothing land nowhere.
 */
export function attachFor(
  kind: "pane" | "tab",
  pane: number,
  under: Under,
  tabs: readonly { focus: number }[],
  workspace: number,
): Attach | null {
  switch (under.part) {
    case "chip":
      return kind === "pane"
        ? { cmd: "attach_pane", args: { pane, workspace: under.index } }
        : { cmd: "attach_tab", args: { pane, workspace: under.index } };
    case "tab": {
      if (kind === "tab") return { cmd: "attach_tab", args: { pane, workspace } };
      const into = tabs[under.index];
      return into ? { cmd: "attach_pane", args: { pane, target: into.focus, edge: "right" } } : null;
    }
    case "bar":
      return kind === "pane"
        ? { cmd: "attach_pane", args: { pane, workspace } }
        : { cmd: "attach_tab", args: { pane, workspace } };
    case "pane":
      if (kind === "tab") return { cmd: "attach_tab", args: { pane, workspace } };
      return under.zone ? { cmd: "attach_pane", args: { pane, target: under.pane, edge: under.zone } } : null;
    default:
      return null;
  }
}

/**
 * The pane the shell is told about and the panes this page lets go of when `source` is released outside the window: a
 * pane is itself; a tab is named by the pane that has its focus, and its panes are known only when it is the shown tab
 * (the layout lists no others), so for another tab only that pane. Null for a tab that is not in the layout.
 */
export function leavingPanes(
  source: { kind: "pane"; pane: number } | { kind: "tab"; index: number },
  view: { tabs: readonly { active: boolean; focus: number }[]; panes: readonly (readonly [number, unknown])[] },
): { kind: "pane" | "tab"; pane: number; ids: number[] } | null {
  if (source.kind === "pane") return { kind: "pane", pane: source.pane, ids: [source.pane] };
  const tab = view.tabs[source.index];
  if (!tab) return null;
  return { kind: "tab", pane: tab.focus, ids: tab.active ? view.panes.map(([id]) => id) : [tab.focus] };
}

/** How long a pane stays marked as leaving, a little longer than the shell waits for the other window's answer. */
export const LEAVE_MS = 6000;

/**
 * The panes this page has let go of by a gesture (released outside, torn off) and whose departure it has not seen in a
 * layout yet. The layout lists only the panes of the shown tab, so "not in the layout" cannot tell a pane that left the
 * window from one in another tab: the page names the pane itself, when it lets go of it, and forgets it when a layout
 * confirms it is gone. A mark that nothing confirmed (the other window never answered) lapses.
 */
export class Leaving {
  private marks = new Map<number, number>();

  /** The panes `ids` are being let go of at `now` (milliseconds on a clock that only goes forward). */
  mark(ids: Iterable<number>, now: number): void {
    for (const id of ids) this.marks.set(id, now);
  }

  /** The gesture of these panes ended without letting go (cancelled, refused or failed). */
  unmark(ids: Iterable<number>): void {
    for (const id of ids) this.marks.delete(id);
  }

  /**
   * The marked panes that a layout showing `shown` does not have: they have left and are no longer marked. A marked pane
   * that is shown stays marked until its mark lapses, because the other window may still answer; one whose mark lapsed
   * is dropped.
   */
  take(shown: ReadonlySet<number>, now: number): number[] {
    const gone: number[] = [];
    for (const [id, at] of this.marks) {
      if (now - at > LEAVE_MS) this.marks.delete(id);
      else if (!shown.has(id)) {
        this.marks.delete(id);
        gone.push(id);
      }
    }
    return gone;
  }

  /** How many panes are marked. */
  get size(): number {
    return this.marks.size;
  }
}
