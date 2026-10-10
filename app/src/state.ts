// The frontend's shared mutable state: one object, owned by the modules that import it.
import type { Metrics } from "./frame.js";
import type { PaneMirror } from "./frames.js";
import type { PaneSize, View } from "./types.js";

export interface State {
  /** The layout the core sent last; null until the first answer. */
  view: View | null;
  /** The mirror of every pane the core has sent frames for, patched by `frame` events. */
  frames: Map<number, PaneMirror>;
  /** The size in cells and the cell size the core was told for every pane. */
  sizes: Map<number, PaneSize>;
  /** The title each pane's program set, as the core sanitised it. Render with `textContent` only. */
  titles: Map<number, string>;
  /** The working directory each pane reported, as the core sanitised it. Render with `textContent` only. */
  cwds: Map<number, string>;
  /** The panes that exited. Their mirror, size, title and cwd are gone; late events about them are dropped. */
  exited: Set<number>;
  /** Cell size, device pixel ratio and canvas size in CSS pixels. */
  metrics: Metrics;
  /**
   * Updated every 30 draws: the average and maximum draw time of those draws in milliseconds, and the arrival rate
   * of `frame` events per second (events that came in, not frames that were drawn).
   */
  stats: { avgMs: number; maxMs: number; fps: number };
}

export const state: State = {
  view: null,
  frames: new Map(),
  sizes: new Map(),
  titles: new Map(),
  cwds: new Map(),
  exited: new Set(),
  // Replaced by the first measurement at start-up; a unit-size cell keeps divisions finite until then.
  metrics: { cellW: 1, cellH: 1, dpr: 1, width: 0, height: 0 },
  stats: { avgMs: 0, maxMs: 0, fps: 0 },
};
