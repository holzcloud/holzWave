// Renderer benchmark: the same synthetic frames through both renderers, no Tauri needed.
// Build with `npm run bench:build --prefix app`, then open dist-bench/index.html in a browser.
// `?renderer=canvas|webgl&scenario=full|sparse`, optionally `&dpr=2`. It is not part of the shipped bundle.
import { Canvas2DRenderer } from "../src/canvas2d.js";
import { Frame, Metrics, Renderer, Run } from "../src/frame.js";
import { WebGLRenderer } from "../src/webgl.js";

const COLS = 160, ROWS = 50;
const PALETTE = [0xe6e0ea, 0xff8fb8, 0x8cc271, 0xe8c15c, 0x8fb8d8, 0xec8272];
const WORDS = ["error", "warning", "src/module", "handler.rs", "größe", "日本語", "→", "let", "fn", "match", "Result", "0x7fff"];

// A small deterministic generator, so both renderers see the same frames.
let seed = 12345;
const rnd = () => (seed = (seed * 1664525 + 1013904223) >>> 0) / 2 ** 32;

function line(): Run[] {
  const runs: Run[] = [];
  let cols = 0;
  while (cols < COLS - 12) {
    const w = WORDS[Math.floor(rnd() * WORDS.length)]!;
    const wide = /[　-鿿]/.test(w);
    const text = wide ? Array.from(w).map((c) => c + "\0").join("") : w + " ";
    const n = wide ? Array.from(w).length * 2 : w.length + 1;
    const flags = rnd() < 0.1 ? 1 : rnd() < 0.05 ? 4 : 0;
    runs.push([text, n, PALETTE[Math.floor(rnd() * PALETTE.length)]!, 0x14091a, flags]);
    cols += n;
  }
  return runs;
}

function frame(): Frame {
  return { cols: COLS, rows: ROWS, cursor: [3, 7], fg: 0xe6e0ea, bg: 0x14091a, lines: Array.from({ length: ROWS }, line) };
}

const q = new URLSearchParams(location.search);
const canvas = document.getElementById("term") as HTMLCanvasElement;
const out = document.getElementById("out") as HTMLPreElement;

const dpr = Number(q.get("dpr")) || window.devicePixelRatio || 1;
const cellW = 8.4, cellH = 18;
const m: Metrics = { cellW, cellH, dpr, width: COLS * cellW, height: ROWS * cellH };
canvas.style.width = `${m.width}px`;
canvas.style.height = `${m.height}px`;
canvas.width = Math.round(m.width * dpr);
canvas.height = Math.round(m.height * dpr);

function run(name: string, r: Renderer) {
  r.configure(m);
  const frames = Array.from({ length: 8 }, frame);
  const sparse = q.get("scenario") === "sparse";
  const pick = (i: number): Frame => {
    if (!sparse) return frames[i % frames.length]!;
    // Only one line changes between frames.
    const f = frames[0]!;
    f.lines[i % ROWS] = frames[(i % 7) + 1]!.lines[i % ROWS]!;
    return f;
  };
  for (let i = 0; i < 30; i++) { const f = pick(i); r.begin(f.bg); r.drawPane(f, { x: 0, y: 0, w: m.width, h: m.height }); r.sync(); } // warm up, fills the glyph atlas
  const times: number[] = [];
  for (let i = 0; i < 300; i++) {
    const t = performance.now();
    const f = pick(i);
    r.begin(f.bg);
    r.drawPane(f, { x: 0, y: 0, w: m.width, h: m.height });
    r.sync();
    times.push(performance.now() - t);
  }
  times.sort((a, b) => a - b);
  const avg = times.reduce((a, b) => a + b, 0) / times.length;
  return { name, avg, p50: times[150]!, p95: times[285]!, max: times[299]! };
}

const which = q.get("renderer") ?? "webgl";
if (!q.get("check")) {
  const r = which === "canvas" ? new Canvas2DRenderer(canvas) : new WebGLRenderer(canvas);
  const res = run(which, r);
  out.textContent = JSON.stringify({ ...res, grid: `${COLS}x${ROWS}`, dpr, scenario: q.get("scenario") ?? "full", ua: navigator.userAgent });
  document.title = "done";
}

// `?check=1`: two panes with different backgrounds side by side, then read pixels back.
// Used to verify that a renderer keeps each pane inside its rectangle.
if (q.get("check")) {
  const r2 = which === "canvas" ? new Canvas2DRenderer(canvas) : new WebGLRenderer(canvas);
  r2.configure(m);
  const solid = (bg: number): Frame => ({
    cols: 20, rows: 4, cursor: null, fg: 0xffffff, bg,
    lines: [[["hello", 5, 0xffffff, bg, 0]], [], [], []],
  });
  const half = m.width / 2;
  r2.begin(0x101010);
  r2.drawPane(solid(0xaa0000), { x: 0, y: 0, w: half - 10, h: m.height });
  r2.drawPane(solid(0x0000aa), { x: half + 10, y: 0, w: half - 10, h: m.height });
  const px = (x: number, y: number): number[] => {
    const X = Math.round(x * dpr), Y = Math.round(y * dpr);
    const gl = canvas.getContext("webgl2");
    if (gl) {
      const b = new Uint8Array(4);
      gl.readPixels(X, canvas.height - 1 - Y, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, b);
      return [...b.slice(0, 3)];
    }
    return [...canvas.getContext("2d")!.getImageData(X, Y, 1, 1).data.slice(0, 3)];
  };
  out.textContent = JSON.stringify({
    left: px(half / 2, m.height - 5), gap: px(half, m.height - 5), right: px(half * 1.5, m.height - 5),
    leftEdge: px(half - 11, m.height - 5), rightEdge: px(half + 11, m.height - 5),
  });
  document.title = "done";
}
