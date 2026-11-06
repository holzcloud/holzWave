// The layout class of the page: which of the three layouts a window gets (docs/design.md section 6.6). A pure rule over
// numbers, so node can test it; the browser module (phone.ts) reads the numbers and decides when to ask again.

/** `desktop` is every desktop and browser window; `phone` and `tablet` exist on Android only. */
export type Layout = "desktop" | "phone" | "tablet";

/** The window width in CSS px from which an Android window is a tablet (the short side of the screen must be wide enough too). */
export const TABLET_MIN_WIDTH = 840;

/** The short side of the screen in CSS px from which an Android window can be a tablet: a landscape phone stays a phone. */
export const TABLET_MIN_SHORT_SIDE = 480;

/** What the rule reads. `os` is the platform name `app_info` gave (`android`, `macos`, `windows`, `linux`, `web`, or empty). */
export interface LayoutFacts {
  os: string;
  /** The width of the window (`innerWidth`) in CSS px. */
  width: number;
  /** The shorter of the two sides of the screen in CSS px. */
  shortSide: number;
}

/**
 * The layout of a window. Only Android takes the touch layouts, so a narrow desktop window or a browser tab is unchanged.
 * On Android the tablet layout needs both a wide window and a screen whose short side is not a phone's; a number that is
 * not a number never reaches the tablet class.
 */
export function layoutClass(facts: LayoutFacts): Layout {
  if (facts.os !== "android") return "desktop";
  return facts.width >= TABLET_MIN_WIDTH && facts.shortSide >= TABLET_MIN_SHORT_SIDE ? "tablet" : "phone";
}
