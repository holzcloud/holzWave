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
