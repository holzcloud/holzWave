// Clickable targets in the visible output: URLs, file paths and hashes, found in plain text. A pure module (no DOM
// at import): node imports it for the unit tests. This file is a stub until the detection is written.

export type TargetKind = "url" | "path" | "hash";

export interface Target {
  kind: TargetKind;
  /** First and last cell column the target covers (both ends inclusive). */
  startCol: number;
  endCol: number;
  text: string;
  line?: number;
  col?: number;
}

export type LinkAction =
  | { command: "open_url"; args: { url: string } }
  | { command: "reveal_path"; args: { path: string; base: string | null } };

export function findTargets(_cells: string[]): Target[] {
  return [];
}

export function targetAt(_targets: Target[], _col: number): Target | null {
  return null;
}

export function linkAction(_target: Target, _base: string | null): LinkAction | null {
  return null;
}

export function isLinkModifier(_e: { metaKey?: boolean; ctrlKey?: boolean }, _os: string): boolean {
  return false;
}
