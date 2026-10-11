// holzWave frontend bootstrap: tabs, workspaces and tiled panes drawn on one canvas.
// No framework and no @tauri-apps/api: the Tauri global is enough for the commands and the events.
// Everything lives in a module of its own: scene (canvas, draw loop, view), chrome (bar) and status (status
// line), overlay (dividers, pane headers, selection and match boxes), keyboard and actions, pointer, selection (the
// clipboard text), search-ui (the search bar), frames (the pane mirrors), bridge, state, geometry. This file only
// starts them.
import { call, listen } from "./bridge.js";
import { initChrome } from "./chrome.js";
import { initKeyboard } from "./keyboard.js";
import { initOverlay } from "./overlay.js";
import { initPointer } from "./pointer.js";
import { applyView, fit, focusCanvas, initEvents, initScene } from "./scene.js";
import { initSearch } from "./search-ui.js";
import { initSelection } from "./selection.js";
import type { View } from "./types.js";

async function main() {
  await initChrome(); // its styles and app_info first: the first measurement needs the bar and the status line
  await initScene();
  initOverlay(applyView);
  initKeyboard(applyView);
  initPointer(applyView);
  initSelection();
  await initSearch();
  await initEvents();
  await listen<View>("layout", applyView);
  fit();
  applyView(await call("get_layout"));
  focusCanvas();
}
void main();
