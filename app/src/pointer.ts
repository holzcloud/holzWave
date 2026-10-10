// Pointer input on the canvas: a click focuses a pane, a drag selects (the legacy frontend selection in `state`),
// the wheel scrolls the pane under the pointer. Also owns `edit.copy`, which copies that selection.
import { registerAction } from "./actions.js";
import { call } from "./bridge.js";
import type { Frame } from "./frame.js";
import { toRenderFrame } from "./frames.js";
import { cellAt, ordered, paneAt } from "./geometry.js";
import { renderOverlay } from "./overlay.js";
import { canvas as sceneCanvas } from "./scene.js";
import { state } from "./state.js";
import type { View } from "./types.js";

// ---- selection text ------------------------------------------------------------------------------

/** The text of one row of a frame, one string element per cell column. */
function rowCells(f: Pick<Frame, "cols" | "lines">, row: number): string[] {
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
  const mirror = state.frames.get(selection.pane);
  if (!mirror) return "";
  const f = toRenderFrame(mirror, false);
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

// ---- gestures ------------------------------------------------------------------------------------

let dragging = false;
let wheelRemainder = 0;

/**
 * Start listening to the canvas; `applyView` takes the layout answer of a focus change. Call after `initScene`:
 * the scene may have replaced the canvas element while it chose the renderer.
 */
export function initPointer(applyView: (v: View) => void): void {
  const canvas = sceneCanvas();
  registerAction({
    id: "edit.copy",
    title: "Copy",
    run: () => {
      const t = selectedText();
      if (t) void navigator.clipboard.writeText(t);
    },
  });

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

  canvas.addEventListener(
    "wheel",
    (e) => {
      const rect = canvas.getBoundingClientRect();
      const hit = paneAt(state.metrics, state.view, e.clientX - rect.left, e.clientY - rect.top);
      if (!hit) return;
      e.preventDefault();
      wheelRemainder += e.deltaMode === 0 ? e.deltaY / state.metrics.cellH : e.deltaY * 3;
      const rows = Math.trunc(wheelRemainder);
      if (rows === 0) return;
      wheelRemainder -= rows;
      void call("scroll", { pane: hit[0], rows });
    },
    { passive: false },
  );
}
