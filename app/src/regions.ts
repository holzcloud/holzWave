// The regions of the window that the keyboard moves between: the terminal and the bar (docs/design.md section 6.1). `F6` and
// `Shift+F6` (the actions `region.focus_next` and `region.focus_prev`) take the keyboard from the terminal to the bar and from the
// bar back to the terminal; the palette entry `Focus the bar` (`bar.focus`) goes to the bar from anywhere. Inside the bar the
// controls keep `Tab`, `Shift+Tab`, `Enter`, `Space` and the arrows (keyboard.ts), and `Escape` or `F6` gives the keyboard back.
// The page walks the bar itself on `Tab` and `Shift+Tab` (`walkBar`): WebKit on macOS makes buttons stops of the tab order only when
// the system's full keyboard access is on, so the engine's own Tab would reach the bar on some computers and not on others. The
// DOM is read when an action runs, never at import, so node can import the module.
import { registerAction } from "./actions.js";

/** What the bar offers to the keyboard: its buttons and links, and anything that was made a stop of the tab order. */
const STOPS = "button, a[href], [role='button'], [tabindex]";

/** The window bar, or null when the page has none. */
function barElement(): HTMLElement | null {
  return document.getElementById("bar");
}

/** Whether `node` is the bar or inside it. */
export function inBar(node: EventTarget | null): boolean {
  return node instanceof Element && node.closest("#bar") !== null;
}

/**
 * The controls of the bar that the keyboard reaches, in the order of the page: shown, enabled and in the tab order. A bar that
 * is not drawn (the phone layout has its own strips) has none.
 */
export function barControls(): HTMLElement[] {
  const bar = barElement();
  if (!bar) return [];
  return [...bar.querySelectorAll<HTMLElement>(STOPS)].filter(
    (node) =>
      node.tabIndex >= 0 &&
      !(node instanceof HTMLButtonElement && node.disabled) &&
      node.closest("[hidden], [inert]") === null &&
      node.getClientRects().length > 0,
  );
}

/** Give the keyboard to the first or the last control of the bar; false (and the keyboard stays) when the bar has none. */
export function focusBar(which: "first" | "last" = "first"): boolean {
  const controls = barControls();
  const target = which === "first" ? controls[0] : controls[controls.length - 1];
  if (!target) return false;
  target.focus();
  return document.activeElement === target;
}

/**
 * Move the keyboard from the control `from` of the bar to the next one (or the previous one with `back`), in the order of the page.
 * False when there is none that way (`from` is the last control, or the first when going back, or is none of the bar's controls):
 * the caller then gives the keyboard to the terminal, so `Tab` never leaves the page's regions and never gets stuck.
 */
export function walkBar(from: EventTarget | null, back: boolean): boolean {
  const controls = barControls();
  const at = from instanceof HTMLElement ? controls.indexOf(from) : -1;
  const target = at < 0 ? undefined : controls[at + (back ? -1 : 1)];
  if (!target) return false;
  target.focus();
  return document.activeElement === target;
}

/**
 * Register the three actions. The palette offers them only while the bar is drawn. `giveBack` is the way to the terminal: F6 in the
 * bar ends there, F6 in the terminal goes to the bar (the first control for `F6`, the last for `Shift+F6`).
 */
export function registerRegionActions(giveBack: () => void): void {
  const drawn = (): boolean => barControls().length > 0;
  const step = (which: "first" | "last"): void => {
    if (inBar(document.activeElement)) giveBack();
    else focusBar(which);
  };
  registerAction({ id: "region.focus_next", title: "Focus next region", run: () => step("first"), available: drawn });
  registerAction({ id: "region.focus_prev", title: "Focus previous region", run: () => step("last"), available: drawn });
  registerAction({ id: "bar.focus", title: "Focus the bar", run: () => void focusBar("first"), available: drawn });
}
