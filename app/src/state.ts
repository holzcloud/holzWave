// The frontend's shared mutable state: one object, owned by the modules that import it.
import type { Metrics } from "./frame.js";
import type { PaneMirror } from "./frames.js";
import type { PaneSize, Selection, View } from "./types.js";

export interface State {
  /** The layout the core sent last; null until the first answer. */
  view: View | null;
  /** The mirror of every pane the core has sent frames for, patched by `frame` events. */
  frames: Map<number, PaneMirror>;
  /** The size in cells and the cell size the core was told for every pane. */
  sizes: Map<number, PaneSize>;
  /** Cell size, device pixel ratio and canvas size in CSS pixels. */
  metrics: Metrics;
  /** The legacy frontend selection; plan 17 replaces it with the core's tracked selection. */
  selection: Selection | null;
  /** Draw timings of the last frames, in milliseconds and frames per second. */
  stats: { avgMs: number; maxMs: number; fps: number };
}

export const state: State = {
  view: null,
  frames: new Map(),
  sizes: new Map(),
  // Replaced by the first measurement at start-up; a unit-size cell keeps divisions finite until then.
  metrics: { cellW: 1, cellH: 1, dpr: 1, width: 0, height: 0 },
  selection: null,
  stats: { avgMs: 0, maxMs: 0, fps: 0 },
};
