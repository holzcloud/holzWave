// The key binding editor (DESK-25): every action with its title and the chord in force, and for each a Change, an
// Unbind and a Reset. It renders from `state.settings` only - the core's resolved table and the warnings of its last
// view - and a change goes out as the whole overrides map through `set_setting("keybindings", map)`; the core
// validates (plain keys and Ctrl+letter are refused, an override beats a default and unbinds the displaced action)
// and the row shows what the core answered. A chord the core refused leaves the row as it was and its sentence
// appears beside the row. Every string goes in with `textContent`. While the dialog is open it consumes the keys
// (the key handler stack); while a chord is being captured it consumes every key.
import { el, fitNodes, loadStyles, setAttr, setText } from "./dom.js";
import { pushKeyHandler } from "./keystack.js";
import { primaryChord } from "./keymap.js";
import { state } from "./state.js";

/** What the editor needs from `settings.ts` (injected: the editor never imports the module that opens it). */
export interface KeybindingsApi {
  /** Ask the core to set `key` to `value()`; resolves to null on success and to the core's reason on failure. */
  change(key: string, value: () => unknown): Promise<string | null>;
  /** Call `fn` whenever a new view was accepted; the returned function stops it. */
  watch(fn: () => void): () => void;
}

/** The action id of the setting the core keeps the overrides under. */
const KEY = "keybindings";

/** The hint under the title; the core's refusals are shown beside the rows they concern. */
export const KEYBINDINGS_HINT =
  "Choose Change, then press the new key combination; Escape cancels. A combination needs Cmd, or two modifiers " +
  "including Ctrl or Alt; the core refuses the others and says why.";

/** The words that mark a warning as one about key bindings (the core's resolver and its load notes). */
const BINDING_NOTE = /binding|key combination/i;

let opened: Promise<void> | null = null;

/** Open the editor. A second ask while it is open returns the same promise. Resolves when it is closed. */
export function openKeybindings(api: KeybindingsApi): Promise<void> {
  opened ??= show(api).finally(() => {
    opened = null;
  });
  return opened;
}

/** Whether `text` names `action`: the id as a whole word (ids hold letters, digits, dots and underscores). */
export function mentions(text: string, action: string): boolean {
  let from = 0;
  for (;;) {
    const at = text.indexOf(action, from);
    if (at < 0) return false;
    const before = at === 0 ? "" : (text[at - 1] as string);
    const after = text[at + action.length] ?? "";
    if (!/[A-Za-z0-9_.]/.test(before) && !/[A-Za-z0-9_]/.test(after)) return true;
    from = at + 1;
  }
}

/** The parts of one row, kept so a render updates the elements in place and focus survives it. */
interface Row {
  root: HTMLLIElement;
  title: HTMLElement;
  chord: HTMLElement;
  tag: HTMLElement;
  change: HTMLButtonElement;
  unbind: HTMLButtonElement;
  reset: HTMLButtonElement;
  warn: HTMLUListElement;
  warnNodes: HTMLLIElement[];
  /** The action this element shows now. */
  action: string;
}

async function show(api: KeybindingsApi): Promise<void> {
  try {
    await loadStyles("./dialogs.css");
    await loadStyles("./settings.css");
  } catch {
    // the dialog still works without its style sheets
  }

  return new Promise<void>((resolve) => {
    const backdrop = el("div", "dlg-backdrop", document.body);
    const box = el("div", "dlg settings keybindings", backdrop);
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-modal", "true");
    box.setAttribute("aria-labelledby", "kb-title");
    box.tabIndex = -1; // takes the focus while a chord is captured, so no button sees the keys

    const title = el("h2", "set-title", box);
    title.id = "kb-title";
    title.textContent = "Key bindings";
    el("p", "kb-hint", box).textContent = KEYBINDINGS_HINT;
    const statusLine = el("p", "set-status", box);
    statusLine.setAttribute("role", "status");
    const notes = el("ul", "set-warnings kb-notes", box);
    const noteNodes: HTMLLIElement[] = [];
    const list = el("ul", "kb-list", box);
    list.setAttribute("aria-label", "Key bindings");
    const rows: Row[] = [];

    const actionsBar = el("div", "dlg-actions", box);
    const close = el("button", "dlg-ok", actionsBar);
    close.type = "button";
    close.textContent = "Close";

    /** The action whose chord is being captured; null when none. */
    let capturing: string | null = null;
    let release: (() => void) | null = null;

    // ---- talking to the core ----------------------------------------------------------------------

    /**
     * Set one action's entry in the overrides map, `null` removing it. The map is built when the request's turn
     * comes, from the last view, so two quick changes both land.
     */
    const apply = async (action: string, chord: string | null): Promise<void> => {
      setText(statusLine, "");
      const reason = await api.change(KEY, () => {
        const map: Record<string, string> = { ...(state.settings?.settings.keybindings ?? {}) };
        if (chord === null) delete map[action];
        else map[action] = chord;
        return map;
      });
      setText(statusLine, reason ?? "");
      render();
    };

    // ---- capturing a chord ------------------------------------------------------------------------

    const endCapture = (): void => {
      if (capturing === null) return;
      const action = capturing;
      capturing = null;
      release?.();
      release = null;
      render();
      rows.find((r) => r.action === action)?.change.focus();
    };

    const startCapture = (action: string): void => {
      if (capturing !== null) endCapture();
      capturing = action;
      render();
      box.focus({ preventScroll: true });
      release = pushKeyHandler((e) => {
        // Every key is the editor's now: none reaches a button, a shortcut or the terminal.
        e.preventDefault();
        if (e.repeat) return true;
        if (e.key === "Escape" && !e.metaKey && !e.ctrlKey && !e.altKey && !e.shiftKey) {
          endCapture();
          return true;
        }
        const chord = primaryChord(e);
        if (chord === null) return true; // a modifier on its own or a key with no name: keep waiting
        const target = capturing;
        endCapture();
        if (target !== null) void apply(target, chord);
        return true;
      });
    };

    // ---- the rows ---------------------------------------------------------------------------------

    const makeRow = (): Row => {
      const root = el("li", "kb-row");
      const titleNode = el("span", "kb-title", root);
      const chord = el("kbd", "kb-chord", root);
      const tag = el("span", "kb-tag", root);
      const buttons = el("span", "kb-buttons", root);
      const make = (text: string): HTMLButtonElement => {
        const b = el("button", "", buttons);
        b.type = "button";
        b.textContent = text;
        return b;
      };
      const change = make("Change");
      const unbind = make("Unbind");
      const reset = make("Reset");
      const warn = el("ul", "kb-warn", root);
      const row: Row = { root, title: titleNode, chord, tag, change, unbind, reset, warn, warnNodes: [], action: "" };
      change.addEventListener("click", () => startCapture(row.action));
      unbind.addEventListener("click", () => void apply(row.action, ""));
      reset.addEventListener("click", () => void apply(row.action, null));
      return row;
    };

    /** Disable a button; a focused one hands the focus to the row's Change button first, so the keyboard stays put. */
    const setDisabled = (button: HTMLButtonElement, row: Row, disabled: boolean): void => {
      if (disabled && document.activeElement === button) row.change.focus({ preventScroll: true });
      if (button.disabled !== disabled) button.disabled = disabled;
    };

    /**
     * Show `state.settings`: the core's table (a row per action, the chord in force), the core's sentences beside
     * the rows whose action they name, and the binding sentences that name no row as notes above the list.
     */
    function render(): void {
      const view = state.settings;
      if (!view) {
        setText(statusLine, "The settings are not available.");
        return;
      }
      const table = view.keybindings;
      const overrides = view.settings.keybindings;
      while (rows.length < table.length) {
        const row = makeRow();
        list.append(row.root);
        rows.push(row);
      }
      while (rows.length > table.length) rows.pop()?.root.remove();

      table.forEach((entry, i) => {
        const row = rows[i] as Row;
        row.action = entry.action;
        const look = entry.chord === null ? "unbound" : entry.is_default ? "default" : "custom";
        const here = capturing === entry.action;
        setAttr(row.root, "data-action", entry.action);
        setAttr(row.root, "data-state", look);
        row.root.classList.toggle("capturing", here);
        setText(row.title, entry.title);
        setText(row.chord, here ? "Press the new key combination" : (entry.chord ?? "Unbound"));
        // The tag is the cue besides the chord's own text: custom and default are told apart without colour.
        setText(row.tag, here ? "" : look === "unbound" ? "" : look);
        setAttr(row.change, "aria-label", `Change ${entry.title}`);
        setAttr(row.unbind, "aria-label", `Unbind ${entry.title}`);
        setAttr(row.reset, "aria-label", `Reset ${entry.title}`);
        setDisabled(row.unbind, row, entry.chord === null || here);
        // An entry the core refused is still in the map, so it can be reset although the row shows the default.
        setDisabled(row.reset, row, !Object.hasOwn(overrides, entry.action) || here);
        const mine = view.warnings.filter((w) => mentions(w, entry.action));
        fitNodes(row.warn, row.warnNodes, mine.length, () => el("li"));
        mine.forEach((w, j) => setText(row.warnNodes[j] as HTMLLIElement, w));
      });

      const loose = view.warnings.filter((w) => BINDING_NOTE.test(w) && !table.some((r) => mentions(w, r.action)));
      fitNodes(notes, noteNodes, loose.length, () => el("li"));
      loose.forEach((w, i) => setText(noteNodes[i] as HTMLLIElement, w));
    }
    render();

    // ---- closing and keys -------------------------------------------------------------------------

    let stop = (): void => {};
    let pop = (): void => {};
    let done = false;
    const finish = (): void => {
      if (done) return;
      done = true;
      release?.();
      release = null;
      capturing = null;
      stop();
      pop();
      backdrop.remove();
      resolve();
    };

    stop = api.watch(render);
    close.addEventListener("click", finish);
    backdrop.addEventListener("pointerdown", (e) => {
      if (e.target === backdrop) finish();
    });

    /** Tab and Shift+Tab stay inside the dialog: from its last button to its first and back. */
    const trap = (e: KeyboardEvent): void => {
      const items = Array.from(box.querySelectorAll<HTMLButtonElement>("button")).filter((b) => !b.disabled);
      if (items.length === 0) return;
      const at = items.indexOf(document.activeElement as HTMLButtonElement);
      const next = e.shiftKey ? (at <= 0 ? items.length - 1 : at - 1) : at < 0 || at === items.length - 1 ? 0 : at + 1;
      (items[next] as HTMLButtonElement).focus();
    };

    pop = pushKeyHandler((e) => {
      if (e.key === "Escape") {
        e.preventDefault();
        finish();
      } else if (e.key === "Tab") {
        e.preventDefault();
        trap(e);
      }
      // Everything else is the focused button's (Enter and Space press it); nothing reaches the terminal or the
      // shortcuts behind the dialog.
      return true;
    });

    (rows[0]?.change ?? close).focus();
  });
}
