// The pure half of the group `Window frame` of the Settings dialog (docs/design.md section 6.33): how the answer of the two
// commands `window_frame_status` and `window_frame_set` is read, the page's own word tables, and the rows and sentences the
// group draws. No DOM, no command call, no timer, and nothing is imported. The shell's words are keys that select among the
// page's own labels and are never shown as they are.
//
// The group exists on Windows and Linux in the desktop app. Where the shell says `applies: false`, and on every other system,
// the page draws nothing at all.

/** The words of the first row, spelled as the shell spells them: the contract between the page and the host. */
export type FrameWord = "auto" | "own" | "system";
/** The words of the second row (Linux only). */
export type ButtonsWord = "auto" | "flat" | "round";

/** What the shell said: whether the group applies, whether the stored choice waits for the next start, and the two words. */
export interface FrameView {
  applies: boolean;
  /** The stored choice differs from the one the open windows were built with. */
  pending: boolean;
  frame: FrameWord;
  buttons: ButtonsWord;
}

/** A value and its label, as the dialog's other choice tables write them. */
export type FrameOption = readonly [value: string, label: string];

/** The choices of the row `Frame`. */
export const FRAME_OPTIONS: readonly FrameOption[] = [["auto", "Auto"], ["own", "Own"], ["system", "System"]];
/** The choices of the row `Buttons`. */
export const BUTTON_OPTIONS: readonly FrameOption[] = [["auto", "Auto"], ["flat", "Flat"], ["round", "Round"]];

/** An own member of `object`, never an inherited one. */
function own(object: object, key: string): unknown {
  return Object.hasOwn(object, key) ? (object as Record<string, unknown>)[key] : undefined;
}

/** `word` when it is exactly the value of an option, else `auto`: the result can be used as a table key. */
function known<T extends string>(table: readonly FrameOption[], word: unknown): T {
  return (table.find(([value]) => value === word)?.[0] ?? "auto") as T;
}

/**
 * Read the answer of `window_frame_status` or `window_frame_set`: a plain object whose own members `applies` and `pending` are
 * booleans. The four members are named one by one and nothing is copied by the name it came with, so a member a newer shell adds
 * and an own `__proto__` never reach the page. A word that is not exactly one of its table (another case, a trailing blank,
 * markup, a number, null, a missing member) reads as `auto` for that member and the view is kept. Anything that is not an
 * object, an array, or an object without the two booleans gives null.
 */
export function readFrameView(raw: unknown): FrameView | null {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return null;
  const applies = own(raw, "applies");
  const pending = own(raw, "pending");
  if (typeof applies !== "boolean" || typeof pending !== "boolean") return null;
  return {
    applies,
    pending,
    frame: known<FrameWord>(FRAME_OPTIONS, own(raw, "frame")),
    buttons: known<ButtonsWord>(BUTTON_OPTIONS, own(raw, "buttons")),
  };
}

// ---- the page's own words (docs/design.md section 6.33) ------------------------------------------------------------------

/** The group `Window frame`: the legend, the rows, the dim metas, the advice, the status sentence and the line of a failure. */
export const GROUP = {
  legend: "Window frame",
  frameName: "Frame",
  frameMeta: "Windows and Linux",
  buttonsName: "Buttons",
  buttonsMeta: "Linux",
  adviceFrame: "Auto draws holzWave's own bar as the window bar. System keeps the system's title bar above holzWave's bar.",
  /** Linux only, after the first sentence. The page cannot know the session type, so it is said on every Linux. */
  adviceWayland:
    "Under Wayland the own frame has not been tried; if the window cannot be moved or resized, choose System in this dialog (open it from the palette).",
  adviceButtons: "Auto follows the desktop: round on GNOME, flat on every other desktop.",
  status: "Applies after the next start.",
  /** The line when a command failed with no reason to show (the one sentence of the dialog's groups). */
  failed: "The command failed.",
} as const;

/** The row to draw: which setting it is, its name and dim meta, the choices, the one selected, and the advice under it. */
export interface FrameRow {
  key: "frame" | "buttons";
  name: string;
  meta: string;
  options: FrameOption[];
  selected: string;
  advice: string[];
}

/** The group to draw: the legend, the rows in order and the status sentence (empty when nothing waits). */
export interface FrameDescription {
  legend: string;
  rows: FrameRow[];
  status: string;
}

/**
 * The group for `view` on `os`, or null when it is not built: any system but `windows` and `linux`, a view that does not
 * apply, no view. Windows has the row `Frame` with one advice sentence; Linux has the Wayland sentence after it and the row
 * `Buttons`. Every table in the result is a copy.
 */
export function describeFrameGroup(view: FrameView | null, os: unknown): FrameDescription | null {
  if (view === null || !view.applies || (os !== "windows" && os !== "linux")) return null;
  const linux = os === "linux";
  const rows: FrameRow[] = [
    {
      key: "frame",
      name: GROUP.frameName,
      meta: GROUP.frameMeta,
      options: FRAME_OPTIONS.map(([value, label]) => [value, label] as const) as FrameOption[],
      selected: view.frame,
      advice: linux ? [GROUP.adviceFrame, GROUP.adviceWayland] : [GROUP.adviceFrame],
    },
  ];
  if (linux) {
    rows.push({
      key: "buttons",
      name: GROUP.buttonsName,
      meta: GROUP.buttonsMeta,
      options: BUTTON_OPTIONS.map(([value, label]) => [value, label] as const) as FrameOption[],
      selected: view.buttons,
      advice: [GROUP.adviceButtons],
    });
  }
  return { legend: GROUP.legend, rows, status: view.pending ? GROUP.status : "" };
}
