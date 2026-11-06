// The UI scale of this device. `ui_scale` is a setting that travels in `settings.json` to every computer, so a pinch on a phone
// must not resize a desktop: the value of a pinch is kept here, in the page's own storage (`localStorage` of the web view), never in
// the settings and never synced. While it exists it replaces the shared value on this device, on the touch layouts only; changing
// the UI scale in the preferences on this device removes it (settings-ui.ts).
//
// Pure: the storage is a parameter (`touch.ts` and `settings.ts` pass `window.localStorage`), so node tests a storage that throws.
import { FONT_SIZE_RANGE } from "./settings-model.js";
import { PINCH_RANGE, PINCH_STEP } from "./touchstate.js";

/** The one key the value lives under. */
export const DEVICE_SCALE_KEY = "hw.deviceScale";

/** The part of the Web Storage interface used here; a missing or a throwing storage is survived. */
export interface ScaleStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/** Whether `n` is a scale a pinch can give: a whole multiple of the step inside the range. */
export function isDeviceScale(n: unknown): n is number {
  return typeof n === "number" && Number.isInteger(n) && n >= PINCH_RANGE[0] && n <= PINCH_RANGE[1] && n % PINCH_STEP === 0;
}

/**
 * The stored scale, or null. Only the plain decimal text of a valid scale counts (`125`; not `125.0`, `1e3`, ` 125`, `0125` or
 * anything longer than three digits), so what the page reads is exactly what it once wrote.
 */
export function getDeviceScale(storage: ScaleStorage | null | undefined): number | null {
  if (!storage) return null;
  let text: string | null;
  try {
    text = storage.getItem(DEVICE_SCALE_KEY);
  } catch {
    return null;
  }
  if (typeof text !== "string" || !/^[1-9]\d\d?$/.test(text)) return null;
  const n = Number(text);
  return isDeviceScale(n) ? n : null;
}

/** Store `percent`, or remove the value for null. A value a pinch cannot give is not stored; a storage that throws is ignored. */
export function setDeviceScale(percent: number | null, storage: ScaleStorage | null | undefined): void {
  if (!storage) return;
  try {
    if (percent === null) storage.removeItem(DEVICE_SCALE_KEY);
    else if (isDeviceScale(percent)) storage.setItem(DEVICE_SCALE_KEY, String(percent));
  } catch {
    // private mode or a full quota: the pinch then lasts until the page is closed
  }
}

/** The UI scale in force: the device's on a touch layout when there is one, else the shared one. */
export function effectiveScale(o: { shared: number; device: number | null; touch: boolean }): number {
  return o.touch && o.device !== null ? o.device : o.shared;
}

/**
 * The terminal's font size in CSS pixels. On a touch layout the cell size follows the font size times the scale divided by 100,
 * so a pinch changes the text a person sees; held to the font range. Everywhere else it is exactly `fontPx`, whatever the scale.
 */
export function terminalFontPx(o: { fontPx: number; scale: number; touch: boolean }): number {
  if (!o.touch || !Number.isFinite(o.scale) || !Number.isFinite(o.fontPx)) return o.fontPx;
  const px = Math.round((o.fontPx * o.scale) / 100);
  return Math.max(FONT_SIZE_RANGE[0], Math.min(FONT_SIZE_RANGE[1], px));
}
