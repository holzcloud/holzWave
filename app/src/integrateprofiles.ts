// The integration profiles dialog (docs/design.md section 6.27): where a person manages what holzWave remembers about a
// connection of an integration tool. It is a sibling of the AI providers dialog (6.13), with the same box, the same key handling
// and the same write-only rule for secrets. Every string goes in with `textContent` or `value`, never as markup. The dialog
// validates nothing but how long a field is: the core checks a profile and says why it refuses one, and the dialog shows that
// sentence. It builds no name, argument list, path or program, and it runs nothing.
import { call } from "./bridge.js";
import { el, loadStyles, setText } from "./dom.js";
import { readProfiles, profileSelectionText } from "./integratestate.js";
import type { ProfileEntry } from "./integratestate.js";
import { loadTools, onTools, toolOf } from "./integratetools.js";
import { pushKeyHandler } from "./keystack.js";

let opened: Promise<void> | null = null;

/**
 * Open the dialog. A second ask while it is open returns the same promise. Resolves when it is closed; whoever opened it gives
 * the keyboard back to where it came from.
 */
export function openIntegrationProfiles(): Promise<void> {
  opened ??= show().finally(() => {
    opened = null;
  });
  return opened;
}

async function show(): Promise<void> {
  try {
    await loadStyles("./dialogs.css");
    await loadStyles("./aisettings.css");
    await loadStyles("./integrate.css");
  } catch {
    // the dialog still works without its style sheets
  }

  return new Promise<void>((resolve) => {
    const backdrop = el("div", "dlg-backdrop", document.body);
    const box = el("div", "dlg ai-settings ip-box", backdrop);
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-modal", "true");
    box.setAttribute("aria-labelledby", "ip-title");
    box.tabIndex = -1;

    const title = el("h2", "set-title", box);
    title.id = "ip-title";
    title.textContent = "Integration profiles";
    const statusLine = el("p", "aiset-status ip-status", box);
    statusLine.setAttribute("role", "status");
    const noteList = el("ul", "aiset-notes ip-notes", box);
    noteList.hidden = true;

    const status = (text: string, kind: "" | "error" | "warn" = ""): void => {
      setText(statusLine, text);
      if (kind === "") statusLine.removeAttribute("data-kind");
      else statusLine.dataset.kind = kind;
    };

    const group = (cls: string, legend: string): HTMLFieldSetElement => {
      const set = el("fieldset", `aiset-group ${cls}`, box);
      el("legend", "label", set).textContent = legend;
      return set;
    };

    // ---- the list ------------------------------------------------------------------------------------------------

    const listGroup = group("ip-list", "Profiles");
    const rowsBox = el("div", "aiset-rows ip-rows", listGroup);
    rowsBox.setAttribute("role", "radiogroup");
    rowsBox.setAttribute("aria-label", "Profiles");
    const empty = el("p", "aiset-empty", listGroup);
    empty.textContent = "No profile yet.";
    const addButton = el("button", "aiset-add ip-add", listGroup);
    addButton.type = "button";
    addButton.textContent = "＋ New";

    const actions = el("div", "dlg-actions", box);
    const closeButton = el("button", "aiset-close dlg-ok", actions);
    closeButton.type = "button";
    closeButton.textContent = "Close";

    // ---- state ---------------------------------------------------------------------------------------------------

    let list: ProfileEntry[] = [];
    /** The id of the profile the form shows; null for a new one. */
    let selected: string | null = null;
    let done = false;
    let readNo = 0;

    const drawNotes = (notes: string[]): void => {
      noteList.replaceChildren();
      for (const note of notes) el("li", "", noteList).textContent = note;
      noteList.hidden = notes.length === 0;
    };

    const drawList = (): void => {
      rowsBox.replaceChildren();
      for (const p of list) {
        const item = el("label", "aiset-row ip-row", rowsBox);
        item.dataset.id = p.id;
        const radio = el("input", "", item);
        radio.type = "radio";
        radio.name = "ip-profile";
        radio.value = p.id;
        radio.checked = p.id === selected;
        item.dataset.chosen = p.id === selected ? "true" : "false";
        el("span", "aiset-name ip-name", item).textContent = p.name;
        el("span", "aiset-meta ip-tool", item).textContent = toolOf(p.tool)?.label ?? p.tool;
        el("span", "aiset-host ip-sel", item).textContent = profileSelectionText(p);
        if (p.production) el("span", "ip-prod", item).textContent = "production";
        radio.addEventListener("change", () => {
          if (radio.checked) select(p.id);
        });
      }
      empty.hidden = list.length > 0;
    };

    function select(id: string | null): void {
      selected = id !== null && list.some((p) => p.id === id) ? id : null;
      for (const item of rowsBox.querySelectorAll<HTMLElement>(".ip-row")) {
        const chosen = item.dataset.id === selected;
        item.dataset.chosen = chosen ? "true" : "false";
        const radio = item.querySelector<HTMLInputElement>('input[type="radio"]');
        if (radio) radio.checked = chosen;
      }
    }

    /** Read the list. A read that a later one replaced shows nothing. `prefer` is the profile to show afterwards. */
    const read = async (prefer?: string | null): Promise<boolean> => {
      const mine = ++readNo;
      rowsBox.setAttribute("aria-busy", "true");
      let raw: unknown = null;
      let ok = true;
      try {
        raw = await call<unknown>("integration_profiles");
      } catch {
        ok = false;
      }
      if (done || mine !== readNo) return false;
      rowsBox.removeAttribute("aria-busy");
      if (!ok) {
        list = [];
        drawNotes([]);
        status("The profiles could not be read.", "error");
      } else {
        const parsed = readProfiles(raw);
        list = parsed.profiles;
        drawNotes(parsed.notes);
      }
      const want = prefer === undefined ? selected : prefer;
      selected = list.some((p) => p.id === want) ? want : null;
      drawList();
      return ok;
    };

    addButton.addEventListener("click", () => {
      select(null);
      addButton.focus();
    });

    // ---- closing and keys ----------------------------------------------------------------------------------------

    let pop = (): void => {};
    let stopTools = (): void => {};
    const finish = (): void => {
      if (done) return;
      done = true;
      pop();
      stopTools();
      backdrop.remove();
      resolve();
    };
    closeButton.addEventListener("click", finish);
    backdrop.addEventListener("pointerdown", (e) => {
      if (e.target === backdrop) finish();
    });

    /** The controls Tab visits: every field and button, one radio per group (the chosen one). */
    const tabStops = (): HTMLElement[] => {
      const groups = new Set<string>();
      return Array.from(box.querySelectorAll<HTMLElement>("input, button")).filter((n) => {
        if ((n as HTMLInputElement).disabled || n.closest("[hidden]") !== null) return false;
        if (!(n instanceof HTMLInputElement) || n.type !== "radio") return true;
        if (groups.has(n.name)) return false;
        const chosen = Array.from(box.querySelectorAll<HTMLInputElement>('input[type="radio"]')).find(
          (r) => r.name === n.name && r.checked && !r.disabled,
        );
        if (chosen && chosen !== n) return false;
        groups.add(n.name);
        return true;
      });
    };

    /** Tab and Shift+Tab stay inside the dialog: from its last control to its first and back. */
    const trap = (e: KeyboardEvent): void => {
      const items = tabStops();
      if (items.length === 0) return;
      const active = document.activeElement as HTMLElement | null;
      let at = active ? items.indexOf(active) : -1;
      // A radio that is not the chosen one (reached with the arrows) counts as its group's stop.
      if (at < 0 && active instanceof HTMLInputElement && active.type === "radio") {
        at = items.findIndex((n) => n instanceof HTMLInputElement && n.type === "radio" && n.name === active.name);
      }
      const next = e.shiftKey ? (at <= 0 ? items.length - 1 : at - 1) : at < 0 || at === items.length - 1 ? 0 : at + 1;
      items[next]?.focus();
    };

    pop = pushKeyHandler((e) => {
      if (e.key === "Escape") {
        e.preventDefault();
        finish();
      } else if (e.key === "Tab") {
        e.preventDefault();
        trap(e);
      }
      // Everything else is the focused control's: typing, Space and the arrows are left alone, and nothing reaches the
      // terminal or the shortcuts behind the dialog.
      return true;
    });

    stopTools = onTools(() => drawList());
    box.focus({ preventScroll: true });
    void Promise.all([loadTools(), read(null)]).then(() => {
      if (done) return;
      drawList();
      if (selected === null && list.length > 0) select(list[0]?.id ?? null);
      const chosen = rowsBox.querySelector<HTMLInputElement>('input[type="radio"]:checked');
      (chosen ?? addButton).focus({ preventScroll: true });
    });
  });
}
