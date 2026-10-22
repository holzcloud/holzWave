// The integration profiles dialog (docs/design.md section 6.27): where a person manages what holzWave remembers about a
// connection of an integration tool. It is a sibling of the AI providers dialog (6.13), with the same box, the same key handling
// and the same write-only rule for secrets. Every string goes in with `textContent` or `value`, never as markup. The dialog
// validates nothing but how long a field is: the core checks a profile and says why it refuses one, and the dialog shows that
// sentence. It builds no name, argument list, path or program, and it runs nothing. The page names no tool: the tools, their
// names and the fields they select come from the core's list.
import { call } from "./bridge.js";
import { el, loadStyles, setText } from "./dom.js";
import {
  FIELD_WORDS, PROFILE_CAPS, duplicateName, fieldsFor, profileSelectionText, profileToSave, readFault, readProfiles, slug, takesOwnFolder, uniqueId,
} from "./integratestate.js";
import type { FormValues, ProfileEntry, SelectKind } from "./integratestate.js";
import { listedTools, loadTools, onTools, toolOf } from "./integratetools.js";
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

const CLOSE_TEXT = "Close";
const CLOSE_ARMED = "Discard changes?";

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

    // ---- the form ------------------------------------------------------------------------------------------------

    const form = group("ip-form", "New profile");
    const legend = form.querySelector("legend") as HTMLElement;

    const field = (id: string, text: string, parent: Element = form): HTMLDivElement => {
      const wrap = el("div", "aiset-field ip-field", parent);
      const label = el("label", "", wrap);
      label.htmlFor = id;
      label.textContent = text;
      return wrap;
    };
    const textInput = (wrap: Element, id: string, max: number): HTMLInputElement => {
      const input = el("input", "", wrap);
      input.id = id;
      input.type = "text";
      input.maxLength = max;
      input.autocomplete = "off";
      input.spellcheck = false;
      return input;
    };

    const nameInput = textInput(field("ip-name", "Name"), "ip-name", PROFILE_CAPS.nameField);

    const toolSet = el("fieldset", "aiset-choice ip-tools", form);
    el("legend", "", toolSet).textContent = "Tool";
    const toolOptions = el("div", "aiset-options", toolSet);
    const toolNote = el("p", "aiset-hint", toolSet);
    toolNote.textContent = "The tools could not be read.";
    toolNote.hidden = true;

    const fieldsBox = el("div", "ip-fields", form);

    const checkRow = (id: string, text: string, hint: string, parent: Element): HTMLInputElement => {
      const wrap = el("div", "aiset-field aiset-check ip-check", parent);
      const input = el("input", "", wrap);
      input.id = id;
      input.type = "checkbox";
      const label = el("label", "", wrap);
      label.htmlFor = id;
      label.textContent = text;
      const note = el("p", "aiset-hint", wrap);
      note.id = `${id}-hint`;
      note.textContent = hint;
      input.setAttribute("aria-describedby", note.id);
      return input;
    };
    const productionInput = checkRow("ip-production", "Production context", "Actions on this context ask twice.", form);
    const ownSlot = el("div", "ip-own", form);

    const errorLine = el("p", "aiset-error ip-error", form);
    errorLine.id = "ip-error";
    errorLine.setAttribute("role", "alert");
    errorLine.hidden = true;

    const formActions = el("div", "aiset-form-actions", box);
    const saveButton = el("button", "aiset-save ip-save", formActions);
    saveButton.type = "button";
    saveButton.textContent = "Save";

    const actions = el("div", "dlg-actions", box);
    const closeButton = el("button", "aiset-close dlg-ok", actions);
    closeButton.type = "button";
    closeButton.textContent = CLOSE_TEXT;

    // ---- state ---------------------------------------------------------------------------------------------------

    let list: ProfileEntry[] = [];
    /** The id of the profile the form shows; null for a new one. */
    let selected: string | null = null;
    /** The tool the form is for: the id the core uses for it. */
    let currentTool = "";
    /** The text of the selection fields the current tool selects; fields the tool does not select are not kept. */
    let pinned: Partial<Record<SelectKind, string>> = {};
    let isolated = false;
    let envRows: { name: string; value: string }[] = [];
    let secretNames: string[] = [];
    /** What the form was filled with, to tell a change from a profile that was only shown. */
    let filled = "";
    let done = false;
    let busy = false;
    let shownUnsaved = false;
    let closeArmed = false;
    let readNo = 0;

    let ownInput: HTMLInputElement | null = null;
    const fieldInputs = new Map<SelectKind, HTMLInputElement>();

    // ---- reading the form ----------------------------------------------------------------------------------------

    /** The text of the selection fields now in the form, merged into what was kept for the fields of other tools. */
    const readPinned = (): void => {
      for (const [kind, input] of fieldInputs) pinned[kind] = input.value;
    };

    const readForm = (): FormValues => {
      readPinned();
      return {
        id: selected ?? "",
        tool: currentTool,
        name: nameInput.value,
        fields: { ...pinned },
        production: productionInput.checked,
        isolated: ownInput?.checked ?? isolated,
        env: envRows.map((r) => ({ ...r })),
        secrets: [...secretNames],
      };
    };

    const saveBody = (values: FormValues) => {
      const entry = toolOf(values.tool);
      return profileToSave(values, fieldsFor(entry), takesOwnFolder(entry));
    };

    const snapshot = (): string => JSON.stringify(saveBody(readForm()));
    const dirty = (): boolean => snapshot() !== filled;

    // ---- drawing the form ----------------------------------------------------------------------------------------

    /** The radio group of the tools: the tools the core listed, and the tool of the profile shown when the core did not list it. */
    const drawTools = (): void => {
      const tools = listedTools().map((t) => ({ id: t.id as string, label: t.label }));
      if (currentTool !== "" && !tools.some((t) => t.id === currentTool)) tools.push({ id: currentTool, label: currentTool });
      toolOptions.replaceChildren();
      for (const t of tools) {
        const item = el("label", "aiset-option", toolOptions);
        const radio = el("input", "", item);
        radio.type = "radio";
        radio.name = "ip-tool";
        radio.value = t.id;
        radio.checked = t.id === currentTool;
        el("span", "", item).textContent = t.label;
      }
      toolNote.hidden = tools.length > 0;
    };

    /** The fields the current tool selects, and the folder switch when it takes one. */
    const drawFields = (): void => {
      const entry = toolOf(currentTool);
      const kinds = fieldsFor(entry);
      // Only the fields of this tool are kept: what was typed for another tool does not travel with a tool change.
      pinned = Object.fromEntries(kinds.filter((k) => pinned[k] !== undefined).map((k) => [k, pinned[k]])) as Partial<Record<SelectKind, string>>;
      fieldsBox.replaceChildren();
      fieldInputs.clear();
      for (const kind of kinds) {
        const words = FIELD_WORDS[kind];
        const wrap = field(`ip-field-${kind}`, words.label, fieldsBox);
        const input = textInput(wrap, `ip-field-${kind}`, PROFILE_CAPS.selection);
        input.placeholder = words.example;
        input.value = pinned[kind] ?? "";
        fieldInputs.set(kind, input);
      }
      const own = takesOwnFolder(entry);
      if (!own) isolated = false;
      ownSlot.replaceChildren();
      ownInput = null;
      if (own) {
        ownInput = checkRow("ip-isolated", "Own configuration folder", "Sign in again inside it. holzWave keeps it on this computer.", ownSlot);
        ownInput.checked = isolated;
      }
    };

    const clearError = (): void => {
      errorLine.hidden = true;
      errorLine.textContent = "";
    };

    /** Put a profile (or a new one, for null) into the form. */
    const fill = (profile: ProfileEntry | null): void => {
      selected = profile?.id ?? null;
      setText(legend, profile === null ? "New profile" : "Profile");
      nameInput.value = profile?.name ?? "";
      currentTool = profile?.tool ?? listedTools()[0]?.id ?? "";
      pinned = {};
      if (profile !== null) {
        for (const kind of ["context", "namespace", "subscription", "section", "node"] as const) {
          const value = profile[kind];
          if (value !== null) pinned[kind] = value;
        }
      }
      isolated = profile?.isolated ?? false;
      productionInput.checked = profile?.production ?? false;
      envRows = (profile?.env ?? []).map((r) => ({ ...r }));
      secretNames = (profile?.secrets ?? []).map((s) => s.name);
      drawTools();
      drawFields();
      clearError();
      filled = snapshot();
      shownUnsaved = false;
    };

    // ---- the list: drawing and choosing --------------------------------------------------------------------------

    const markChosen = (): void => {
      for (const item of rowsBox.querySelectorAll<HTMLElement>(".ip-row")) {
        const chosen = item.dataset.id === selected;
        item.dataset.chosen = chosen ? "true" : "false";
        const radio = item.querySelector<HTMLInputElement>('input[type="radio"]');
        if (radio) radio.checked = chosen;
      }
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
          if (radio.checked) choose(p.id);
        });
      }
      empty.hidden = list.length > 0;
    };

    const drawNotes = (notes: string[]): void => {
      noteList.replaceChildren();
      for (const note of notes) el("li", "", noteList).textContent = note;
      noteList.hidden = notes.length === 0;
    };

    /** A person picked another row or `＋ New`: an edit that was not saved is lost, and the line says so. */
    function choose(id: string | null): void {
      status(dirty() ? "Changes were not saved." : "");
      disarmClose(false);
      const profile = id === null ? null : (list.find((p) => p.id === id) ?? null);
      fill(profile);
      markChosen();
    }

    /** Take a list the core answered (a read or a save) into the page. */
    const take = (raw: unknown): void => {
      const parsed = readProfiles(raw);
      list = parsed.profiles;
      drawNotes(parsed.notes);
    };

    /** Read the list. A read that a later one replaced shows nothing. */
    const read = async (): Promise<boolean> => {
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
      if (ok) take(raw);
      else {
        list = [];
        drawNotes([]);
        status("The profiles could not be read.", "error");
      }
      drawList();
      return ok;
    };

    // ---- unsaved changes and the question before closing ---------------------------------------------------------

    /** The status line follows the form: `unsaved` while it differs from what was saved. */
    const refreshDirty = (): void => {
      if (busy || done || closeArmed) return;
      if (dirty()) {
        status("unsaved", "warn");
        shownUnsaved = true;
      } else if (shownUnsaved) {
        status("");
        shownUnsaved = false;
      }
    };

    function disarmClose(redraw = true): void {
      if (!closeArmed) return;
      closeArmed = false;
      closeButton.removeAttribute("data-armed");
      setText(closeButton, CLOSE_TEXT);
      if (redraw) {
        shownUnsaved = false;
        refreshDirty();
        if (!shownUnsaved) status("");
      }
    }

    // ---- save ----------------------------------------------------------------------------------------------------

    const setBusy = (value: boolean): void => {
      busy = value;
      saveButton.disabled = value;
      addButton.disabled = value;
    };

    const showFault = (raw: unknown): void => {
      const fault = readFault(raw);
      errorLine.textContent = fault.text;
      errorLine.hidden = false;
    };

    const save = async (): Promise<void> => {
      if (busy || done) return;
      clearError();
      const values = readForm();
      // A new profile gets the id of its name, made unique among the listed ones, so that it can never replace another.
      if (selected === null) values.id = uniqueId(slug(values.name), list.map((p) => p.id));
      const twice = duplicateName(values.env);
      if (twice !== null) {
        errorLine.textContent = `Two rows use the name ${twice}.`;
        errorLine.hidden = false;
        return;
      }
      const body = saveBody(values);
      disarmClose(false);
      setBusy(true);
      status("Saving…");
      let raw: unknown;
      try {
        raw = await call<unknown>("integration_profile_save", { profile: body });
      } catch (e) {
        if (done) return;
        setBusy(false);
        status("");
        shownUnsaved = false;
        showFault(e);
        refreshDirty();
        return;
      }
      if (done) return;
      take(raw);
      drawList();
      const saved = list.find((p) => p.id === body.id) ?? null;
      fill(saved);
      markChosen();
      setBusy(false);
      status("Saved.");
    };

    // ---- events --------------------------------------------------------------------------------------------------

    toolOptions.addEventListener("change", (e) => {
      const radio = e.target;
      if (!(radio instanceof HTMLInputElement) || !radio.checked) return;
      readPinned();
      if (ownInput) isolated = ownInput.checked;
      currentTool = radio.value;
      drawFields();
      clearError();
      refreshDirty();
    });
    addButton.addEventListener("click", () => {
      choose(null);
      nameInput.focus();
    });
    saveButton.addEventListener("click", () => void save());

    // Any change of the form takes the sentence of the core away and updates the line; a value typed into a secret field is no change.
    const edited = (e: Event): void => {
      if (!(e.target instanceof HTMLElement) || e.target.dataset.secret !== undefined) return;
      if (e.target === errorLine) return;
      clearError();
      disarmClose(false);
      refreshDirty();
    };
    form.addEventListener("input", edited);
    form.addEventListener("change", edited);

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

    /** Close, or ask first when the form holds changes that were not saved; the second ask closes. */
    const requestClose = (): void => {
      if (done) return;
      if (busy || !dirty() || closeArmed) {
        finish();
        return;
      }
      closeArmed = true;
      closeButton.dataset.armed = "true";
      setText(closeButton, CLOSE_ARMED);
      status("Unsaved changes. Close again to discard them.", "warn");
    };
    closeButton.addEventListener("click", requestClose);
    backdrop.addEventListener("pointerdown", (e) => {
      if (e.target === backdrop) requestClose();
    });
    // Any other press, or a key typed in a field, takes the question back.
    box.addEventListener(
      "click",
      (e) => {
        if (e.target !== closeButton) disarmClose();
      },
      true,
    );
    box.addEventListener("keydown", (e) => {
      if (e.target instanceof HTMLInputElement && e.key !== "Tab" && e.key !== "Shift" && e.key !== "Escape") disarmClose();
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
        requestClose();
      } else if (e.key === "Tab") {
        e.preventDefault();
        trap(e);
      } else if (e.key === "Enter" && e.target instanceof HTMLInputElement && e.target.type === "text") {
        // Enter in a text field saves; a button, a checkbox and a radio keep their own Enter.
        e.preventDefault();
        void save();
      }
      // Everything else is the focused control's: typing, Space and the arrows are left alone, and nothing reaches the
      // terminal or the shortcuts behind the dialog.
      return true;
    });

    stopTools = onTools(() => {
      readPinned();
      drawTools();
      drawFields();
      drawList();
    });
    fill(null);
    box.focus({ preventScroll: true });
    void Promise.all([loadTools(), read()]).then(() => {
      if (done) return;
      drawList();
      const first = list[0] ?? null;
      fill(first);
      markChosen();
      const chosen = rowsBox.querySelector<HTMLInputElement>('input[type="radio"]:checked');
      (chosen ?? nameInput).focus({ preventScroll: true });
    });
  });
}
