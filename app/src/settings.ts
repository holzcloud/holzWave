// The settings of the page: the core's last view (`state.settings`), the theme as CSS custom properties and the
// renderer's cursor colour, and the actions that ask the core to change something. The core owns every value
// (D-06): the page sends `set_setting` and then shows what the core answered, never what it asked for. The pure
// rules (colour validation, reading the view) are in settings-model.ts, which node can import.
import { replaceAction, unregisterAction } from "./actions.js";
import { call, listen } from "./bridge.js";
import { CURSOR_COLOR } from "./frame.js";
import { colorSchemeOf, cursorColorOf, readSettingsView, tokenEntries } from "./settings-model.js";
import type { SettingsView, ThemeInfo } from "./settings-model.js";
import { state } from "./state.js";

export { parseHex, tokenEntries } from "./settings-model.js";

/** What the settings need from the scene, which owns the renderer and the draw loop (injected: no import cycle). */
export interface SettingsHost {
  /** The renderer draws the cursor in this colour (0xRRGGBB) from the next frame. */
  setCursorColor(rgb: number): void;
  /** Ask for a draw on the next animation frame. */
  schedule(): void;
}

let host: SettingsHost | null = null;

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

/** Take a view the core sent: keep it in the state and make everything that follows from it current. */
function accept(raw: unknown): SettingsView | null {
  const view = readSettingsView(raw);
  if (!view) return null;
  const json = JSON.stringify(view);
  if (json === lastJson) return view;
  lastJson = json;
  state.settings = view;
  applyTheme(view);
  syncThemeActions(view.themes);
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

/** The theme and the settings: get the core's view once at start-up, then follow its `settings` events. */
export async function initSettings(h: SettingsHost): Promise<void> {
  host = h;
  await listen<unknown>("settings", (raw) => void accept(raw));
  try {
    accept(await call<unknown>("get_settings"));
  } catch {
    // A core that cannot answer leaves the built-in look and the default sizes; the page still works.
  }
}
