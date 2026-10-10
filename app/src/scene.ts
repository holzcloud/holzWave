// The scene: the canvas, the renderer, the cell metrics and the draw loop, plus everything that follows a layout
// change (the bar, the pane sizes the core is told, the overlay). It owns the canvas and its resize listener.
// The canvas element can be replaced once, while the renderer is chosen (see `initScene`), so nobody keeps a
// reference to it from before: modules that listen on it ask `canvas()` after `initScene` has run.
import { call, listen } from "./bridge.js";
import { Canvas2DRenderer } from "./canvas2d.js";
import { renderBar } from "./chrome.js";
import { BOLD, FONT_PX, ITALIC, Metrics, Renderer, font } from "./frame.js";
import { applyFrame, spansSignature, toRenderFrame } from "./frames.js";
import type { FrameMsg } from "./frames.js";
import { fitCells, gridRect } from "./geometry.js";
import { renderOverlay } from "./overlay.js";
import { onSelectionSpans } from "./selection.js";
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
 * Pick the renderer, load the terminal font, take the first measurement and follow window resizes. Call once at
 * start-up and wait for it, before any module attaches listeners to the canvas.
 */
export async function initScene(): Promise<void> {
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
  await loadFonts();
  state.metrics = measure();
  window.addEventListener("resize", fit);
}

/**
 * The three bundled faces at the terminal size, loaded before anything is measured: a measurement against a
 * fallback face would size the grid for the wrong font. A face that cannot be loaded is not fatal; the page then
 * runs on the fallback families of `FONT_FAMILY`.
 */
async function loadFonts(): Promise<void> {
  try {
    await Promise.all([font(0), font(BOLD), font(ITALIC)].map((f) => document.fonts.load(f)));
  } catch {
    // keep going with the fallback face
  }
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

/**
 * Cell size in CSS pixels, snapped so that `cell * dpr` is a whole number of device pixels: the glyph grid the
 * renderers draw, the overlay and the hit tests then all use the same pitch (a fractional cell drifts at dpr 2).
 */
function measure(): Metrics {
  const probe = document.createElement("canvas").getContext("2d")!;
  probe.font = font(0);
  const dpr = window.devicePixelRatio || 1;
  const snap = (cssPx: number) => Math.max(1, Math.round(cssPx * dpr)) / dpr;
  return {
    cellW: snap(probe.measureText("M").width),
    cellH: snap(Math.ceil(FONT_PX * 1.25)),
    dpr,
    width: canvas().clientWidth,
    height: canvas().clientHeight,
  };
}

// ---- layout changes -----------------------------------------------------------------------------

/** Take a new layout: the one place every answer and event of the core with a view ends up. */
export function applyView(v: View): void {
  state.view = v;
  renderBar(v, applyView);
  syncSizes();
  renderOverlay();
  schedule();
}

/**
 * Tell the core how many cells each visible pane has and how big a cell is in device pixels (it answers size
 * reports with it); the first call also starts its shell and then asks for a full frame, because a webview that
 * reloaded attaches to a pane that is already running and may stay idle.
 */
function syncSizes(): void {
  const view = state.view;
  if (!view) return;
  const m = state.metrics;
  const cellW = Math.max(1, Math.round(m.cellW * m.dpr));
  const cellH = Math.max(1, Math.round(m.cellH * m.dpr));
  for (const [id, r] of view.panes) {
    const { cols, rows } = fitCells(m, r);
    const prev = state.sizes.get(id);
    if (!prev) {
      state.sizes.set(id, { cols, rows, cellW, cellH });
      void call("start_pane", { pane: id, cols, rows, cell_w: cellW, cell_h: cellH })
        .then(() => call("request_full", { pane: id }))
        .catch(() => {}); // a pane that left the layout meanwhile is not an error of the page
    } else if (prev.cols !== cols || prev.rows !== rows || prev.cellW !== cellW || prev.cellH !== cellH) {
      state.sizes.set(id, { cols, rows, cellW, cellH });
      void call("resize", { pane: id, cols, rows, cell_w: cellW, cell_h: cellH });
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

// ---- events of the core ---------------------------------------------------------------------------

/** Panes whose sequence gap was reported: frames are ignored until the full frame that answers the request. */
const waiting = new Set<number>();

/** One `frame` event: patch the pane's mirror, or on a gap ask for a full frame (once) and wait for it. */
function onFrame(msg: FrameMsg): void {
  if (typeof msg !== "object" || msg === null || !Number.isSafeInteger(msg.pane)) return;
  const pane = msg.pane;
  if (!msg.full && waiting.has(pane)) return;
  const { mirror, gap } = applyFrame(state.frames.get(pane), msg);
  if (gap) {
    waiting.add(pane);
    void call("request_full", { pane }).catch(() => {});
    return;
  }
  if (!mirror) return;
  if (msg.full) waiting.delete(pane);
  const before = spansSignature(state.frames.get(pane)?.selection);
  state.frames.set(pane, mirror);
  // The cache of the selection's text follows the spans; the highlight is redrawn when the focused pane's change.
  onSelectionSpans(pane, mirror.selection, msg.changed);
  if (pane === state.view?.focus && before !== spansSignature(mirror.selection)) renderOverlay();
  schedule();
}

/** Listen to the events a pane produces. Call once at start-up, before the first layout is asked for. */
export async function initEvents(): Promise<void> {
  await listen<FrameMsg>("frame", onFrame);
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
    const mirror = state.frames.get(id);
    if (!mirror) continue;
    renderer.drawPane(toRenderFrame(mirror, id === view.focus), gridRect(state.metrics, r));
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
