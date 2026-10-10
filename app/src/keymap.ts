// Placeholder for the RED commit of plan 02-18 task 1: the real types and signatures, no behaviour yet.

/** The fields of a `KeyboardEvent` this module reads; a plain object with these fields is enough. */
export interface KeyLike {
  code: string;
  key: string;
  shiftKey?: boolean;
  ctrlKey?: boolean;
  altKey?: boolean;
  metaKey?: boolean;
  repeat?: boolean;
  isComposing?: boolean;
  keyCode?: number;
}

/** What the page tells the core about one key event. */
export interface KeyPayload {
  code: string;
  key: string;
  mods: number;
  repeat: boolean;
  release: boolean;
  composing: boolean;
}

export function keyPayload(_e: KeyLike, _release = false): KeyPayload {
  return { code: "", key: "", mods: 0, repeat: false, release: false, composing: false };
}

export function chordsOf(_e: KeyLike): string[] {
  return [];
}

export function isComposingKey(_e: KeyLike): boolean {
  return false;
}

export function isModifierKey(_e: KeyLike): boolean {
  return false;
}
