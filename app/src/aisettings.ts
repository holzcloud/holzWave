// The AI providers dialog (docs/design.md section 6.13): the providers the chat talks to, added, edited and removed. It
// is a modal of its own with the box of the Settings dialog. Every string goes in with `textContent` or `value`, never as
// markup, and the dialog keeps the keys while it is open (the key handler stack): nothing reaches the terminal behind it.
//
// The dialog validates nothing and guesses nothing about where a request goes. It sends an address exactly as typed
// (trimmed), shows what the core answered (the host of a row is the core's `host`), and names no place: where a request
// goes is told by the chat after a turn. It probes nothing: no test connection, no model list, no scan for local servers.
// The list of providers is read with `ai_profiles` when the dialog opens and after a write, never anywhere else; every
// successful read is handed to `onProfilesChanged` listeners (the chat), so a saved provider shows without a reload.
import { call } from "./bridge.js";
import { el, fitNodes, loadStyles, setText } from "./dom.js";
import { pushKeyHandler } from "./keystack.js";
import {
  PRESETS, WIRE_LABELS, addressHint, coreText, draftOf, inputOf, keyWord, newDraft, presetOf, readProfileViews, sameDraft,
} from "./aistate.js";
import type { PresetTag, ProfileDraft, ProfileRow, WireTag } from "./aistate.js";

/** Called with the raw answer of `ai_profiles` after every successful read the dialog makes. */
type ProfilesListener = (raw: unknown) => void;

const listeners = new Set<ProfilesListener>();
let opened: Promise<void> | null = null;

/** Call `fn` with the raw answer of `ai_profiles` after every successful read the dialog makes; the returned function stops it. */
export function onProfilesChanged(fn: ProfilesListener): () => void {
  listeners.add(fn);
  return () => void listeners.delete(fn);
}

/**
 * Open the dialog. A second ask while it is open returns the same promise. Resolves when it is closed; whoever opened
 * it gives the keyboard back. `focus` is the id of a provider to select.
 */
export function openProviders(focus?: string): Promise<void> {
  opened ??= show(focus).finally(() => {
    opened = null;
  });
  return opened;
}

/** The bounds of the text fields: the core's own limits. */
const MAX_NAME = 80;
const MAX_MODEL = 200;
const MAX_ADDRESS = 2048;

async function show(focus: string | undefined): Promise<void> {
  try {
    await loadStyles("./dialogs.css");
    await loadStyles("./aisettings.css");
  } catch {
    // the dialog still works without its style sheets
  }

  return new Promise<void>((resolve) => {
    const backdrop = el("div", "dlg-backdrop", document.body);
    const box = el("div", "dlg ai-settings", backdrop);
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-modal", "true");
    box.setAttribute("aria-labelledby", "aiset-title");
    box.tabIndex = -1;

    const title = el("h2", "set-title", box);
    title.id = "aiset-title";
    title.textContent = "AI providers";
    const statusLine = el("p", "aiset-status", box);
    statusLine.setAttribute("role", "status");
    const noteList = el("ul", "aiset-notes", box);
    noteList.hidden = true;
    const noteNodes: HTMLLIElement[] = [];

    const status = (text: string, kind: "" | "error" = ""): void => {
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

    const listGroup = group("aiset-list", "Providers");
    const rowsBox = el("div", "aiset-rows", listGroup);
    rowsBox.setAttribute("role", "radiogroup");
    rowsBox.setAttribute("aria-label", "Providers");
    const empty = el("p", "aiset-empty", listGroup);
    empty.textContent = "No provider yet.";
    const addButton = el("button", "aiset-add", listGroup);
    addButton.type = "button";
    addButton.textContent = "Add provider";

    // ---- the form ------------------------------------------------------------------------------------------------

    const form = group("aiset-form", "New provider");
    const legend = form.querySelector("legend") as HTMLElement;

    const field = (id: string, text: string, parent: Element = form): HTMLDivElement => {
      const wrap = el("div", "aiset-field", parent);
      const label = el("label", "", wrap);
      label.htmlFor = id;
      label.textContent = text;
      return wrap;
    };

    const nameInput = el("input", "", field("aiset-name", "Name"));
    nameInput.id = "aiset-name";
    nameInput.type = "text";
    nameInput.maxLength = MAX_NAME;
    nameInput.autocomplete = "off";
    nameInput.spellcheck = false;

    const presetSet = el("fieldset", "aiset-choice aiset-presets", form);
    el("legend", "", presetSet).textContent = "Type";
    const presetOptions = el("div", "aiset-options", presetSet);
    const presetRadios: HTMLInputElement[] = PRESETS.map((p) => {
      const item = el("label", "aiset-option", presetOptions);
      const radio = el("input", "", item);
      radio.type = "radio";
      radio.name = "aiset-preset";
      radio.value = p.tag;
      el("span", "", item).textContent = p.label;
      return radio;
    });

    const wireSet = el("fieldset", "aiset-choice aiset-wires", form);
    el("legend", "", wireSet).textContent = "API shape";
    const wireOptions = el("div", "aiset-options", wireSet);
    let wireRadios: HTMLInputElement[] = [];

    const addressWrap = field("aiset-address", "Address");
    const addressInput = el("input", "", addressWrap);
    addressInput.id = "aiset-address";
    addressInput.type = "text";
    addressInput.inputMode = "url";
    addressInput.maxLength = MAX_ADDRESS;
    addressInput.autocomplete = "off";
    addressInput.spellcheck = false;
    const addressHintNode = el("p", "aiset-hint", addressWrap);
    addressHintNode.id = "aiset-address-hint";
    addressInput.setAttribute("aria-describedby", "aiset-address-hint");

    const lanWrap = el("div", "aiset-field aiset-check", form);
    const lanInput = el("input", "", lanWrap);
    lanInput.id = "aiset-lan";
    lanInput.type = "checkbox";
    const lanLabel = el("label", "", lanWrap);
    lanLabel.htmlFor = "aiset-lan";
    lanLabel.textContent = "Plain http on my local network";

    const modelWrap = field("aiset-model", "Model");
    const modelInput = el("input", "", modelWrap);
    modelInput.id = "aiset-model";
    modelInput.type = "text";
    modelInput.maxLength = MAX_MODEL;
    modelInput.autocomplete = "off";
    modelInput.spellcheck = false;
    el("p", "aiset-hint", modelWrap).textContent =
      "Type the model name the provider uses. holzWave does not ask the provider for a list.";

    const formActions = el("div", "aiset-form-actions", form);
    const saveButton = el("button", "aiset-save", formActions);
    saveButton.type = "button";
    saveButton.textContent = "Save";

    const actions = el("div", "dlg-actions", box);
    const closeButton = el("button", "aiset-close dlg-ok", actions);
    closeButton.type = "button";
    closeButton.textContent = "Close";

    // ---- state ---------------------------------------------------------------------------------------------------

    let list: ProfileRow[] = [];
    /** The id of the provider the form edits; null for a new one. */
    let selected: string | null = null;
    /** What the form was filled with, to tell a change from a row that was only shown. */
    let filled: ProfileDraft = newDraft();
    let done = false;
    let busy = false;
    let readNo = 0;
    let tail: Promise<void> = Promise.resolve();

    const setBusy = (value: boolean): void => {
      busy = value;
      saveButton.disabled = value;
      addButton.disabled = value;
    };

    // ---- the form: filling and reading ---------------------------------------------------------------------------

    /** Build the API shape radios for the type `tag` with `wire` chosen; the group is hidden for a type that speaks one. */
    const drawWires = (tag: PresetTag, wire: WireTag): void => {
      const info = presetOf(tag);
      const wires = info?.wires ?? [];
      wireOptions.replaceChildren();
      wireRadios = wires.map((w) => {
        const item = el("label", "aiset-option", wireOptions);
        const radio = el("input", "", item);
        radio.type = "radio";
        radio.name = "aiset-wire";
        radio.value = w;
        radio.checked = w === wire;
        el("span", "", item).textContent = WIRE_LABELS[w];
        return radio;
      });
      const first = wireRadios[0];
      if (first !== undefined && !wireRadios.some((r) => r.checked)) first.checked = true;
      wireSet.hidden = wires.length < 2;
    };

    const chosenPreset = (): PresetTag => (presetRadios.find((r) => r.checked)?.value ?? PRESETS[0]?.tag ?? "anthropic") as PresetTag;
    const chosenWire = (): WireTag => {
      const tag = chosenPreset();
      return (wireRadios.find((r) => r.checked)?.value ?? presetOf(tag)?.wires[0] ?? "anthropic") as WireTag;
    };

    const drawHint = (): void => {
      const info = presetOf(chosenPreset());
      setText(addressHintNode, info === null ? "" : addressHint(info, chosenWire()));
    };

    /** Put a draft into the form. `saved` is true for a provider that exists: its type cannot change. */
    const fillForm = (draft: ProfileDraft, saved: boolean): void => {
      filled = draft;
      setText(legend, saved ? "Provider" : "New provider");
      nameInput.value = draft.name;
      for (const r of presetRadios) {
        r.checked = r.value === draft.preset;
        r.disabled = saved;
      }
      drawWires(draft.preset, draft.wire);
      addressInput.value = draft.base_url;
      lanInput.checked = draft.allow_lan_http;
      modelInput.value = draft.model;
      drawHint();
    };

    const readDraft = (): ProfileDraft => ({
      id: filled.id,
      name: nameInput.value,
      preset: chosenPreset(),
      wire: chosenWire(),
      base_url: addressInput.value,
      model: modelInput.value,
      allow_lan_http: lanInput.checked,
    });

    // ---- the list: drawing and reading ---------------------------------------------------------------------------

    const drawList = (): void => {
      rowsBox.replaceChildren();
      for (const r of list) {
        const item = el("label", "aiset-row", rowsBox);
        item.dataset.id = r.id;
        const radio = el("input", "", item);
        radio.type = "radio";
        radio.name = "aiset-profile";
        radio.value = r.id;
        radio.checked = r.id === selected;
        item.dataset.chosen = r.id === selected ? "true" : "false";
        el("span", "aiset-name", item).textContent = r.name === "" ? r.id : r.name;
        const model = r.model === "" ? "no model" : r.model;
        el("span", "aiset-meta", item).textContent = `${presetOf(r.preset)?.label ?? r.preset} · ${model}`;
        el("span", "aiset-host", item).textContent = r.host;
        const word = el("span", "aiset-key", item);
        word.dataset.state = r.key;
        word.textContent = keyWord(r.key);
        radio.addEventListener("change", () => {
          if (radio.checked) choose(r.id);
        });
      }
      empty.hidden = list.length > 0;
    };

    /** Show the provider `id` in the form, or a new provider for null. */
    const select = (id: string | null): void => {
      const row = id === null ? undefined : list.find((r) => r.id === id);
      selected = row === undefined ? null : row.id;
      for (const item of rowsBox.querySelectorAll<HTMLElement>(".aiset-row")) {
        const chosen = item.dataset.id === selected;
        item.dataset.chosen = chosen ? "true" : "false";
        const radio = item.querySelector<HTMLInputElement>('input[type="radio"]');
        if (radio) radio.checked = chosen;
      }
      fillForm(row === undefined ? newDraft() : draftOf(row), row !== undefined);
    };

    /** The person picked another row or `Add provider`: an edit that was not saved is lost, and the line says so. */
    const leave = (): void => {
      if (sameDraft(readDraft(), filled)) status("");
      else status("Changes were not saved.");
    };

    function choose(id: string | null): void {
      leave();
      select(id);
    }

    /**
     * Read the list of providers. Reads are one at a time; a read that a later one replaced neither asks the core nor shows.
     * `prefer` is the provider to show afterwards (the one just saved), else the one shown now, else the first.
     */
    const read = (prefer?: string | null): Promise<boolean> => {
      const mine = ++readNo;
      const run = tail.then(async () => {
        if (done || mine !== readNo) return false;
        rowsBox.setAttribute("aria-busy", "true");
        let raw: unknown = null;
        let ok = true;
        try {
          raw = await call<unknown>("ai_profiles");
        } catch {
          ok = false;
        }
        if (done || mine !== readNo) return false;
        rowsBox.removeAttribute("aria-busy");
        if (!ok) {
          list = [];
          status("The providers could not be read.", "error");
        } else {
          for (const fn of [...listeners]) fn(raw);
          const parsed = readProfileViews(raw);
          list = parsed.profiles;
          fitNodes(noteList, noteNodes, parsed.notes.length, () => el("li"));
          parsed.notes.forEach((note, i) => setText(noteNodes[i] as HTMLLIElement, note));
          noteList.hidden = parsed.notes.length === 0;
        }
        const want = prefer === undefined ? selected : prefer;
        drawList();
        select(list.some((r) => r.id === want) ? want : null);
        return ok;
      });
      tail = run.then(
        () => {},
        () => {},
      );
      return run;
    };

    // ---- save ----------------------------------------------------------------------------------------------------

    const save = async (): Promise<void> => {
      if (busy || done) return;
      const draft = readDraft();
      setBusy(true);
      status("Saving…");
      let id: unknown;
      try {
        id = await call<unknown>("ai_profile_save", { input: inputOf(draft, false) });
      } catch (e) {
        if (done) return;
        setBusy(false);
        status(coreText(e), "error");
        return;
      }
      if (done) return;
      const savedId = typeof id === "string" ? id : draft.id;
      const ok = await read(savedId);
      if (done) return;
      setBusy(false);
      if (ok) status("Saved.");
    };

    // ---- events --------------------------------------------------------------------------------------------------

    presetOptions.addEventListener("change", () => {
      // A new type starts on its first API shape; the name, the address and the model typed so far stay.
      const tag = chosenPreset();
      drawWires(tag, presetOf(tag)?.wires[0] ?? "anthropic");
      drawHint();
    });
    wireOptions.addEventListener("change", drawHint);
    addButton.addEventListener("click", () => {
      choose(null);
      nameInput.focus();
    });
    saveButton.addEventListener("click", () => void save());

    // ---- closing and keys ----------------------------------------------------------------------------------------

    let pop = (): void => {};
    const finish = (): void => {
      if (done) return;
      done = true;
      pop();
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
      } else if (e.key === "Enter" && e.target instanceof HTMLInputElement && e.target.type === "text") {
        // Enter in a text field saves; a button, a checkbox and a radio keep their own Enter.
        e.preventDefault();
        void save();
      }
      // Everything else is the focused control's: typing, Space and the arrows are left alone, and nothing reaches the
      // terminal or the shortcuts behind the dialog.
      return true;
    });

    // The keyboard moves into the dialog at once; it goes to the first row, or to the name of a new provider, when the
    // list has been read.
    fillForm(newDraft(), false);
    box.focus({ preventScroll: true });
    void read(focus ?? null).then(() => {
      if (done) return;
      if (focus === undefined && selected === null && list.length > 0) select(list[0]?.id ?? null);
      const row = selected === null ? undefined : rowsBox.querySelector<HTMLInputElement>('input[type="radio"]:checked');
      (row ?? nameInput).focus({ preventScroll: true });
    });
  });
}
