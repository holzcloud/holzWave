// holzWave frontend bootstrap: tabs, workspaces and tiled panes drawn on one canvas.
// No framework and no @tauri-apps/api: the Tauri global is enough for the commands and the events.
// Everything lives in a module of its own: scene (canvas, draw loop, view), chrome (bar) and status (status
// line), overlay (dividers, pane headers, selection and match boxes), keyboard and actions, pointer, selection (the
// clipboard text), search-ui (the search bar), palette (the command palette), frames (the pane mirrors), bridge,
// state, geometry. This file only starts them.
// Every window runs this same page; it listens with its own label (bridge) and says `window_ready` when it has drawn.
import { call, listen } from "./bridge.js";
import { initChrome } from "./chrome.js";
import { initCopyMode } from "./copymode.js";
import { initHints } from "./hints.js";
import { initKeyboard } from "./keyboard.js";
import { initLinks } from "./links.js";
import { initOverlay } from "./overlay.js";
import { initPalette } from "./palette.js";
import { initDragDrop } from "./dragdrop.js";
import { initPointer } from "./pointer.js";
import { applyView, fit, focusCanvas, initEvents, initScene } from "./scene.js";
import { initSearch } from "./search-ui.js";
import { initSelection } from "./selection.js";
import { renderStatus } from "./status.js";
import type { View } from "./types.js";

// A layout command of a window that is just going (its last pane was closed or moved away, and the shell destroys it)
// is refused with `no such window`. Nothing is left to show it to: it is not an error of the page.
window.addEventListener("unhandledrejection", (e) => {
  if (String(e.reason).includes("no such window")) e.preventDefault();
});

async function main() {
  await initChrome(); // its styles and app_info first: the first measurement needs the bar and the status line
  await initScene();
  initOverlay(applyView);
  initKeyboard(applyView);
  initPointer(applyView);
  initDragDrop(applyView);
  initSelection();
  await initSearch();
  await initLinks();
  await initHints();
  await initPalette();
  initCopyMode(renderStatus);
  await initEvents();
  await listen<View>("layout", applyView);
  fit();
  applyView(await call("get_layout"));
  focusCanvas();
  // Every window says so once it has drawn its first layout and listens: the shell shows a window it built hidden (a
  // torn off or restored one) now, and tells it the last title and directory of its panes. A no-op for a shown one.
  await call("window_ready").catch(() => {});
}
void main();
