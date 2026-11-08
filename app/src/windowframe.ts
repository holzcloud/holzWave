// The window buttons of the own window frame (docs/design.md section 6.33). On Windows the shell builds every window without
// the system title bar and tells the page what to draw through one global, written by an initialization script before the page
// runs: `window.__HW_FRAME__ = {"os":"windows","style":"flat","side":"right","buttons":[...]}` (app/src-tauri/src/windowframe.rs).
// The page treats that global as untrusted input: anything that is not exactly that shape is ignored and nothing is drawn, so a
// browser, macOS, Android and a window with the system frame keep the bar as it was. Dragging, the double click that maximises
// and the resize edges come from the window runtime, not from here; the page draws no edge, never listens to the close request
// of the window (a listener in the page would keep the window from closing) and never changes the frame of its window. Elements
// are made with `el` and named by properties; nothing is parsed as markup.
import { backend, call } from "./bridge.js";
import { el, loadStyles } from "./dom.js";
import { focusInput } from "./keyboard.js";

/** The words of a button, in the order the system draws them. */
const BUTTONS: readonly string[] = ["minimize", "maximize", "close"];

/** The keys of the facts, in the order the shell writes them. */
const KEYS: readonly string[] = ["os", "style", "side", "buttons"];

/** What the page draws from the facts. Only the close button is drawn for now, the others follow. */
interface Facts {
  readonly buttons: readonly string[];
}

/**
 * The facts the shell wrote, or `null` for anything else: the object must have exactly the four keys, the platform `windows`,
 * the style `flat`, the side `right`, and a list of one to three different button words that holds `close`.
 */
export function readFrameFacts(raw: unknown): Facts | null {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return null;
  const record = raw as Record<string, unknown>;
  const keys = Object.keys(record);
  if (keys.length !== KEYS.length || !KEYS.every((key) => keys.includes(key))) return null;
  if (record.os !== "windows" || record.style !== "flat" || record.side !== "right") return null;
  const buttons = record.buttons;
  if (!Array.isArray(buttons) || buttons.length < 1 || buttons.length > BUTTONS.length) return null;
  const words: string[] = [];
  for (const word of buttons as unknown[]) {
    if (typeof word !== "string" || !BUTTONS.includes(word) || words.includes(word)) return null;
    words.push(word);
  }
  return words.includes("close") ? { buttons: words } : null;
}

/**
 * Draw the window buttons in `bar` when the shell asked for an own frame. Returns once the style sheet is there, so the first
 * measurement of the page already includes the group. Does nothing in a browser or when the facts are not the shell's.
 */
export async function initWindowFrame(bar: HTMLElement): Promise<void> {
  if (backend !== "tauri") return;
  const facts = readFrameFacts((window as unknown as { __HW_FRAME__?: unknown }).__HW_FRAME__);
  if (facts === null) return;

  const root = document.documentElement;
  root.classList.add("frame-own", "frame-flat", "frame-right");
  // Not a drag region: a press on a button is a press on the button.
  const group = el("div", "bar-window", bar);
  const close = el("button", "wf-btn", group);
  close.type = "button";
  close.dataset.button = "close";
  close.title = "Close";
  close.setAttribute("aria-label", "Close");
  close.addEventListener("click", () => {
    // The runtime closes the window it is called from; a refusal leaves the window as it is.
    call("plugin:window|close").catch(() => {});
    focusInput(); // a bar button must not keep the keyboard
  });
  await loadStyles("./windowframe.css").catch(() => {
    // the button still works without its style sheet
  });
}
