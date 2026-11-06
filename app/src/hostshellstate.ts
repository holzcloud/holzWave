// The pure half of the group `Host shell` of the Settings dialog (docs/design.md section 6.32): how the answer of the two
// commands `host_shell_status` and `host_shell_set` is read, and the page's own words for each state. No DOM, no command call,
// no timer, and nothing is imported. The shell's state words are keys that select among the page's own sentences and are
// never shown as they are.
//
// The group exists only inside a Flatpak. Where the shell says `applies: false` the page draws nothing at all.

/** The three state words of the shell: the contract between the page and the host. */
export type HostState = "off" | "on" | "unavailable";

/** What the shell said: whether the group applies (a Flatpak) and the state. */
export interface HostShellView {
  applies: boolean;
  state: HostState;
}

/** The state words, spelled as the shell spells them. */
const STATES: readonly HostState[] = ["off", "on", "unavailable"];

/** An own member of `object`, never an inherited one. */
function own(object: object, key: string): unknown {
  return Object.hasOwn(object, key) ? (object as Record<string, unknown>)[key] : undefined;
}

/**
 * Read the answer of `host_shell_status` or `host_shell_set`: a plain object whose own member `applies` is a boolean and whose
 * own member `state` is a string. The two members are named one by one and nothing is copied by the name it came with, so a
 * member a newer shell adds and an own `__proto__` never reach the page. A state word that is not exactly one of the three
 * (another case, a trailing blank, markup, a number, a missing member) reads as `unavailable`, so the result can be used as
 * a table key. Anything that is not an object, an array, or an object without a boolean `applies` and a string `state` gives
 * null.
 */
export function readHostShell(raw: unknown): HostShellView | null {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return null;
  const applies = own(raw, "applies");
  const word = own(raw, "state");
  if (typeof applies !== "boolean" || typeof word !== "string") return null;
  const state = STATES.find((known) => known === word) ?? "unavailable";
  return { applies, state };
}

// ---- the page's own words (docs/design.md section 6.32) -----------------------------------------------------------------

/** The group `Host shell`: its legend, the name of the row, the dim meta and the line of a command that failed. */
export const GROUP = {
  legend: "Host shell",
  name: "Run panes on the host",
  meta: "Flatpak",
  /** The line when a command failed with no reason to show. */
  failed: "The command failed.",
} as const;

/** How one state is shown: the word, the tone, the cues besides the colour, the advice and whether the box can be pressed. */
export interface StateWords {
  /** The state in words (the first cue, the one that never depends on colour). */
  word: string;
  tone: "warn" | "plain";
  /** The word is underlined (a state that needs attention). */
  underline: boolean;
  /** A mark before the word, or none. */
  mark: "" | "!" | "?";
  advice: string;
  /** The checkbox is ticked. */
  checked: boolean;
  /** The checkbox can be pressed: false when this Flatpak has no permission to run commands on the host. */
  enabled: boolean;
}

/** One entry per state word of the shell; the shell's word is a key and is never shown as it is. */
export const STATE_WORDS: Readonly<Record<HostState, StateWords>> = {
  off: {
    word: "off", tone: "plain", underline: false, mark: "", checked: false, enabled: true,
    advice: "New panes start inside the Flatpak sandbox. Your own tools and the files outside it are not reachable from them.",
  },
  on: {
    word: "on", tone: "warn", underline: true, mark: "!", checked: true, enabled: true,
    advice: "New panes start on this computer through flatpak-spawn, with your rights and outside the sandbox. Panes that are already open keep running where they are.",
  },
  unavailable: {
    word: "not available", tone: "plain", underline: false, mark: "?", checked: false, enabled: false,
    advice: "This Flatpak was started without the permission to run commands on the host (org.freedesktop.Flatpak), so panes can only start in the sandbox. See the Flatpak page of the documentation.",
  },
};

/** The entry of a state with its key, for drawing the row. */
export type StateRow = StateWords & { state: HostState };

/** The row for `view`: the entry of its state, and the `unavailable` entry when there is no view. */
export function describeHostShell(view: HostShellView | null): StateRow {
  const state: HostState = view !== null && STATES.includes(view.state) ? view.state : "unavailable";
  return { state, ...STATE_WORDS[state] };
}
