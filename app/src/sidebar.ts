// The sidebar (docs/design.md section 6.11): a column at the right of the stage with the saved hosts, a history search and
// the widgets. It is hidden until asked (`Cmd+B`, the title bar button, the palette) and remembers whether it was shown.
// Every string goes in with `textContent`. The sidebar's fields are text fields of the page: they keep their own keys, and
// `Escape` gives the keyboard back to the terminal. A command from the history or a widget is put on the prompt of the
// focused pane and never run; a link is opened by the system.
import { registerAction } from "./actions.js";
import { aiFormShown, columnChanged, initAiSidebar, showSidebarForm } from "./aisidebar.js";
import { call } from "./bridge.js";
import { connect, connections } from "./connections.js";
import { paneConns } from "./connstate.js";
import { el, setText } from "./dom.js";
import { mountContextSection } from "./integratecontext.js";
import { readHits, meta } from "./historystate.js";
import type { Hit } from "./historystate.js";
import { fit } from "./scene.js";
import { state } from "./state.js";
import { IDLE, hostState, left, mmss, pause, phaseWord, progress, readWidgets, resume, start, tick } from "./sidebarstate.js";
import type { TimerState, WidgetDef } from "./sidebarstate.js";

const KEY = "holzwave.sidebar";
const HISTORY_ROWS = 8;

let side: HTMLElement | null = null;
let button: HTMLButtonElement | null = null;
let shown = false;
let focusInput: () => void = () => {};
let timers: ReturnType<typeof setInterval>[] = [];
let hostsBox: HTMLElement | null = null;
let widgetsBox: HTMLElement | null = null;
let lastWidgets = "";
const clocks = new Map<string, TimerState>();

function remembered(): boolean {
  try {
    return localStorage.getItem(KEY) === "1";
  } catch {
    return false;
  }
}

function remember(on: boolean): void {
  try {
    localStorage.setItem(KEY, on ? "1" : "0");
  } catch {
    // a remembered choice is a convenience
  }
}

/** Put `text` on the prompt of the focused pane. Never runs it: no newline goes with it. */
function toPrompt(text: string): void {
  const pane = state.view?.focus;
  if (pane !== undefined) void call("paste", { pane, text, allow_unsafe: false }).catch(() => {});
  focusInput();
}

function section(parent: HTMLElement, title: string, hint = ""): HTMLElement {
  const sec = el("section", "sb-section", parent);
  const head = el("div", "sb-head label", sec);
  el("span", "", head).textContent = title;
  if (hint) el("span", "sb-hint", head).textContent = hint;
  return sec;
}

// ---- hosts ---------------------------------------------------------------------------------------------------------

function drawHosts(): void {
  if (!hostsBox) return;
  const list = connections();
  hostsBox.replaceChildren();
  if (list.length === 0) {
    el("p", "sb-empty", hostsBox).textContent = "No saved connections. Add one with Connections… in the palette.";
    return;
  }
  for (const c of list) {
    const { state: st, note } = hostState(c.id, paneConns.values());
    const row = el("button", "sb-host", hostsBox);
    row.type = "button";
    row.dataset.state = st;
    el("span", "sb-dot", row).dataset.state = st;
    el("span", "sb-name", row).textContent = c.name;
    el("span", "sb-tag", row).textContent = note || c.kind;
    row.addEventListener("click", () => void connect(c.id));
  }
}

// ---- history -------------------------------------------------------------------------------------------------------

function buildHistory(parent: HTMLElement): void {
  const sec = section(parent, "History");
  const input = el("input", "sb-input", sec);
  input.type = "text";
  input.placeholder = "Search commands";
  input.setAttribute("aria-label", "Search the history");
  input.autocomplete = "off";
  input.spellcheck = false;
  const list = el("ul", "sb-list", sec);
  list.setAttribute("role", "listbox");
  list.setAttribute("aria-label", "Commands");
  let hits: Hit[] = [];
  let active = 0;
  let seq = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const mark = (): void => Array.from(list.children).forEach((r, i) => r.setAttribute("aria-selected", i === active ? "true" : "false"));
  const draw = (): void => {
    list.replaceChildren();
    const now = Math.floor(Date.now() / 1000);
    hits.forEach((h) => {
      const row = el("li", "sb-row", list);
      row.setAttribute("role", "option");
      row.title = meta(h, now);
      el("span", "sb-cmd", row).textContent = h.cmd;
      row.addEventListener("click", () => toPrompt(h.cmd));
    });
    if (hits.length === 0) el("li", "sb-empty", list).textContent = input.value.trim() === "" ? "Nothing typed yet. See wsh init." : "No match";
    active = 0;
    mark();
  };
  const search = async (): Promise<void> => {
    const mine = ++seq;
    try {
      const found = readHits(await call<unknown>("history_search", { query: input.value, limit: HISTORY_ROWS }));
      if (mine === seq) hits = found;
    } catch {
      if (mine === seq) hits = [];
    }
    if (mine === seq) draw();
  };
  input.addEventListener("input", () => {
    clearTimeout(timer);
    timer = setTimeout(() => void search(), 80);
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      focusInput();
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (hits.length > 0) active = (active + (e.key === "ArrowDown" ? 1 : -1) + hits.length) % hits.length;
      mark();
    } else if (e.key === "Enter" && hits[active]) {
      e.preventDefault();
      toPrompt(hits[active]!.cmd);
    }
  });
  void search();
}

// ---- widgets -------------------------------------------------------------------------------------------------------

function noteWidget(box: HTMLElement, def: WidgetDef, saved: string): void {
  const area = el("textarea", "sb-note", box);
  area.value = saved;
  area.maxLength = 65536;
  area.rows = 5;
  area.spellcheck = false;
  area.setAttribute("aria-label", def.title);
  const status = el("span", "sb-hint", box);
  let timer: ReturnType<typeof setTimeout> | undefined;
  area.addEventListener("input", () => {
    setText(status, "unsaved");
    clearTimeout(timer);
    timer = setTimeout(() => {
      void call("widget_note_save", { id: def.id, text: area.value }).then(
        () => setText(status, "saved"),
        (e) => setText(status, String(e)),
      );
    }, 600);
  });
  area.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      focusInput();
    }
  });
}

function timerWidget(box: HTMLElement, def: Extract<WidgetDef, { kind: "timer" }>): void {
  const face = el("div", "sb-clock", box);
  const bar = el("div", "sb-progress", box);
  const fill = el("i", "", bar);
  const word = el("span", "sb-hint", box);
  const controls = el("div", "sb-controls", box);
  const go = el("button", "sb-btn", controls);
  const reset = el("button", "sb-btn", controls);
  reset.textContent = "Reset";
  go.type = reset.type = "button";
  const get = (): TimerState => clocks.get(def.id) ?? IDLE;
  const set = (s: TimerState): void => {
    clocks.set(def.id, s);
    draw();
  };
  const draw = (): void => {
    const now = Date.now();
    const s = tick(get(), def, now);
    if (s !== get()) clocks.set(def.id, s);
    setText(face, mmss(left(s, def, now)));
    fill.dataset.mode = s.mode;
    bar.style.setProperty("--p", String(progress(s, def, now)));
    setText(word, phaseWord(s));
    setText(go, s.mode === "idle" ? "Start" : s.paused !== null ? "Resume" : "Pause");
    reset.hidden = s.mode === "idle";
  };
  go.addEventListener("click", () => {
    const s = get();
    set(s.mode === "idle" ? start(def, Date.now()) : s.paused !== null ? resume(s, Date.now()) : pause(s, Date.now()));
  });
  reset.addEventListener("click", () => set(IDLE));
  draw();
  timers.push(setInterval(draw, 1000));
}

function listWidget(box: HTMLElement, def: Extract<WidgetDef, { kind: "list" }>): void {
  const list = el("ul", "sb-list", box);
  for (const item of def.items) {
    const row = el("li", "sb-row sb-item", list);
    const b = el("button", "sb-link", row);
    b.type = "button";
    el("span", "sb-cmd", b).textContent = item.label;
    el("span", "sb-tag", b).textContent = item.command !== undefined ? "prompt" : "link";
    b.title = item.command ?? item.url ?? "";
    b.addEventListener("click", () => {
      if (item.command !== undefined) toPrompt(item.command);
      else if (item.url !== undefined) void call("open_url", { url: item.url }).catch(() => {});
    });
  }
}

async function loadWidgets(): Promise<void> {
  if (!widgetsBox) return;
  let raw: unknown = null;
  try {
    raw = await call<unknown>("widgets_list");
  } catch {
    return;
  }
  const key = JSON.stringify(raw);
  // The widgets are drawn again only when they changed, so a note being typed in keeps its focus.
  if (key === lastWidgets) return;
  lastWidgets = key;
  const { widgets, notes, warnings } = readWidgets(raw);
  for (const t of timers) clearInterval(t);
  timers = [];
  widgetsBox.replaceChildren();
  for (const def of widgets) {
    const box = el("div", "sb-widget", widgetsBox);
    el("div", "sb-title", box).textContent = def.title;
    if (def.kind === "note") noteWidget(box, def, notes[def.id] ?? "");
    else if (def.kind === "timer") timerWidget(box, def);
    else listWidget(box, def);
  }
  for (const w of warnings) el("p", "sb-warn", widgetsBox).textContent = w;
}

// ---- showing it -----------------------------------------------------------------------------------------------------

/** The sections go into `div.sb-main`, which the column hides while it shows the AI form (aisidebar.ts); the box is answered. */
function build(container: HTMLElement): HTMLElement {
  container.replaceChildren();
  const main = el("div", "sb-main", container);
  const hosts = section(main, "Hosts");
  hostsBox = el("div", "sb-hosts", hosts);
  // The tools the person already uses on the command line: drawn once the core has listed them (integratecontext.ts).
  mountContextSection(el("div", "cx-slot", main), () => {
    setSidebar(true);
    showSidebarForm();
  });
  buildHistory(main);
  const widgets = section(main, "Widgets");
  widgetsBox = el("div", "", widgets);
  return main;
}

/** Show or hide the sidebar. The stage changes size, so the grid is measured again. */
export function setSidebar(on: boolean): void {
  if (!side) return;
  shown = on;
  // The AI form is told before the column hides, so it can give the keyboard back while the focus is still inside it.
  columnChanged(on);
  side.hidden = !on;
  button?.setAttribute("aria-pressed", on ? "true" : "false");
  remember(on);
  // Hosts and widgets are drawn only while the column shows the sidebar form.
  if (on && !aiFormShown()) {
    drawHosts();
    void loadWidgets();
  }
  fit();
}

export function toggleSidebar(): void {
  setSidebar(!shown);
}

/** Make the sidebar, its button and its action. Call once at start-up, after the title bar exists. */
export async function initSidebar(): Promise<void> {
  side = document.getElementById("sidebar");
  if (!side) return;
  ({ focusInput } = await import("./keyboard.js"));
  try {
    const { loadStyles } = await import("./dom.js");
    await loadStyles("./sidebar.css");
  } catch {
    // the sidebar still works without its style sheet
  }
  const main = build(side);
  await initAiSidebar(side, main, {
    shown: () => shown,
    show: () => setSidebar(true),
    fit,
    refresh: () => {
      drawHosts();
      void loadWidgets();
    },
  });
  const info = document.querySelector(".bar-info");
  if (info) {
    button = el("button", "sidebar-btn label");
    button.type = "button";
    button.textContent = "▥ Sidebar";
    button.setAttribute("aria-pressed", "false");
    button.addEventListener("click", toggleSidebar);
    info.append(button);
  }
  registerAction({ id: "sidebar.toggle", title: "Show or hide the sidebar", run: toggleSidebar });
  // The hosts follow the connections; the widget files are read again now and then (a hand edit shows up).
  setInterval(() => {
    if (!shown || aiFormShown()) return;
    drawHostsIfChanged();
  }, 1000);
  setInterval(() => {
    if (shown && !aiFormShown()) void loadWidgets();
  }, 10_000);
  if (remembered()) setSidebar(true);
}

let lastHosts = "";
function drawHostsIfChanged(): void {
  const key = JSON.stringify([connections().map((c) => [c.id, c.name, c.kind]), [...paneConns.values()].map((c) => [c.profile, c.state, c.attempt, c.of])]);
  if (key === lastHosts) return;
  lastHosts = key;
  drawHosts();
}
