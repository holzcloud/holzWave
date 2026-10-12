// The settings dialog: one modal with a labelled control per setting. It renders from `state.settings` only, which
// is the last view the core sent; a control that changes asks the core through `api.change` and shows whatever the
// core answered, so a clamped number appears clamped and a refused value goes back to the last good one. Every
// string (a theme name, a reason, a warning) goes in with `textContent`. While it is open it consumes the keys
// (the key handler stack): nothing reaches the terminal behind it.
//
// A choice (theme, cursor blink, motion, contrast) is a group of radio buttons, not a `<select>`: WebKit builds
// the inner element of a select with an inline style attribute, which the page's strict `style-src` reports as a
// violation (measured: one `style-src-attr` per select). Radios have no such element, are one tab stop per group
// and move with the arrow keys.
import { el, fitNodes, loadStyles, setText } from "./dom.js";
import { pushKeyHandler } from "./keystack.js";
import { FONT_SIZE_RANGE, SCROLLBACK_RANGE, UI_SCALE_RANGE, typedInteger } from "./settings-model.js";
import type { SettingsValues } from "./settings-model.js";
import { state } from "./state.js";

/** What the dialog needs from `settings.ts` (injected: the dialog never imports the module that opens it). */
export interface SettingsApi {
  /** Ask the core to set `key` to `value()`; resolves to null on success and to the core's reason on failure. */
  change(key: string, value: () => unknown): Promise<string | null>;
  /** Ask the core to look at the files now; resolves like `change`. */
  reload(): Promise<string | null>;
  /** Call `fn` whenever a new view was accepted; the returned function stops it. */
  watch(fn: () => void): () => void;
  /** The person asked for the key binding editor: called as the dialog closes, which then opens the editor. */
  editBindings(): void;
}

/** One control: the setting it shows, how to read what was entered and how to show the setting's value. */
interface Control {
  key: keyof SettingsValues;
  /** The element whose `change` event means the person changed it (a radio group's fieldset gets its radios' events). */
  node: HTMLElement;
  /** Move the keyboard to the control. */
  focus(): void;
  /** What the control holds as the typed value the core expects, or null when it is not a value. */
  read(): string | number | boolean | null;
  /** Show the setting's current value. */
  write(value: SettingsValues[keyof SettingsValues]): void;
  /** The person typed into the field since it was last committed or shown. */
  dirty: boolean;
  /** The value of the request in flight, so a second commit of the same value is not a second request. */
  pending: string | number | boolean | undefined;
}

let opened: Promise<void> | null = null;

/** Open the dialog. A second ask while it is open returns the same promise. Resolves when it is closed. */
export function openSettings(api: SettingsApi): Promise<void> {
  opened ??= show(api).finally(() => {
    opened = null;
  });
  return opened;
}

/** The text of the scrollback control: the number is an upper limit, not a promise (the engine's byte limit comes first). */
export const SCROLLBACK_HINT =
  "Upper limit per pane, for panes opened from now on. The engine also stops at a memory limit, so a pane can keep " +
  "fewer lines than this (about 1800 at 40 columns).";

/** The text of the two effect toggles: they follow reduced motion, whatever the toggles say. */
export const EFFECTS_HINT =
  "Optional effects of the drawing, off by default. Both stay off while reduced motion is in effect, from the system " +
  "or from the Motion setting below.";

const CURSOR_BLINK = [["system", "System"], ["on", "On"], ["off", "Off"]] as const;
const MOTION = [["system", "System"], ["reduce", "Reduce"], ["full", "Full"]] as const;
const CONTRAST = [["system", "System"], ["normal", "Normal"], ["high", "High"]] as const;

async function show(api: SettingsApi): Promise<void> {
  try {
    await loadStyles("./dialogs.css");
    await loadStyles("./settings.css");
  } catch {
    // the dialog still works without its style sheets
  }

  return new Promise<void>((resolve) => {
    const backdrop = el("div", "dlg-backdrop", document.body);
    const box = el("div", "dlg settings", backdrop);
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-modal", "true");
    box.setAttribute("aria-labelledby", "set-title");

    const title = el("h2", "set-title", box);
    title.id = "set-title";
    title.textContent = "Settings";
    const statusLine = el("p", "set-status", box);
    statusLine.setAttribute("role", "status");
    const warnings = el("ul", "set-warnings", box);
    const warningNodes: HTMLLIElement[] = [];

    const controls: Control[] = [];
    const byNode = new Map<Element, Control>();

    const commit = async (c: Control): Promise<void> => {
      c.dirty = false;
      const typed = c.read();
      const current = state.settings?.settings[c.key];
      if (typed === null) {
        setText(statusLine, "Enter a whole number.");
        render();
        return;
      }
      if (typed === current || typed === c.pending) {
        render();
        return;
      }
      c.pending = typed;
      setText(statusLine, "");
      const reason = await api.change(c.key, () => typed);
      c.pending = undefined;
      setText(statusLine, reason ?? "");
      render();
    };

    const add = (c: Omit<Control, "dirty" | "pending">): Control => {
      const control: Control = { ...c, dirty: false, pending: undefined };
      controls.push(control);
      byNode.set(control.node, control);
      const isNumber = control.node instanceof HTMLInputElement && control.node.type === "number";
      if (isNumber) {
        // Typing is not a request: the value is committed with Enter or when the field is left.
        control.node.addEventListener("input", () => {
          control.dirty = true;
        });
      }
      control.node.addEventListener("change", () => void commit(control));
      return control;
    };

    const group = (legend: string): HTMLFieldSetElement => {
      const set = el("fieldset", "set-group", box);
      el("legend", "", set).textContent = legend;
      return set;
    };
    const rowOf = (parent: Element, id: string, text: string): HTMLDivElement => {
      const row = el("div", "set-row", parent);
      const label = el("label", "", row);
      label.htmlFor = id;
      label.textContent = text;
      return row;
    };

    /** A group of radio buttons for one setting; `options` can be replaced (the theme list is the core's). */
    const choice = (
      parent: Element, id: string, text: string, key: "theme" | "cursor_blink" | "motion" | "contrast",
    ): { setOptions(options: readonly (readonly [string, string])[]): void } => {
      const set = el("fieldset", "set-choice", parent);
      el("legend", "", set).textContent = text;
      const list = el("div", "set-options", set);
      let radios: HTMLInputElement[] = [];
      const setOptions = (options: readonly (readonly [string, string])[]): void => {
        list.replaceChildren();
        radios = options.map(([value, label], i) => {
          const item = el("label", "set-option", list);
          const radio = el("input", "", item);
          radio.type = "radio";
          radio.name = id;
          radio.id = `${id}-${i}`;
          radio.value = value;
          el("span", "", item).textContent = label;
          return radio;
        });
      };
      add({
        key,
        node: set,
        focus: () => (radios.find((r) => r.checked) ?? radios[0])?.focus(),
        read: () => radios.find((r) => r.checked)?.value ?? null,
        write: (v) => {
          for (const r of radios) r.checked = r.value === String(v);
        },
      });
      return { setOptions };
    };

    const toggle = (
      parent: Element, id: string, text: string,
      key: "option_as_alt" | "confirm_multiline_paste" | "smooth_scroll" | "cursor_trail", describedBy?: string,
    ): void => {
      const row = rowOf(parent, id, text);
      const node = el("input", "", row);
      node.type = "checkbox";
      node.id = id;
      if (describedBy) node.setAttribute("aria-describedby", describedBy);
      add({
        key,
        node,
        focus: () => node.focus(),
        read: () => node.checked,
        write: (v) => void (node.checked = v === true),
      });
    };

    const whole = (
      parent: Element, id: string, text: string, key: "font_size" | "ui_scale" | "scrollback_lines",
      range: readonly [number, number], unit: string, hint?: { id: string; text: string; data: string },
    ): void => {
      const row = rowOf(parent, id, text);
      const node = el("input", "", row);
      node.type = "number";
      node.id = id;
      node.min = String(range[0]);
      node.max = String(range[1]);
      node.step = "1";
      if (unit) el("span", "set-unit", row).textContent = unit;
      if (hint) {
        node.setAttribute("aria-describedby", hint.id);
        const p = el("p", "set-hint", parent);
        p.id = hint.id;
        p.setAttribute("data-hint", hint.data);
        p.textContent = hint.text;
      }
      add({
        key,
        node,
        focus: () => node.focus(),
        read: () => typedInteger(node.value),
        write: (v) => void (node.value = String(v)),
      });
    };

    // ---- the controls, grouped -------------------------------------------------------------------

    const look = group("Appearance");
    const theme = choice(look, "set-theme", "Theme", "theme");
    whole(look, "set-font-size", "Font size", "font_size", FONT_SIZE_RANGE, "px");
    whole(look, "set-ui-scale", "UI scale", "ui_scale", UI_SCALE_RANGE, "%");

    const terminal = group("Terminal");
    toggle(terminal, "set-option-as-alt", "Option as Alt", "option_as_alt");
    choice(terminal, "set-cursor-blink", "Cursor blink", "cursor_blink").setOptions(CURSOR_BLINK);
    whole(terminal, "set-scrollback", "Scrollback lines", "scrollback_lines", SCROLLBACK_RANGE, "", {
      id: "set-scrollback-hint", text: SCROLLBACK_HINT, data: "scrollback",
    });
    toggle(terminal, "set-confirm-paste", "Confirm multi-line paste", "confirm_multiline_paste");
    toggle(terminal, "set-smooth-scroll", "Smooth scrolling", "smooth_scroll", "set-effects-hint");
    toggle(terminal, "set-cursor-trail", "Cursor trail", "cursor_trail", "set-effects-hint");
    const effects = el("p", "set-hint", terminal);
    effects.id = "set-effects-hint";
    effects.setAttribute("data-hint", "effects");
    effects.textContent = EFFECTS_HINT;

    const access = group("Accessibility");
    choice(access, "set-motion", "Motion", "motion").setOptions(MOTION);
    choice(access, "set-contrast", "Contrast", "contrast").setOptions(CONTRAST);

    const actions = el("div", "dlg-actions", box);
    const reload = el("button", "", actions);
    reload.type = "button";
    reload.textContent = "Reload from file";
    const bindings = el("button", "", actions);
    bindings.type = "button";
    bindings.textContent = "Key bindings";
    const close = el("button", "dlg-ok", actions);
    close.type = "button";
    close.textContent = "Close";

    // ---- showing the state -----------------------------------------------------------------------

    let themeNames = "";

    /** Show `state.settings`. A field the person is typing into is left alone until it is committed. */
    function render(): void {
      const view = state.settings;
      if (!view) {
        setText(statusLine, "The settings are not available.");
        return;
      }
      // The theme list is the core's; a name the core keeps but cannot find stays selectable as what it is.
      const known = view.themes.map((t) => t.name);
      const names = known.includes(view.settings.theme) ? known : [...known, view.settings.theme];
      if (names.join("\n") !== themeNames) {
        themeNames = names.join("\n");
        theme.setOptions(
          names.map((name) => [name, known.includes(name) ? name : `${name} (not found, aubergine is used)`] as const),
        );
      }
      for (const c of controls) {
        if (c.dirty) continue;
        c.write(view.settings[c.key]);
      }
      fitNodes(warnings, warningNodes, view.warnings.length, () => el("li"));
      view.warnings.forEach((w, i) => setText(warningNodes[i]!, w));
    }
    render();

    // ---- closing and keys ------------------------------------------------------------------------

    let stop = (): void => {};
    let pop = (): void => {};
    let done = false;
    const finish = (): void => {
      if (done) return;
      done = true;
      stop();
      pop();
      backdrop.remove();
      resolve();
    };

    stop = api.watch(render);
    close.addEventListener("click", finish);
    bindings.addEventListener("click", () => {
      api.editBindings();
      finish();
    });
    reload.addEventListener("click", () => {
      reload.disabled = true;
      void api.reload().then((reason) => {
        reload.disabled = false;
        setText(statusLine, reason ?? "Settings reloaded from the files.");
        render();
      });
    });
    backdrop.addEventListener("pointerdown", (e) => {
      if (e.target === backdrop) finish();
    });

    /** The controls Tab visits: every field and button, one radio per group (the chosen one). */
    const tabStops = (): HTMLElement[] => {
      const groups = new Set<string>();
      return Array.from(box.querySelectorAll<HTMLElement>("input, button")).filter((n) => {
        if ((n as HTMLInputElement).disabled) return false;
        if (!(n instanceof HTMLInputElement) || n.type !== "radio") return true;
        if (groups.has(n.name)) return false;
        const chosen = Array.from(box.querySelectorAll<HTMLInputElement>(`input[type="radio"]`)).find(
          (r) => r.name === n.name && r.checked,
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
      // A radio that is not the chosen one (reached with the arrows) counts as its group's stop.
      let at = active ? items.indexOf(active) : -1;
      if (at < 0 && active instanceof HTMLInputElement && active.type === "radio") {
        at = items.findIndex((n) => n instanceof HTMLInputElement && n.type === "radio" && n.name === active.name);
      }
      const next = e.shiftKey ? (at <= 0 ? items.length - 1 : at - 1) : at < 0 || at === items.length - 1 ? 0 : at + 1;
      items[next]!.focus();
    };

    pop = pushKeyHandler((e) => {
      if (e.key === "Escape") {
        e.preventDefault();
        finish();
      } else if (e.key === "Tab") {
        e.preventDefault();
        trap(e);
      } else if (e.key === "Enter" && e.target instanceof HTMLInputElement && e.target.type === "number") {
        // Enter commits a typed number; a button or a checkbox keeps its own Enter.
        e.preventDefault();
        const c = byNode.get(e.target);
        if (c) void commit(c);
      }
      // Everything else is the focused control's: the browser's default is left alone (typing, Space, arrows),
      // and nothing reaches the terminal or the shortcuts behind the dialog.
      return true;
    });

    controls[0]?.focus();
  });
}
