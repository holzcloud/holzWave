// holzWave spike frontend: a canvas-2D renderer for the frames the Rust core sends.
// No framework and no @tauri-apps/api: the Tauri global is enough for three commands and one event.

type Run = [text: string, cols: number, fg: number, bg: number, flags: number];
interface Frame {
  cols: number;
  rows: number;
  cursor: [number, number] | null;
  fg: number;
  bg: number;
  lines: Run[][];
}

interface TauriGlobal {
  core: { invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> };
  event: { listen<T>(name: string, cb: (e: { payload: T }) => void): Promise<() => void> };
}
const tauri = (window as unknown as { __TAURI__: TauriGlobal }).__TAURI__;

const FONT_PX = 14;
const FONT = `${FONT_PX}px "JetBrains Mono", "SF Mono", Menlo, monospace`;
const BOLD = 1, ITALIC = 2, UNDERLINE = 4, INVERSE = 8;

const canvas = document.getElementById("term") as HTMLCanvasElement;
const stats = document.getElementById("stats") as HTMLDivElement;
const ctx = canvas.getContext("2d", { alpha: false })!;

let cellW = 8, cellH = 17, dpr = 1;
let frame: Frame | null = null;
let pending = false;

const css = (c: number) => `#${c.toString(16).padStart(6, "0")}`;

function measure() {
  dpr = window.devicePixelRatio || 1;
  ctx.font = FONT;
  cellW = ctx.measureText("M").width;
  cellH = Math.ceil(FONT_PX * 1.25);
}

function gridSize() {
  return {
    cols: Math.max(2, Math.floor(canvas.clientWidth / cellW)),
    rows: Math.max(1, Math.floor(canvas.clientHeight / cellH)),
  };
}

let lastSent = { cols: 0, rows: 0 };
function fit() {
  measure();
  canvas.width = Math.round(canvas.clientWidth * dpr);
  canvas.height = Math.round(canvas.clientHeight * dpr);
  const g = gridSize();
  if (g.cols !== lastSent.cols || g.rows !== lastSent.rows) {
    lastSent = g;
    void tauri.core.invoke("resize", g);
  }
  schedule();
}

// Frame timing, for the renderer decision.
const draws: number[] = [];
let frames = 0;
const started = performance.now();

function draw() {
  pending = false;
  const f = frame;
  if (!f) return;
  const t0 = performance.now();
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.font = FONT;
  ctx.textBaseline = "alphabetic";
  ctx.fillStyle = css(f.bg);
  ctx.fillRect(0, 0, canvas.clientWidth, canvas.clientHeight);

  const baseline = Math.round(cellH * 0.78);
  for (let y = 0; y < f.lines.length; y++) {
    const line = f.lines[y]!;
    let col = 0;
    for (const [text, cols, fg0, bg0, flags] of line) {
      let fg = fg0, bg = bg0;
      if (flags & INVERSE) [fg, bg] = [bg, fg];
      const x = col * cellW;
      if (bg !== f.bg) {
        ctx.fillStyle = css(bg);
        ctx.fillRect(x, y * cellH, cols * cellW, cellH);
      }
      if (text.trim() !== "" || flags & UNDERLINE) {
        ctx.fillStyle = css(fg);
        ctx.font = `${flags & ITALIC ? "italic " : ""}${flags & BOLD ? "bold " : ""}${FONT}`;
        let c = col;
        for (const ch of text) {
          if (ch === "\0") { c += 1; continue; }
          if (ch !== " ") ctx.fillText(ch, c * cellW, y * cellH + baseline);
          c += 1;
        }
        if (flags & UNDERLINE) ctx.fillRect(x, y * cellH + cellH - 2, cols * cellW, 1);
      }
      col += cols;
    }
  }
  if (f.cursor) {
    ctx.fillStyle = "rgba(255, 143, 184, 0.85)";
    ctx.fillRect(f.cursor[0] * cellW, f.cursor[1] * cellH, cellW, cellH);
  }

  const ms = performance.now() - t0;
  draws.push(ms);
  if (draws.length > 120) draws.shift();
  frames++;
  if (frames % 30 === 0) {
    const avg = draws.reduce((a, b) => a + b, 0) / draws.length;
    const max = Math.max(...draws);
    const fps = frames / ((performance.now() - started) / 1000);
    stats.textContent = `${f.cols}x${f.rows}  draw avg ${avg.toFixed(2)} ms  max ${max.toFixed(2)} ms  ${fps.toFixed(0)} fps`;
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

export {};
