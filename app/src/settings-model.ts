// The settings the page mirrors and the pure rules around them: reading the core's view, the theme as CSS custom
// property values, the ranges. No DOM and no bridge, so node can import it (the stateful half is settings.ts).
// The core owns every value (D-06): nothing here decides what a setting is, it only refuses what is not shaped
// like one, so that a value of the wrong type or a colour that is not #rrggbb never reaches a style.
import { onAccentOf } from "./a11y.js";

/** The view the core sends: `get_settings`, `set_setting`, `reset_setting`, `reload_settings` and the `settings` event. */
export interface SettingsView {
  settings: SettingsValues;
  themes: ThemeInfo[];
  theme: ThemeView;
  /** The core's resolved key binding table, one row per action in the editor's order; empty when it sent none. */
  keybindings: BindingRow[];
  warnings: string[];
}

/** One row of the key binding table: the chord in force in the core's canonical spelling, null when unbound. */
export interface BindingRow {
  action: string;
  title: string;
  chord: string | null;
  is_default: boolean;
}

/** The settings of the dialog. */
export interface SettingsValues {
  theme: string;
  font_size: number;
  ui_scale: number;
  option_as_alt: boolean;
  cursor_blink: "system" | "on" | "off";
  motion: "system" | "reduce" | "full";
  contrast: "system" | "normal" | "high";
  scrollback_lines: number;
  confirm_multiline_paste: boolean;
  /** Copy the selection to the system clipboard when a mouse selection ends. Read leniently: anything but `false` is on. */
  copy_on_select: boolean;
  /** Ease a scrollback jump (DESK-28); off while motion is reduced whatever this says. */
  smooth_scroll: boolean;
  /** A fading trail behind a cursor that jumps (DESK-28); off while motion is reduced whatever this says. */
  cursor_trail: boolean;
  /** AI providers: refuse every request whose destination is not this computer. Read leniently: anything but `true` is false. */
  ai_local_only: boolean;
  /** AI providers: whether a model on this computer asks before it sees terminal text. Anything but `auto` is `ask`. */
  ai_local_context: "ask" | "auto";
  /** The overrides as the user typed them: action id to chord text, the empty text meaning unbound. */
  keybindings: Record<string, string>;
}

export interface ThemeInfo {
  name: string;
  kind: "dark" | "light";
  builtin: boolean;
}

/** The active theme as the core sends it. Its colours are strings: they are validated before they reach CSS. */
export interface ThemeView {
  name: string;
  kind: string;
  terminal: Record<string, unknown>;
  chrome: Record<string, unknown>;
}

/** The most binding rows the page keeps: the core's table has a few dozen, a view with more is cut. */
const MAX_BINDING_ROWS = 256;

/** The most overrides the page keeps (the core's own bound, core/src/settings.rs). */
const MAX_OVERRIDES = 512;

/** The accepted ranges of the numeric settings (core/src/settings.rs); the core clamps, the page never sends beyond. */
export const FONT_SIZE_RANGE: readonly [number, number] = [8, 40];
export const UI_SCALE_RANGE: readonly [number, number] = [75, 200];
export const SCROLLBACK_RANGE: readonly [number, number] = [100, 100_000];

/** The font size in CSS pixels at `font.reset` (the core's default). */
export const DEFAULT_FONT_SIZE = 14;

/** The root font size in CSS pixels at UI scale 100 %. */
export const BASE_ROOT_PX = 12;

/** The root font size for a UI scale in percent: every chrome size is in rem, so this scales the chrome. */
export function rootFontPx(uiScale: number): number {
  return (BASE_ROOT_PX * uiScale) / 100;
}

// ---- colours -------------------------------------------------------------------------------------

const HEX = /^#[0-9a-fA-F]{6}$/;

/** `#rrggbb` (either case) as 0xRRGGBB; null for anything else, including shorthand, alpha and trailing text. */
export function parseHex(value: unknown): number | null {
  if (typeof value !== "string" || !HEX.test(value)) return null;
  return Number.parseInt(value.slice(1), 16);
}

const hexOf = (rgb: number): string => `#${rgb.toString(16).padStart(6, "0")}`;
const rgbOf = (rgb: number, alpha: string): string =>
  `rgb(${(rgb >> 16) & 255} ${(rgb >> 8) & 255} ${rgb & 255} / ${alpha})`;

/** The chrome fields of a theme and the custom property each one sets (docs/design.md section 2). */
const CHROME_TOKENS: readonly (readonly [string, string])[] = [
  ["ground", "--hw-ground"],
  ["ground_2", "--hw-ground-2"],
  ["pane", "--hw-pane"],
  ["edge", "--hw-edge"],
  ["ink", "--hw-ink"],
  ["ink_2", "--hw-ink-2"],
  ["ink_3", "--hw-ink-3"],
  ["rose", "--hw-rose"],
  ["rose_deep", "--hw-rose-deep"],
  ["ok", "--hw-ok"],
  ["warn", "--hw-warn"],
  ["danger", "--hw-danger"],
  ["info", "--hw-info"],
];

/** The opacity of the wash behind the shown tab and workspace (app.css gives the same value to aubergine). */
const ROSE_WASH_ALPHA = 0.14;

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);

/** An opacity as it goes into CSS: three decimals at most, so a float's tail never reaches a style. */
const opacity = (a: number): string => String(Number(a.toFixed(3)));

/**
 * The custom properties of a theme as `[name, value]` pairs: the thirteen chrome tokens, `--hw-selection` (the
 * terminal's selection colour at its opacity), `--hw-rose-wash` (rose at the wash opacity, so a theme with
 * another rose has a matching wash) and `--hw-on-accent` (the text on the rose-deep fill, measured). A colour that
 * is not exactly `#rrggbb`, and an opacity outside 0..1, is left out: what remains is built from numbers here,
 * never copied from the theme's text.
 */
export function tokenEntries(theme: unknown): [string, string][] {
  const out: [string, string][] = [];
  if (!isObject(theme)) return out;
  const { chrome, terminal } = theme;
  if (isObject(chrome)) {
    for (const [field, name] of CHROME_TOKENS) {
      const rgb = parseHex(chrome[field]);
      if (rgb !== null) out.push([name, hexOf(rgb)]);
    }
    const rose = parseHex(chrome.rose);
    if (rose !== null) out.push(["--hw-rose-wash", rgbOf(rose, opacity(ROSE_WASH_ALPHA))]);
    // The text on a solid rose-deep fill (the shown workspace pill): white or black, whichever measures higher
    // against this theme's fill, so it reaches 4.5:1 whatever the fill is.
    const deep = parseHex(chrome.rose_deep);
    if (deep !== null) out.push(["--hw-on-accent", hexOf(onAccentOf(deep))]);
  }
  if (isObject(terminal)) {
    const rgb = parseHex(terminal.selection);
    const alpha = terminal.selection_alpha;
    if (rgb !== null && typeof alpha === "number" && Number.isFinite(alpha) && alpha >= 0 && alpha <= 1) {
      out.push(["--hw-selection", rgbOf(rgb, opacity(alpha))]);
    }
  }
  return out;
}

/** The terminal's cursor colour of a theme, or null when it is not a `#rrggbb`. */
export function cursorColorOf(theme: unknown): number | null {
  return isObject(theme) && isObject(theme.terminal) ? parseHex(theme.terminal.cursor) : null;
}

/** `dark` or `light` for the form controls and scrollbars; null for any other kind. */
export function colorSchemeOf(theme: unknown): "dark" | "light" | null {
  const kind = isObject(theme) ? theme.kind : null;
  return kind === "dark" || kind === "light" ? kind : null;
}

// ---- the view ------------------------------------------------------------------------------------

const oneOf = <T extends string>(v: unknown, allowed: readonly T[]): v is T => allowed.includes(v as T);
const finite = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);

/**
 * The view as the page keeps it, or null when it is not one: every setting of the dialog must have the type and
 * the kind the core defines, else the whole view is refused and the page keeps the one it has. Entries of the theme
 * list and the warnings that are malformed are left out one by one.
 */
export function readSettingsView(raw: unknown): SettingsView | null {
  if (!isObject(raw)) return null;
  const s = raw.settings;
  if (!isObject(s) || !isObject(raw.theme) || !Array.isArray(raw.themes)) return null;
  if (
    typeof s.theme !== "string" ||
    !finite(s.font_size) ||
    !finite(s.ui_scale) ||
    !finite(s.scrollback_lines) ||
    typeof s.option_as_alt !== "boolean" ||
    typeof s.confirm_multiline_paste !== "boolean" ||
    typeof s.smooth_scroll !== "boolean" ||
    typeof s.cursor_trail !== "boolean" ||
    !oneOf(s.cursor_blink, ["system", "on", "off"] as const) ||
    !oneOf(s.motion, ["system", "reduce", "full"] as const) ||
    !oneOf(s.contrast, ["system", "normal", "high"] as const)
  ) {
    return null;
  }
  const themes: ThemeInfo[] = [];
  for (const t of raw.themes) {
    if (!isObject(t) || typeof t.name !== "string" || t.name === "" || !oneOf(t.kind, ["dark", "light"] as const)) continue;
    themes.push({ name: t.name, kind: t.kind, builtin: t.builtin === true });
  }
  const warnings = Array.isArray(raw.warnings) ? raw.warnings.filter((w): w is string => typeof w === "string") : [];
  const keybindings = readBindingRows(raw.keybindings);
  const overrides = readOverrides(s.keybindings);
  const theme = raw.theme;
  return {
    settings: {
      theme: s.theme,
      font_size: s.font_size,
      ui_scale: s.ui_scale,
      option_as_alt: s.option_as_alt,
      cursor_blink: s.cursor_blink,
      motion: s.motion,
      contrast: s.contrast,
      scrollback_lines: s.scrollback_lines,
      confirm_multiline_paste: s.confirm_multiline_paste,
      copy_on_select: s.copy_on_select !== false,
      smooth_scroll: s.smooth_scroll,
      cursor_trail: s.cursor_trail,
      ai_local_only: s.ai_local_only === true,
      ai_local_context: s.ai_local_context === "auto" ? "auto" : "ask",
      keybindings: overrides,
    },
    themes,
    theme: {
      name: typeof theme.name === "string" ? theme.name : "",
      kind: typeof theme.kind === "string" ? theme.kind : "",
      terminal: isObject(theme.terminal) ? theme.terminal : {},
      chrome: isObject(theme.chrome) ? theme.chrome : {},
    },
    keybindings,
    warnings,
  };
}

/** The rows of the binding table: a row that is not `{action, title, chord, is_default}` or repeats an action is left out. */
function readBindingRows(raw: unknown): BindingRow[] {
  if (!Array.isArray(raw)) return [];
  const rows: BindingRow[] = [];
  const seen = new Set<string>();
  for (const r of raw) {
    if (rows.length >= MAX_BINDING_ROWS) break;
    if (!isObject(r) || typeof r.action !== "string" || r.action === "" || typeof r.title !== "string") continue;
    if (r.chord !== null && typeof r.chord !== "string") continue;
    if (typeof r.is_default !== "boolean" || seen.has(r.action)) continue;
    seen.add(r.action);
    rows.push({ action: r.action, title: r.title, chord: r.chord, is_default: r.is_default });
  }
  return rows;
}

/** The overrides map: entries whose value is text, at most as many as the core keeps. */
function readOverrides(raw: unknown): Record<string, string> {
  const out: Record<string, string> = {};
  if (!isObject(raw)) return out;
  let n = 0;
  for (const [action, chord] of Object.entries(raw)) {
    if (typeof chord !== "string" || action === "__proto__" || n >= MAX_OVERRIDES) continue;
    out[action] = chord;
    n++;
  }
  return out;
}

// ---- what a view means for the metrics and for the controls ------------------------------------------

/** The two settings that change the metrics: the terminal font size and the UI scale (percent). */
export interface Look {
  fontPx: number;
  uiScale: number;
}

/**
 * The look a view asks for. Each value stands for itself: one that is not a number, or is outside its range, keeps
 * the previous one, so a bad scale never costs a good font size. A fraction is rounded to the whole number the
 * core stores.
 */
export function lookOf(s: { font_size: unknown; ui_scale: unknown }, previous: Look): Look {
  const pick = (value: unknown, range: readonly [number, number], keep: number): number => {
    if (!finite(value)) return keep;
    const whole = Math.round(value);
    return whole >= range[0] && whole <= range[1] ? whole : keep;
  };
  return {
    fontPx: pick(s.font_size, FONT_SIZE_RANGE, previous.fontPx),
    uiScale: pick(s.ui_scale, UI_SCALE_RANGE, previous.uiScale),
  };
}

const NUMBER_TEXT = /^[+-]?(\d+\.?\d*|\.\d+)(e[+-]?\d+)?$/i;

/**
 * The whole number typed into a field, or null when the text is not a plain decimal number (no hex, no units, no
 * infinity). A fraction is rounded; the range is the core's to enforce, it clamps.
 */
export function typedInteger(text: string): number | null {
  const trimmed = text.trim();
  if (!NUMBER_TEXT.test(trimmed)) return null;
  const n = Number(trimmed);
  return Number.isFinite(n) ? Math.round(n) : null;
}

/** The size a `font.increase` or `font.decrease` asks for: the size now plus `delta`, from the default if unknown. */
export function fontStep(current: number, delta: number): number {
  return (finite(current) ? current : DEFAULT_FONT_SIZE) + delta;
}
