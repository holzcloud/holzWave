// Hint mode: a short label on every target of the focused pane. A pure part (labels, key reading) that node
// imports for the unit tests. This file is a stub until the labels are written.

export const HINT_ALPHABET = "abcdefghijklmnopqrstuvwxyz";
export const MAX_HINTS = 676;

export type HintMatch = { state: "none" } | { state: "partial" } | { state: "hit"; index: number };

export function hintLabels(_n: number): string[] {
  return [];
}

export function resolveHint(_labels: readonly string[], _typed: string): HintMatch {
  return { state: "none" };
}

export function hintLetter(_e: { key: string; ctrlKey?: boolean; metaKey?: boolean; altKey?: boolean }): string | null {
  return null;
}
