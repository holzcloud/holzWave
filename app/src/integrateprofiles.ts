// The integration profiles dialog (docs/design.md section 6.27): where a person manages what holzWave remembers about a
// connection of an integration tool. It is a sibling of the AI providers dialog (6.13), with the same box, the same key handling
// and the same write-only rule for secrets. Every string goes in with `textContent` or `value`, never as markup. The dialog
// validates nothing but how long a field is: the core checks a profile and says why it refuses one, and the dialog shows that
// sentence. It builds no name, argument list, path or program, and it runs nothing. The page names no tool: the tools, their
// names and the fields they select come from the core's list.
import { call } from "./bridge.js";
import { el, loadStyles, setText } from "./dom.js";
import {
  FIELD_WORDS, PROFILE_CAPS, PROFILE_WORDS, ROW_CAPS, duplicateName, fieldsFor, namedRow, profileSelectionText, profileToSave, readFault,
  readProfiles, slug, takesOwnFolder, uniqueId,
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
const DELETE_TEXT = "Delete";
const DELETE_ARMED = "Delete?";

/** A secret variable of the form: its name, what the core said about it, and the controls of its row. The value is in no member. */
interface SecretRow {
  name: string;
  /** The profile on the core's side declares this name, so the core takes a value for it. */
  saved: boolean;
  /** The keychain holds a value, as far as the core said. */
  has: boolean;
  node: HTMLElement;
  input: HTMLInputElement;
  state: HTMLElement;
  set: HTMLButtonElement;
  clear: HTMLButtonElement;
  hint: HTMLElement | null;
  error: HTMLElement;
}

/** A plain variable of the form. */
interface EnvRow {
  node: HTMLElement;
  name: HTMLInputElement;
  value: HTMLInputElement;
  error: HTMLElement;
}

/** Whether a secret field holds text, asked of the field's own state (its placeholder is hidden while it holds text): nothing reads the value. */
function holdsText(input: HTMLInputElement): boolean {
  return !input.matches(":placeholder-shown");
}

/** The value of a secret field, taken: the field is empty from this moment on. The only place a typed value leaves a field. */
function takeValue(input: HTMLInputElement): string {
  const text = input.value;
  input.value = "";
  return text;
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
      if (id !== "") input.id = id;
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

    // ---- the secret variables: rows of a name, a field that is never filled in, and a word ----------------------

    const secretGroup = group("ip-secrets", "Secret variables");
    const secretBox = el("div", "ip-secret-rows", secretGroup);
    const secretAdd = el("div", "aiset-field ip-add-row", secretGroup);
    const secretAddLabel = el("label", "", secretAdd);
    secretAddLabel.htmlFor = "ip-secret-name";
    secretAddLabel.textContent = "Variable name";
    const secretAddLine = el("div", "aiset-keyrow", secretAdd);
    const secretName = textInput(secretAddLine, "ip-secret-name", PROFILE_CAPS.envName);
    secretName.dataset.add = "";
    const secretAddButton = el("button", "ip-secret-add", secretAddLine);
    secretAddButton.type = "button";
    secretAddButton.textContent = "Add";
    el("p", "aiset-hint", secretGroup).textContent = PROFILE_WORDS.secretsHint;

    // ---- the plain variables: rows of a name and a value --------------------------------------------------------

    const envGroup = group("ip-env", "Environment");
    const envBox = el("div", "ip-env-rows", envGroup);
    const envAddButton = el("button", "ip-env-add", envGroup);
    envAddButton.type = "button";
    envAddButton.textContent = "Add";

    const syncNote = el("p", "aiset-hint ip-sync", box);
    syncNote.textContent = PROFILE_WORDS.sync;

    const formActions = el("div", "aiset-form-actions", box);
    const deleteButton = el("button", "aiset-remove ip-delete", formActions);
    deleteButton.type = "button";
    deleteButton.textContent = DELETE_TEXT;
    deleteButton.hidden = true;
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
    const envRows: EnvRow[] = [];
    const secretRows: SecretRow[] = [];
    /** What the form was filled with, to tell a change from a profile that was only shown. */
    let filled = "";
    let done = false;
    let busy = false;
    let shownUnsaved = false;
    let closeArmed = false;
    let deleteArmed = false;
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
        env: envRows.map((r) => ({ name: r.name.value, value: r.value.value })),
        secrets: secretRows.map((r) => r.name),
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
      drawTools();
      drawFields();
      secretBox.replaceChildren();
      secretRows.length = 0;
      for (const s of profile?.secrets ?? []) addSecretRow(s.name);
      envBox.replaceChildren();
      envRows.length = 0;
      for (const r of profile?.env ?? []) addEnvRow(r.name, r.value);
      deleteButton.hidden = profile === null;
      disarmDelete();
      drawRowControls();
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

    // ---- the rows of the secret and the plain variables ---------------------------------------------------------

    let rowNo = 0;

    const rowError = (parent: HTMLElement): HTMLElement => {
      const line = el("p", "aiset-error ip-row-error", parent);
      line.id = `ip-row-error-${++rowNo}`;
      line.setAttribute("role", "alert");
      line.hidden = true;
      return line;
    };
    const rowButton = (parent: HTMLElement, cls: string, text: string): HTMLButtonElement => {
      const b = el("button", cls, parent);
      b.type = "button";
      b.textContent = text;
      return b;
    };
    const showRowError = (line: HTMLElement, input: HTMLInputElement | null, text: string): void => {
      line.textContent = text;
      line.hidden = false;
      if (input) {
        input.setAttribute("aria-invalid", "true");
        input.setAttribute("aria-describedby", line.id);
      }
    };
    const clearRowError = (line: HTMLElement, input: HTMLInputElement | null): void => {
      line.hidden = true;
      line.textContent = "";
      input?.removeAttribute("aria-invalid");
      input?.removeAttribute("aria-describedby");
    };

    /** What the core said about a name of this profile: whether it declares it and whether the keychain holds a value. */
    const secretFact = (name: string): { saved: boolean; has: boolean } => {
      const found = selected === null ? undefined : list.find((p) => p.id === selected)?.secrets.find((x) => x.name === name);
      return { saved: found !== undefined, has: found?.has_value ?? false };
    };

    const addSecretRow = (name: string): SecretRow => {
      const fact = secretFact(name);
      const node = el("div", "ip-secret", secretBox);
      node.dataset.name = name;
      const head = el("div", "ip-secret-head", node);
      el("span", "ip-vname", head).textContent = name;
      const state = el("span", "ip-state", head);
      const line = el("div", "ip-secret-edit", node);
      // A field that is never filled in: empty on every open, never autofilled, no show toggle.
      const input = el("input", "", line);
      input.type = "password";
      input.autocomplete = "off";
      input.spellcheck = false;
      input.autocapitalize = "off";
      input.maxLength = PROFILE_CAPS.envValue;
      input.setAttribute("aria-label", `Value of ${name}`);
      input.placeholder = "Value";
      input.dataset.secret = "";
      const set = rowButton(line, "ip-set", "Set");
      const clear = rowButton(line, "ip-clear", "Clear");
      const remove = rowButton(line, "ip-remove", "Remove");
      let hint: HTMLElement | null = null;
      if (!fact.saved) {
        hint = el("p", "aiset-hint ip-secret-hint", node);
        hint.id = `ip-secret-hint-${++rowNo}`;
        hint.textContent = PROFILE_WORDS.saveFirst;
        set.setAttribute("aria-describedby", hint.id);
      }
      const error = rowError(node);
      const row: SecretRow = { name, saved: fact.saved, has: fact.has, node, input, state, set, clear, hint, error };
      secretRows.push(row);
      input.addEventListener("input", () => {
        clearRowError(error, null);
        drawRowControls();
      });
      set.addEventListener("click", () => void setSecret(row));
      clear.addEventListener("click", () => void clearSecret(row));
      remove.addEventListener("click", () => removeSecret(row));
      return row;
    };

    const addEnvRow = (name: string, value: string): EnvRow => {
      const node = el("div", "ip-env-row", envBox);
      const line = el("div", "ip-env-line", node);
      const nameField = textInput(line, "", PROFILE_CAPS.envName);
      nameField.classList.add("ip-env-name");
      nameField.value = name;
      nameField.setAttribute("aria-label", "Name");
      const valueField = textInput(line, "", PROFILE_CAPS.envValue);
      valueField.classList.add("ip-env-value");
      valueField.value = value;
      const label = (): void => valueField.setAttribute("aria-label", nameField.value.trim() === "" ? "Value" : `Value of ${nameField.value.trim()}`);
      label();
      nameField.addEventListener("input", label);
      const remove = rowButton(line, "ip-env-remove", "Remove");
      const error = rowError(node);
      const row: EnvRow = { node, name: nameField, value: valueField, error };
      envRows.push(row);
      remove.addEventListener("click", () => {
        envRows.splice(envRows.indexOf(row), 1);
        node.remove();
        drawRowControls();
        refreshDirty();
        envAddButton.focus();
      });
      return row;
    };

    /** The controls of the rows follow the state: `Set` needs a saved name and text in the field, `Clear` a saved name with a value. */
    const drawRowControls = (): void => {
      for (const r of secretRows) {
        // The placeholder first: a field with none cannot tell whether it holds text.
        r.input.placeholder = r.has ? "Enter a new value to replace it" : "Value";
        r.set.disabled = busy || !r.saved || !holdsText(r.input);
        r.clear.disabled = busy || !r.saved || !r.has;
        setText(r.state, r.has ? PROFILE_WORDS.saved : PROFILE_WORDS.notSet);
        r.state.dataset.state = r.has ? "saved" : "not_set";
      }
      secretAddButton.disabled = busy || secretRows.length >= ROW_CAPS.secrets;
      envAddButton.disabled = busy || envRows.length >= ROW_CAPS.env;
    };

    /** Put the new fact about a name into the list the page keeps, so that a profile shown again says the same. */
    const rememberValue = (name: string, has: boolean): void => {
      const entry = selected === null ? undefined : list.find((p) => p.id === selected)?.secrets.find((x) => x.name === name);
      if (entry) entry.has_value = has;
    };

    /** `Set`: the value goes in one call. The field is emptied while the call is made, before the core answers; nothing keeps the value. */
    const setSecret = async (row: SecretRow): Promise<void> => {
      const profile = selected;
      if (busy || done || profile === null || !row.saved || !holdsText(row.input)) return;
      const name = row.name;
      clearRowError(row.error, null);
      setBusy(true);
      try {
        await call("integration_secret_set", { profile, name, value: takeValue(row.input) });
      } catch (e) {
        if (done) return;
        setBusy(false);
        showRowError(row.error, null, readFault(e).text);
        return;
      }
      if (done) return;
      row.has = true;
      rememberValue(name, true);
      setBusy(false);
    };

    const clearSecret = async (row: SecretRow): Promise<void> => {
      const profile = selected;
      if (busy || done || profile === null || !row.saved || !row.has) return;
      const name = row.name;
      clearRowError(row.error, null);
      setBusy(true);
      try {
        await call("integration_secret_clear", { profile, name });
      } catch (e) {
        if (done) return;
        setBusy(false);
        showRowError(row.error, null, readFault(e).text);
        return;
      }
      if (done) return;
      row.has = false;
      rememberValue(name, false);
      setBusy(false);
    };

    /** `Remove`: the name leaves the profile at the next save, which clears its value first. Nothing is called now. */
    const removeSecret = (row: SecretRow): void => {
      secretRows.splice(secretRows.indexOf(row), 1);
      row.node.remove();
      drawRowControls();
      refreshDirty();
      secretName.focus();
    };

    const addSecretFromField = (): void => {
      const name = secretName.value.trim();
      if (name === "" || busy) return;
      const existing = secretRows.find((r) => r.name === name);
      if (existing) {
        existing.input.focus();
        return;
      }
      if (secretRows.length >= ROW_CAPS.secrets) return;
      addSecretRow(name);
      secretName.value = "";
      drawRowControls();
      refreshDirty();
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
      deleteButton.disabled = value;
      drawRowControls();
    };

    /** The core's sentence on the line under the profile. */
    const showLine = (raw: unknown): void => {
      errorLine.textContent = readFault(raw).text;
      errorLine.hidden = false;
    };

    /**
     * The core's refusal of a save: its sentence, as it came. It goes on the row that carries the variable name the sentence
     * mentions (a plain variable, then a secret one) and otherwise under the profile. The page does not interpret the sentence.
     */
    const showFault = (raw: unknown): void => {
      const text = readFault(raw).text;
      const env = namedRow(text, envRows.map((r) => r.name.value.trim()));
      const envRow = env >= 0 ? envRows[env] : undefined;
      if (envRow) {
        showRowError(envRow.error, envRow.name, text);
        envRow.name.focus();
        return;
      }
      const secret = namedRow(text, secretRows.map((r) => r.name));
      const secretRow = secret >= 0 ? secretRows[secret] : undefined;
      if (secretRow) {
        showRowError(secretRow.error, null, text);
        return;
      }
      showLine(raw);
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
      // A name that leaves a saved profile takes its value out of the keychain first: saving a profile does not touch values.
      const before = selected === null ? undefined : list.find((p) => p.id === selected);
      for (const dropped of (before?.secrets ?? []).map((x) => x.name).filter((n) => !secretRows.some((r) => r.name === n))) {
        try {
          await call("integration_secret_clear", { profile: before?.id ?? "", name: dropped });
        } catch (e) {
          if (done) return;
          setBusy(false);
          status("");
          shownUnsaved = false;
          showLine(e);
          refreshDirty();
          return;
        }
        rememberValue(dropped, false);
      }
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

    // ---- delete --------------------------------------------------------------------------------------------------

    function disarmDelete(): void {
      if (!deleteArmed) return;
      deleteArmed = false;
      deleteButton.removeAttribute("data-armed");
      setText(deleteButton, DELETE_TEXT);
    }

    /** `Delete` asks on the first press and removes the profile, with its secrets, on the second. */
    const remove = async (): Promise<void> => {
      const id = selected;
      if (busy || done || id === null) return;
      if (!deleteArmed) {
        deleteArmed = true;
        deleteButton.dataset.armed = "true";
        setText(deleteButton, DELETE_ARMED);
        return;
      }
      disarmDelete();
      clearError();
      setBusy(true);
      let raw: unknown;
      try {
        raw = await call<unknown>("integration_profile_remove", { id });
      } catch (e) {
        if (done) return;
        setBusy(false);
        showLine(e);
        return;
      }
      if (done) return;
      take(raw);
      fill(null);
      drawList();
      markChosen();
      setBusy(false);
      status("Deleted.");
      nameInput.focus();
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
    deleteButton.addEventListener("click", () => void remove());
    secretAddButton.addEventListener("click", addSecretFromField);
    envAddButton.addEventListener("click", () => {
      if (envRows.length >= ROW_CAPS.env) return;
      const row = addEnvRow("", "");
      drawRowControls();
      row.name.focus();
    });

    // Any change of the form takes the sentence of the core away and updates the line. A value typed into a secret field, or a name
    // not yet added, is no change of the profile.
    const edited = (e: Event): void => {
      const target = e.target;
      if (!(target instanceof HTMLElement) || target.dataset.secret !== undefined || target.dataset.add !== undefined) return;
      clearError();
      const row = envRows.find((r) => r.node.contains(target));
      if (row) clearRowError(row.error, row.name);
      disarmClose(false);
      refreshDirty();
    };
    box.addEventListener("input", edited);
    box.addEventListener("change", edited);

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
        if (e.target !== deleteButton) disarmDelete();
      },
      true,
    );
    box.addEventListener("keydown", (e) => {
      if (e.target instanceof HTMLInputElement && e.key !== "Tab" && e.key !== "Shift" && e.key !== "Escape") {
        disarmClose();
        disarmDelete();
      }
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
        // An armed delete is taken back first; the next Escape closes.
        if (deleteArmed) disarmDelete();
        else requestClose();
      } else if (e.key === "Tab") {
        e.preventDefault();
        trap(e);
      } else if (e.key === "Enter" && e.target instanceof HTMLInputElement && (e.target.type === "text" || e.target.type === "password")) {
        // Enter in a text field saves, in a secret field it is `Set` for that row, in the name of a new variable it is `Add`; a
        // button, a checkbox and a radio keep their own Enter.
        e.preventDefault();
        const target = e.target;
        if (target.dataset.secret !== undefined) {
          const row = secretRows.find((r) => r.input === target);
          if (row) void setSecret(row);
        } else if (target.dataset.add !== undefined) addSecretFromField();
        else void save();
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
