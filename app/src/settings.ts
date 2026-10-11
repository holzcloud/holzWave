// The settings of the page: the core's last view (`state.settings`), the theme as CSS custom properties and the
// renderer's cursor colour, and the actions that ask the core to change something. The core owns every value
// (D-06): the page sends `set_setting` and then shows what the core answered, never what it asked for. The pure
// rules (colour validation, reading the view) are in settings-model.ts, which node can import.
import { refresh as refreshPreferences, watch as watchPreferences } from "./a11y.js";
import { registerAction, replaceAction, unregisterAction } from "./actions.js";
import { call, listen } from "./bridge.js";
import { CURSOR_COLOR, FONT_PX, setFontPx } from "./frame.js";
import { openKeybindings } from "./keybindings-ui.js";
import { focusInput, setBindings, setPasteConfirm } from "./keyboard.js";
import {
  DEFAULT_FONT_SIZE, colorSchemeOf, cursorColorOf, fontStep, lookOf, readSettingsView, rootFontPx, tokenEntries,
} from "./settings-model.js";
import type { Look, SettingsView, ThemeInfo } from "./settings-model.js";
import { openSettings } from "./settings-ui.js";
import { state } from "./state.js";

export { parseHex, tokenEntries } from "./settings-model.js";

/** What the settings need from the scene, which owns the renderer and the draw loop (injected: no import cycle). */
export interface SettingsHost {
  /** The renderer draws the cursor in this colour (0xRRGGBB) from the next frame. */
  setCursorColor(rgb: number): void;
  /** Ask for a draw on the next animation frame. */
  schedule(): void;
  /** The font size or the root font size changed: measure again, tell the renderer and send `resize` to the panes. */
  refit(): void;
}

let host: SettingsHost | null = null;

/** The first views arrive before the scene has measured: only a later change needs a refit. */
let live = false;

// ---- the theme -----------------------------------------------------------------------------------

/** The custom properties set on the root by the last theme, so a token a later theme lacks is not left behind. */
const applied = new Set<string>();

/**
 * Make `view.theme` the look of the page: the chrome tokens as custom properties on the root, the colour scheme of
 * form controls, and the cursor colour of the renderer. Values are set one property at a time through the CSSOM
 * and only after validation (`tokenEntries`), never as text of a style. A token the theme does not carry validly
 * is removed, so the stylesheet's built-in value applies instead of a value of the previous theme.
 */
export function applyTheme(view: SettingsView): void {
  const root = document.documentElement;
  const next = new Set<string>();
  for (const [name, value] of tokenEntries(view.theme)) {
    root.style.setProperty(name, value);
    next.add(name);
  }
  const scheme = colorSchemeOf(view.theme);
  if (scheme) {
    root.style.setProperty("color-scheme", scheme);
    next.add("color-scheme");
  }
  for (const name of applied) if (!next.has(name)) root.style.removeProperty(name);
  applied.clear();
  for (const name of next) applied.add(name);

  host?.setCursorColor(cursorColorOf(view.theme) ?? CURSOR_COLOR);
  host?.schedule();
}

// ---- font size and UI scale ----------------------------------------------------------------------

/** The look the metrics were last taken for. */
let look: Look = { fontPx: FONT_PX, uiScale: 100 };

/**
 * Follow the font size and the UI scale of the view. The UI scale is the root font size (12 px at 100 %): every
 * chrome size is in rem and the pane header is derived from it, so one change moves chrome, headers and grid
 * together. A value that is not in the core's range keeps the previous one. A change after start-up asks the scene
 * to measure again, which sends `resize` to every visible pane.
 */
function applyLook(view: SettingsView): void {
  const next = lookOf(view.settings, look);
  const fontChanged = next.fontPx !== look.fontPx;
  const scaleChanged = next.uiScale !== look.uiScale;
  if (!fontChanged && !scaleChanged) return;
  look = next;
  if (fontChanged) setFontPx(next.fontPx);
  if (scaleChanged) {
    const root = document.documentElement;
    if (next.uiScale === 100) root.style.removeProperty("font-size"); // the stylesheet's 12 px
    else root.style.setProperty("font-size", `${rootFontPx(next.uiScale)}px`);
  }
  if (live) host?.refit();
}

// ---- the actions ---------------------------------------------------------------------------------

/** The ids of the theme actions registered now, in the core's order. */
let themeIds: string[] = [];

const themeActionId = (name: string): string => `theme.set.${name}`;

/** One "Theme: <name>" action per listed theme; the set follows the list the core sends. */
function syncThemeActions(themes: ThemeInfo[]): void {
  const wanted = themes.map((t) => themeActionId(t.name));
  if (wanted.length === themeIds.length && wanted.every((id, i) => id === themeIds[i])) return;
  for (const id of themeIds) if (!wanted.includes(id)) unregisterAction(id);
  for (const t of themes) {
    replaceAction({
      id: themeActionId(t.name),
      title: `Theme: ${t.name}`,
      run: () => void change("theme", () => t.name),
    });
  }
  themeIds = wanted;
}

// ---- talking to the core -------------------------------------------------------------------------

/** The JSON of the view in force: a view the core repeats (a reload that found nothing) changes nothing. */
let lastJson = "";

/** What follows a new view besides the page itself: the settings dialog re-renders from the state. */
const watchers = new Set<() => void>();

/** Take a view the core sent: keep it in the state and make everything that follows from it current. */
function accept(raw: unknown): SettingsView | null {
  const view = readSettingsView(raw);
  if (!view) return null;
  const json = JSON.stringify(view);
  if (json === lastJson) return view;
  lastJson = json;
  state.settings = view;
  applyTheme(view);
  applyLook(view);
  refreshPreferences(); // motion, contrast and cursor blink: the setting against what the OS says
  setPasteConfirm(view.settings.confirm_multiline_paste);
  setBindings(view.keybindings); // shortcut dispatch follows the core's resolved table (T-02-72)
  syncThemeActions(view.themes);
  for (const fn of [...watchers]) fn();
  return view;
}

/** Changes go out one after the other, so a second one reads the state the first one produced. */
let queue: Promise<unknown> = Promise.resolve();

/**
 * Ask the core to set `key` to `value()` (read when the request is sent, after earlier requests were answered).
 * Resolves to null on success and to the core's reason on failure; it never rejects. The view in the answer is
 * what the page then shows: a number the core clamped is shown clamped, a refused value changes nothing.
 */
export function change(key: string, value: () => unknown): Promise<string | null> {
  const run = queue.then(async () => {
    try {
      accept(await call<unknown>("set_setting", { key, value: value() }));
      return null;
    } catch (e) {
      return typeof e === "string" ? e : e instanceof Error ? e.message : "the setting could not be changed";
    }
  });
  queue = run;
  return run;
}

/** Ask the core to look at the settings and theme files now. Resolves like `change`; a view that changed is applied. */
export async function reload(): Promise<string | null> {
  try {
    accept(await call<unknown>("reload_settings"));
    return null;
  } catch (e) {
    return typeof e === "string" ? e : e instanceof Error ? e.message : "the settings could not be reloaded";
  }
}

/** Window focus reloads, at most once a second: the core compares modification times before it reads anything. */
const RELOAD_MIN_MS = 1000;
let lastReload = Number.NEGATIVE_INFINITY;

function reloadOnFocus(): void {
  const now = performance.now();
  if (now - lastReload < RELOAD_MIN_MS) return;
  lastReload = now;
  void reload();
}

/** Call `fn` after every accepted view; the returned function stops it. */
function watch(fn: () => void): () => void {
  watchers.add(fn);
  return () => void watchers.delete(fn);
}

/** Open the key binding editor and give the keyboard back to the terminal when it closes. */
function openBindings(): void {
  void openKeybindings({ change, watch }).then(focusInput);
}

/**
 * Open the dialog and give the keyboard back to the terminal when it closes, unless the person asked for the
 * key binding editor from it: the editor takes over then and returns the keyboard when it closes.
 */
function openDialog(): void {
  let next = false;
  void openSettings({
    change,
    reload,
    watch,
    editBindings: () => {
      next = true;
    },
  }).then(() => {
    if (next) openBindings();
    else focusInput();
  });
}

/** The size now, for the actions that step it; NaN when the core has not said (`fontStep` then starts at 14). */
const currentFont = (): number => state.settings?.settings.font_size ?? Number.NaN;

function registerSettingsActions(): void {
  registerAction({
    id: "font.increase",
    title: "Increase font size",
    run: () => void change("font_size", () => fontStep(currentFont(), 1)),
  });
  registerAction({
    id: "font.decrease",
    title: "Decrease font size",
    run: () => void change("font_size", () => fontStep(currentFont(), -1)),
  });
  registerAction({
    id: "font.reset",
    title: "Reset font size",
    run: () => void change("font_size", () => DEFAULT_FONT_SIZE),
  });
  registerAction({ id: "settings.open", title: "Open settings", run: openDialog });
  registerAction({ id: "keybindings.open", title: "Edit key bindings", run: openBindings });
}

/**
 * The theme and the settings: register the actions, get the core's view once, then follow its `settings` events and
 * the window focus. Call before the first measurement, so the font size and the UI scale are in force when it runs.
 */
export async function initSettings(h: SettingsHost): Promise<void> {
  host = h;
  registerSettingsActions();
  watchPreferences(); // before the first answer: a page whose core cannot answer still follows the OS
  await listen<unknown>("settings", (raw) => void accept(raw));
  try {
    accept(await call<unknown>("get_settings"));
  } catch {
    // A core that cannot answer leaves the built-in look and the default sizes; the page still works.
  }
  live = true;
  window.addEventListener("focus", reloadOnFocus);
}
