// holzWave frontend bootstrap: tabs, workspaces and tiled panes drawn on one canvas.
// No framework and no @tauri-apps/api: the Tauri global is enough for the commands and the events.
// Everything lives in a module of its own: scene (canvas, draw loop, view), chrome (bar) and status (status
// line), overlay (dividers, pane headers, selection and match boxes), keyboard and actions, pointer, selection (the
// clipboard text), search-ui (the search bar), palette (the command palette), frames (the pane mirrors), bridge,
// state, geometry. This file only starts them.
// Every window runs this same page; it listens with its own label (bridge) and says `window_ready` when it has drawn.
import { call, listen } from "./bridge.js";
import { initAiSettings } from "./aisettings.js";
import { initAsk } from "./askcard.js";
import { attention } from "./attentionstate.js";
import { initBlocks, openBlock } from "./blocks.js";
import { initChat } from "./chat.js";
import { initChrome } from "./chrome.js";
import { initConnections } from "./connections.js";
import { initSync } from "./sync.js";
import { initHistory } from "./historyview.js";
import { initLoginPrompt } from "./loginprompt.js";
import { initLocalNetwork } from "./localnet.js";
import { initInline } from "./inline.js";
import { initIntegrationConfirm } from "./integrateconfirm.js";
import { initIntegrationContext } from "./integratecontext.js";
import { decorateReply } from "./inlinecode.js";
import { sidebarChat } from "./aisidebar.js";
import { initSidebar } from "./sidebar.js";
import { openConnections } from "./connections-ui.js";
import { initCopyMode } from "./copymode.js";
import { initHints } from "./hints.js";
import { initKeyboard } from "./keyboard.js";
import { initLinks } from "./links.js";
import { initOverlay, renderOverlay } from "./overlay.js";
import { initPalette } from "./palette.js";
import { initDragDrop, initDragDropEvents } from "./dragdrop.js";
import { initPointer } from "./pointer.js";
import { applyView, fit, focusCanvas, initEvents, initScene } from "./scene.js";
import { initSearch } from "./search-ui.js";
import { change, watch as watchSettings } from "./settings.js";
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
  // File browsers and previews: panes that show files instead of a terminal.
  await initBlocks(document.getElementById("overlay") as HTMLElement, applyView, () => {
    renderStatus();
    renderOverlay();
    // The list of chat blocks can change after the layout was drawn: who waits may sit in another pane now.
    attention.changed();
  });
  // Saved connections: a palette entry for each, the manager dialog, and the state of the panes that run one.
  await initConnections(applyView, () => {
    renderStatus();
    renderOverlay();
  }, () => void openConnections());
  // Encrypted sync: the state on the title bar and its popover.
  await initSync(() => {});
  // The AI chat: its title-bar button lands between the sync button and the sidebar button.
  await initChat(openBlock, { watchSettings, sidebar: sidebarChat, decorate: decorateReply });
  // The cards of the requests that programs in a pane make with `wsh ai`: a person answers them, a program never can.
  await initAsk();
  // The palette actions of the chat: they hand a message and a card to the chat module, so it must exist first.
  await initInline();
  // The providers dialog changes the two privacy settings through the same queue as the Settings dialog.
  initAiSettings({ change, watch: watchSettings });
  initHistory();
  initLoginPrompt();
  await initSidebar();
  // The Context section, the status segments and the pane chips read one answer of the core (docs/design.md 6.11).
  initIntegrationContext({
    changed: () => {
      renderStatus();
      renderOverlay();
    },
    applyView,
  });
  // The card that asks before an integration action changes something shows a returned layout the way the rest of the page does.
  initIntegrationConfirm({ applyView });
  initCopyMode(renderStatus);
  await initEvents();
  await listen<View>("layout", applyView);
  await initDragDropEvents();
  fit();
  applyView(await call("get_layout"));
  focusCanvas();
  // Every window says so once it has drawn its first layout and listens: the shell shows a window it built hidden (a
  // torn off or restored one) now, and tells it the last title and directory of its panes. A no-op for a shown one.
  await call("window_ready").catch(() => {});
  // The first window of a Mac asks once, after its first layout is on screen, whether holzWave may use the local network (docs/design.md 6.24).
  void initLocalNetwork();
}
void main();
