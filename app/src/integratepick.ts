// The two small parts that the integration block (integrate.ts, design 6.25) and the Context section of the sidebar
// (integratecontext.ts, design 6.11) share, so that there is one implementation of each: the listbox that offers the entries
// of a list the core read, and the row with the field for the path of a program. Everything is set with `textContent`; the
// page sends the core a key it chose from a list the core sent, and a path the person typed, and nothing else.
import { call } from "./bridge.js";
import { el } from "./dom.js";
import { PICK_VERBS, pickerEntries, readFault, readRunAnswer, stateWords } from "./integratestate.js";
import type { SelectKind, ToolId } from "./integratestate.js";

export interface PickerOptions {
  tool: ToolId;
  kind: SelectKind;
  /** The pane whose selection the list is read for, or null for the pick of the tool. */
  pane: number | null;
  /** The value that is selected now; it is marked and gets the keyboard first. */
  current: string | undefined;
  /** Where the listbox goes: into `host`, before `before` (at the end without one). */
  host: HTMLElement;
  before?: Node | null;
  /** The person chose the entry with this key. The picker stays open; the caller closes it. */
  choose(key: string): void;
  /** `Escape` was pressed inside the list. The caller closes it and gives the keyboard back. */
  escape(): void;
}

export interface Picker {
  node: HTMLElement;
  /** Remove the list. An answer that is still on its way is dropped. */
  close(): void;
}

function message(node: HTMLElement, text: string, warn: boolean): void {
  node.replaceChildren();
  const p = el("p", "ig-list-msg", node);
  p.setAttribute("role", "status");
  if (warn) p.dataset.tone = "warn";
  p.textContent = text;
}

function keys(e: KeyboardEvent, node: HTMLElement, escape: () => void): void {
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
      escape();
      break;
    default:
  }
}

/**
 * Open a listbox with the entries of the list the core reads for `kind` (contexts, namespaces, nodes, subscriptions,
 * sections). The entries are a read the core makes for this pane; the page shows no value the core did not send. A list that
 * cannot be read shows the core's sentence in its place.
 */
export function openPicker(o: PickerOptions): Picker {
  const node = el("div", "ig-list");
  node.setAttribute("role", "listbox");
  node.setAttribute("aria-label", `${o.kind} list`);
  node.addEventListener("keydown", (e) => keys(e, node, o.escape));
  o.host.insertBefore(node, o.before ?? null);
  let open = true;
  message(node, stateWords("reading").text, false);
  void (async () => {
    try {
      const answer = readRunAnswer(await call<unknown>("integration_run", { tool: o.tool, verb: PICK_VERBS[o.kind], pane: o.pane, args: {} }));
      if (!open) return;
      const entries = pickerEntries(answer?.view ?? null);
      if (answer === null) {
        message(node, readFault(null).text, true);
      } else if (entries.length === 0) {
        message(node, stateWords("empty").text, false);
      } else {
        node.replaceChildren();
        for (const entry of entries) {
          const opt = el("button", "ig-opt", node);
          opt.type = "button";
          opt.dataset.field = "";
          opt.setAttribute("role", "option");
          opt.setAttribute("aria-selected", String(entry.key === o.current));
          el("span", "ig-mark", opt).textContent = entry.key === o.current ? "●" : "○";
          el("span", "ig-opt-label", opt).textContent = entry.label;
          if (entry.detail !== "") el("span", "ig-opt-detail", opt).textContent = entry.detail;
          opt.addEventListener("click", () => o.choose(entry.key));
        }
        const options = Array.from(node.querySelectorAll<HTMLElement>(".ig-opt"));
        (options.find((x) => x.getAttribute("aria-selected") === "true") ?? options[0])?.focus({ preventScroll: true });
      }
    } catch (e) {
      if (open) message(node, readFault(e).text, true);
    }
  })();
  return {
    node,
    close() {
      open = false;
      node.remove();
    },
  };
}

export interface PathRowOptions {
  /** What was typed before, so a redraw keeps it. */
  value: string;
  input(value: string): void;
  /** `Set path` or `Enter`. */
  set(): void;
  /** `Check again`. */
  again(): void;
  /** `Escape` in the field: the keyboard goes back to the terminal. */
  escape(): void;
}

/** The field for the absolute path of a program, `Set path` and `Check again`. The core checks the path; the page only sends it. */
export function pathRow(parent: HTMLElement, o: PathRowOptions): HTMLInputElement {
  const row = el("div", "ig-pathrow", parent);
  const input = el("input", "ig-path", row);
  input.type = "text";
  input.dataset.field = "";
  input.spellcheck = false;
  input.autocomplete = "off";
  input.placeholder = "Absolute path of the program";
  input.setAttribute("aria-label", "Path of the program");
  input.value = o.value;
  input.addEventListener("input", () => o.input(input.value));
  input.addEventListener("keydown", (e) => {
    if (e.isComposing) return;
    if (e.key === "Escape") {
      e.preventDefault();
      o.escape();
    } else if (e.key === "Enter") {
      e.preventDefault();
      o.set();
    }
  });
  const set = el("button", "ig-pathbtn", row);
  set.type = "button";
  set.dataset.field = "";
  set.textContent = "Set path";
  set.addEventListener("click", () => o.set());
  const again = el("button", "ig-pathbtn", row);
  again.type = "button";
  again.dataset.field = "";
  again.textContent = "Check again";
  again.addEventListener("click", () => o.again());
  return input;
}
