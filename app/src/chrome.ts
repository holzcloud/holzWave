// The window bar above the stage: workspace chips, numbered tabs, and the host name and clock at the right. It is
// drawn from `state.view` and the pane titles; the elements are keyed by position and updated in place, and every
// name from the core is put in with `textContent`, never parsed as markup. The bar's free space drags the window.
import { call } from "./bridge.js";
import { el, fitNodes, loadStyles, setAttr, setText } from "./dom.js";
import { focusInput } from "./keyboard.js";
import { clockText, tabLabel } from "./labels.js";
import { state } from "./state.js";
import type { View } from "./types.js";

const bar = document.getElementById("bar") as HTMLDivElement;

/** Mark an element of the bar as part of the window's drag region (Tauri reads the attribute of the target only). */
function draggable<T extends HTMLElement>(node: T): T {
  node.setAttribute("data-tauri-drag-region", "");
  return node;
}

/** What a click on a chip, tab or plus button asks the core for; its answer is a view for `applyView`. */
let send: (request: Promise<View>) => void = () => {};

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
draggable(el("div", "bar-fill", bar));
const info = draggable(el("div", "bar-info", bar));
const host = draggable(el("span", "label host", info));
const clock = draggable(el("span", "label clock", info));

const chips: HTMLButtonElement[] = [];
const tabButtons: HTMLButtonElement[] = [];

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

  // New elements go before the plus button, which stays last in its group.
  fitNodes(
    workspaces,
    chips,
    view.workspaces.length,
    () => button("ws-chip label", (index) => call("select_workspace", { index })),
    newWorkspace,
  );
  view.workspaces.forEach((w, i) => {
    const chip = chips[i] as HTMLButtonElement;
    setAttr(chip, "data-index", String(i));
    setText(chip, w.name);
    setActive(chip, w.active, "true");
  });

  fitNodes(tabs, tabButtons, view.tabs.length, () => button("tab", (index) => call("select_tab", { index })), newTab);
  view.tabs.forEach((t, i) => {
    const tab = tabButtons[i] as HTMLButtonElement;
    // The title the focus pane's program set wins over the tab's own title.
    const label = tabLabel(i, state.titles.get(t.focus) || t.title);
    setAttr(tab, "data-index", String(i));
    setText(tab, label);
    setAttr(tab, "title", label);
    setActive(tab, t.active, "page");
  });
}

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
  tick();
  setInterval(tick, CLOCK_MS);
  await Promise.all([
    loadStyles("./chrome.css").catch(() => {
      // the page stays usable without its chrome styles; the layout tests would notice
    }),
    loadInfo(),
  ]);
}
