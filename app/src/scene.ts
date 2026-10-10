// The scene: the canvas, the renderer, the cell metrics and the draw loop, plus everything that follows a layout
// change (the bar, the pane sizes the core is told, the overlay). It owns the canvas and its resize listener.
import { call } from "./bridge.js";
import { Canvas2DRenderer } from "./canvas2d.js";
import { renderBar } from "./chrome.js";
import { FONT_PX, Metrics, Renderer, font } from "./frame.js";
import { fitCells, gridRect } from "./geometry.js";
import { renderOverlay } from "./overlay.js";
import { state } from "./state.js";
import type { View } from "./types.js";
import { WebGLRenderer } from "./webgl.js";

const canvas = document.getElementById("term") as HTMLCanvasElement;
const stats = document.getElementById("stats") as HTMLDivElement;

let renderer: Renderer | null = null;
let rendererName = "canvas2d";

/** Pick the renderer, take the first measurement and follow window resizes. Call once at start-up. */
export function initScene(): void {
  // `?renderer=canvas` or `?renderer=webgl`; WebGL2 is the default and Canvas 2D is the fallback.
  const wanted = new URLSearchParams(location.search).get("renderer");
  try {
    if (wanted === "canvas") throw new Error("canvas requested");
    renderer = new WebGLRenderer(canvas);
    rendererName = "webgl2";
  } catch {
    renderer = new Canvas2DRenderer(canvas);
  }
  state.metrics = measure();
  window.addEventListener("resize", fit);
}

/** Give the keyboard to the canvas. */
export function focusCanvas(): void {
  canvas.focus();
}

function measure(): Metrics {
  const probe = document.createElement("canvas").getContext("2d")!;
  probe.font = font(0);
  return {
    cellW: probe.measureText("M").width,
    cellH: Math.ceil(FONT_PX * 1.25),
    dpr: window.devicePixelRatio || 1,
    width: canvas.clientWidth,
    height: canvas.clientHeight,
  };
}

// ---- layout changes -----------------------------------------------------------------------------

/** Take a new layout: the one place every answer and event of the core with a view ends up. */
export function applyView(v: View): void {
  state.view = v;
  const live = new Set(v.panes.map(([id]) => id));
  if (state.selection && !live.has(state.selection.pane)) state.selection = null;
  renderBar(v, applyView);
  syncSizes();
  renderOverlay();
  schedule();
}

/** Tell the core how many cells each visible pane has; the first call also starts its shell. */
function syncSizes(): void {
  const view = state.view;
  if (!view) return;
  for (const [id, r] of view.panes) {
    const { cols, rows } = fitCells(state.metrics, r);
    const prev = state.sizes.get(id);
    if (!prev) {
      state.sizes.set(id, { cols, rows });
      void call("start_pane", { pane: id, cols, rows });
    } else if (prev.cols !== cols || prev.rows !== rows) {
      state.sizes.set(id, { cols, rows });
      void call("resize", { pane: id, cols, rows });
    }
  }
}

/** Re-measure after a window or font change: resize the canvas, tell the renderer and the core. */
export function fit(): void {
  state.metrics = measure();
  const m = state.metrics;
  canvas.width = Math.round(m.width * m.dpr);
  canvas.height = Math.round(m.height * m.dpr);
  renderer?.configure(m);
  syncSizes();
  renderOverlay();
  schedule();
}

// ---- drawing -------------------------------------------------------------------------------------

const draws: number[] = [];
let drawn = 0;
let pending = false;
const started = performance.now();

function draw(): void {
  pending = false;
  const view = state.view;
  if (!view || !renderer) return;
  const t0 = performance.now();
  const first = view.panes.map(([id]) => state.frames.get(id)).find(Boolean);
  renderer.begin(first?.bg ?? 0x14091a);
  for (const [id, r] of view.panes) {
    const f = state.frames.get(id);
    if (!f) continue;
    renderer.drawPane(id === view.focus ? f : { ...f, cursor: null }, gridRect(state.metrics, r));
  }
  draws.push(performance.now() - t0);
  if (draws.length > 120) draws.shift();
  drawn++;
  if (drawn % 30 === 0) {
    state.stats = {
      avgMs: draws.reduce((a, b) => a + b, 0) / draws.length,
      maxMs: Math.max(...draws),
      fps: drawn / ((performance.now() - started) / 1000),
    };
    const s = state.stats;
    stats.textContent = `${rendererName}  ${view.panes.length} pane(s)  cpu ${s.avgMs.toFixed(2)} ms  max ${s.maxMs.toFixed(2)} ms  ${s.fps.toFixed(0)} fps`;
  }
}

/** Ask for one draw on the next animation frame; any number of calls before it make one draw. */
export function schedule(): void {
  if (!pending) {
    pending = true;
    requestAnimationFrame(draw);
  }
}
