// The history search (docs/design.md section 6.10): a floating dialog like the palette, with a field and the matching
// commands below it. Typing searches (the core matches fuzzily), Up and Down choose, Enter puts the command on the
// prompt of the focused pane WITHOUT running it, Shift+Delete forgets the command, Escape closes. While it is open it takes
// every key. Every command goes in with `textContent`; the matched letters are wrapped in elements the page makes.
import { registerAction } from "./actions.js";
import { call } from "./bridge.js";
import { el, loadStyles, setAttr, setText } from "./dom.js";
import { pushKeyHandler } from "./keystack.js";
import { state } from "./state.js";
import { meta, pieces, readHits } from "./historystate.js";
import type { Hit } from "./historystate.js";

export const HISTORY_ACTION = "history.open";
const LIMIT = 50;

let open: Promise<void> | null = null;

/** Open the history search. A second ask while it is open returns the same promise. */
export function openHistory(): Promise<void> {
  open ??= show().finally(() => {
    open = null;
  });
  return open;
}

async function show(): Promise<void> {
  try {
    await loadStyles("./palette.css");
    await loadStyles("./history.css");
  } catch {
    // the dialog still works without its style sheets
  }
  const { focusInput } = await import("./keyboard.js");
  return new Promise<void>((resolve) => {
    const backdrop = el("div", "palette-backdrop open history-backdrop", document.body);
    const box = el("div", "palette history", backdrop);
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-modal", "true");
    box.setAttribute("aria-label", "History");
    const input = el("input", "palette-input", box);
    input.type = "text";
    input.setAttribute("role", "combobox");
    input.setAttribute("aria-label", "Search the history");
    input.setAttribute("aria-controls", "history-list");
    input.setAttribute("aria-autocomplete", "list");
    input.setAttribute("aria-expanded", "true");
    input.autocomplete = "off";
    input.spellcheck = false;
    input.placeholder = "Search commands you typed";
    const list = el("ul", "palette-list", box);
    list.id = "history-list";
    list.setAttribute("role", "listbox");
    const empty = el("p", "palette-empty", box);
    empty.setAttribute("role", "status");
    const foot = el("div", "history-foot", box);
    for (const t of ["↑↓ choose", "⏎ to the prompt", "⇧⌫ forget"]) el("span", "", foot).textContent = t;
    const clear = el("button", "history-clear", foot);
    clear.type = "button";
    clear.textContent = "Clear all";

    let hits: Hit[] = [];
    let active = -1;
    let seq = 0;
    let armed = false;

    const setActive = (at: number): void => {
      active = at;
      Array.from(list.children).forEach((row, n) => setAttr(row as HTMLElement, "aria-selected", n === at ? "true" : "false"));
      setAttr(input, "aria-activedescendant", at >= 0 ? `history-opt-${at}` : null);
      (list.children[at] as HTMLElement | undefined)?.scrollIntoView({ block: "nearest" });
    };

    const draw = (): void => {
      list.replaceChildren();
      const now = Math.floor(Date.now() / 1000);
      hits.forEach((h, n) => {
        const row = el("li", "palette-row history-row", list);
        row.id = `history-opt-${n}`;
        row.setAttribute("role", "option");
        const cmd = el("span", "palette-title history-cmd", row);
        for (const [text, hit] of pieces(h.cmd, h.marks)) {
          if (hit) el("mark", "", cmd).textContent = text;
          else cmd.append(document.createTextNode(text));
        }
        const m = el("span", "history-meta", row);
        m.textContent = meta(h, now);
        if (h.exit !== null && h.exit !== 0) m.dataset.failed = "true";
      });
      const none = hits.length === 0;
      empty.hidden = !none;
      setText(empty, none ? (input.value.trim() === "" ? "Nothing typed yet. Add the hook of your shell: wsh init bash" : `Nothing matches "${input.value.trim()}"`) : "");
      list.hidden = none;
      setActive(none ? -1 : 0);
    };

    const search = async (): Promise<void> => {
      const mine = ++seq;
      try {
        const found = readHits(await call<unknown>("history_search", { query: input.value, limit: LIMIT }));
        if (mine !== seq) return;
        hits = found;
      } catch {
        if (mine !== seq) return;
        hits = [];
      }
      draw();
    };

    let pop: () => void = () => {};
    const finish = (): void => {
      pop();
      backdrop.remove();
      focusInput();
      resolve();
    };

    const choose = (at: number): void => {
      const hit = hits[at];
      const pane = state.view?.focus;
      finish();
      // On the prompt, never run: no newline goes with it, and the core brackets it when the program asked.
      if (hit && pane !== undefined) void call("paste", { pane, text: hit.cmd, allow_unsafe: false }).catch(() => {});
    };

    const forget = async (at: number): Promise<void> => {
      const hit = hits[at];
      if (!hit) return;
      await call("history_delete", { cmd: hit.cmd }).catch(() => {});
      await search();
    };

    input.addEventListener("input", (e) => {
      if (!(e as InputEvent).isComposing) void search();
    });
    list.addEventListener("mousedown", (e) => e.preventDefault());
    list.addEventListener("click", (e) => {
      const row = e.target instanceof Element ? e.target.closest(".history-row") : null;
      const at = row ? Array.from(list.children).indexOf(row) : -1;
      if (at >= 0) choose(at);
    });
    backdrop.addEventListener("click", (e) => {
      if (e.target === backdrop) finish();
    });
    clear.addEventListener("click", () => {
      if (!armed) {
        armed = true;
        clear.dataset.armed = "true";
        clear.textContent = "Click again to clear";
        return;
      }
      void call("history_clear").catch(() => {}).then(() => finish());
    });

    pop = pushKeyHandler((e) => {
      if (document.activeElement !== input && document.activeElement !== clear) input.focus({ preventScroll: true });
      if (e.metaKey && !["a", "c", "v", "x", "z"].includes(e.key.toLowerCase())) {
        e.preventDefault();
        return true;
      }
      switch (e.key) {
        case "Escape":
          e.preventDefault();
          finish();
          break;
        case "Enter":
          if (document.activeElement === clear) return true; // the button's own click
          e.preventDefault();
          if (active >= 0) choose(active);
          break;
        case "ArrowDown":
          e.preventDefault();
          if (hits.length > 0) setActive((active + 1) % hits.length);
          break;
        case "ArrowUp":
          e.preventDefault();
          if (hits.length > 0) setActive((active - 1 + hits.length) % hits.length);
          break;
        case "Delete":
        case "Backspace":
          if (e.shiftKey) {
            e.preventDefault();
            void forget(active);
          }
          break;
        case "Tab":
          e.preventDefault();
          (document.activeElement === input ? clear : input).focus();
          break;
        default:
      }
      return true;
    });
    input.focus({ preventScroll: true });
    void search();
  });
}

/** Register the action. Call once at start-up. */
export function initHistory(): void {
  registerAction({ id: HISTORY_ACTION, title: "Search the history", run: () => void openHistory() });
}
