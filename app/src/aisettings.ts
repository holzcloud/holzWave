// The AI providers dialog (docs/design.md section 6.13): the providers the chat talks to, added, edited and removed. It
// is a modal of its own with the box of the Settings dialog. Every string goes in with `textContent` or `value`, never as
// markup, and the dialog keeps the keys while it is open (the key handler stack): nothing reaches the terminal behind it.
//
// The dialog validates nothing and guesses nothing about where a request goes. It sends an address exactly as typed
// (trimmed), shows what the core answered (the host of a row is the core's `host`), and names no place: where a request
// goes is told by the chat after a turn. It probes nothing: no test connection, no model list, no scan for local servers.
// The list of providers is read with `ai_profiles` when the dialog opens and after a write, never anywhere else; every
// successful read is handed to `onProfilesChanged` listeners (the chat), so a saved provider shows without a reload.
import { registerAction } from "./actions.js";
import { call } from "./bridge.js";
import { el, fitNodes, loadStyles, setText } from "./dom.js";
import { focusInput } from "./keyboard.js";
import { pushKeyHandler } from "./keystack.js";
import { state } from "./state.js";
import {
  LOCAL_CONTEXT_HINT, LOCAL_CONTEXT_OPTIONS, LOCAL_ONLY_HINT, PRESETS, WIRE_LABELS, addressHint, describeFault,
  disclosureWords, draftOf, inputOf, keyAdvice, keyTone, keyWord, newDraft, presetOf, readProfileViews, sameDraft,
} from "./aistate.js";
import type { Fault, KeyState, PresetTag, ProfileDraft, ProfileRow, Slot, WireTag } from "./aistate.js";

/** Called with the raw answer of `ai_profiles` after every successful read the dialog makes. */
type ProfilesListener = (raw: unknown) => void;

/** What the dialog needs from `settings.ts` (injected: the dialog never imports the module that changes settings). */
export interface AiSettingsApi {
  /** Ask the core to set `key` to `value()`; resolves to null on success and to the core's reason on failure. */
  change(key: string, value: () => unknown): Promise<string | null>;
  /** Call `fn` whenever a new settings view was accepted; the returned function stops it. */
  watch(fn: () => void): () => void;
}

const listeners = new Set<ProfilesListener>();
let opened: Promise<void> | null = null;
let api: AiSettingsApi | null = null;

/**
 * Hand the dialog the settings it changes and register the page action `AI providers…` (no chord and no row in the core's
 * key table, like `keybindings.open`). Without this call the dialog has no privacy group.
 */
export function initAiSettings(settings: AiSettingsApi): void {
  api = settings;
  registerAction({ id: "ai.settings", title: "AI providers…", run: () => void openProviders().then(focusInput) });
}

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
const MAX_KEY = 2048;
const REMOVE_TEXT = "Remove provider";
const REMOVE_ARMED = "Click again to remove";

/** One key of the form: the password field, the word for its state, the advice and the refusal. */
interface KeyPart {
  wrap: HTMLElement;
  input: HTMLInputElement;
  state: HTMLElement;
  clear: HTMLButtonElement;
  advice: HTMLElement;
  error: HTMLElement;
  slot: Slot;
}

/**
 * Whether a key field holds text, asked of the field's own state (its placeholder is hidden while it holds text) so that
 * nothing but a save ever reads what was typed.
 */
function holdsText(part: KeyPart): boolean {
  return !part.input.matches(":placeholder-shown");
}

/** The line under a field where the core's refusal for it is written; empty and hidden until there is one. */
function errorLine(input: HTMLInputElement, name: string, parent?: Element): HTMLParagraphElement {
  const line = el("p", "aiset-error", parent ?? input.parentElement ?? undefined);
  line.id = `aiset-error-${name}`;
  line.dataset.for = name;
  line.hidden = true;
  return line;
}

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

    // ---- privacy: two switches that show only what the core answered ---------------------------------------------

    const settings = api;
    let drawPrivacy = (): void => {};
    let stopWatching = (): void => {};
    if (settings !== null) {
      const privacy = group("aiset-privacy", "Privacy");
      const onlyWrap = el("div", "aiset-field aiset-check", privacy);
      const onlyInput = el("input", "", onlyWrap);
      onlyInput.id = "aiset-local-only";
      onlyInput.type = "checkbox";
      onlyInput.setAttribute("aria-describedby", "aiset-local-only-hint");
      const onlyLabel = el("label", "", onlyWrap);
      onlyLabel.htmlFor = "aiset-local-only";
      onlyLabel.textContent = "Local only";
      const onlyHint = el("p", "aiset-hint", onlyWrap);
      onlyHint.id = "aiset-local-only-hint";
      onlyHint.textContent = LOCAL_ONLY_HINT;

      const contextSet = el("fieldset", "aiset-choice", privacy);
      contextSet.dataset.choice = "local-context";
      el("legend", "", contextSet).textContent = "Models on this computer";
      const contextOptions = el("div", "aiset-options", contextSet);
      const contextRadios = LOCAL_CONTEXT_OPTIONS.map(([value, text]) => {
        const item = el("label", "aiset-option", contextOptions);
        const radio = el("input", "", item);
        radio.type = "radio";
        radio.name = "aiset-local-context";
        radio.value = value;
        el("span", "", item).textContent = text;
        return radio;
      });
      el("p", "aiset-hint", contextSet).textContent = LOCAL_CONTEXT_HINT;

      // The controls are drawn from the last view the core sent, never from what was asked for.
      drawPrivacy = (): void => {
        const view = state.settings;
        onlyInput.disabled = view === null;
        for (const r of contextRadios) r.disabled = view === null;
        onlyInput.checked = view?.settings.ai_local_only === true;
        for (const r of contextRadios) r.checked = view !== null && r.value === view.settings.ai_local_context;
        if (view === null) status("The settings are not available.");
      };
      const ask = (key: string, wanted: unknown): void => {
        drawPrivacy(); // back to what the core last said, until it answers
        void settings.change(key, () => wanted).then((reason) => {
          if (done) return;
          if (reason !== null) status(reason, "error");
          drawPrivacy();
        });
      };
      onlyInput.addEventListener("change", () => ask("ai_local_only", onlyInput.checked));
      contextOptions.addEventListener("change", () => {
        const chosen = contextRadios.find((r) => r.checked);
        if (chosen) ask("ai_local_context", chosen.value);
      });
      stopWatching = settings.watch(() => drawPrivacy());
    }

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
    const nameError = errorLine(nameInput, "name");

    const presetSet = el("fieldset", "aiset-choice aiset-presets", form);
    el("legend", "", presetSet).textContent = "Type";
    const presetOptions = el("div", "aiset-options", presetSet);
    const typeNote = el("p", "aiset-hint", presetSet);
    typeNote.textContent = "The type of a saved provider cannot change. Add a new provider for another type.";
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
    const wireError = el("p", "aiset-error", wireSet);
    wireError.id = "aiset-error-wire";
    wireError.dataset.for = "wire";
    wireError.hidden = true;
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
    const addressError = errorLine(addressInput, "address");

    const lanWrap = el("div", "aiset-field aiset-check", form);
    const lanInput = el("input", "", lanWrap);
    lanInput.id = "aiset-lan";
    lanInput.type = "checkbox";
    const lanLabel = el("label", "", lanWrap);
    lanLabel.htmlFor = "aiset-lan";
    lanLabel.textContent = "Plain http on my local network";
    const lanHint = el("p", "aiset-hint", lanWrap);
    lanHint.id = "aiset-lan-hint";
    lanHint.textContent = "Only for a server on your own network. Never for the internet.";
    lanInput.setAttribute("aria-describedby", lanHint.id);

    const modelWrap = field("aiset-model", "Model");
    const modelInput = el("input", "", modelWrap);
    modelInput.id = "aiset-model";
    modelInput.type = "text";
    modelInput.maxLength = MAX_MODEL;
    modelInput.autocomplete = "off";
    modelInput.spellcheck = false;
    el("p", "aiset-hint", modelWrap).textContent =
      "Type the model name the provider uses. holzWave does not ask the provider for a list.";
    const modelError = errorLine(modelInput, "model");

    // The keys: a password field that is never filled in, the state of the key in words, and the advice for that state.
    const keyPart = (id: string, text: string, slot: Slot): KeyPart => {
      const wrap = field(id, text);
      wrap.classList.add("aiset-keyfield");
      const line = el("div", "aiset-keyrow", wrap);
      const input = el("input", "", line);
      input.id = id;
      input.type = "password";
      input.autocomplete = "new-password";
      input.spellcheck = false;
      input.autocapitalize = "off";
      input.maxLength = MAX_KEY;
      const state = el("span", slot === "provider" ? "aiset-keystate" : "aiset-gateway-state", line);
      const clear = el("button", slot === "provider" ? "aiset-key-clear" : "aiset-gateway-clear", line);
      clear.type = "button";
      clear.textContent = "Remove key";
      const advice = el("p", "aiset-hint", wrap);
      const error = errorLine(input, slot === "provider" ? "key" : "gateway-key", wrap);
      return { wrap, input, state, clear, advice, error, slot };
    };
    const keyOf = keyPart("aiset-key", "API key", "provider");
    const gatewayOf = keyPart("aiset-gateway-key", "Gateway token", "gateway");

    // The notice for a gateway or a server of the user's own: shown only when the core's view of the provider carries one.
    const card = el("div", "aiset-disclosure", form);
    card.hidden = true;
    el("span", "aiset-mark", card).textContent = "!";
    const cardText = el("div", "aiset-disclosure-text", card);
    const cardTitle = el("p", "aiset-disclosure-title", cardText);
    const cardBody = el("p", "aiset-disclosure-body", cardText);
    const allowButton = el("button", "aiset-allow", cardText);
    allowButton.type = "button";

    const formActions = el("div", "aiset-form-actions", form);
    const removeButton = el("button", "aiset-remove", formActions);
    removeButton.type = "button";
    removeButton.textContent = REMOVE_TEXT;
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
      removeButton.disabled = value;
      allowButton.disabled = value;
      keyOf.clear.disabled = value;
      gatewayOf.clear.disabled = value;
    };

    const savedRow = (): ProfileRow | undefined => (selected === null ? undefined : list.find((r) => r.id === selected));

    // ---- refusals at their fields --------------------------------------------------------------------------------

    /** Where each part of the form writes its refusal. */
    const marks: [HTMLElement | null, HTMLElement][] = [
      [nameInput, nameError], [addressInput, addressError], [modelInput, modelError], [keyOf.input, keyOf.error],
      [gatewayOf.input, gatewayOf.error], [null, wireError],
    ];

    const clearMark = (line: HTMLElement): void => {
      line.hidden = true;
      line.textContent = "";
      const mark = marks.find(([, l]) => l === line);
      const input = mark?.[0];
      if (input) {
        input.removeAttribute("aria-invalid");
        if (input === addressInput) input.setAttribute("aria-describedby", addressHintNode.id);
        else input.removeAttribute("aria-describedby");
      }
    };
    const clearErrors = (): void => {
      for (const [, line] of marks) clearMark(line);
    };

    const markOf = (field: Fault["field"], slot: Slot): [HTMLElement | null, HTMLElement] | null => {
      switch (field) {
        case "name": return [nameInput, nameError];
        case "address": return [addressInput, addressError];
        case "model": return [modelInput, modelError];
        case "key": return slot === "gateway" ? [gatewayOf.input, gatewayOf.error] : [keyOf.input, keyOf.error];
        case "wire": return [null, wireError];
        default: return null;
      }
    };

    /** Say a refusal at its field, with the focus on the field; a refusal that belongs to no field goes to the status line. */
    const showFault = (fault: Fault, slot: Slot = "provider"): void => {
      const mark = markOf(fault.field, slot);
      if (mark === null) {
        status(fault.text, "error");
        return;
      }
      const [input, line] = mark;
      clearMark(line);
      line.textContent = fault.text;
      line.hidden = false;
      if (input) {
        input.setAttribute("aria-invalid", "true");
        input.setAttribute("aria-describedby", input === addressInput ? `${line.id} ${addressHintNode.id}` : line.id);
        input.focus();
      } else {
        (wireRadios.find((r) => r.checked) ?? wireRadios[0])?.focus();
      }
    };

    for (const [input, line] of marks) {
      // Any edit of a field takes its refusal away.
      input?.addEventListener("input", () => clearMark(line));
    }
    wireOptions.addEventListener("change", () => clearMark(wireError));

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

    /** Show the state of one key: the word, its tone, the advice and the button that removes it. */
    const drawKey = (part: KeyPart, state: KeyState, need: "optional" | "required", known: boolean): void => {
      part.state.dataset.state = state;
      part.state.dataset.tone = keyTone(state, need);
      setText(part.state, keyWord(state));
      setText(part.advice, keyAdvice(state));
      part.advice.hidden = part.advice.textContent === "";
      part.clear.hidden = !known || state === "not_set";
      part.input.placeholder = state === "present" ? "Enter a new key to replace it" : part.slot === "provider" ? "API key" : "Gateway token";
    };

    /** The keys of the form for the type chosen now and the row shown (none for a new provider). */
    const drawKeys = (): void => {
      const info = presetOf(chosenPreset());
      const row = savedRow();
      drawKey(keyOf, row?.key ?? "not_set", info?.key ?? "optional", row !== undefined);
      const token = info?.gateway_token === true;
      gatewayOf.wrap.hidden = !token;
      if (!token) gatewayOf.input.value = "";
      drawKey(gatewayOf, row?.gateway_key ?? "not_set", "optional", row !== undefined && row.gateway_key !== null);
    };

    /** The notice of the provider shown, from the core's view of it; nothing for a new provider or one with no notice. */
    const drawDisclosure = (): void => {
      const notice = savedRow()?.disclosure ?? null;
      card.hidden = notice === null;
      const row = savedRow();
      if (notice === null || row === undefined) return;
      const words = disclosureWords(notice.kind, row.preset, notice.host);
      card.dataset.kind = notice.kind;
      setText(cardTitle, words.title);
      setText(cardBody, words.body);
      setText(allowButton, words.allow);
    };

    /** Put a draft into the form. `saved` is true for a provider that exists: its type cannot change. */
    const fillForm = (draft: ProfileDraft, saved: boolean, keepKeys: boolean): void => {
      filled = draft;
      setText(legend, saved ? "Provider" : "New provider");
      nameInput.value = draft.name;
      for (const r of presetRadios) {
        r.checked = r.value === draft.preset;
        r.disabled = saved;
      }
      typeNote.hidden = !saved;
      drawWires(draft.preset, draft.wire);
      addressInput.value = draft.base_url;
      lanInput.checked = draft.allow_lan_http;
      modelInput.value = draft.model;
      // A key typed is read once, when it is saved; it is never put back into a field, only taken out of it.
      if (!keepKeys) {
        keyOf.input.value = "";
        gatewayOf.input.value = "";
      }
      removeButton.hidden = !saved;
      disarm();
      drawHint();
      drawKeys();
      drawDisclosure();
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
        const info = presetOf(r.preset);
        el("span", "aiset-meta", item).textContent = `${info?.label ?? r.preset} · ${model}`;
        el("span", "aiset-host", item).textContent = r.host;
        const word = el("span", "aiset-key", item);
        word.dataset.state = r.key;
        word.dataset.tone = keyTone(r.key, info?.key ?? "optional");
        word.textContent = keyWord(r.key);
        if (r.disclosure !== null) el("span", "aiset-review", item).textContent = "review";
        radio.addEventListener("change", () => {
          if (radio.checked) choose(r.id);
        });
      }
      empty.hidden = list.length > 0;
    };

    /** Show the provider `id` in the form, or a new provider for null. `keepKeys` leaves what was typed in the key fields. */
    const select = (id: string | null, keepKeys = false, keepForm = false): void => {
      const row = id === null ? undefined : list.find((r) => r.id === id);
      const same = row !== undefined && row.id === selected;
      selected = row === undefined ? null : row.id;
      for (const item of rowsBox.querySelectorAll<HTMLElement>(".aiset-row")) {
        const chosen = item.dataset.id === selected;
        item.dataset.chosen = chosen ? "true" : "false";
        const radio = item.querySelector<HTMLInputElement>('input[type="radio"]');
        if (radio) radio.checked = chosen;
      }
      if (keepForm && same) {
        // The provider shown is the same: what is typed stays, and the states and the notice follow the core's view.
        filled = draftOf(row);
        drawKeys();
        drawDisclosure();
        return;
      }
      clearErrors();
      fillForm(row === undefined ? newDraft() : draftOf(row), row !== undefined, keepKeys);
    };

    /** The person picked another row or `Add provider`: an edit that was not saved is lost, and the line says so. */
    const leave = (): void => {
      if (sameDraft(readDraft(), filled) && !holdsText(keyOf) && !holdsText(gatewayOf)) status("");
      else status("Changes were not saved.");
    };

    function choose(id: string | null): void {
      leave();
      select(id);
    }

    /**
     * Read the list of providers. Reads are one at a time; a read that a later one replaced neither asks the core nor shows.
     * `prefer` is the provider to show afterwards (the one just saved), else the one shown now, else a new one. `keepKeys`
     * leaves what is typed in the key fields (a key the core refused stays for the next try).
     */
    const read = (prefer?: string | null, keepKeys = false, keepForm = false): Promise<boolean> => {
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
        select(list.some((r) => r.id === want) ? want : null, keepKeys, keepForm);
        return ok;
      });
      tail = run.then(
        () => {},
        () => {},
      );
      return run;
    };

    // ---- save ----------------------------------------------------------------------------------------------------

    /**
     * Send the key typed into `part` in one call: the only place a key text leaves a field, read into a local at this moment.
     * Null when the core took it (the field is emptied) or nothing was typed; else the core's refusal, with the text left in
     * the field for the next try.
     */
    const sendKey = async (profile: string, part: KeyPart): Promise<Fault | null> => {
      const text = part.input.value;
      if (text === "") return null;
      try {
        await call("ai_key_set", { profile, slot: part.slot, api_key: text });
      } catch (e) {
        return describeFault(e, "key");
      }
      part.input.value = "";
      return null;
    };

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
        status("");
        showFault(describeFault(e, "profile"));
        return;
      }
      if (done) return;
      clearErrors();
      const savedId = typeof id === "string" ? id : draft.id;
      // The profile first, then the keys typed in this visit: the provider key, then the gateway token. A key the core
      // refuses stays in its field with the words at that field; one it took leaves the field.
      let refused: { fault: Fault; slot: Slot } | null = null;
      if (savedId !== null) {
        const first = await sendKey(savedId, keyOf);
        if (first !== null) refused = { fault: first, slot: "provider" };
        if (presetOf(draft.preset)?.gateway_token === true) {
          const second = await sendKey(savedId, gatewayOf);
          if (second !== null && refused === null) refused = { fault: second, slot: "gateway" };
        }
      }
      if (done) return;
      const ok = await read(savedId, true);
      if (done) return;
      setBusy(false);
      if (ok) status("Saved.");
      if (refused !== null) showFault(refused.fault, refused.slot);
    };

    // ---- clear a key, remove a provider --------------------------------------------------------------------------

    const clearKey = async (part: KeyPart): Promise<void> => {
      const row = savedRow();
      if (busy || done || row === undefined) return;
      setBusy(true);
      try {
        await call("ai_key_clear", { profile: row.id, slot: part.slot });
      } catch (e) {
        if (done) return;
        setBusy(false);
        showFault(describeFault(e, "key"), part.slot);
        return;
      }
      if (done) return;
      const ok = await read(row.id, true);
      if (done) return;
      setBusy(false);
      if (ok) status("Removed.");
    };

    /**
     * Allow the host of the provider shown. This is the one call that sends `acknowledge_gateway` true, and it sends the
     * fields of the saved provider, never what is typed in the form; `Save` and every other path send false.
     */
    const allow = async (): Promise<void> => {
      const row = savedRow();
      if (busy || done || row === undefined || row.disclosure === null) return;
      const saved = draftOf(row);
      setBusy(true);
      status("Saving…");
      try {
        await call("ai_profile_save", { input: inputOf(saved, true) });
      } catch (e) {
        if (done) return;
        setBusy(false);
        status("");
        showFault(describeFault(e, "profile"));
        return;
      }
      if (done) return;
      const ok = await read(row.id, true, true);
      if (done) return;
      setBusy(false);
      if (ok) status("Allowed.");
    };

    let armed = false;
    function disarm(): void {
      armed = false;
      removeButton.removeAttribute("data-armed");
      setText(removeButton, REMOVE_TEXT);
    }

    const remove = async (): Promise<void> => {
      const row = savedRow();
      if (busy || done || row === undefined) return;
      if (!armed) {
        // The first click only asks; nothing is called.
        armed = true;
        removeButton.dataset.armed = "true";
        setText(removeButton, REMOVE_ARMED);
        return;
      }
      disarm();
      setBusy(true);
      try {
        await call("ai_profile_remove", { profile: row.id });
      } catch (e) {
        if (done) return;
        setBusy(false);
        showFault(describeFault(e, "profile"));
        return;
      }
      if (done) return;
      const ok = await read(null);
      if (done) return;
      setBusy(false);
      if (ok) status("Removed.");
    };

    // ---- events --------------------------------------------------------------------------------------------------

    presetOptions.addEventListener("change", () => {
      // A new type starts on its first API shape; the name, the address and the model typed so far stay.
      const tag = chosenPreset();
      drawWires(tag, presetOf(tag)?.wires[0] ?? "anthropic");
      drawHint();
      drawKeys();
    });
    wireOptions.addEventListener("change", drawHint);
    addButton.addEventListener("click", () => {
      choose(null);
      nameInput.focus();
    });
    saveButton.addEventListener("click", () => void save());
    removeButton.addEventListener("click", () => void remove());
    allowButton.addEventListener("click", () => void allow());
    keyOf.clear.addEventListener("click", () => void clearKey(keyOf));
    gatewayOf.clear.addEventListener("click", () => void clearKey(gatewayOf));
    // The first click on the remove button only asks; any other click or a key in a field takes the question back.
    box.addEventListener(
      "click",
      (e) => {
        if (e.target !== removeButton) disarm();
      },
      true,
    );
    box.addEventListener("keydown", (e) => {
      if (e.target instanceof HTMLInputElement && e.key !== "Tab" && e.key !== "Shift") disarm();
    });

    // ---- closing and keys ----------------------------------------------------------------------------------------

    let pop = (): void => {};
    const finish = (): void => {
      if (done) return;
      done = true;
      pop();
      stopWatching();
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
      } else if (e.key === "Enter" && e.target instanceof HTMLInputElement && (e.target.type === "text" || e.target.type === "password")) {
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
    fillForm(newDraft(), false, false);
    drawPrivacy();
    box.focus({ preventScroll: true });
    void read(focus ?? null).then(() => {
      if (done) return;
      if (focus === undefined && selected === null && list.length > 0) select(list[0]?.id ?? null);
      // A provider that waits for its review is opened on the button that allows it.
      if (savedRow()?.disclosure && focus !== undefined) {
        allowButton.focus({ preventScroll: true });
        return;
      }
      const row = selected === null ? undefined : rowsBox.querySelector<HTMLInputElement>('input[type="radio"]:checked');
      (row ?? nameInput).focus({ preventScroll: true });
    });
  });
}
