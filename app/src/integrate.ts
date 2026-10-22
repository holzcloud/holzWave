// The integration block (docs/design.md section 6.25): one pane that draws what the core reads from a tool (Kubernetes,
// Talos, Azure, GAM) as a table, a list of values or a text. The core runs the tool and cleans what it printed; this
// module asks for a view, draws the model it gets with `textContent` (integrateview.ts) and says what it is doing in
// words. It builds no name, no argument list and no program: it sends the facts the core listed.
import { call } from "./bridge.js";
import { el, loadStyles, setText } from "./dom.js";
import type { BlockInfo } from "./blockstate.js";
import type { Instance } from "./blocks.js";
import {
  clearIntegrationContext, contextText, readFault, readRunAnswer, readTools, setIntegrationContext,
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
  let selected = 0;

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

  const draw = (): void => {
    if (model === null) {
      body.replaceChildren();
    } else if (model.kind === "table") {
      selected = Math.max(0, Math.min(selected, model.rows.length - 1));
      renderTable(body, model, options());
    } else if (model.kind === "kv") {
      renderKv(body, model, options());
    } else {
      renderText(body, model);
    }
    setText(status, phase === "reading" ? "Reading …" : fault ? fault.text : "");
  };

  let busy = false;
  const run = async (): Promise<void> => {
    if (busy || !alive) return;
    busy = true;
    try {
      const raw = await call<unknown>("integration_run", { tool: tool(), verb: current.path, pane: current.pane, args: {} });
      if (!alive) return;
      const answer = readRunAnswer(raw);
      if (answer === null) {
        fault = readFault(null);
        phase = "failed";
      } else {
        model = answer.view;
        selection = answer.selection.selection;
        production = answer.selection.production;
        fault = null;
        phase = "ready";
        describe();
      }
    } catch (e) {
      if (!alive) return;
      fault = readFault(e);
      phase = "failed";
    } finally {
      busy = false;
    }
    draw();
  };

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
      toolListeners.delete(describe);
      clearIntegrationContext(info.pane);
      root.replaceChildren();
    },
  };
}
