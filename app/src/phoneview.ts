// The phone shows one pane at a time. The core's layout is unit rectangles, so the phone's view is a transform of it: the
// focused pane at the full rectangle and nothing else. Pure, so node can test it; scene.ts applies it where a view enters
// the page, so every module that measures from the view sees the full-size pane.
import { idsOf } from "./attentionstate.js";
import type { View } from "./types.js";

/**
 * The view as the phone draws it: `panes` is the focused pane alone at the full rectangle and `dividers` is empty. Every other
 * member is the core's. A view whose focus is not among its panes (an empty tab) is returned as it is: the transform never
 * invents a pane. The input is not changed.
 */
export function phoneView(view: View): View {
  if (!view.panes.some(([id]) => id === view.focus)) return view;
  return { ...view, panes: [[view.focus, { x: 0, y: 0, w: 1, h: 1 }]], dividers: [] };
}

/** The ids of the panes of the shown tab, in the core's order. */
function shownIds(view: View): number[] {
  return idsOf(view.tabs.find((t) => t.active)?.pane_ids);
}

/** The place of the focused pane among the panes of the shown tab, counted from 1 (0 when it is not among them). */
export function shownIndex(view: View): number {
  return shownIds(view).indexOf(view.focus) + 1;
}

/** How many panes the shown tab has. */
export function shownCount(view: View): number {
  const ids = shownIds(view);
  if (ids.length > 0) return ids.length;
  const n = view.tabs.find((t) => t.active)?.panes;
  return typeof n === "number" && Number.isSafeInteger(n) && n > 0 ? n : 0;
}
