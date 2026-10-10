// holzWave frontend: tabs, workspaces and tiled panes drawn on one canvas.
// No framework and no @tauri-apps/api: the Tauri global is enough for the commands and two events.
import { Canvas2DRenderer } from "./canvas2d.js";
import { FONT_PX, Frame, Metrics, PxRect, Renderer, font } from "./frame.js";
import { WebGLRenderer } from "./webgl.js";

interface TauriGlobal {
  core: { invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> };
  event: { listen<T>(name: string, cb: (e: { payload: T }) => void): Promise<() => void> };
}
const tauri = (window as unknown as { __TAURI__: TauriGlobal }).__TAURI__;
const call = <T = View>(cmd: string, args?: Record<string, unknown>) => tauri.core.invoke<T>(cmd, args);

// ---- types shared with the Rust side ----------------------------------------------------------

interface UnitRect { x: number; y: number; w: number; h: number }
interface Divider { split: number; axis: "Row" | "Column"; area: UnitRect; ratio: number }
interface View {
  workspaces: { name: string; active: boolean }[];
  tabs: { title: string; active: boolean }[];
  focus: number;
  panes: [number, UnitRect][];
  dividers: Divider[];
}
interface PaneFrame { pane: number; frame: Frame & { scrollbar: [number, number, number] } }

// ---- setup -------------------------------------------------------------------------------------

const PAD = 6; // CSS px between a pane's edge and its grid
const canvas = document.getElementById("term") as HTMLCanvasElement;
const overlay = document.getElementById("overlay") as HTMLDivElement;
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

let metrics = measure();
let view: View | null = null;
const frames = new Map<number, PaneFrame["frame"]>();
const sizes = new Map<number, { cols: number; rows: number }>();
let selection: { pane: number; a: [number, number]; b: [number, number] } | null = null;

// ---- geometry ----------------------------------------------------------------------------------

/** The pane rectangle in CSS pixels inside the canvas. */
function paneRect(r: UnitRect): PxRect {
  return { x: r.x * metrics.width, y: r.y * metrics.height, w: r.w * metrics.width, h: r.h * metrics.height };
}

/** Where the grid of a pane is drawn: the pane rectangle without padding. */
function gridRect(r: UnitRect): PxRect {
  const p = paneRect(r);
  return { x: p.x + PAD, y: p.y + PAD, w: Math.max(0, p.w - 2 * PAD), h: Math.max(0, p.h - 2 * PAD) };
}

function paneAt(x: number, y: number): [number, UnitRect] | null {
  if (!view) return null;
  for (const [id, r] of view.panes) {
    const p = paneRect(r);
    if (x >= p.x && x < p.x + p.w && y >= p.y && y < p.y + p.h) return [id, r];
  }
  return null;
}

function cellAt(r: UnitRect, x: number, y: number): [number, number] {
  const g = gridRect(r);
  return [
    Math.max(0, Math.floor((x - g.x) / metrics.cellW)),
    Math.max(0, Math.floor((y - g.y) / metrics.cellH)),
  ];
}

// ---- layout changes -----------------------------------------------------------------------------

function applyView(v: View) {
  view = v;
  const live = new Set(v.panes.map(([id]) => id));
  if (selection && !live.has(selection.pane)) selection = null;
  renderBar();
  syncSizes();
  renderOverlay();
  schedule();
}

/** Tell the core how many cells each visible pane has; the first call also starts its shell. */
function syncSizes() {
  if (!view) return;
  for (const [id, r] of view.panes) {
    const g = gridRect(r);
    const cols = Math.max(2, Math.floor(g.w / metrics.cellW));
    const rows = Math.max(1, Math.floor(g.h / metrics.cellH));
    const prev = sizes.get(id);
    if (!prev) {
      sizes.set(id, { cols, rows });
      void call("start_pane", { pane: id, cols, rows });
    } else if (prev.cols !== cols || prev.rows !== rows) {
      sizes.set(id, { cols, rows });
      void call("resize", { pane: id, cols, rows });
    }
  }
}

function fit() {
  metrics = measure();
  canvas.width = Math.round(metrics.width * metrics.dpr);
  canvas.height = Math.round(metrics.height * metrics.dpr);
  renderer.configure(metrics);
  syncSizes();
  renderOverlay();
  schedule();
}

// ---- tab bar -------------------------------------------------------------------------------------

function button(label: string, active: boolean, onClick: () => void, cls = "") {
  const b = document.createElement("button");
  b.textContent = label;
  b.className = `${active ? "active" : ""} ${cls}`.trim();
  b.addEventListener("click", () => { onClick(); canvas.focus(); });
  return b;
}

function renderBar() {
  if (!view) return;
  bar.replaceChildren();
  view.workspaces.forEach((w, i) => bar.append(button(w.name, w.active, () => void call("select_workspace", { index: i }).then(applyView))));
  bar.append(button("+", false, () => void call("new_workspace", { name: `ws ${view!.workspaces.length + 1}` }).then(applyView), "plus"));
  const sep = document.createElement("span");
  sep.className = "sep";
  bar.append(sep);
  view.tabs.forEach((t, i) => bar.append(button(t.title, t.active, () => void call("select_tab", { index: i }).then(applyView))));
  bar.append(button("+", false, () => void call("new_tab").then(applyView), "plus"));
}

// ---- overlay: dividers, focus ring, selection ----------------------------------------------------

function box(cls: string, r: PxRect) {
  const d = document.createElement("div");
  d.className = cls;
  d.style.cssText = `left:${r.x}px;top:${r.y}px;width:${r.w}px;height:${r.h}px`;
  return d;
}

function renderOverlay() {
  overlay.replaceChildren();
  if (!view) return;
  const multi = view.panes.length > 1;
  for (const [id, r] of view.panes) {
    if (multi && id === view.focus) overlay.append(box("focus", paneRect(r)));
  }
  for (const d of view.dividers) {
    const a = paneRect(d.area);
    const hit = 8;
    const el = d.axis === "Row"
      ? box("divider row", { x: a.x + a.w * d.ratio - hit / 2, y: a.y, w: hit, h: a.h })
      : box("divider column", { x: a.x, y: a.y + a.h * d.ratio - hit / 2, w: a.w, h: hit });
    el.addEventListener("pointerdown", (e) => {
      el.setPointerCapture(e.pointerId);
      const move = (ev: PointerEvent) => {
        const rect = canvas.getBoundingClientRect();
        const ratio = d.axis === "Row"
          ? (ev.clientX - rect.left - a.x) / a.w
          : (ev.clientY - rect.top - a.y) / a.h;
        void call("set_ratio", { split: d.split, ratio }).then(applyView);
      };
      const up = () => { el.removeEventListener("pointermove", move); el.removeEventListener("pointerup", up); };
      el.addEventListener("pointermove", move);
      el.addEventListener("pointerup", up);
    });
    overlay.append(el);
  }
  if (selection) {
    const pane = view.panes.find(([id]) => id === selection!.pane);
    if (pane) {
      const g = gridRect(pane[1]);
      const [s, e] = ordered(selection);
      const cols = sizes.get(selection.pane)?.cols ?? 0;
      for (let row = s[1]; row <= e[1]; row++) {
        const from = row === s[1] ? s[0] : 0;
        const to = row === e[1] ? e[0] : cols - 1;
        overlay.append(box("sel", {
          x: g.x + from * metrics.cellW, y: g.y + row * metrics.cellH,
          w: (to - from + 1) * metrics.cellW, h: metrics.cellH,
        }));
      }
    }
  }
}

function ordered(s: NonNullable<typeof selection>): [[number, number], [number, number]] {
  const [a, b] = [s.a, s.b];
  return a[1] < b[1] || (a[1] === b[1] && a[0] <= b[0]) ? [a, b] : [b, a];
}

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
  if (!selection) return "";
  const f = frames.get(selection.pane);
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
  if (!view) return;
  const t0 = performance.now();
  const first = view.panes.map(([id]) => frames.get(id)).find(Boolean);
  renderer.begin(first?.bg ?? 0x14091a);
  for (const [id, r] of view.panes) {
    const f = frames.get(id);
    if (!f) continue;
    renderer.drawPane(id === view.focus ? f : { ...f, cursor: null }, gridRect(r));
  }
  draws.push(performance.now() - t0);
  if (draws.length > 120) draws.shift();
  drawn++;
  if (drawn % 30 === 0) {
    const avg = draws.reduce((a, b) => a + b, 0) / draws.length;
    const fps = drawn / ((performance.now() - started) / 1000);
    stats.textContent = `${rendererName}  ${view.panes.length} pane(s)  cpu ${avg.toFixed(2)} ms  max ${Math.max(...draws).toFixed(2)} ms  ${fps.toFixed(0)} fps`;
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
  if (!view) return;
  if (selection) { selection = null; renderOverlay(); }
  void call("input", { pane: view.focus, data });
}

/** Window-level shortcuts. Returns true if the key was one. */
function shortcut(e: KeyboardEvent): boolean {
  if (!e.metaKey) return false;
  const k = e.key.toLowerCase();
  const run = (p: Promise<View>) => void p.then(applyView);
  if (k === "d") run(call("split", { column: e.shiftKey }));
  else if (k === "t") run(call("new_tab"));
  else if (k === "n" && e.shiftKey) run(call("new_workspace", { name: `ws ${(view?.workspaces.length ?? 0) + 1}` }));
  else if (k === "w" && e.shiftKey) run(call("close_pane", { pane: view!.focus }));
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
  const hit = paneAt(e.clientX - rect.left, e.clientY - rect.top);
  if (!hit) return;
  const [id, r] = hit;
  if (view && id !== view.focus) void call("focus_pane", { pane: id }).then(applyView);
  const c = cellAt(r, e.clientX - rect.left, e.clientY - rect.top);
  selection = { pane: id, a: c, b: c };
  dragging = true;
  canvas.setPointerCapture(e.pointerId);
  renderOverlay();
});
canvas.addEventListener("pointermove", (e) => {
  if (!dragging || !selection || !view) return;
  const pane = view.panes.find(([id]) => id === selection!.pane);
  if (!pane) return;
  const rect = canvas.getBoundingClientRect();
  const sz = sizes.get(selection.pane);
  const c = cellAt(pane[1], e.clientX - rect.left, e.clientY - rect.top);
  selection.b = [Math.min(c[0], (sz?.cols ?? 1) - 1), Math.min(c[1], (sz?.rows ?? 1) - 1)];
  renderOverlay();
});
canvas.addEventListener("pointerup", () => {
  dragging = false;
  if (selection && selection.a[0] === selection.b[0] && selection.a[1] === selection.b[1]) {
    selection = null; // a plain click is not a selection
    renderOverlay();
  }
});

let wheelRemainder = 0;
canvas.addEventListener("wheel", (e) => {
  const rect = canvas.getBoundingClientRect();
  const hit = paneAt(e.clientX - rect.left, e.clientY - rect.top);
  if (!hit) return;
  e.preventDefault();
  wheelRemainder += (e.deltaMode === 0 ? e.deltaY / metrics.cellH : e.deltaY * 3);
  const rows = Math.trunc(wheelRemainder);
  if (rows === 0) return;
  wheelRemainder -= rows;
  void call("scroll", { pane: hit[0], rows });
}, { passive: false });

window.addEventListener("resize", fit);

// ---- start ---------------------------------------------------------------------------------------

async function main() {
  await tauri.event.listen<PaneFrame>("screen", (e) => {
    frames.set(e.payload.pane, e.payload.frame);
    schedule();
  });
  await tauri.event.listen<View>("layout", (e) => applyView(e.payload));
  fit();
  applyView(await call("get_layout"));
  canvas.focus();
}
void main();
