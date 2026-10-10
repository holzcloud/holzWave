// The scene: the canvas, the renderer, the cell metrics and the draw loop, plus everything that follows a layout
// change (the bar, the pane sizes the core is told, the overlay). It owns the canvas and its resize listener.
// The canvas element can be replaced once, while the renderer is chosen (see `initScene`), so nobody keeps a
// reference to it from before: modules that listen on it ask `canvas()` after `initScene` has run.
import { call } from "./bridge.js";
import { Canvas2DRenderer } from "./canvas2d.js";
import { renderBar } from "./chrome.js";
import { FONT_PX, Metrics, Renderer, font } from "./frame.js";
import { fitCells, gridRect } from "./geometry.js";
import { renderOverlay } from "./overlay.js";
import { state } from "./state.js";
import type { View } from "./types.js";
import { WebGLRenderer } from "./webgl.js";

const stats = document.getElementById("stats") as HTMLDivElement;

let renderer: Renderer | null = null;
let rendererName = "canvas2d";

/**
 * The canvas element of the page. The id survives a replacement, so this is always the live element; call it when
 * attaching listeners (after `initScene`), never at module load.
 */
export function canvas(): HTMLCanvasElement {
  return document.getElementById("term") as HTMLCanvasElement;
}

/**
 * Pick the renderer, take the first measurement and follow window resizes. Call once at start-up, before any
 * module attaches listeners to the canvas.
 */
export function initScene(): void {
  // `?renderer=canvas` or `?renderer=webgl`; WebGL2 is the default and Canvas 2D is the fallback.
  const wanted = new URLSearchParams(location.search).get("renderer");
  if (wanted === "canvas") {
    renderer = new Canvas2DRenderer(canvas());
  } else {
    try {
      renderer = new WebGLRenderer(canvas());
      rendererName = "webgl2";
    } catch {
      // A canvas that handed out a WebGL context never returns a 2D one, even when the program failed to link
      // afterwards: draw on a fresh element in its place.
      renderer = new Canvas2DRenderer(replaceCanvas());
    }
  }
  state.metrics = measure();
  window.addEventListener("resize", fit);
}

/** Put a new canvas element with the same attributes and inline properties where the current one is. */
function replaceCanvas(): HTMLCanvasElement {
  const old = canvas();
  const next = document.createElement("canvas");
  for (const { name, value } of Array.from(old.attributes)) {
    if (name !== "style") next.setAttribute(name, value); // a style attribute is copied property by property
  }
  for (const name of Array.from(old.style)) {
    next.style.setProperty(name, old.style.getPropertyValue(name), old.style.getPropertyPriority(name));
  }
  const focused = document.activeElement === old;
  // Hand the GPU context back now instead of waiting for the garbage collector.
  old.getContext("webgl2")?.getExtension("WEBGL_lose_context")?.loseContext();
  old.replaceWith(next);
  if (focused) next.focus();
  return next;
}

/** Give the keyboard to the canvas. */
export function focusCanvas(): void {
  canvas().focus();
}

function measure(): Metrics {
  const probe = document.createElement("canvas").getContext("2d")!;
  probe.font = font(0);
  return {
    cellW: probe.measureText("M").width,
    cellH: Math.ceil(FONT_PX * 1.25),
    dpr: window.devicePixelRatio || 1,
    width: canvas().clientWidth,
    height: canvas().clientHeight,
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
  const c = canvas();
  c.width = Math.round(m.width * m.dpr);
  c.height = Math.round(m.height * m.dpr);
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
