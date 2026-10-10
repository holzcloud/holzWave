// Renderer benchmark: the same synthetic frames through both renderers, no Tauri needed.
// Open dist/bench.html in a browser. `?renderer=canvas|webgl&scenario=full|sparse`.
import { Canvas2DRenderer } from "./canvas2d.js";
import { Frame, Metrics, Renderer, Run } from "./frame.js";
import { WebGLRenderer } from "./webgl.js";

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
  for (let i = 0; i < 30; i++) { r.draw(pick(i)); r.sync(); } // warm up, fills the glyph atlas
  const times: number[] = [];
  for (let i = 0; i < 300; i++) {
    const t = performance.now();
    r.draw(pick(i));
    r.sync();
    times.push(performance.now() - t);
  }
  times.sort((a, b) => a - b);
  const avg = times.reduce((a, b) => a + b, 0) / times.length;
  return { name, avg, p50: times[150]!, p95: times[285]!, max: times[299]! };
}

const which = q.get("renderer") ?? "webgl";
const r = which === "canvas" ? new Canvas2DRenderer(canvas) : new WebGLRenderer(canvas);
const res = run(which, r);
const text = JSON.stringify({ ...res, grid: `${COLS}x${ROWS}`, dpr, scenario: q.get("scenario") ?? "full", ua: navigator.userAgent });
out.textContent = text;
document.title = "done";
