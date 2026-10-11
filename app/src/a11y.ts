// Accessibility preferences (DESK-24): placeholder so the tests of the next commit load; the behaviour follows.
export type MotionSetting = "system" | "reduce" | "full";
export type ContrastSetting = "system" | "normal" | "high";
export type BlinkSetting = "system" | "on" | "off";

export function effectiveMotion(_setting: MotionSetting, _osReduce: boolean): "reduce" | "full" {
  return "full";
}

export function effectiveContrast(
  _setting: ContrastSetting, _osMore: boolean, _forcedColors?: boolean,
): "high" | "normal" {
  return "normal";
}

export function effectiveBlink(_setting: BlinkSetting, _motionReduced: boolean, _engineBlinks: boolean): boolean {
  return false;
}

export function contrastRatio(_a: number, _b: number): number {
  return 1;
}

export function onAccentOf(_fill: number): number {
  return 0;
}
