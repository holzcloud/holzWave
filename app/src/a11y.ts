// Accessibility preferences (DESK-24). The core owns the values (D-06): `motion`, `contrast` and `cursor_blink` are
// `system` or an explicit choice. This module resolves them against what the web view can report, which is the
// three media queries `prefers-reduced-motion`, `prefers-contrast` and `forced-colors` (nothing on macOS says how
// large the system text is, so the scalable UI font is the user's `ui_scale` and the font size actions, not an OS
// signal). The result is two classes on the root element, `motion-reduce` and `contrast-high`, that the
// stylesheets key on, and the effective cursor blink the scene asks for.
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

/** What the page resolved: the classes on the root and the blink setting the scene reads. */
export interface Preferences {
  motionReduced: boolean;
  contrastHigh: boolean;
  blink: BlinkSetting;
}

let current: Preferences = { motionReduced: false, contrastHigh: false, blink: "system" };
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

/** The cursor blink setting the core holds (`system` until it said). */
export const blinkSetting = (): BlinkSetting => current.blink;

/**
 * Resolve the settings of `state.settings` against the OS now: put the two classes on the root and tell the
 * listeners when anything changed. Cheap, so every settings view and every media query change calls it.
 */
export function refresh(): void {
  const s = state.settings?.settings;
  const next: Preferences = {
    motionReduced: effectiveMotion((s?.motion ?? "system") as MotionSetting, os(QUERIES.reduce)) === "reduce",
    contrastHigh:
      effectiveContrast((s?.contrast ?? "system") as ContrastSetting, os(QUERIES.more), os(QUERIES.forced)) === "high",
    blink: (s?.cursor_blink ?? "system") as BlinkSetting,
  };
  const root = document.documentElement;
  root.classList.toggle(MOTION_CLASS, next.motionReduced);
  root.classList.toggle(CONTRAST_CLASS, next.contrastHigh);
  const changed =
    next.motionReduced !== current.motionReduced ||
    next.contrastHigh !== current.contrastHigh ||
    next.blink !== current.blink;
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

/** Call `fn` when the motion, the contrast or the blink setting in force changed; the returned function stops it. */
export function onPreferenceChange(fn: () => void): () => void {
  listeners.add(fn);
  return () => void listeners.delete(fn);
}
