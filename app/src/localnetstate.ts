// The pure half of the Local Network notice and row: how the answer of the three commands `local_network_status`,
// `local_network_answer` and `local_network_open_settings` is read. No DOM, no command call, no timer, and nothing is
// imported. The shell's state words are keys that select among the page's own sentences and are never shown as they are.
//
// The permission exists on macOS 15 or later only. Where the shell says `supported: false` the page draws nothing and calls
// nothing more.

/** The four states of the permission the shell can report. */
export type LocalState = "allowed" | "denied" | "not_asked" | "unknown";

/** What the shell said: whether the system has the setting, whether the notice is still to be shown, and the state. */
export interface LocalNetworkView {
  supported: boolean;
  noticeDue: boolean;
  state: LocalState;
}

/** The state words, spelled as the shell spells them. */
const STATES: readonly LocalState[] = ["allowed", "denied", "not_asked", "unknown"];

/** An own member of `object`, never an inherited one. */
function own(object: object, key: string): unknown {
  return Object.hasOwn(object, key) ? (object as Record<string, unknown>)[key] : undefined;
}

/**
 * Read the answer of `local_network_status` or `local_network_answer`: a plain object whose own members `supported` and
 * `notice_due` are booleans. The three members are named one by one and nothing is copied by the name it came with, so a
 * member a newer shell adds and an own `__proto__` never reach the page. A state word that is not exactly one of the four
 * (another case, a trailing blank, markup, a number) reads as `unknown`, so the result can be used as a table key. Anything
 * that is not an object, an array, or an object whose flags are not booleans gives null.
 */
export function readLocalNetwork(raw: unknown): LocalNetworkView | null {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return null;
  const supported = own(raw, "supported");
  const noticeDue = own(raw, "notice_due");
  if (typeof supported !== "boolean" || typeof noticeDue !== "boolean") return null;
  const word = own(raw, "state");
  const state = STATES.find((known) => known === word) ?? "unknown";
  return { supported, noticeDue, state };
}

// ---- the page's own words (docs/design.md section 6.24) -----------------------------------------------------------------

/** The notice of the first start: every sentence is the design record's, and the page shows nothing else. */
export const NOTICE = {
  title: "Allow local network access?",
  lead: "holzWave reaches computers on your local network:",
  items: [
    "SSH to machines on your network.",
    "Local AI servers on other computers, such as Ollama or LM Studio.",
    "Sync with a Nextcloud on your network.",
  ],
  note: "macOS asks for your permission next. Programs you run in a pane, such as ssh, count as holzWave. You can change this any time in Settings.",
  notNow: "Not now",
  allow: "Allow",
} as const;

/** The group `Local network` of the Settings dialog: its legend, the row's name and meta, the buttons and the lines. */
export const GROUP = {
  legend: "Local network",
  name: "Access",
  meta: "Privacy & Security",
  open: "Open System Settings",
  allow: "Allow",
  check: "Check again",
  /** The line while a read runs. */
  checking: "Checking…",
  /** The line after `Check again`. */
  checked: "Checked.",
  /** The line after `Allow`: the system's question is open and the person is asked to read again. */
  asked: "Asked. Answer the macOS question, then choose Check again.",
} as const;

/** How one state is shown: the word, the tone, the cues besides the colour, the advice and which buttons follow the opener. */
export interface StateWords {
  /** The state in words (the first cue, the one that never depends on colour). */
  word: string;
  tone: "ok" | "warn" | "plain";
  /** The word is underlined (a state that needs attention). */
  underline: boolean;
  /** A mark before the word, or none. */
  mark: "" | "!" | "?";
  advice: string;
  /** The button `Allow` is offered: macOS has not been asked. */
  allow: boolean;
  /** The button `Check again` is offered: the state may have changed or could not be told. */
  check: boolean;
}

/** One entry per state word of the shell; the shell's word is a key and is never shown as it is. */
export const STATE_WORDS: Readonly<Record<LocalState, StateWords>> = {
  allowed: {
    word: "allowed", tone: "ok", underline: false, mark: "", allow: false, check: false,
    advice: "holzWave can reach computers on your local network.",
  },
  denied: {
    word: "denied", tone: "warn", underline: true, mark: "!", allow: false, check: true,
    advice: "macOS blocks holzWave from your local network. Turn holzWave on in System Settings, then choose Check again.",
  },
  not_asked: {
    word: "not asked yet", tone: "plain", underline: false, mark: "", allow: true, check: false,
    advice: "macOS has not been asked. Choose Allow and macOS shows its own question.",
  },
  unknown: {
    word: "not known", tone: "plain", underline: false, mark: "?", allow: false, check: true,
    advice: "holzWave could not tell. macOS may still be waiting for your answer. Choose Check again.",
  },
};

/** The entry of a state with its key, for drawing the row. */
export type StateRow = StateWords & { state: LocalState };

/** The row for `view`: the entry of its state, and the `unknown` entry when there is no view (a read that failed). */
export function describeLocalNetwork(view: LocalNetworkView | null): StateRow {
  const state: LocalState = view !== null && STATES.includes(view.state) ? view.state : "unknown";
  return { state, ...STATE_WORDS[state] };
}
