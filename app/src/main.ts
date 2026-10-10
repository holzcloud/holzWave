// holzWave frontend: tabs, workspaces and tiled panes drawn on one canvas.
// No framework and no @tauri-apps/api: the Tauri global is enough for the commands and two events.
// The pieces live in their own modules: bridge, types, state, geometry, dom and overlay.
import { call, listen } from "./bridge.js";
import { Canvas2DRenderer } from "./canvas2d.js";
import { el } from "./dom.js";
import { FONT_PX, Frame, Metrics, Renderer, font } from "./frame.js";
import { cellAt, fitCells, gridRect, ordered, paneAt } from "./geometry.js";
import { initOverlay, renderOverlay } from "./overlay.js";
import { state } from "./state.js";
import type { PaneFrame, View } from "./types.js";
import { WebGLRenderer } from "./webgl.js";

// ---- setup -------------------------------------------------------------------------------------

const canvas = document.getElementById("term") as HTMLCanvasElement;
const bar = document.getElementById("bar") as HTMLDivElement;
const stats = document.getElementById("stats") as HTMLDivElement;

// `?renderer=canvas` or `?renderer=webgl`; WebGL2 is the default and Canvas 2D is the fallback.
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

state.metrics = measure();

// ---- layout changes -----------------------------------------------------------------------------

function applyView(v: View) {
  state.view = v;
  const live = new Set(v.panes.map(([id]) => id));
  if (state.selection && !live.has(state.selection.pane)) state.selection = null;
  renderBar();
  syncSizes();
  renderOverlay();
  schedule();
}

/** Tell the core how many cells each visible pane has; the first call also starts its shell. */
function syncSizes() {
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

function fit() {
  state.metrics = measure();
  const m = state.metrics;
  canvas.width = Math.round(m.width * m.dpr);
  canvas.height = Math.round(m.height * m.dpr);
  renderer.configure(m);
  syncSizes();
  renderOverlay();
  schedule();
}

// ---- tab bar -------------------------------------------------------------------------------------

function button(label: string, active: boolean, onClick: () => void, cls = "") {
  const b = el("button", `${active ? "active" : ""} ${cls}`.trim());
  b.textContent = label;
  b.addEventListener("click", () => { onClick(); canvas.focus(); });
  return b;
}

function renderBar() {
  const view = state.view;
  if (!view) return;
  bar.replaceChildren();
  view.workspaces.forEach((w, i) => bar.append(button(w.name, w.active, () => void call("select_workspace", { index: i }).then(applyView))));
  bar.append(button("+", false, () => void call("new_workspace", { name: `ws ${view.workspaces.length + 1}` }).then(applyView), "plus"));
  el("span", "sep", bar);
  view.tabs.forEach((t, i) => bar.append(button(t.title, t.active, () => void call("select_tab", { index: i }).then(applyView))));
  bar.append(button("+", false, () => void call("new_tab").then(applyView), "plus"));
}

// ---- selection text ------------------------------------------------------------------------------

/** The text of one row of a frame, one string element per cell column. */
function rowCells(f: Frame, row: number): string[] {
  const out: string[] = [];
  for (const [text] of f.lines[row] ?? []) {
    for (const ch of text) out.push(ch);
  }
  while (out.length < f.cols) out.push(" ");
  return out;
}

function selectedText(): string {
  const selection = state.selection;
  if (!selection) return "";
  const f = state.frames.get(selection.pane);
  if (!f) return "";
  const [s, e] = ordered(selection);
  const lines: string[] = [];
  for (let row = s[1]; row <= e[1] && row < f.rows; row++) {
    const cells = rowCells(f, row);
    const from = row === s[1] ? s[0] : 0;
    const to = row === e[1] ? e[0] : f.cols - 1;
    lines.push(cells.slice(from, to + 1).filter((c) => c !== "\0").join("").trimEnd());
  }
  return lines.join("\n");
}

// ---- drawing -------------------------------------------------------------------------------------

const draws: number[] = [];
let drawn = 0;
let pending = false;
const started = performance.now();

function draw() {
  pending = false;
  const view = state.view;
  if (!view) return;
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

function schedule() {
  if (!pending) {
    pending = true;
    requestAnimationFrame(draw);
  }
}

// ---- input ---------------------------------------------------------------------------------------

const KEYS: Record<string, string> = {
  Enter: "\r", Backspace: "\x7f", Tab: "\t", Escape: "\x1b",
  ArrowUp: "\x1b[A", ArrowDown: "\x1b[B", ArrowRight: "\x1b[C", ArrowLeft: "\x1b[D",
  Home: "\x1b[H", End: "\x1b[F", Delete: "\x1b[3~", PageUp: "\x1b[5~", PageDown: "\x1b[6~",
};

function send(data: string) {
  if (!state.view) return;
  if (state.selection) { state.selection = null; renderOverlay(); }
  void call("input", { pane: state.view.focus, data });
}

/** Window-level shortcuts. Returns true if the key was one. */
function shortcut(e: KeyboardEvent): boolean {
  if (!e.metaKey) return false;
  const k = e.key.toLowerCase();
  const run = (p: Promise<View>) => void p.then(applyView);
  if (k === "d") run(call("split", { column: e.shiftKey }));
  else if (k === "t") run(call("new_tab"));
  else if (k === "n" && e.shiftKey) run(call("new_workspace", { name: `ws ${(state.view?.workspaces.length ?? 0) + 1}` }));
  else if (k === "w" && e.shiftKey) run(call("close_pane", { pane: state.view!.focus }));
  else if (k === "]") run(call("cycle_focus", { step: 1 }));
  else if (k === "[") run(call("cycle_focus", { step: -1 }));
  else if (/^[1-9]$/.test(k)) run(call(e.altKey ? "select_workspace" : "select_tab", { index: Number(k) - 1 }));
  else if (k === "c") { const t = selectedText(); if (t) void navigator.clipboard.writeText(t); }
  else return false;
  e.preventDefault();
  return true;
}

window.addEventListener("keydown", (e) => {
  if (shortcut(e) || e.metaKey) return;
  let data = KEYS[e.key];
  if (data === undefined && e.ctrlKey && e.key.length === 1) {
    const c = e.key.toLowerCase().charCodeAt(0);
    if (c >= 97 && c <= 122) data = String.fromCharCode(c - 96);
  }
  if (data === undefined && e.key.length === 1 && !e.ctrlKey) data = e.altKey ? "\x1b" + e.key : e.key;
  if (data === undefined) return;
  e.preventDefault();
  send(data);
});

window.addEventListener("paste", (e) => {
  const text = e.clipboardData?.getData("text");
  if (text) send(text);
});

// Mouse: click focuses a pane, drag selects, the wheel scrolls the pane under the pointer.
let dragging = false;
canvas.addEventListener("pointerdown", (e) => {
  const rect = canvas.getBoundingClientRect();
  const hit = paneAt(state.metrics, state.view, e.clientX - rect.left, e.clientY - rect.top);
  if (!hit) return;
  const [id, r] = hit;
  if (state.view && id !== state.view.focus) void call("focus_pane", { pane: id }).then(applyView);
  const c = cellAt(state.metrics, r, e.clientX - rect.left, e.clientY - rect.top);
  state.selection = { pane: id, a: c, b: c };
  dragging = true;
  canvas.setPointerCapture(e.pointerId);
  renderOverlay();
});
canvas.addEventListener("pointermove", (e) => {
  const selection = state.selection;
  if (!dragging || !selection || !state.view) return;
  const pane = state.view.panes.find(([id]) => id === selection.pane);
  if (!pane) return;
  const rect = canvas.getBoundingClientRect();
  const sz = state.sizes.get(selection.pane);
  const c = cellAt(state.metrics, pane[1], e.clientX - rect.left, e.clientY - rect.top);
  selection.b = [Math.min(c[0], (sz?.cols ?? 1) - 1), Math.min(c[1], (sz?.rows ?? 1) - 1)];
  renderOverlay();
});
canvas.addEventListener("pointerup", () => {
  dragging = false;
  const selection = state.selection;
  if (selection && selection.a[0] === selection.b[0] && selection.a[1] === selection.b[1]) {
    state.selection = null; // a plain click is not a selection
    renderOverlay();
  }
});

let wheelRemainder = 0;
canvas.addEventListener("wheel", (e) => {
  const rect = canvas.getBoundingClientRect();
  const hit = paneAt(state.metrics, state.view, e.clientX - rect.left, e.clientY - rect.top);
  if (!hit) return;
  e.preventDefault();
  wheelRemainder += (e.deltaMode === 0 ? e.deltaY / state.metrics.cellH : e.deltaY * 3);
  const rows = Math.trunc(wheelRemainder);
  if (rows === 0) return;
  wheelRemainder -= rows;
  void call("scroll", { pane: hit[0], rows });
}, { passive: false });

window.addEventListener("resize", fit);

// ---- start ---------------------------------------------------------------------------------------

async function main() {
  initOverlay(applyView);
  await listen<PaneFrame>("screen", (p) => {
    state.frames.set(p.pane, p.frame);
    schedule();
  });
  await listen<View>("layout", applyView);
  fit();
  applyView(await call("get_layout"));
  canvas.focus();
}
void main();
