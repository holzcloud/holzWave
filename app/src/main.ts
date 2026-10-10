// holzWave spike frontend: renders the frames the Rust core sends.
// No framework and no @tauri-apps/api: the Tauri global is enough for three commands and one event.
import { Canvas2DRenderer } from "./canvas2d.js";
import { FONT_PX, Frame, Metrics, Renderer, font } from "./frame.js";
import { WebGLRenderer } from "./webgl.js";

interface TauriGlobal {
  core: { invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> };
  event: { listen<T>(name: string, cb: (e: { payload: T }) => void): Promise<() => void> };
}
const tauri = (window as unknown as { __TAURI__: TauriGlobal }).__TAURI__;

const canvas = document.getElementById("term") as HTMLCanvasElement;
const stats = document.getElementById("stats") as HTMLDivElement;

// `?renderer=canvas` or `?renderer=webgl`; WebGL2 is the default and canvas is the fallback.
const wanted = new URLSearchParams(location.search).get("renderer");
let renderer: Renderer;
let rendererName = "canvas2d";
try {
  if (wanted === "canvas") throw new Error("canvas requested");
  renderer = new WebGLRenderer(canvas);
  rendererName = "webgl2";
} catch {
  renderer = new Canvas2DRenderer(canvas);
}

function measure(): Metrics {
  const dpr = window.devicePixelRatio || 1;
  const probe = document.createElement("canvas").getContext("2d")!;
  probe.font = font(0);
  return {
    cellW: probe.measureText("M").width,
    cellH: Math.ceil(FONT_PX * 1.25),
    dpr,
    width: canvas.clientWidth,
    height: canvas.clientHeight,
  };
}

let metrics = measure();
let frame: Frame | null = null;
let pending = false;
let lastSent = { cols: 0, rows: 0 };

function fit() {
  metrics = measure();
  canvas.width = Math.round(canvas.clientWidth * metrics.dpr);
  canvas.height = Math.round(canvas.clientHeight * metrics.dpr);
  renderer.configure(metrics);
  const g = {
    cols: Math.max(2, Math.floor(metrics.width / metrics.cellW)),
    rows: Math.max(1, Math.floor(metrics.height / metrics.cellH)),
  };
  if (g.cols !== lastSent.cols || g.rows !== lastSent.rows) {
    lastSent = g;
    void tauri.core.invoke("resize", g);
  }
  schedule();
}

// Frame timing, for the renderer decision.
const draws: number[] = [];
let drawn = 0;
const started = performance.now();

function draw() {
  pending = false;
  if (!frame) return;
  const t0 = performance.now();
  renderer.draw(frame);
  draws.push(performance.now() - t0);
  if (draws.length > 120) draws.shift();
  drawn++;
  if (drawn % 30 === 0) {
    const avg = draws.reduce((a, b) => a + b, 0) / draws.length;
    const fps = drawn / ((performance.now() - started) / 1000);
    stats.textContent = `${rendererName}  ${frame.cols}x${frame.rows}  cpu ${avg.toFixed(2)} ms  max ${Math.max(...draws).toFixed(2)} ms  ${fps.toFixed(0)} fps`;
  }
}

function schedule() {
  if (!pending) {
    pending = true;
    requestAnimationFrame(draw);
  }
}

// Keyboard: the minimum a shell needs. A real mapping belongs in the core (libghostty key encoder).
const KEYS: Record<string, string> = {
  Enter: "\r", Backspace: "\x7f", Tab: "\t", Escape: "\x1b",
  ArrowUp: "\x1b[A", ArrowDown: "\x1b[B", ArrowRight: "\x1b[C", ArrowLeft: "\x1b[D",
  Home: "\x1b[H", End: "\x1b[F", Delete: "\x1b[3~", PageUp: "\x1b[5~", PageDown: "\x1b[6~",
};

window.addEventListener("keydown", (e) => {
  if (e.metaKey) return; // leave app shortcuts alone
  let data = KEYS[e.key];
  if (data === undefined && e.ctrlKey && e.key.length === 1) {
    const c = e.key.toLowerCase().charCodeAt(0);
    if (c >= 97 && c <= 122) data = String.fromCharCode(c - 96);
  }
  if (data === undefined && e.key.length === 1 && !e.ctrlKey) data = e.altKey ? "\x1b" + e.key : e.key;
  if (data === undefined) return;
  e.preventDefault();
  void tauri.core.invoke("input", { data });
});

window.addEventListener("paste", (e) => {
  const text = e.clipboardData?.getData("text");
  if (text) void tauri.core.invoke("input", { data: text });
});

window.addEventListener("resize", fit);

async function main() {
  await tauri.event.listen<Frame>("screen", (e) => {
    frame = e.payload;
    schedule();
  });
  fit();
  await tauri.core.invoke("start", lastSent);
  canvas.focus();
}
void main();
