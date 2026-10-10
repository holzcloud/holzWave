// Pure helpers for mouse reporting and the wheel (D-24). No DOM at import time, no state: node imports this module.

/** The modifier bits of the `mouse` and `wheel` commands (same set as the key commands). */
export const MOD_SHIFT = 1;
export const MOD_ALT = 2;
export const MOD_CTRL = 4;
export const MOD_SUPER = 8;

export function modifierBits(_e: { shiftKey: boolean; altKey: boolean; ctrlKey: boolean; metaKey: boolean }): number {
  return -1;
}

export function gridPoint(
  _clientX: number,
  _clientY: number,
  _canvas: { left: number; top: number },
  _grid: { x: number; y: number; w: number; h: number },
  _dpr: number,
): [number, number] {
  return [-1, -1];
}

export function wheelRows(
  _remainder: number,
  _deltaY: number,
  _deltaMode: number,
  _cellH: number,
  _pageRows = 24,
): { rows: number; remainder: number } {
  return { rows: -1, remainder: -1 };
}
