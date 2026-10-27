// The pure part of the people dialog (docs/design.md 6.29): the reading of what `GET /api/admin/accounts` answers, the state
// words with their glyphs, the time words, the menu of a row and the rule for the last active administrator.
// No DOM and no network here, so node tests it on the compiled file.

/** The most accounts the page takes from the server. */
export const MAX_ACCOUNTS = 500;

export type Role = "admin" | "member";
export type State = "active" | "disabled" | "locked_out";

/** One account as a row: name, role, state and last sign-in, and nothing else. */
export interface Row {
  id: string;
  username: string;
  displayName: string;
  role: Role;
  state: State;
  /** When the person last signed in, or null when they never did. */
  lastSigninMs: number | null;
}

/** What the list answers, after checking. */
export interface People {
  /** The number of active administrators. */
  admins: number;
  rows: Row[];
}

/** The state of a row as a word, a glyph (its own cue besides the colour) and a tone. */
export interface StateWord {
  word: string;
  glyph: string;
  tone: "ok" | "dim" | "warn";
}

export function readAccounts(_raw: unknown): People | null {
  return null;
}

export function stateWord(_state: string): StateWord | null {
  return null;
}

export function lastSigninText(_ms: number | null, _now: number): string {
  return "";
}
