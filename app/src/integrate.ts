// The integration block (docs/design.md section 6.25): one pane that draws what the core reads from a tool (Kubernetes,
// Talos, Azure, GAM) as a table, a list of values or a text. The core runs the tool and cleans what it printed; this
// module asks for a view, draws the model it gets with `textContent` (integrateview.ts) and says what it is doing in
// words. It builds no name, no argument list and no program: it sends the facts the core listed.
import { call, listen } from "./bridge.js";
import { el, loadStyles, setAttr, setText } from "./dom.js";
import type { BlockInfo } from "./blockstate.js";
import type { Instance } from "./blocks.js";
import { focusInput } from "./keyboard.js";
import {
  PICK_VERBS, ageWords, clearIntegrationContext, contextText, nextSelection, pickerEntries, readFault, readLaunchAnswer,
  readRunAnswer, readTools, refreshDue, setIntegrationContext, stateWords,
} from "./integratestate.js";
import type { Fault, RowAction, Sel, SelectKind, ToolEntry, ToolId, ViewModel } from "./integratestate.js";
import { renderKv, renderTable, renderText } from "./integrateview.js";
import type { DrawOptions } from "./integrateview.js";
import type { View } from "./types.js";

/** What the block needs from the block layer, handed over so that this module does not import it. */
export interface Deps {
  /** Show a layout (the answer of a launch). */
  applyView(v: View): void;
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

// ---- the event of the core ---------------------------------------------------------------------------------------

/** What an event says to a block: it carries no data, the block asks. `gap` is true when an event was missed. */
interface Hint {
  kind: string;
  tool: string | null;
  pane: number | null;
  gap: boolean;
}
const hints = new Set<(h: Hint) => void>();
let listening = false;
let lastSeq = 0;

/** Listen for the event `integration` once for the window. It holds {seq, kind, tool, pane, code} and nothing else. */
function listenOnce(): void {
  if (listening) return;
  listening = true;
  listen<unknown>("integration", (payload) => {
    if (typeof payload !== "object" || payload === null) return;
    const o = payload as Record<string, unknown>;
    if (typeof o.kind !== "string" || typeof o.seq !== "number") return;
    const gap = lastSeq > 0 && o.seq !== lastSeq + 1;
    lastSeq = o.seq;
    if (o.kind === "tools" || gap) void loadTools(true);
    const hint: Hint = {
      kind: o.kind,
      tool: typeof o.tool === "string" ? o.tool : null,
      pane: typeof o.pane === "number" ? o.pane : null,
      gap,
    };
    for (const h of [...hints]) h(hint);
  }).catch(() => {
    listening = false;
  });
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
  /** The key of the selected row, so that a read that moves the rows keeps the same one selected. */
  let selectedKey: string | null = null;
  /** Counts the selections of this pane: a read that began before one is not drawn. */
  let gen = 0;
  /** When the last read started, in the page's clock. */
  let lastStart = 0;
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

  /** A button on a row is drawn for an action that only reads and opens a terminal, and that the core listed as such. */
  const drawn = (action: RowAction): boolean =>
    action.effect === "read" &&
    (toolOf(current.source)?.verbs.some((v) => v.id === action.verb && v.effect === "read" && v.output === "terminal") ?? false);

  const options = (): DrawOptions => ({
    selected,
    drawn,
    press: (action) => void launch(action),
    pick: (i) => {
      select(i);
    },
  });

  const select = (i: number): void => {
    selected = i;
    selectedKey = model?.kind === "table" ? (model.rows[i]?.key ?? null) : null;
    mark();
  };

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
    drawBar();
    if (unusable()) {
      drawMissing();
    } else if (model === null) {
      body.replaceChildren();
    } else if (model.kind === "table") {
      const at = selectedKey === null ? -1 : model.rows.findIndex((r) => r.key === selectedKey);
      selected = at >= 0 ? at : Math.max(0, Math.min(selected, model.rows.length - 1));
      selectedKey = model.rows[selected]?.key ?? null;
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
    lastStart = Date.now();
    const mine = gen;
    try {
      const raw = await call<unknown>("integration_run", { tool: tool(), verb: current.path, pane: current.pane, args: {} });
      if (!alive) return;
      if (mine !== gen) {
        // The selection changed while this was on its way: its rows are the old selection's, so ask again.
        again = true;
        return;
      }
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
      if (mine !== gen) {
        again = true;
        return;
      }
      refusal = null;
      fault = readFault(e);
      phase = "failed";
    } finally {
      busy = false;
      if (again && alive) {
        again = false;
        void run();
      }
    }
    draw();
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

  // ---- the bar: the chips of the selection, the production word and the button that reads again ---------------------

  const prod = el("span", "ig-prod", bar);
  prod.textContent = "production";
  prod.hidden = true;
  const refresh = el("button", "ig-refresh", bar);
  refresh.type = "button";
  refresh.dataset.field = "";
  refresh.textContent = "↻ Refresh";
  refresh.setAttribute("aria-label", "Refresh");
  refresh.addEventListener("click", () => void run());
  const chips = new Map<SelectKind, HTMLButtonElement>();

  const drawBar = (): void => {
    const entry = toolOf(current.source);
    const kinds = entry?.selects ?? [];
    for (const [kind, chip] of chips) {
      if (!kinds.includes(kind)) {
        chip.remove();
        chips.delete(kind);
      }
    }
    for (const kind of kinds) {
      let chip = chips.get(kind);
      if (!chip) {
        chip = el("button", "ig-chip");
        chip.type = "button";
        chip.dataset.field = "";
        chip.dataset.select = kind;
        chip.setAttribute("aria-haspopup", "listbox");
        chip.setAttribute("aria-expanded", "false");
        chip.addEventListener("click", () => void openPicker(kind));
        bar.insertBefore(chip, prod);
        chips.set(kind, chip);
      }
      const value = selection[kind];
      setText(chip, `${kind} ${value ?? "—"}`);
      chip.setAttribute("aria-label", `${kind}: ${value ?? "none"}`);
    }
    prod.hidden = !production;
  };

  // ---- the list under a chip ---------------------------------------------------------------------------------------

  let picker: { kind: SelectKind; node: HTMLElement } | null = null;
  let pickToken = 0;

  const closePicker = (focus: "chip" | "terminal" | "none"): void => {
    if (picker === null) return;
    const { kind, node } = picker;
    picker = null;
    pickToken += 1;
    node.remove();
    const chip = chips.get(kind);
    chip?.setAttribute("aria-expanded", "false");
    if (focus === "chip") chip?.focus({ preventScroll: true });
    else if (focus === "terminal") focusInput();
  };

  const listMessage = (node: HTMLElement, text: string, warn: boolean): void => {
    node.replaceChildren();
    const p = el("p", "ig-list-msg", node);
    p.setAttribute("role", "status");
    if (warn) p.dataset.tone = "warn";
    p.textContent = text;
  };

  const openPicker = async (kind: SelectKind): Promise<void> => {
    if (picker?.kind === kind) {
      closePicker("chip");
      return;
    }
    closePicker("none");
    const node = el("div", "ig-list");
    node.setAttribute("role", "listbox");
    node.setAttribute("aria-label", `${kind} list`);
    node.addEventListener("keydown", (e) => pickerKey(e, node));
    root.insertBefore(node, body);
    picker = { kind, node };
    const mine = ++pickToken;
    chips.get(kind)?.setAttribute("aria-expanded", "true");
    listMessage(node, stateWords("reading").text, false);
    try {
      // The entries are a list the core reads for this pane; the page shows no value that the core did not send.
      const answer = readRunAnswer(
        await call<unknown>("integration_run", { tool: tool(), verb: PICK_VERBS[kind], pane: current.pane, args: {} }),
      );
      if (mine !== pickToken || !alive) return;
      const entries = pickerEntries(answer?.view ?? null);
      if (answer === null) {
        listMessage(node, readFault(null).text, true);
      } else if (entries.length === 0) {
        listMessage(node, stateWords("empty").text, false);
      } else {
        node.replaceChildren();
        const now = selection[kind];
        for (const entry of entries) {
          const opt = el("button", "ig-opt", node);
          opt.type = "button";
          opt.dataset.field = "";
          opt.setAttribute("role", "option");
          opt.setAttribute("aria-selected", String(entry.key === now));
          el("span", "ig-mark", opt).textContent = entry.key === now ? "●" : "○";
          el("span", "ig-opt-label", opt).textContent = entry.label;
          if (entry.detail !== "") el("span", "ig-opt-detail", opt).textContent = entry.detail;
          opt.addEventListener("click", () => void choose(kind, entry.key));
        }
        const options = Array.from(node.querySelectorAll<HTMLElement>(".ig-opt"));
        (options.find((o) => o.getAttribute("aria-selected") === "true") ?? options[0])?.focus({ preventScroll: true });
      }
    } catch (e) {
      if (mine !== pickToken || !alive) return;
      listMessage(node, readFault(e).text, true);
    }
  };

  const pickerKey = (e: KeyboardEvent, node: HTMLElement): void => {
    const opts = Array.from(node.querySelectorAll<HTMLElement>(".ig-opt"));
    const at = opts.findIndex((o) => o === document.activeElement);
    const go = (i: number): void => {
      e.preventDefault();
      opts[(i + opts.length) % opts.length]?.focus({ preventScroll: true });
    };
    switch (e.key) {
      case "ArrowDown":
        go(at + 1);
        break;
      case "ArrowUp":
        go(at < 0 ? 0 : at - 1);
        break;
      case "Home":
        go(0);
        break;
      case "End":
        go(opts.length - 1);
        break;
      case "Escape":
        e.preventDefault();
        e.stopPropagation();
        closePicker("terminal");
        break;
      default:
    }
  };

  /** The person chose an entry: the core takes it for this pane only, then the view is read again. */
  const choose = async (kind: SelectKind, key: string): Promise<void> => {
    closePicker("chip");
    try {
      await call("integration_select", { tool: tool(), pane: current.pane, selection: nextSelection(tool(), selection, kind, key) });
    } catch (e) {
      refusal = readFault(e);
      draw();
      return;
    }
    refusal = null;
    gen += 1;
    void run();
  };

  // ---- the actions of a row ----------------------------------------------------------------------------------------

  /** A read verb that opens a terminal (the logs): the core launches it beside this pane with the row's own arguments. */
  const launch = async (action: RowAction): Promise<void> => {
    try {
      const answer = readLaunchAnswer(
        await call<unknown>("integration_launch", {
          tool: tool(), verb: action.verb, pane: current.pane, args: action.args, placement: "right",
        }),
      );
      if (!alive) return;
      if (answer === null) {
        refusal = readFault(null);
        draw();
        return;
      }
      refusal = null;
      deps.applyView(answer.view);
    } catch (e) {
      if (!alive) return;
      refusal = readFault(e);
      draw();
    }
  };

  // ---- time: the age of the data, and the verbs that read again by themselves ---------------------------------------

  const interval = (): number | null => toolOf(current.source)?.verbs.find((v) => v.id === current.path)?.refresh_secs ?? null;

  const timer = setInterval(() => {
    if (!alive) return;
    if (phase === "ready" && fault === null && refusal === null) setText(status, ageWords((Date.now() - received) / 1000));
    // Only a block that is on screen, in a visible window, with no request of its own on the way, reads by itself.
    const due = refreshDue({
      secs: interval(), last: lastStart, now: Date.now(), hidden: document.hidden, shown: root.offsetParent !== null, busy, alive,
    });
    if (due && !unusable()) void run();
  }, 1000);

  const hint = (h: Hint): void => {
    if (!alive) return;
    if (h.kind !== "context" && h.kind !== "held" && !h.gap) return;
    if (h.gap || h.pane === current.pane || (h.pane === null && (h.tool === null || h.tool === current.source))) void run();
  };
  hints.add(hint);
  listenOnce();

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
      const moved = next.path !== current.path || next.source !== current.source;
      current = next;
      if (!moved) return;
      // Another view of the tool (the core moved the pane): start again, with the words of reading.
      closePicker("none");
      model = null;
      fault = null;
      refusal = null;
      phase = "reading";
      selected = 0;
      selectedKey = null;
      gen += 1;
      describe();
      void run();
    },
    key(e) {
      if (picker !== null) return false;
      const rows = model?.kind === "table" ? model.rows.length : 0;
      switch (e.key) {
        case "ArrowDown":
          if (rows > 0) select(Math.min(rows - 1, selected + 1));
          else body.scrollBy({ top: 48 });
          return true;
        case "ArrowUp":
          if (rows > 0) select(Math.max(0, selected - 1));
          else body.scrollBy({ top: -48 });
          return true;
        case "Home":
          if (rows > 0) select(0);
          else body.scrollTo({ top: 0 });
          return true;
        case "End":
          if (rows > 0) select(rows - 1);
          else body.scrollTo({ top: body.scrollHeight });
          return true;
        case "r":
          void run();
          return true;
        default:
          return false;
      }
    },
    dispose() {
      alive = false;
      clearInterval(timer);
      hints.delete(hint);
      closePicker("none");
      toolListeners.delete(describe);
      clearIntegrationContext(info.pane);
      root.replaceChildren();
    },
  };
}
