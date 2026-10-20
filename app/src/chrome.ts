// The window bar above the stage, in two rows (docs/design.md section 6.1): workspace pills and the host name and clock
// in the title row, numbered tabs in the row below. It is
// drawn from `state.view` and the pane titles; the elements are keyed by position and updated in place, and every
// name from the core is put in with `textContent`, never parsed as markup. The bar's free space drags the window.
// Every chip and tab has a × beside it that asks before it closes (confirm.ts).
import { registerAction } from "./actions.js";
import { type Marking, attention } from "./attentionstate.js";
import { call } from "./bridge.js";
import { dirtyPanes, paneBlocks } from "./blockstate.js";
import { type CloseAsk, confirmClose, paneAsk, tabAsk, workspaceAsk } from "./confirm.js";
import { el, fitNodes, loadStyles, setAttr, setText } from "./dom.js";
import { focusInput } from "./keyboard.js";
import { clockText, tabLabel } from "./labels.js";
import { paneLabel, renamePane, setPaneCloseHandler, setPaneRenameHandler } from "./panehead.js";
import { startRename } from "./rename.js";
import { state } from "./state.js";
import type { View } from "./types.js";

const bar = document.getElementById("bar") as HTMLDivElement;

/** The title a tab has until the user names it (core/src/layout.rs). */
const AUTO_TAB = "shell";

/** Mark an element of the bar as part of the window's drag region (Tauri reads the attribute of the target only). */
function draggable<T extends HTMLElement>(node: T): T {
  node.setAttribute("data-tauri-drag-region", "");
  return node;
}

/** What a click on a chip, tab or plus button asks the core for; its answer is a view for `applyView`. */
let send: (request: Promise<View>) => void = () => {};
/** The `apply` of the last `renderBar`, so a change of the marks can draw the bar again without a new layout. */
let lastApply: (v: View) => void = () => {};

function button(cls: string, pick: (index: number) => Promise<View>): HTMLButtonElement {
  const node = el("button", cls);
  node.type = "button";
  node.addEventListener("click", () => {
    send(pick(Number(node.dataset.index)));
    focusInput(); // a bar button must not keep the keyboard
  });
  return node;
}

// ---- the structure: built once, at import ---------------------------------------------------------------------

const workspaces = el("div", "bar-group bar-workspaces", bar);
const newWorkspace = el("button", "plus", workspaces);
newWorkspace.type = "button";
newWorkspace.textContent = "+";
newWorkspace.title = "New workspace";
newWorkspace.setAttribute("aria-label", "New workspace");
draggable(el("div", "bar-sep", bar));
const tabs = el("div", "bar-group bar-tabs", bar);
const newTab = el("button", "plus", tabs);
newTab.type = "button";
newTab.textContent = "+";
newTab.title = "New tab";
newTab.setAttribute("aria-label", "New tab");
// The free space of the tab row drags the window too: the bar has two rows (docs/design.md section 6.1).
draggable(el("div", "tabs-fill", tabs));
draggable(el("div", "bar-fill", bar));

const info = draggable(el("div", "bar-info", bar));
const host = draggable(el("span", "label host", info));
const clock = draggable(el("span", "label clock", info));

/** The items of the two groups: a chip or tab and its ×, side by side. A button must not hold another button. */
const chipItems: HTMLElement[] = [];
const tabItems: HTMLElement[] = [];

/** The chip or tab of an item: its first child. */
const mainOf = (item: HTMLElement | undefined): HTMLButtonElement | undefined =>
  item?.firstElementChild as HTMLButtonElement | undefined;

/**
 * An item of the bar: `main` (the chip or tab button), the attention marks (a span, empty until a pane the item holds
 * needs the person, design 6.23) and a × button after them, always visible, named `Close <kind>`.
 * The × is not inside `main`, so a press on it is no press on the tab: it neither selects it, nor starts a drag
 * (dragdrop.ts looks for `.tab`), nor counts as the double click that renames. `onClose` gets the item's index.
 */
function item(
  cls: string,
  kind: "workspace" | "tab",
  main: HTMLButtonElement,
  onClose: (index: number) => void,
): HTMLElement {
  const root = el("div", `bar-item ${cls}`);
  root.append(main);
  // The marks are not a control and take no keyboard stop (the tab is the stop): a press on them is a press on the tab.
  const attn = el("span", "attn", root);
  attn.setAttribute("aria-hidden", "true");
  attn.addEventListener("click", () => main.click());
  const close = el("button", "close", root);
  close.type = "button";
  close.title = `Close ${kind}`;
  close.setAttribute("aria-label", `Close ${kind}`);
  close.addEventListener("click", (e) => {
    e.stopPropagation();
    onClose(Number(root.dataset.index));
  });
  return root;
}

/** The span of the attention marks of an item: its second child. */
const attnOf = (item: HTMLElement): HTMLElement => item.children[1] as HTMLElement;

/**
 * Show `marking` on an item: one `span.mk` per kind inside its marks span (glyph by `textContent`, the kind in
 * `data-kind` for the colour), the kinds in the root's `data-attention` and their number in `data-marks` (the stylesheet widens a tab by it), and the words in the title and the accessible
 * name of the main button. No mark: the span is empty, `data-attention` and `aria-label` go, and the title is `plain`.
 */
function setMarking(root: HTMLElement, main: HTMLButtonElement, label: string, plain: string | null, marking: Marking | null): void {
  const attn = attnOf(root);
  const kinds = marking ? marking.marks.map((m) => m.kind).join(" ") : null;
  if ((root.getAttribute("data-attention") ?? null) !== kinds) {
    attn.replaceChildren();
    for (const m of marking?.marks ?? []) {
      const mark = el("span", "mk", attn);
      mark.setAttribute("data-kind", m.kind);
      mark.textContent = m.glyph;
    }
    setAttr(root, "data-attention", kinds);
    setAttr(root, "data-marks", marking ? String(marking.marks.length) : null);
  }
  setAttr(main, "title", marking ? `${label} \u00b7 ${marking.words}` : plain);
  setAttr(main, "aria-label", marking ? `${label}, ${marking.words}` : null);
}

/** Mark `node` as the shown one: a class for the style and `aria-current` for assistive technology. */
function setActive(node: HTMLElement, active: boolean, current: string): void {
  node.classList.toggle("active", active);
  setAttr(node, "aria-current", active ? current : null);
}

/**
 * Bring the bar in line with `view`: one chip per workspace and one numbered tab per tab, the clicks sending
 * `select_workspace` and `select_tab` with the element's current index. `apply` takes the answers of the core.
 */
export function renderBar(view: View, apply: (v: View) => void): void {
  send = (request) => void request.then(apply);
  lastApply = apply;
  const marks = attention.marks(view);

  // New elements go before the plus button, which stays last in its group.
  fitNodes(
    workspaces,
    chipItems,
    view.workspaces.length,
    () => item("ws-item", "workspace", button("ws-chip label", (index) => call("select_workspace", { index })), closeWorkspaceAt),
    newWorkspace,
  );
  view.workspaces.forEach((w, i) => {
    const root = chipItems[i] as HTMLElement;
    const chip = mainOf(root) as HTMLButtonElement;
    setAttr(root, "data-index", String(i));
    setAttr(chip, "data-index", String(i));
    setText(chip, w.name);
    setActive(chip, w.active, "true");
    root.classList.toggle("active", w.active);
    setMarking(root, chip, w.name, null, marks.workspaces[i] ?? null);
  });

  fitNodes(
    tabs,
    tabItems,
    view.tabs.length,
    () => item("tab-item", "tab", button("tab", (index) => call("select_tab", { index })), closeTabAt),
    newTab,
  );
  view.tabs.forEach((t, i) => {
    const root = tabItems[i] as HTMLElement;
    const tab = mainOf(root) as HTMLButtonElement;
    const label = tabLabel(i, tabName(t));
    setAttr(root, "data-index", String(i));
    setAttr(tab, "data-index", String(i));
    setText(tab, label);
    setActive(tab, t.active, "page");
    root.classList.toggle("active", t.active);
    setMarking(root, tab, label, label, marks.tabs[i] ?? null);
  });
}

/** The name a tab shows: one the user gave wins; a tab still called `shell` shows the title its focus pane's program set. */
function tabName(t: View["tabs"][number]): string {
  return t.title !== AUTO_TAB ? t.title : state.titles.get(t.focus) || t.title;
}

// ---- renaming ---------------------------------------------------------------------------------------------

/** Rename the tab at `index`: the editor opens over its button and the confirmed name goes to the core. */
function renameTabAt(index: number): void {
  const t = state.view?.tabs[index];
  const node = mainOf(tabItems[index]);
  if (!t || !node) return;
  startRename(node, t.title === AUTO_TAB ? "" : t.title, state.titles.get(t.focus) || AUTO_TAB, (name) =>
    send(call("rename_tab", { index, name })),
  );
}

/** Rename the workspace at `index`. */
function renameWorkspaceAt(index: number): void {
  const w = state.view?.workspaces[index];
  const node = mainOf(chipItems[index]);
  if (!w || !node) return;
  const auto = `ws ${index + 1}`;
  startRename(node, w.name === auto ? "" : w.name, auto, (name) => send(call("rename_workspace", { index, name })));
}

/** Rename a pane: the name goes to the core, which cleans it. */
function renamePaneTo(pane: number, name: string): void {
  send(call("rename_pane", { pane, name }));
}

/** Register the three rename actions (palette and key binding table, unbound by default). Call once at start-up. */
function registerRenameActions(): void {
  registerAction({
    id: "tab.rename",
    title: "Rename tab",
    run: () => renameTabAt(state.view?.tabs.findIndex((t) => t.active) ?? -1),
  });
  registerAction({
    id: "workspace.rename",
    title: "Rename workspace",
    run: () => renameWorkspaceAt(state.view?.workspaces.findIndex((w) => w.active) ?? -1),
  });
  registerAction({
    id: "pane.rename",
    title: "Rename pane",
    run: () => {
      if (state.view) renamePane(state.view.focus);
    },
  });
}

// ---- closing ----------------------------------------------------------------------------------------------

/**
 * Ask, then close: the × buttons always ask first. `still` says whether what was asked about is still what is there
 * after the answer (a layout can change while the dialog is open: a shell that ends, another window), so an index
 * never closes something else. Focus goes back to the terminal either way.
 */
async function closeAfterAsking(ask: CloseAsk | null, still: () => boolean, close: () => Promise<View>): Promise<void> {
  if (!ask) return;
  let yes = false;
  try {
    yes = await confirmClose(ask);
  } finally {
    focusInput();
  }
  if (yes && still()) send(close());
}

/** The × of the tab at `index` of the shown workspace. */
function closeTabAt(index: number): void {
  const view = state.view;
  const t = view?.tabs[index];
  if (!view || !t) return;
  const same = (): boolean => {
    const now = state.view?.tabs[index];
    return now !== undefined && now.focus === t.focus && now.title === t.title && now.panes === t.panes;
  };
  void closeAfterAsking(tabAsk(view, index, tabName(t)), same, () => call("close_tab", { index }));
}

/** The × of the workspace at `index`. */
function closeWorkspaceAt(index: number): void {
  const view = state.view;
  const w = view?.workspaces[index];
  if (!view || !w) return;
  const same = (): boolean => {
    const now = state.view?.workspaces[index];
    return now !== undefined && now.name === w.name && now.panes === w.panes;
  };
  void closeAfterAsking(workspaceAsk(view, index, w.name), same, () => call("close_workspace", { index }));
}

/** The × in the header of `pane`. */
function closePaneAfterAsking(pane: number): void {
  const view = state.view;
  if (!view?.panes.some(([id]) => id === pane)) return;
  const same = (): boolean => state.view?.panes.some(([id]) => id === pane) ?? false;
  const ask = paneAsk(view, paneLabel(pane), dirtyPanes.has(pane));
  // A block (a chat, a file, a preview) has no shell: nothing ends when it closes, so it goes at once, unless it holds
  // unsaved changes or it is everything the window has.
  if (paneBlocks.has(pane) && !ask.unsaved && ask.ends === "nothing") {
    send(call("close_pane", { pane }));
    focusInput();
    return;
  }
  void closeAfterAsking(ask, same, () => call("close_pane", { pane }));
}

/**
 * Register `tab.close` and `workspace.close` (palette and key binding table, unbound by default). They close the shown
 * tab or workspace after the same question as the × buttons: they end every shell of the group, which one keystroke
 * must not do unasked. `pane.close` (Cmd+W) closes one pane at once and asks nothing.
 */
function registerCloseActions(): void {
  registerAction({
    id: "tab.close",
    title: "Close tab",
    run: () => closeTabAt(state.view?.tabs.findIndex((t) => t.active) ?? -1),
  });
  registerAction({
    id: "workspace.close",
    title: "Close workspace",
    run: () => closeWorkspaceAt(state.view?.workspaces.findIndex((w) => w.active) ?? -1),
  });
}

// A double click on a tab or a workspace chip renames it. The first click of the pair has already selected it.
tabs.addEventListener("dblclick", (e) => {
  const index = tabItems.findIndex((i) => mainOf(i) === (e.target as Element).closest("button"));
  if (index >= 0) renameTabAt(index);
});
workspaces.addEventListener("dblclick", (e) => {
  const index = chipItems.findIndex((i) => mainOf(i) === (e.target as Element).closest("button"));
  if (index >= 0) renameWorkspaceAt(index);
});

// The plus buttons send the layout commands without an index.
newWorkspace.addEventListener("click", () => {
  const count = state.view?.workspaces.length ?? 0;
  send(call("new_workspace", { name: `ws ${count + 1}` }));
  focusInput();
});
newTab.addEventListener("click", () => {
  send(call("new_tab"));
  focusInput();
});

// ---- the clock, the host name and the platform ---------------------------------------------------------------

const CLOCK_MS = 30_000;

function tick(): void {
  setText(clock, clockText(new Date()));
}

/** A string field of the `app_info` answer, cut to `max` characters; anything else is the empty string. */
function field(info: unknown, key: string, max: number): string {
  const value = typeof info === "object" && info !== null ? (info as Record<string, unknown>)[key] : undefined;
  return typeof value === "string" ? value.slice(0, max) : "";
}

/** Ask the core for the version, the platform and the host name once; a missing answer leaves them empty. */
async function loadInfo(): Promise<void> {
  let answer: unknown = null;
  try {
    answer = await call<unknown>("app_info");
  } catch {
    // the page runs without it
  }
  state.os = field(answer, "os", 16);
  state.version = field(answer, "version", 64);
  state.hostname = field(answer, "hostname", 255);
  setText(host, state.hostname);
  // The platform class is what the stylesheet keys on (room for the traffic lights on macOS).
  for (const name of Array.from(document.documentElement.classList)) {
    if (name.startsWith("os-")) document.documentElement.classList.remove(name);
  }
  if (/^[a-z]{2,16}$/.test(state.os)) document.documentElement.classList.add(`os-${state.os}`);
}

/**
 * Load the chrome stylesheet, ask for `app_info` and start the clock. Call once at start-up and wait for it, before
 * the first measurement: the bar and the status line need their sizes by then.
 */
export async function initChrome(): Promise<void> {
  // The marks change without a new layout (a bell, a card that waits); the bar is drawn again from the view it has.
  attention.subscribe(() => {
    if (state.view) renderBar(state.view, lastApply);
  });
  registerRenameActions();
  registerCloseActions();
  setPaneRenameHandler(renamePaneTo);
  setPaneCloseHandler(closePaneAfterAsking);
  tick();
  setInterval(tick, CLOCK_MS);
  await Promise.all([
    loadStyles("./chrome.css").catch(() => {
      // the page stays usable without its chrome styles; the layout tests would notice
    }),
    loadInfo(),
  ]);
}
