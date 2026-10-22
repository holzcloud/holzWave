// The integration block (docs/design.md section 6.25): one pane that draws what the core reads from a tool (Kubernetes,
// Talos, Azure, GAM) as a table, a list of values or a text. The core runs the tool and cleans what it printed; this
// module asks for a view, draws the model it gets with `textContent` (integrateview.ts) and says what it is doing in
// words. It builds no name, no argument list and no program: it sends the facts the core listed.
import { call } from "./bridge.js";
import { el, loadStyles, setAttr, setText } from "./dom.js";
import type { BlockInfo } from "./blockstate.js";
import type { Instance } from "./blocks.js";
import { focusInput } from "./keyboard.js";
import {
  ageWords, clearIntegrationContext, contextText, readFault, readRunAnswer, readTools, setIntegrationContext, stateWords,
} from "./integratestate.js";
import type { Fault, Sel, ToolEntry, ToolId, ViewModel } from "./integratestate.js";
import { renderKv, renderTable, renderText } from "./integrateview.js";
import type { DrawOptions } from "./integrateview.js";
import type { View } from "./types.js";

/** What the block needs from the block layer, handed over so that this module does not import it. */
export interface Deps {
  /** Show a layout (the answer of a launch). */
  applyView(v: View): void;
  /** Ask the core which panes are blocks, and draw them. */
  refreshBlocks(): Promise<void>;
  /** The header of a pane has new words: draw the strips again. */
  changed(): void;
}

// ---- the tools, shared by the blocks of the window ---------------------------------------------------------------

let tools: ToolEntry[] | null = null;
let toolsAsk: Promise<ToolEntry[]> | null = null;
const toolListeners = new Set<() => void>();

/** The tool entries the core listed, asked once; `force` asks again (the person set a path, or the core said they changed). */
function loadTools(force = false): Promise<ToolEntry[]> {
  if (toolsAsk && !force) return toolsAsk;
  const ask = (async () => {
    try {
      tools = readTools(await call<unknown>("integration_tools"));
    } catch {
      // no answer is no list: the block still reads its view
      tools ??= [];
    }
    for (const notify of [...toolListeners]) notify();
    return tools ?? [];
  })();
  toolsAsk = ask;
  return ask;
}

/** The core answered with the tool entries again (a path was set or cleared). */
function setTools(raw: unknown): void {
  const list = readTools(raw);
  if (list.length === 0) return;
  tools = list;
  toolsAsk = Promise.resolve(list);
  for (const notify of [...toolListeners]) notify();
}

function toolOf(id: string): ToolEntry | null {
  return tools?.find((t) => t.id === id) ?? null;
}

// ---- the block ---------------------------------------------------------------------------------------------------

export function integrationBlock(info: BlockInfo, root: HTMLElement, deps: Deps): Instance {
  root.classList.add("block-integration");
  void loadStyles("./integrate.css").catch(() => {
    // the block still works without its style sheet
  });
  let current = info;
  let alive = true;

  const bar = el("div", "ig-bar", root);
  const body = el("div", "ig-body", root);
  const status = el("div", "ig-status", root);
  status.setAttribute("role", "status");

  let model: ViewModel | null = null;
  let selection: Sel = {};
  let production = false;
  let phase: "reading" | "ready" | "failed" = "reading";
  let fault: Fault | null = null;
  /** The core refused what the person asked for in the path field: said on the status line, the state stays. */
  let refusal: Fault | null = null;
  let selected = 0;
  /** When the last good answer came, in the page's clock. */
  let received = 0;
  /** What was typed in the path field: it survives a redraw. */
  let pathText = "";

  const tool = (): ToolId => current.source as ToolId;

  /** Say in the header what the pane reads and from where. */
  const describe = (): void => {
    const entry = toolOf(current.source);
    const verb = entry?.verbs.find((v) => v.id === current.path);
    const text = contextText(
      { label: entry?.label ?? current.source_label, selects: entry?.selects ?? [] },
      selection,
      verb?.refresh_secs ?? null,
    );
    if (setIntegrationContext(current.pane, text, production)) deps.changed();
    if (alive) draw();
  };

  const options = (): DrawOptions => ({
    selected,
    drawn: () => false,
    press: () => {},
    pick: (i) => {
      selected = i;
      mark();
    },
  });

  /** Move the highlight without drawing the table again. */
  const mark = (): void => {
    const rows = body.querySelectorAll("tr.ig-row");
    rows.forEach((row, i) => row.setAttribute("aria-selected", String(i === selected)));
    (rows[selected] as HTMLElement | undefined)?.scrollIntoView({ block: "nearest" });
  };

  /** The tool cannot be started: the core said it is missing or unusable. */
  const unusable = (): boolean => fault !== null && fault.code === "missing";

  const note = (text: string, kind?: string): void => {
    const p = el("p", "ig-note", body);
    if (kind) p.dataset.kind = kind;
    p.textContent = text;
  };

  /** The tool is not there: the places that were looked in, a field for a path, and the two buttons. */
  const drawMissing = (): void => {
    body.replaceChildren();
    const box = el("div", "ig-missing", body);
    const entry = toolOf(current.source);
    if (entry && entry.searched.length > 0) {
      el("p", "ig-places-title", box).textContent = "Looked in";
      const list = el("ul", "ig-places", box);
      for (const place of entry.searched) el("li", "", list).textContent = place;
    }
    const row = el("div", "ig-pathrow", box);
    const input = el("input", "ig-path", row);
    input.type = "text";
    input.dataset.field = "";
    input.spellcheck = false;
    input.autocomplete = "off";
    input.placeholder = "Absolute path of the program";
    input.setAttribute("aria-label", "Path of the program");
    input.value = pathText;
    input.addEventListener("input", () => {
      pathText = input.value;
    });
    input.addEventListener("keydown", (e) => {
      if (e.isComposing) return;
      if (e.key === "Escape") {
        e.preventDefault();
        focusInput();
      } else if (e.key === "Enter") {
        e.preventDefault();
        void setPath();
      }
    });
    const set = el("button", "ig-pathbtn", row);
    set.type = "button";
    set.dataset.field = "";
    set.textContent = "Set path";
    set.addEventListener("click", () => void setPath());
    const again = el("button", "ig-pathbtn", row);
    again.type = "button";
    again.dataset.field = "";
    again.textContent = "Check again";
    again.addEventListener("click", () => void checkAgain());
  };

  const draw = (): void => {
    const keep = focusedAction();
    if (unusable()) {
      drawMissing();
    } else if (model === null) {
      body.replaceChildren();
    } else if (model.kind === "table") {
      selected = Math.max(0, Math.min(selected, model.rows.length - 1));
      renderTable(body, model, options());
      if (model.rows.length === 0) note(stateWords("empty").text, "empty");
      if (model.truncated) note(model.truncated.note, "cut");
      if (model.note) note(model.note);
    } else if (model.kind === "kv") {
      renderKv(body, model, options());
      if (model.note) note(model.note);
    } else {
      renderText(body, model);
      if (model.note) note(model.note);
    }
    // The rows of the last good answer are dimmed while a later read has failed.
    setAttr(body, "data-stale", fault !== null && model !== null && !unusable() ? "true" : null);
    const said = refusal ?? fault;
    const words =
      phase === "reading" ? stateWords("reading") : said ? { text: said.text, tone: "warn" as const } : { text: ageWords((Date.now() - received) / 1000), tone: "plain" as const };
    setText(status, words.text);
    setAttr(status, "data-tone", words.tone === "warn" ? "warn" : null);
    keep?.();
  };

  /** A redraw removes the buttons of the rows; the one that has the keyboard gets it back on the same row. */
  const focusedAction = (): (() => void) | null => {
    const active = document.activeElement;
    if (!(active instanceof HTMLElement) || !body.contains(active) || active.dataset.act === undefined) return null;
    const row = active.closest("tr");
    const key = model?.kind === "table" ? model.rows[Number(row?.dataset.i)]?.key : undefined;
    const at = active.dataset.act;
    if (key === undefined) return null;
    return () => {
      if (model?.kind !== "table") return;
      const i = model.rows.findIndex((r) => r.key === key);
      body.querySelector<HTMLElement>(`tr[data-i="${i}"] [data-act="${at}"]`)?.focus({ preventScroll: true });
    };
  };

  let busy = false;
  let again = false;
  const run = async (): Promise<void> => {
    if (!alive) return;
    if (busy) {
      again = true;
      return;
    }
    busy = true;
    try {
      const raw = await call<unknown>("integration_run", { tool: tool(), verb: current.path, pane: current.pane, args: {} });
      if (!alive) return;
      const answer = readRunAnswer(raw);
      refusal = null;
      if (answer === null) {
        fault = readFault(null);
        phase = "failed";
      } else {
        model = answer.view;
        selection = answer.selection.selection;
        production = answer.selection.production;
        fault = null;
        phase = "ready";
        received = Date.now();
        describe();
      }
    } catch (e) {
      if (!alive) return;
      refusal = null;
      fault = readFault(e);
      phase = "failed";
    } finally {
      busy = false;
    }
    draw();
    if (again && alive) {
      again = false;
      void run();
    }
  };

  /** The person typed a path: the core checks it, the page only sends it. */
  let working = false;
  const setPath = async (): Promise<void> => {
    const path = pathText.trim();
    if (path === "" || working) return;
    working = true;
    try {
      setTools(await call<unknown>("integration_binary_set", { tool: tool(), path }));
      pathText = "";
      await run();
    } catch (e) {
      refusal = readFault(e);
      draw();
    } finally {
      working = false;
    }
  };

  const checkAgain = async (): Promise<void> => {
    if (working) return;
    working = true;
    try {
      await loadTools(true);
      await run();
    } finally {
      working = false;
    }
  };

  const refresh = el("button", "ig-refresh", bar);
  refresh.type = "button";
  refresh.dataset.field = "";
  refresh.textContent = "↻ Refresh";
  refresh.setAttribute("aria-label", "Refresh");
  refresh.addEventListener("click", () => void run());

  // The age advances once a second.
  const timer = setInterval(() => {
    if (alive && phase === "ready" && fault === null) setText(status, ageWords((Date.now() - received) / 1000));
  }, 1000);

  toolListeners.add(describe);
  void loadTools();
  draw();
  void run();

  return {
    pane: info.pane,
    root,
    get info() {
      return current;
    },
    update(next) {
      current = next;
    },
    key() {
      return false;
    },
    dispose() {
      alive = false;
      clearInterval(timer);
      toolListeners.delete(describe);
      clearIntegrationContext(info.pane);
      root.replaceChildren();
    },
  };
}
