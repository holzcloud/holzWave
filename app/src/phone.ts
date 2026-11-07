// The phone's chrome and the layout class (docs/design.md section 6.6). On Android the page is a phone or a tablet by the
// width of its window and the short side of its screen (layoutclass.ts); every other window is a desktop and this module
// creates nothing there. On a phone one pane fills the screen (scene.ts cuts the core's view to the focused pane, phoneview.ts)
// and the window bar is replaced by three things drawn here: a top strip with the workspace and the tab, a bottom bar with the
// tabs of the shown workspace, and a sheet from the bottom with the workspaces, the tabs and the panes of the shown tab.
//
// The strips and the sheet send the same layout commands as the desktop bar, through the same helper (chrome.ts `sendLayout`),
// and every name in them is put in with `textContent`. The class is decided from the width only, and only when the width
// changed: an on-screen keyboard changes the height and must never flip the layout.
//
// Later modules follow the class through the `hw:layout` event on `document` (`detail.layout`), fired on every change and once
// at start when the class is not desktop. A module that starts after `initPhone` reads `layoutNow()` first.
import { idsOf } from "./attentionstate.js";
import { call } from "./bridge.js";
import { sendLayout, tabName } from "./chrome.js";
import { el, fitNodes, loadStyles, setAttr, setText } from "./dom.js";
import { focusInput } from "./keyboard.js";
import { pushKeyHandler } from "./keystack.js";
import { shortenHome, tabLabel } from "./labels.js";
import { type Layout, layoutClass } from "./layoutclass.js";
import { shownCount, shownIndex } from "./phoneview.js";
import { applyView, fit, rawViewNow } from "./scene.js";
import { state } from "./state.js";
import type { View } from "./types.js";

export type { Layout } from "./layoutclass.js";

/** The class the page has now. */
let layout: Layout = "desktop";

/** The window width the class was decided for: a resize that leaves it alone is not a new decision. */
let lastWidth = Number.NaN;

/** The class the page has now; `desktop` until `initPhone` has run. */
export function layoutNow(): Layout {
  return layout;
}

/** The numbers the rule reads. The short side of the screen is read here, in the browser; the rule itself takes numbers only. */
function facts(): { os: string; width: number; shortSide: number } {
  return { os: state.os, width: window.innerWidth, shortSide: Math.min(window.screen.width, window.screen.height) };
}

// ---- the three element groups ---------------------------------------------------------------------

/** What `build` makes. Every element is made once for the phone class and removed when the class goes. */
interface Ui {
  top: HTMLElement;
  title: HTMLButtonElement;
  workspace: HTMLElement;
  tab: HTMLElement;
  count: HTMLElement;
  nav: HTMLElement;
  navTabs: HTMLElement;
  navPlus: HTMLButtonElement;
  navSheet: HTMLButtonElement;
  scrim: HTMLElement;
  sheet: HTMLElement;
  pills: HTMLElement;
  pillPlus: HTMLButtonElement;
  tabsCap: HTMLElement;
  tabsList: HTMLElement;
  panesCap: HTMLElement;
  panesList: HTMLElement;
  sheetClose: HTMLButtonElement;
  /** The elements of the keyed lists, by position. */
  navItems: HTMLButtonElement[];
  pillItems: HTMLButtonElement[];
  tabRows: HTMLButtonElement[];
  paneRows: HTMLButtonElement[];
  /** The keyed lists' last rendering of the shown tab's active index, so the bottom bar scrolls only when it changed. */
  shownTab: number;
  /** The captions and lists that later modules added with `addSheetRow`, so they can be made again. */
  extra: HTMLElement[];
}

let ui: Ui | null = null;

/** A button with a class; `type="button"` so it never submits anything. */
function button(cls: string, parent?: Element): HTMLButtonElement {
  const node = el("button", cls, parent);
  node.type = "button";
  return node;
}

/** A button of a keyed list: its `data-index` is read when it is pressed, so the same element serves a changed list. */
function indexed(cls: string, pick: (index: number) => void): HTMLButtonElement {
  const node = button(cls);
  node.addEventListener("click", () => pick(Number(node.dataset.index)));
  return node;
}

/** Mark `node` as the shown one: a class for the style and `aria-current` for assistive technology. */
function setCurrent(node: HTMLElement, on: boolean, current: string): void {
  node.classList.toggle("active", on);
  setAttr(node, "aria-current", on ? current : null);
}

function build(): Ui {
  const app = document.getElementById("app") as HTMLElement;

  const top = el("header", "");
  top.id = "phone-top";
  const title = button("phone-title", top);
  title.setAttribute("aria-haspopup", "dialog");
  title.setAttribute("aria-expanded", "false");
  const dia = el("span", "phone-dia", title);
  dia.setAttribute("aria-hidden", "true");
  const workspace = el("span", "phone-ws", title);
  const sep = el("span", "phone-sep", title);
  sep.setAttribute("aria-hidden", "true");
  sep.textContent = "·";
  const tab = el("span", "phone-tab", title);
  const count = el("span", "phone-count", title);
  // The place of a status chip: empty now, kept so that nothing moves when a later feature fills it.
  const chip = el("span", "", top);
  chip.id = "phone-chip";
  app.prepend(top);

  const nav = el("nav", "", app);
  nav.id = "phone-nav";
  nav.setAttribute("aria-label", "Tabs");
  const navTabs = el("div", "phone-tabs", nav);
  const navPlus = button("phone-plus", nav);
  navPlus.textContent = "+";
  navPlus.title = "New tab";
  navPlus.setAttribute("aria-label", "New tab");
  navPlus.addEventListener("click", () => {
    sendLayout(call<View>("new_tab"));
    focusInput();
  });
  const navSheet = button("phone-opener", nav);
  navSheet.textContent = "Sheet";
  navSheet.setAttribute("aria-haspopup", "dialog");
  navSheet.setAttribute("aria-expanded", "false");

  const scrim = el("div", "", document.body);
  scrim.id = "phone-scrim";
  scrim.hidden = true;
  scrim.addEventListener("click", () => closeSheet("opener"));
  const sheet = el("div", "", document.body);
  sheet.id = "phone-sheet";
  sheet.hidden = true;
  sheet.setAttribute("role", "dialog");
  sheet.setAttribute("aria-modal", "true");
  sheet.setAttribute("aria-label", "Workspaces, tabs and panes");
  sheet.tabIndex = -1;

  el("div", "label phone-cap", sheet).textContent = "Workspaces";
  const pills = el("div", "phone-pills", sheet);
  pills.setAttribute("role", "group");
  pills.setAttribute("aria-label", "Workspaces");
  const pillPlus = button("phone-pill phone-pill-plus", pills);
  pillPlus.textContent = "+";
  pillPlus.title = "New workspace";
  pillPlus.setAttribute("aria-label", "New workspace");
  pillPlus.addEventListener("click", () => {
    sendLayout(call<View>("new_workspace", { name: `ws ${(rawViewNow()?.workspaces.length ?? 0) + 1}` }));
  });
  const tabsCap = el("div", "label phone-cap", sheet);
  const tabsList = el("div", "phone-list", sheet);
  tabsList.setAttribute("role", "group");
  tabsList.setAttribute("aria-label", "Tabs");
  const panesCap = el("div", "label phone-cap", sheet);
  const panesList = el("div", "phone-list", sheet);
  panesList.setAttribute("role", "group");
  panesList.setAttribute("aria-label", "Panes");
  const sheetClose = button("phone-sheet-close", sheet);
  sheetClose.textContent = "Sheet";
  sheetClose.setAttribute("aria-expanded", "true");
  sheetClose.addEventListener("click", () => closeSheet("opener"));

  title.addEventListener("click", () => openSheet(title));
  navSheet.addEventListener("click", () => openSheet(navSheet));

  const made: Ui = {
    top, title, workspace, tab, count, nav, navTabs, navPlus, navSheet, scrim, sheet, pills, pillPlus, tabsCap, tabsList,
    panesCap, panesList, sheetClose, navItems: [], pillItems: [], tabRows: [], paneRows: [], shownTab: -1, extra: [],
  };
  mountExtraRows(made);
  return made;
}

// ---- rows that a later module adds to the sheet -----------------------------------------------------

/** A row of a group that another module owns (the file hand-over's two rows): its caption, its words and what a tap does. */
interface SheetRow {
  caption: string;
  label: string;
  run: () => void;
}

const extraRows: SheetRow[] = [];

/**
 * Add a row to the sheet, in the group named `caption` (the first row of a caption makes the group, after the panes and before
 * the footer). A tap closes the sheet and then runs `run`; the focus is left where it is, because what runs may open a system
 * dialog. The sheet exists on the phone only and is made again when the class changes, so the rows are kept here and put in each
 * time it is made. Nothing is drawn on a tablet or a desktop.
 */
export function addSheetRow(caption: string, label: string, run: () => void): void {
  extraRows.push({ caption, label, run });
  if (ui) mountExtraRows(ui);
}

/** Make the groups of the added rows again, so a row added after the sheet was made shows and none shows twice. */
function mountExtraRows(g: Ui): void {
  for (const node of g.extra) node.remove();
  g.extra = [];
  const lists = new Map<string, HTMLElement>();
  for (const row of extraRows) {
    let list = lists.get(row.caption);
    if (!list) {
      const cap = el("div", "label phone-cap");
      cap.textContent = row.caption;
      list = el("div", "phone-list");
      list.setAttribute("role", "group");
      list.setAttribute("aria-label", row.caption);
      g.sheet.insertBefore(cap, g.sheetClose);
      g.sheet.insertBefore(list, g.sheetClose);
      g.extra.push(cap, list);
      lists.set(row.caption, list);
    }
    const node = button("phone-row", list);
    el("span", "phone-row-name", node).textContent = row.label;
    node.addEventListener("click", () => {
      closeSheet("none");
      row.run();
    });
  }
}

/** Remove everything `build` made, and anything the sheet changed on the page. */
function teardown(): void {
  closeSheet("none");
  ui?.top.remove();
  ui?.nav.remove();
  ui?.scrim.remove();
  ui?.sheet.remove();
  ui = null;
}

// ---- drawing from the view ------------------------------------------------------------------------

/** What a pane's row calls the pane: the name the user gave, else the title its program set, else `Pane N`. */
function paneName(view: View, id: number, position: number): string {
  return view.pane_names?.find(([pane]) => pane === id)?.[1] || state.titles.get(id) || `Pane ${position}`;
}

/**
 * Bring the strips and the sheet in line with the core's view. Called after every view and every change of a title; it does
 * nothing unless the class is `phone`. The elements are kept and updated in place, so a focused row stays focused.
 */
export function renderPhone(): void {
  const view = rawViewNow() ?? state.view;
  if (!ui || layout !== "phone" || !view) return;
  const g = ui;
  const wi = view.workspaces.findIndex((w) => w.active);
  const ti = view.tabs.findIndex((t) => t.active);
  const w = view.workspaces[wi];
  const t = view.tabs[ti];
  const wsName = w?.name ?? "";
  const tName = t ? tabLabel(ti, tabName(t)) : "";

  // The top strip: workspace, tab, and `2/3` when the tab has several panes.
  setText(g.workspace, wsName);
  setText(g.tab, tName);
  const many = shownCount(view) > 1;
  setText(g.count, many ? `${shownIndex(view)}/${shownCount(view)}` : "");
  g.count.hidden = !many;

  // The bottom bar: one numbered tab for each tab of the shown workspace, the shown one underlined.
  fitNodes(g.navTabs, g.navItems, view.tabs.length, () => indexed("phone-tab-btn", selectTab));
  view.tabs.forEach((tab, i) => {
    const node = g.navItems[i] as HTMLButtonElement;
    setAttr(node, "data-index", String(i));
    setText(node, tabLabel(i, tabName(tab)));
    setCurrent(node, tab.active, "page");
  });
  if (g.shownTab !== ti) {
    g.shownTab = ti;
    const node = g.navItems[ti];
    if (node) {
      const left = node.offsetLeft;
      const right = left + node.offsetWidth;
      if (left < g.navTabs.scrollLeft) g.navTabs.scrollLeft = left;
      else if (right > g.navTabs.scrollLeft + g.navTabs.clientWidth) g.navTabs.scrollLeft = right - g.navTabs.clientWidth;
    }
  }

  // The sheet: workspace pills (the plus stays last), the tabs of the shown workspace, the panes of the shown tab.
  fitNodes(g.pills, g.pillItems, view.workspaces.length, () => indexed("phone-pill", selectWorkspace), g.pillPlus);
  view.workspaces.forEach((ws, i) => {
    const node = g.pillItems[i] as HTMLButtonElement;
    setAttr(node, "data-index", String(i));
    setText(node, ws.name);
    setCurrent(node, ws.active, "true");
  });
  setText(g.tabsCap, `Tabs in ${wsName}`);
  fitNodes(g.tabsList, g.tabRows, view.tabs.length, () => indexed("phone-row", pickTab));
  view.tabs.forEach((tab, i) => {
    const node = g.tabRows[i] as HTMLButtonElement;
    setAttr(node, "data-index", String(i));
    let name = node.firstElementChild as HTMLElement | null;
    if (!name) name = el("span", "phone-row-name", node);
    setText(name, tabLabel(i, tabName(tab)));
    setCurrent(node, tab.active, "page");
  });
  const ids = idsOf(t?.pane_ids);
  setText(g.panesCap, `Panes in ${t ? tabName(t) : ""}`);
  fitNodes(g.panesList, g.paneRows, ids.length, () => indexed("phone-row phone-pane", pickPane));
  ids.forEach((id, i) => {
    const node = g.paneRows[i] as HTMLButtonElement;
    setAttr(node, "data-index", String(i));
    setAttr(node, "data-pane", String(id));
    let dot = node.children[0] as HTMLElement | undefined;
    if (!dot) {
      dot = el("span", "ph-dot", node);
      dot.setAttribute("aria-hidden", "true");
      el("span", "phone-row-name", node);
      el("span", "phone-row-ctx", node);
    }
    const focused = id === view.focus;
    setAttr(dot, "data-state", focused ? "focused" : null);
    setText(node.children[1] as HTMLElement, paneName(view, id, i + 1));
    const cwd = state.cwds.get(id);
    setText(node.children[2] as HTMLElement, cwd ? shortenHome(cwd, state.os) : "");
    setCurrent(node, focused, "true");
    setAttr(node, "aria-label", focused ? `${paneName(view, id, i + 1)}, shown` : null);
  });
}

// ---- what a tap does ------------------------------------------------------------------------------

/** A tab of the bottom bar. */
function selectTab(index: number): void {
  sendLayout(call<View>("select_tab", { index }));
  focusInput();
}

/** A workspace pill: the sheet stays open, so the person can pick one of its tabs next. */
function selectWorkspace(index: number): void {
  sendLayout(call<View>("select_workspace", { index }));
}

/** A tab row of the sheet: show the tab and close the sheet. */
function pickTab(index: number): void {
  sendLayout(call<View>("select_tab", { index }));
  closeSheet("terminal");
}

/** A pane row of the sheet: focus the pane (it then fills the screen) and close the sheet. */
function pickPane(index: number): void {
  const id = idsOf(rawViewNow()?.tabs.find((t) => t.active)?.pane_ids)[index];
  if (id !== undefined) sendLayout(call<View>("focus_pane", { pane: id }));
  closeSheet("terminal");
}

// ---- the sheet --------------------------------------------------------------------------------------

/** The control that opened the sheet, which gets the focus back when the sheet is dismissed. */
let opener: HTMLElement | null = null;
/** Takes the sheet's key handler off the stack again. */
let popKeys: (() => void) | null = null;

/** The sheet's focusable controls in the order of the page. */
function sheetControls(): HTMLElement[] {
  return ui ? [...ui.sheet.querySelectorAll<HTMLElement>("button")] : [];
}

/** Escape closes the sheet, Tab stays inside it, and nothing else reaches the terminal behind it. */
function onSheetKey(e: KeyboardEvent): boolean {
  if (e.metaKey) return true;
  if (e.key === "Escape") {
    e.preventDefault();
    closeSheet("opener");
  } else if (e.key === "Tab") {
    const controls = sheetControls();
    const first = controls[0];
    const last = controls[controls.length - 1];
    const at = controls.indexOf(document.activeElement as HTMLElement);
    if (first && last && e.shiftKey && at <= 0) {
      e.preventDefault();
      last.focus();
    } else if (first && last && !e.shiftKey && at === controls.length - 1) {
      e.preventDefault();
      first.focus();
    }
  }
  return true;
}

function openSheet(from: HTMLElement): void {
  if (!ui || !ui.sheet.hidden) return;
  opener = from;
  renderPhone();
  ui.scrim.hidden = false;
  ui.sheet.hidden = false;
  ui.title.setAttribute("aria-expanded", "true");
  ui.navSheet.setAttribute("aria-expanded", "true");
  // Everything behind the sheet is out of reach while it is open: no tap, no focus, no screen reader stop.
  document.getElementById("app")?.setAttribute("inert", "");
  popKeys = pushKeyHandler(onSheetKey);
  (ui.tabsList.querySelector<HTMLElement>(".active") ?? ui.sheet).focus({ preventScroll: true });
}

/** Close the sheet. `opener` gives the focus back to the control that opened it, `terminal` to the terminal input, `none` leaves it. */
function closeSheet(focus: "opener" | "terminal" | "none"): void {
  if (!ui || ui.sheet.hidden) return;
  ui.sheet.hidden = true;
  ui.scrim.hidden = true;
  ui.title.setAttribute("aria-expanded", "false");
  ui.navSheet.setAttribute("aria-expanded", "false");
  document.getElementById("app")?.removeAttribute("inert");
  popKeys?.();
  popKeys = null;
  const back = opener;
  opener = null;
  if (focus === "opener") back?.focus({ preventScroll: true });
  else if (focus === "terminal") focusInput();
}

// ---- the class ---------------------------------------------------------------------------------------

/** Put the page in the class `next`: the root classes, the stylesheet and the elements, then tell the others. */
async function setLayout(next: Layout): Promise<void> {
  if (next === layout) return;
  const previous = layout;
  layout = next;
  const root = document.documentElement;
  root.classList.toggle("layout-phone", next === "phone");
  root.classList.toggle("layout-tablet", next === "tablet");
  if (next !== "desktop") await loadStyles("./phone.css").catch(() => {});
  if (layout !== next) return; // the window changed again while the stylesheet loaded; that call has the last word
  if (next === "phone" && !ui) ui = build();
  if (next !== "phone" && ui) teardown();
  // The panes the page measured belong to the layout before: measure again, then show the view the new class makes of the core's.
  const view = rawViewNow();
  if (view && previous !== next) {
    fit();
    applyView(view);
  }
  if (next === "phone") renderPhone();
  if (previous !== "desktop" || next !== "desktop") {
    document.dispatchEvent(new CustomEvent("hw:layout", { detail: { layout: next } }));
  }
}

/** A resize or a turn of the screen: decide again only when the width changed, never for the height the keyboard takes. */
function onResize(): void {
  if (window.innerWidth === lastWidth) return;
  lastWidth = window.innerWidth;
  void setLayout(layoutClass(facts()));
}

/**
 * Decide the class once, before the first measurement (the stylesheet of the class is waited for), and follow the width
 * from then on. On a desktop it adds a listener and nothing else. Call once at start-up, after `initChrome` (the platform is known).
 */
export async function initPhone(): Promise<void> {
  lastWidth = window.innerWidth;
  await setLayout(layoutClass(facts()));
  window.addEventListener("resize", onResize);
  window.addEventListener("orientationchange", onResize);
  // The vault's chip, sheet and notices, and the file hand-over's entries, exist on Android only, and neither module is fetched
  // anywhere else.
  if (state.os === "android") {
    void import("./vault.js").then((m) => m.initVault()).catch(() => {});
    void import("./handover.js").then((m) => m.initHandover()).catch(() => {});
  }
}
