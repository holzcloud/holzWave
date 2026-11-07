// The pure model of the file hand-over page (docs/design.md section 6.6, rule File hand-over): the words, the size as a person
// reads it, the host's answer read defensively and the one line each answer gives. No DOM, so node can import it.
//
// The host answers one of five kinds (`done`, `cancelled`, `too_large`, `failed`, `unavailable`) with a cleaned file name and a
// size and never a sentence of its own; every sentence of the page is made here from the constants below. A name is another app's
// text: it is cut to 255 characters before anything shows it, and the page puts it in with `textContent` only.

/** The largest file the host hands over, in the unit the spec's lines use (the host's limit is 256 MiB). */
export const LIMIT_MB = 256;

/** The most characters of a file name the page ever shows (the host cleans it to 255 bytes; this guards against a host that did not). */
export const MAX_NAME_CHARS = 255;

/** The most characters of a place the page ever shows. */
const MAX_SHOWN_CHARS = 512;

/** Every fixed text of the hand-over, one table. A guard test ties each value to the spec. */
export const STRINGS = {
  /** The caption of the group in the sheet, and the section of the two palette entries (the id's section, upper case in the palette). */
  files: "Files",
  copyIn: "Copy a file into the shell folder",
  copyOut: "Copy a file out of the shell folder",
  /** The label of the one-line box that asks which file to copy out. */
  askPath: "File to copy out, from the shell folder",
  /** The one plain line of a copy that did not happen, whatever the reason. */
  failed: "Could not copy the file",
  /** The name the page shows for a name it cannot read. */
  unknownSize: "unknown size",
} as const;

/** The mark before a refusal or a failure: a second cue besides the colour and the underline. */
export const WARN_MARK = "!";

/** The ids of the two actions: the section `files` is the group of the palette. */
export const ACTION_IN = "files.copy_in";
export const ACTION_OUT = "files.copy_out";

/** Which way a file went. */
export type Direction = "in" | "out";

/** What the host answers, as the page reads it. */
export type Outcome =
  | { kind: "done"; name: string; size: number; shown: string; renamed: boolean }
  | { kind: "cancelled" }
  | { kind: "too_large"; name: string; size: number | null }
  | { kind: "failed" }
  | { kind: "unavailable" };

const UNITS = ["B", "kB", "MB", "GB", "TB"] as const;

/**
 * A size as a person reads it, in decimal units as the spec's lines are (`1.2 MB`, `612 MB`, `900 B`, `1.5 kB`): one decimal
 * below 10 and none above, no `.0`, and cut down and not rounded up, so a size is never shown larger than it is. A value that is not
 * a size (negative, `NaN`, infinite, beyond what a number holds exactly) gives a plain word and never `undefined`.
 */
export function formatSize(bytes: number): string {
  if (typeof bytes !== "number" || !Number.isFinite(bytes) || bytes < 0) return STRINGS.unknownSize;
  if (bytes > Number.MAX_SAFE_INTEGER) return STRINGS.unknownSize;
  const whole = Math.floor(bytes);
  if (whole < 1000) return `${whole} B`;
  let unit = 1;
  let step = 1000;
  while (unit < UNITS.length - 1 && whole >= step * 1000) {
    unit += 1;
    step *= 1000;
  }
  // Tenths of the unit, by integer division, so no rounding of a fraction ever moves a digit.
  const tenths = Math.floor(whole / (step / 10));
  const text = tenths < 100 ? `${Math.floor(tenths / 10)}.${tenths % 10}` : String(Math.floor(tenths / 10));
  return `${text.endsWith(".0") ? text.slice(0, -2) : text} ${UNITS[unit]}`;
}

/** At most `max` characters of `text`, cut between characters (never inside a surrogate pair). */
function capped(text: string, max: number): string {
  const chars = [...text];
  return chars.length <= max ? text : chars.slice(0, max).join("");
}

/**
 * Read the host's answer. The kinds are exactly the host's; an unknown kind, a missing field or a field of the wrong type is
 * `null`, and so is anything that throws when it is read. A name is cut to 255 characters and a place to 512. Fields the page
 * does not know are ignored.
 */
export function parseOutcome(raw: unknown): Outcome | null {
  try {
    return readOutcome(raw);
  } catch {
    return null; // an object with a getter that throws is no answer of the host
  }
}

function readOutcome(raw: unknown): Outcome | null {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return null;
  const o = raw as Record<string, unknown>;
  const text = (v: unknown): v is string => typeof v === "string";
  const count = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v) && v >= 0;
  switch (o.kind) {
    case "cancelled":
    case "failed":
    case "unavailable":
      return { kind: o.kind };
    case "done":
      if (!text(o.name) || !count(o.size) || !text(o.shown) || typeof o.renamed !== "boolean") return null;
      return { kind: "done", name: capped(o.name, MAX_NAME_CHARS), size: o.size, shown: capped(o.shown, MAX_SHOWN_CHARS), renamed: o.renamed };
    case "too_large":
      if (!text(o.name) || !(o.size === null || o.size === undefined || count(o.size))) return null;
      return { kind: "too_large", name: capped(o.name, MAX_NAME_CHARS), size: o.size ?? null };
    default:
      return null;
  }
}

/** The tone of a line: `warn` for a refusal or a failure (a `!` mark and an underline besides the colour), `plain` otherwise. */
export type Tone = "plain" | "warn";

/** A line of the notice style. */
export interface Line {
  text: string;
  tone: Tone;
}

/**
 * The line an answer gives, or `null` for a cancelled dialog, which changes nothing and says nothing. A copy in names the place it
 * went to (`~/handover/report-2.pdf` when the name was taken, so the number is told by the place); a copy out ends in the word `out`.
 */
export function messageFor(outcome: Outcome, direction: Direction): Line | null {
  switch (outcome.kind) {
    case "done":
      return {
        tone: "plain",
        text:
          direction === "in"
            ? `Copied ${outcome.name} (${formatSize(outcome.size)}) to ${outcome.shown}`
            : `Copied ${outcome.name} (${formatSize(outcome.size)}) out`,
      };
    case "too_large":
      return {
        tone: "warn",
        text:
          outcome.size === null
            ? `${outcome.name} is larger than ${LIMIT_MB} MB`
            : `${outcome.name} is ${formatSize(outcome.size)}; the limit is ${LIMIT_MB} MB`,
      };
    case "cancelled":
      return null;
    default:
      return { tone: "warn", text: STRINGS.failed };
  }
}
