// Accessibility preferences (DESK-24). The core owns the values (D-06): `motion`, `contrast` and `cursor_blink` are
// `system` or an explicit choice. This module resolves them against what the web view can report, which is the
// three media queries `prefers-reduced-motion`, `prefers-contrast` and `forced-colors` (nothing on macOS says how
// large the system text is, so the scalable UI font is the user's `ui_scale` and the font size actions, not an OS
// signal). The result is two classes on the root element, `motion-reduce` and `contrast-high`, that the
// stylesheets key on, the effective cursor blink the scene asks for, and whether the two optional grid effects of
// DESK-28 (smooth scrolling, cursor trail) may run.
//
// The resolution functions are pure. Nothing here touches the DOM at import, so node can import the file.
import { state } from "./state.js";

export type MotionSetting = "system" | "reduce" | "full";
export type ContrastSetting = "system" | "normal" | "high";
export type BlinkSetting = "system" | "on" | "off";

/** The root classes the stylesheets use: transitions live under `:root:not(.motion-reduce)`. */
export const MOTION_CLASS = "motion-reduce";
export const CONTRAST_CLASS = "contrast-high";

// ---- the rules ------------------------------------------------------------------------------------

/**
 * Motion in effect: an explicit choice stands for itself, `system` (and any value this build does not know) follows
 * the OS request for reduced motion.
 */
export function effectiveMotion(setting: MotionSetting, osReduce: boolean): "reduce" | "full" {
  if (setting === "reduce") return "reduce";
  if (setting === "full") return "full";
  return osReduce ? "reduce" : "full";
}

/**
 * Contrast in effect: an explicit choice stands for itself, `system` is high when the OS asks for more contrast or
 * forces its colours (a forced palette is the strongest request for it).
 */
export function effectiveContrast(
  setting: ContrastSetting, osMore: boolean, forcedColors = false,
): "high" | "normal" {
  if (setting === "high") return "high";
  if (setting === "normal") return "normal";
  return osMore || forcedColors ? "high" : "normal";
}

/**
 * Whether a cursor that the engine asked to blink does blink. A cursor the program made steady never does; `off`
 * never does; `on` is the user's choice and blinks even under reduced motion; `system` blinks unless motion is
 * reduced.
 */
export function effectiveBlink(setting: BlinkSetting, motionReduced: boolean, engineBlinks: boolean): boolean {
  if (!engineBlinks) return false;
  if (setting === "off") return false;
  if (setting === "on") return true;
  return !motionReduced;
}

/**
 * Whether an optional grid effect (smooth scrolling, the cursor trail) runs: the user turned it on and motion is not
 * reduced. Reduced motion wins over the setting, from the OS (`motion: "system"`) or from the app (`motion: "reduce"`).
 */
export function effectiveEffect(setting: boolean, motionReduced: boolean): boolean {
  return setting && !motionReduced;
}

/** The text size the OS asks for, in percent, as the shell reads it: 100 (the default, also macOS) up to 300. */
export const OS_TEXT_RANGE: readonly [number, number] = [100, 300];

/** The range the effective UI scale is held to, in percent: the user's 75 to 200 times the OS factor, never beyond. */
export const EFFECTIVE_SCALE_RANGE: readonly [number, number] = [75, 300];

/**
 * The OS text size the shell answered, rounded to a whole percent. An answer that is not a finite number in 100 to
 * 300 (nothing, null, text, a value the shell should have clamped) is read as 100: no OS preference.
 */
export function osTextScaleOf(answer: unknown): number {
  if (typeof answer !== "number" || !Number.isFinite(answer)) return OS_TEXT_RANGE[0];
  const whole = Math.round(answer);
  return whole >= OS_TEXT_RANGE[0] && whole <= OS_TEXT_RANGE[1] ? whole : OS_TEXT_RANGE[0];
}

/**
 * The UI scale in effect, in percent: the user's `ui_scale` times the OS text size, rounded and held to 75 to 300.
 * Only the chrome's root font size uses it; the terminal's `font_size` never sees the OS value. An OS value of 100,
 * or one that is missing or invalid, leaves the user's value as it is.
 */
export function effectiveUiScale(userPercent: number, osPercent?: unknown): number {
  const scaled = Math.round((userPercent * osTextScaleOf(osPercent)) / 100);
  return Math.min(EFFECTIVE_SCALE_RANGE[1], Math.max(EFFECTIVE_SCALE_RANGE[0], scaled));
}

// ---- measured contrast ----------------------------------------------------------------------------

/** WCAG relative luminance of 0xRRGGBB. */
function luminance(rgb: number): number {
  const channel = (v: number): number => {
    const s = v / 255;
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel((rgb >> 16) & 255) + 0.7152 * channel((rgb >> 8) & 255) + 0.0722 * channel(rgb & 255);
}

/** The WCAG contrast ratio of two colours (0xRRGGBB), 1 to 21, whichever is the lighter. */
export function contrastRatio(a: number, b: number): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x) as [number, number];
  return (hi + 0.05) / (lo + 0.05);
}

/**
 * The text colour for a solid fill: white or black, whichever measures higher against it (white on a tie). One of
 * the two is at least 4.58:1 on every colour, so text on a fill from any theme, a user's included, reaches 4.5:1
 * without a threshold being weakened.
 */
export function onAccentOf(fill: number): number {
  return contrastRatio(0xffffff, fill) >= contrastRatio(0x000000, fill) ? 0xffffff : 0x000000;
}

// ---- what is in force ------------------------------------------------------------------------------

/** What the page resolved: the classes on the root, the blink setting and the two grid effects the scene reads. */
export interface Preferences {
  motionReduced: boolean;
  contrastHigh: boolean;
  blink: BlinkSetting;
  /** Smooth scrolling is on and allowed (`smooth_scroll` and not reduced motion). */
  smoothScroll: boolean;
  /** The cursor trail is on and allowed (`cursor_trail` and not reduced motion). */
  cursorTrail: boolean;
}

let current: Preferences = {
  motionReduced: false, contrastHigh: false, blink: "system", smoothScroll: false, cursorTrail: false,
};
const listeners = new Set<() => void>();
let attached: MediaQueryList[] = [];

const QUERIES = {
  reduce: "(prefers-reduced-motion: reduce)",
  more: "(prefers-contrast: more)",
  forced: "(forced-colors: active)",
} as const;

/** A media query's answer now; a page without `matchMedia` has no OS preference. */
function os(query: string): boolean {
  return typeof window.matchMedia === "function" && window.matchMedia(query).matches;
}

/** The preferences in force now. The object is replaced when something changes, so compare by value. */
export function preferences(): Preferences {
  return current;
}

/** Whether chrome transitions are off. */
export const motionReduced = (): boolean => current.motionReduced;

/** Whether smooth scrolling may run now: the setting is on and motion is not reduced. */
export const smoothScrollOn = (): boolean => current.smoothScroll;

/** Whether the cursor trail may run now: the setting is on and motion is not reduced. */
export const cursorTrailOn = (): boolean => current.cursorTrail;

/** The cursor blink setting the core holds (`system` until it said). */
export const blinkSetting = (): BlinkSetting => current.blink;

/**
 * Resolve the settings of `state.settings` against the OS now: put the two classes on the root and tell the
 * listeners when anything changed. Cheap, so every settings view and every media query change calls it.
 */
export function refresh(): void {
  const s = state.settings?.settings;
  const reduced = effectiveMotion((s?.motion ?? "system") as MotionSetting, os(QUERIES.reduce)) === "reduce";
  const next: Preferences = {
    motionReduced: reduced,
    contrastHigh:
      effectiveContrast((s?.contrast ?? "system") as ContrastSetting, os(QUERIES.more), os(QUERIES.forced)) === "high",
    blink: (s?.cursor_blink ?? "system") as BlinkSetting,
    smoothScroll: effectiveEffect(s?.smooth_scroll === true, reduced),
    cursorTrail: effectiveEffect(s?.cursor_trail === true, reduced),
  };
  const root = document.documentElement;
  root.classList.toggle(MOTION_CLASS, next.motionReduced);
  root.classList.toggle(CONTRAST_CLASS, next.contrastHigh);
  const changed =
    next.motionReduced !== current.motionReduced ||
    next.contrastHigh !== current.contrastHigh ||
    next.blink !== current.blink ||
    next.smoothScroll !== current.smoothScroll ||
    next.cursorTrail !== current.cursorTrail;
  current = next;
  if (changed) for (const fn of [...listeners]) fn();
}

/**
 * Follow the OS: resolve now and again whenever one of the three media queries changes. Call once at start-up;
 * a second call changes nothing. The returned function detaches the listeners (tests).
 */
export function watch(): () => void {
  if (attached.length === 0 && typeof window.matchMedia === "function") {
    attached = Object.values(QUERIES).map((q) => window.matchMedia(q));
    for (const list of attached) list.addEventListener("change", refresh);
  }
  refresh();
  return () => {
    for (const list of attached) list.removeEventListener("change", refresh);
    attached = [];
  };
}

/** Call `fn` when the motion, the contrast, the blink or an effect in force changed; the returned function stops it. */
export function onPreferenceChange(fn: () => void): () => void {
  listeners.add(fn);
  return () => void listeners.delete(fn);
}
