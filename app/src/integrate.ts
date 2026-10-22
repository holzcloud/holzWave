// The integration block (docs/design.md section 6.25): one pane that draws what the core reads from a tool (Kubernetes,
// Talos, Azure, GAM) as a table, a list of values or a text. The core runs the tool and cleans what it printed; this
// module asks for a view, draws the model it gets with `textContent` (integrateview.ts) and says what it is doing in
// words. It builds no name, no argument list and no program: it sends the facts the core listed.
import { registerAction, unregisterAction } from "./actions.js";
import { call } from "./bridge.js";
import { el, loadStyles, setAttr, setText } from "./dom.js";
import type { BlockInfo } from "./blockstate.js";
import type { Instance } from "./blocks.js";
import { focusInput } from "./keyboard.js";
import {
  ageWords, clearIntegrationContext, contextText, entryId, entryTitle, localVerbFor, nextSelection, readFault, readLaunchAnswer,
  readRunAnswer, refreshDue, setIntegrationContext, setPaneSelection, stateWords, terminalVerbs, viewVerbs,
} from "./integratestate.js";
import type { Fault, RowAction, Sel, SelectKind, ToolEntry, ToolId, ViewModel } from "./integratestate.js";
import { requestAction } from "./integrateconfirm.js";
import { openPicker, pathRow } from "./integratepick.js";
import type { Picker } from "./integratepick.js";
import { listedTools, loadTools, onHint, onTools, setTools, toolOf } from "./integratetools.js";
import type { Hint } from "./integratetools.js";
import { renderKv, renderTable, renderText } from "./integrateview.js";
import type { DrawOptions } from "./integrateview.js";
import { state } from "./state.js";
import type { View } from "./types.js";

/** What the block needs from the block layer, handed over so that this module does not import it. */
export interface Deps {
  /** Show a layout (the answer of a launch). */
  applyView(v: View): void;
  /** The header of a pane has new words: draw the strips again. */
  changed(): void;
}

// ---- the palette entries -----------------------------------------------------------------------------------------

/** What the entries need from the page, handed over so that this module does not import the block layer or the status line. */
export interface ActionDeps {
  /** Open an integration block in a new tab; resolves to null, or to the core's reason. */
  open(source: string, path: string): Promise<string | null>;
  /** Show a layout (the answer of a launch). */
  applyView(v: View): void;
  /** Say a sentence in the status line. */
  say(text: string): void;
}

let actionDeps: ActionDeps = { open: async () => null, applyView: () => {}, say: () => {} };
const entryIds = new Set<string>();

/** Whether the page runs where the entries exist: a phone has no palette. */
function entriesWanted(): boolean {
  return state.os !== "ios" && state.os !== "android";
}

/**
 * A terminal verb that needs no argument, from the palette. A read verb opens its terminal in a new tab at once, as the Context
 * section's `Shell` does; a verb that is not read (Exec) asks first through the card, like a button of a row. The focused pane is
 * named so that the terminal starts with its context; the core decides what the pane means.
 */
async function runTerminal(tool: string, verb: string, effect: string): Promise<void> {
  const pane = state.view?.focus ?? null;
  if (effect !== "read") {
    const result = await requestAction(tool, verb, pane, {});
    if (result.kind === "refused") actionDeps.say(result.fault.text);
    return;
  }
  try {
    const answer = readLaunchAnswer(await call<unknown>("integration_launch", { tool, verb, pane, args: {}, placement: "tab" }));
    if (answer === null) {
      actionDeps.say(readFault(null).text);
      return;
    }
    actionDeps.applyView(answer.view);
    focusInput();
  } catch (e) {
    actionDeps.say(readFault(e).text);
  }
}

/**
 * Make the palette entries from the core's list of tools: one for each view (`integration.open.<tool>.<verb>`) and one for each
 * terminal verb that needs no argument (`integration.run.<tool>.<verb>`). The titles and the verbs are the core's and the page names
 * no tool. The entries made before are taken away first, so a tool that left the list loses its entries and a tool that arrived gets
 * them. A tool that is not installed keeps its entries: the block it opens offers the path.
 */
export function registerIntegrationActions(tools: readonly ToolEntry[]): void {
  for (const id of entryIds) unregisterAction(id);
  entryIds.clear();
  if (!entriesWanted()) return;
  for (const tool of tools) {
    for (const verb of viewVerbs(tool)) {
      const id = entryId("open", tool.id, verb.id);
      if (entryIds.has(id)) continue;
      registerAction({
        id,
        title: entryTitle(tool.label, verb.label),
        run: () => void actionDeps.open(tool.id, verb.id).then((reason) => reason !== null && actionDeps.say(reason)),
      });
      entryIds.add(id);
    }
    for (const verb of terminalVerbs(tool)) {
      const id = entryId("run", tool.id, verb.id);
      if (entryIds.has(id)) continue;
      registerAction({ id, title: entryTitle(tool.label, verb.label), run: () => void runTerminal(tool.id, verb.id, verb.effect) });
      entryIds.add(id);
    }
  }
}

/** Register the entries from the core's list now and whenever the list changes (the first answer, a `tools` event, a path that was set). */
export function initIntegrationActions(handed: ActionDeps): void {
  actionDeps = handed;
  if (!entriesWanted()) return;
  onTools(() => registerIntegrationActions(listedTools()));
  void loadTools();
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
    setPaneSelection(current.pane, selection);
    if (setIntegrationContext(current.pane, text, production)) deps.changed();
    if (alive) draw();
  };

  /**
   * A button on a row is drawn for an action that the core's own list of verbs backs up: the verb is listed with the same effect, and an
   * action that only reads must open a terminal. The effect the row claims is never trusted over the list.
   */
  const drawn = (action: RowAction): boolean => {
    const verb = toolOf(current.source)?.verbs.find((v) => v.id === action.verb);
    if (verb === undefined || verb.effect !== action.effect) return false;
    return action.effect === "read" ? verb.output === "terminal" : true;
  };

  const options = (): DrawOptions => ({
    selected,
    drawn,
    press: (action) => void (action.effect === "read" ? launch(action) : ask(action.verb, action.args)),
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
    pathRow(box, {
      value: pathText,
      input: (v) => {
        pathText = v;
      },
      set: () => void setPath(),
      again: () => void checkAgain(),
      escape: () => focusInput(),
    });
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
        chip.addEventListener("click", () => togglePicker(kind));
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

  let picker: { kind: SelectKind; list: Picker } | null = null;

  const closePicker = (focus: "chip" | "terminal" | "none"): void => {
    if (picker === null) return;
    const { kind, list } = picker;
    picker = null;
    list.close();
    const chip = chips.get(kind);
    chip?.setAttribute("aria-expanded", "false");
    if (focus === "chip") chip?.focus({ preventScroll: true });
    else if (focus === "terminal") focusInput();
  };

  /** A press on a chip: the list of the shared picker opens under the bar, or closes when it was this chip's. */
  const togglePicker = (kind: SelectKind): void => {
    if (picker?.kind === kind) {
      closePicker("chip");
      return;
    }
    closePicker("none");
    chips.get(kind)?.setAttribute("aria-expanded", "true");
    // The last entry: the Local verb that makes this value the tool's own setting, found from the core's list, asks first.
    const local = localVerbFor(toolOf(current.source), kind);
    const value = selection[kind];
    const list = openPicker({
      tool: tool(), kind, pane: current.pane, current: value, host: root, before: body,
      choose: (key) => void choose(kind, key),
      escape: () => closePicker("terminal"),
      makeDefault: local !== null && value !== undefined && value !== "" ? () => {
        closePicker("chip");
        void ask(local.verb, { [local.arg]: value });
      } : undefined,
    });
    picker = { kind, list };
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

  /** An action that asks first: the core prepares the card, the person decides, and a verb that ran makes the block read again. */
  const ask = async (verb: string, args: Record<string, string>): Promise<void> => {
    const result = await requestAction(tool(), verb, current.pane, args);
    if (!alive) return;
    if (result.kind === "refused") {
      refusal = result.fault;
      draw();
      return;
    }
    refusal = null;
    if (result.kind === "done") {
      gen += 1;
      void run();
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
  const unhint = onHint(hint);
  const untools = onTools(describe);
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
      unhint();
      closePicker("none");
      untools();
      clearIntegrationContext(info.pane);
      root.replaceChildren();
    },
  };
}
