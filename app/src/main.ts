// holzWave frontend bootstrap: tabs, workspaces and tiled panes drawn on one canvas.
// No framework and no @tauri-apps/api: the Tauri global is enough for the commands and two events.
// Everything lives in a module of its own: scene (canvas, draw loop, view), chrome (bar), overlay (dividers,
// focus, selection boxes), keyboard and actions, pointer, bridge, state, geometry. This file only starts them.
import { call, listen } from "./bridge.js";
import { initKeyboard } from "./keyboard.js";
import { initOverlay } from "./overlay.js";
import { initPointer } from "./pointer.js";
import { applyView, fit, focusCanvas, initScene, schedule } from "./scene.js";
import { state } from "./state.js";
import type { PaneFrame, View } from "./types.js";

async function main() {
  await initScene();
  initOverlay(applyView);
  initKeyboard(applyView);
  initPointer(applyView);
  await listen<PaneFrame>("screen", (p) => {
    state.frames.set(p.pane, p.frame);
    schedule();
  });
  await listen<View>("layout", applyView);
  fit();
  applyView(await call("get_layout"));
  focusCanvas();
}
void main();
