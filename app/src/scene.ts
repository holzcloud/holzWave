// The scene: the canvas, the renderer, the cell metrics and the draw loop, plus everything that follows a layout
// change (the bar, the pane sizes the core is told, the overlay). It owns the canvas and its resize listener.
// The canvas element can be replaced once, while the renderer is chosen (see `initScene`), so nobody keeps a
// reference to it from before: modules that listen on it ask `canvas()` after `initScene` has run.
import { blinkSetting, cursorTrailOn, effectiveBlink, motionReduced, onPreferenceChange, smoothScrollOn } from "./a11y.js";
import { call, listen } from "./bridge.js";
import { Canvas2DRenderer } from "./canvas2d.js";
import { renderBar } from "./chrome.js";
import { BOLD, ITALIC, Metrics, Renderer, font, getFontPx } from "./frame.js";
import type { Frame, PaneFx } from "./frame.js";
import { RateMeter, applyFrame, decorationSignature, forgetPane, leavePane, toRenderFrame } from "./frames.js";
import type { FrameMsg, PaneMirror } from "./frames.js";
import { Leaving } from "./dropzone.js";
import { fitCells, gridRect, headerHeight } from "./geometry.js";
import { renderOverlay } from "./overlay.js";
import { renderPaneHeads } from "./panehead.js";
import { Motion } from "./scroll.js";
import { initSettings } from "./settings.js";
import { forgetSelection, onSelectionSpans } from "./selection.js";
import { state } from "./state.js";
import { renderStatus } from "./status.js";
import type { View } from "./types.js";
import { WebGLRenderer } from "./webgl.js";

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
      // A lost context (GPU reset, sleep and wake, a GPU switch) is rebuilt by the renderer; the page then draws the
      // frames it has again.
      renderer = new WebGLRenderer(canvas(), { onRestored: schedule });
      rendererName = "webgl2";
    } catch {
      // A canvas that handed out a WebGL context never returns a 2D one, even when the program failed to link
      // afterwards: draw on a fresh element in its place.
      renderer = new Canvas2DRenderer(replaceCanvas());
    }
  }
  state.renderer = rendererName;
  await loadFonts();
  // The settings come before the first measurement: the font size and the UI scale decide the cell size.
  await initSettings({ setCursorColor: (rgb) => renderer?.setCursorColor(rgb), schedule, refit: fit });
  state.metrics = measure();
  window.addEventListener("resize", fit);
  watchDpr();
  // The cursor phase follows the preferences, and a key press (heard before any handler) restarts it.
  onPreferenceChange(syncBlink);
  window.addEventListener("keydown", restartBlink, true);
  // The two grid effects follow the settings and reduced motion, live.
  onPreferenceChange(syncMotion);
  syncMotion();
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
 * It also derives the pane header height from the root font size (`state.headerPx`, and `--hw-header` for the
 * stylesheet), so the UI scale moves the header and the grid below it together.
 */
function measure(): Metrics {
  const probe = document.createElement("canvas").getContext("2d")!;
  probe.font = font(0);
  const dpr = window.devicePixelRatio || 1;
  const root = document.documentElement;
  state.headerPx = headerHeight(parseFloat(getComputedStyle(root).fontSize), dpr);
  root.style.setProperty("--hw-header", `${state.headerPx}px`);
  const snap = (cssPx: number) => Math.max(1, Math.round(cssPx * dpr)) / dpr;
  return {
    cellW: snap(probe.measureText("M").width),
    cellH: snap(Math.ceil(getFontPx() * 1.25)),
    dpr,
    width: canvas().clientWidth,
    height: canvas().clientHeight,
  };
}

/**
 * Follow the device pixel ratio. A window dragged to a display with another scale factor, or a change of the display
 * scaling, changes it without a `resize` event when the CSS size of the window stays: the cell size, the canvas
 * backing store and the glyph atlas would stay at the old ratio. The query matches only the ratio it was made for, so
 * its `change` event is the end of that ratio, and the next one is watched after the re-measurement (WR-07).
 */
function watchDpr(): void {
  if (typeof window.matchMedia !== "function") return;
  const query = window.matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
  query.addEventListener(
    "change",
    () => {
      fit();
      watchDpr();
    },
    { once: true },
  );
}

// ---- layout changes -----------------------------------------------------------------------------

/** The `rev` of the view the page shows; 0 before the first one. */
let shownRev = 0;

/**
 * Take a new layout: the one place every answer and event of the core with a view ends up.
 *
 * The answer of a command and the `layout` event of another take different paths, so one that was built earlier can
 * arrive later. A view with a lower `rev` than the one shown is that: applying it would put the page back to a layout
 * the core has left (WR-06). The same `rev` again is the same view (the answer and the event of one change) and is
 * applied as it always was.
 */
export function applyView(v: View): void {
  // A view without a number is not one of the core's; the page does not drop what it cannot order.
  if (typeof v.rev === "number") {
    if (v.rev < shownRev) return;
    shownRev = v.rev;
  }
  state.view = v;
  const shown = new Set(v.panes.map(([id]) => id));
  for (const id of shown) state.away.delete(id); // a pane that comes back is a first appearance (syncSizes)
  for (const id of leaving.take(shown, performance.now())) leave(id);
  renderBar(v, applyView);
  renderStatus();
  syncSizes();
  renderOverlay();
  syncBlink(); // the focus may have moved to a pane whose cursor blinks, or away from one
  schedule();
}

/** The panes this page has let go of by a gesture and that no layout has confirmed gone yet. */
const leaving = new Leaving();

/**
 * This page lets go of these panes (released outside the window, torn off): they are forgotten when the layout that
 * shows them gone arrives. Their ids come from the gesture, because a layout lists only the shown tab's panes.
 */
export function markLeaving(panes: Iterable<number>): void {
  leaving.mark(panes, performance.now());
}

/** The gesture did not let go of these panes after all (cancelled, refused, failed). */
export function unmarkLeaving(panes: Iterable<number>): void {
  leaving.unmark(panes);
}

/** A pane moved to another window: nothing of it stays here, so it gets a full frame when it comes back. */
function leave(pane: number): void {
  if (state.exited.has(pane)) return; // it ended, it did not move: the exit already forgot it
  leavePane(state, pane);
  forgetSelection(pane);
  waiting.delete(pane);
  motion.forget(pane);
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
    if (state.exited.has(id)) continue; // its exit may arrive before the layout that drops it
    const { cols, rows } = fitCells(m, r, state.headerPx);
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

/**
 * Re-measure after a window, font or UI scale change (the root font size): resize the canvas, derive the header
 * height again, tell the renderer and the core.
 */
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

/** When frame events arrived; `state.stats.fps` is its rate. */
const arrivals = new RateMeter(1000);

/** A pane id as the core sends it: a whole number (layout ids stay below 2^53). */
const isPane = (v: unknown): v is number => typeof v === "number" && Number.isSafeInteger(v);

/** One `frame` event: patch the pane's mirror, or on a gap ask for a full frame (once) and wait for it. */
function onFrame(msg: FrameMsg): void {
  arrivals.note(performance.now());
  if (typeof msg !== "object" || msg === null || !isPane(msg.pane)) return;
  const pane = msg.pane;
  if (state.exited.has(pane) || state.away.has(pane)) return; // a late frame of a pane that is gone must not bring it back
  if (!msg.full && waiting.has(pane)) return;
  const { mirror, gap } = applyFrame(state.frames.get(pane), msg);
  if (gap) {
    waiting.add(pane);
    void call("request_full", { pane }).catch(() => {});
    return;
  }
  if (!mirror) return;
  if (msg.full) waiting.delete(pane);
  const previous = state.frames.get(pane);
  const before = decorationSignature(previous);
  state.frames.set(pane, mirror);
  // The effects only add drawing after this frame; they never hold it back (see scroll.ts).
  motion.frame(pane, previous, mirror, msg.full === true, Array.isArray(msg.changed) && msg.changed.length > 0);
  // The cache of the selection's text follows the spans; the overlay (selection, search matches and counter, copy
  // cursor) is redrawn when what the focused pane's header says about them changes.
  onSelectionSpans(pane, mirror.selection, msg.changed);
  if (pane === state.view?.focus && before !== decorationSignature(mirror)) renderOverlay();
  if (pane === state.view?.focus) {
    syncBlink(); // the program may have changed the cursor's blink with DECSCUSR
    restartBlink(); // output shows the cursor: a cursor does not vanish while the program writes
  }
  schedule();
}

/** Bring everything that shows a title, a working directory or an exit in line with the state. */
function renderLabels(): void {
  if (state.view) renderBar(state.view, applyView);
  renderPaneHeads();
}

/** A program set a pane's title. The string was sanitised by the core and is only ever rendered as text. */
function onTitle(p: { pane: number; title: string }): void {
  if (typeof p !== "object" || p === null || !isPane(p.pane) || typeof p.title !== "string") return;
  if (state.exited.has(p.pane) || state.away.has(p.pane)) return;
  state.titles.set(p.pane, p.title);
  renderLabels();
}

/** A pane reported its working directory (OSC 7). Same rules as the title. */
function onCwd(p: { pane: number; cwd: string }): void {
  if (typeof p !== "object" || p === null || !isPane(p.pane) || typeof p.cwd !== "string") return;
  if (state.exited.has(p.pane) || state.away.has(p.pane)) return;
  state.cwds.set(p.pane, p.cwd);
  renderLabels();
}

/**
 * A pane's shell ended or the pane was closed: forget everything the page holds for it. The signal is this event,
 * not the layout: a layout only lists the panes of the active tab, and the panes of other tabs are still running.
 */
function onPaneExit(p: { pane: number; code: number | null }): void {
  if (typeof p !== "object" || p === null || !isPane(p.pane)) return;
  forgetPane(state, p.pane);
  forgetSelection(p.pane);
  waiting.delete(p.pane);
  motion.forget(p.pane);
  renderLabels();
  syncBlink();
  schedule();
}

/** Listen to the events a pane produces. Call once at start-up, before the first layout is asked for. */
export async function initEvents(): Promise<void> {
  await listen<FrameMsg>("frame", onFrame);
  await listen<{ pane: number; title: string }>("title", onTitle);
  await listen<{ pane: number; cwd: string }>("cwd", onCwd);
  await listen<{ pane: number; code: number | null }>("pane-exit", onPaneExit);
}

// ---- cursor blink --------------------------------------------------------------------------------

/** Half a period of the blink: the cursor is shown for this long and then hidden for as long. */
const BLINK_MS = 530;

/** The phase timer; it exists only while the focused pane's cursor blinks, so a solid cursor costs nothing. */
let blinkTimer: number | null = null;

/** Whether the focused pane's cursor blinks: the engine asked for it and the setting and reduced motion allow it. */
function blinkWanted(): boolean {
  const view = state.view;
  if (!view || state.exited.has(view.focus)) return false;
  const cursor = state.frames.get(view.focus)?.cursor;
  if (!cursor) return false;
  return effectiveBlink(blinkSetting(), motionReduced(), cursor.blink === true);
}

/** Flip the phase and draw it. */
function flipBlink(): void {
  state.blink.on = !state.blink.on;
  schedule();
}

/**
 * Start or stop the phase timer to match `blinkWanted()`. Stopping leaves the cursor visible and draws it if it was
 * in the off phase. Call after anything that can change the answer: focus, a frame's cursor, the preferences.
 */
function syncBlink(): void {
  const want = blinkWanted();
  if (want && blinkTimer === null) {
    state.blink.on = true;
    blinkTimer = window.setInterval(flipBlink, BLINK_MS);
  } else if (!want && blinkTimer !== null) {
    window.clearInterval(blinkTimer);
    blinkTimer = null;
    if (!state.blink.on) {
      state.blink.on = true;
      schedule();
    }
  }
  state.blink.running = blinkTimer !== null;
}

/** Output or a key: show the cursor and start the phase over, so it never vanishes under the typist. */
function restartBlink(): void {
  if (blinkTimer === null) return;
  window.clearInterval(blinkTimer);
  blinkTimer = window.setInterval(flipBlink, BLINK_MS);
  if (!state.blink.on) {
    state.blink.on = true;
    schedule();
  }
}

// ---- grid effects ---------------------------------------------------------------------------------

/** Smooth scrolling and the cursor trail (DESK-28): what they hold per pane, and which of them may run. */
const motion = new Motion();

/** Take the effects the settings and the motion preference allow now; an effect that is off drops what it held. */
function syncMotion(): void {
  motion.setEnabled(smoothScrollOn(), cursorTrailOn());
  schedule(); // the plain frame replaces a slide or a trail that was cut short
}

/**
 * The effects' share of one pane's draw now, or undefined when nothing animates for it (the common case). The grid
 * slides by rows times the cell height; the frames under it come without a cursor; the ghosts are only drawn for the
 * cursor that is drawn, so never for the off phase of a blink or a hidden cursor.
 */
function paneFx(id: number, mirror: PaneMirror, focused: boolean, frame: Frame): PaneFx | undefined {
  const mo = motion.of(id, mirror.rows);
  if (!mo) return undefined;
  const cellH = state.metrics.cellH;
  const fx: PaneFx = {};
  if (mo.rows !== 0) {
    fx.offsetY = mo.rows * cellH;
    fx.under = mo.under.map((u) => ({
      frame: { ...toRenderFrame(u.mirror, false), cursor: null },
      offsetY: u.rows * cellH,
    }));
  }
  if (focused && frame.cursor && mo.ghosts.length > 0) fx.ghosts = mo.ghosts;
  return fx;
}

// ---- drawing -------------------------------------------------------------------------------------

/** Draw times since the last stats update. */
let batch: number[] = [];
let pending = false;

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
    const focused = id === view.focus;
    const frame = toRenderFrame(mirror, focused);
    // The off phase of a blinking cursor: the focused pane is drawn without it.
    if (focused && blinkTimer !== null && !state.blink.on) frame.cursor = null;
    renderer.drawPane(frame, gridRect(state.metrics, r, state.headerPx), paneFx(id, mirror, focused, frame));
  }
  const now = performance.now();
  batch.push(now - t0);
  if (batch.length >= 30) {
    state.stats = {
      avgMs: batch.reduce((a, b) => a + b, 0) / batch.length,
      maxMs: Math.max(...batch),
      fps: arrivals.perSecond(now), // frame events that arrived, not frames that were drawn
    };
    batch = [];
    renderStatus();
  }
  // The single owner of the animation loop: another animation frame only while an effect has something left to draw.
  if (motion.active()) schedule();
}

/** Ask for one draw on the next animation frame; any number of calls before it make one draw. */
export function schedule(): void {
  if (!pending) {
    pending = true;
    requestAnimationFrame(draw);
  }
}
