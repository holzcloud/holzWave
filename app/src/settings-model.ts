// The settings the page mirrors and the pure rules around them: reading the core's view, the theme as CSS custom
// property values, the ranges. No DOM and no bridge, so node can import it (the stateful half is settings.ts).

/** The view the core sends: `get_settings`, `set_setting`, `reset_setting`, `reload_settings` and the `settings` event. */
export interface SettingsView {
  settings: SettingsValues;
  themes: ThemeInfo[];
  theme: ThemeView;
  warnings: string[];
}

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
}

export interface ThemeInfo {
  name: string;
  kind: "dark" | "light";
  builtin: boolean;
}

/** The active theme as the core sends it. Its colours are strings the page validates before they reach CSS. */
export interface ThemeView {
  name: string;
  kind: string;
  terminal: Record<string, unknown>;
  chrome: Record<string, unknown>;
}

export function parseHex(_value: unknown): number | null {
  return null;
}

export function tokenEntries(_theme: unknown): [string, string][] {
  return [];
}

export function readSettingsView(_raw: unknown): SettingsView | null {
  return null;
}

export function rootFontPx(_uiScale: number): number {
  return Number.NaN;
}
