// The layer every dialog of the browser build is made of (docs/design.md 6.28 to 6.30): a backdrop, the box of the Settings
// dialog, the keys taken from the key handler stack while it is open, a focus that stays inside, and a focus that goes back
// where it came from. The unlock dialog, the ended layer, `Confirm it is you` and the account dialog build on it, so that
// the four behave alike. Layers stack: the newest takes every key, and an older one gets the keys back when it closes.
import { el, loadStyles } from "./dom.js";
import { pushKeyHandler } from "./keystack.js";

/** What a layer is made with. */
export interface LayerOptions {
  /** Classes of the box besides `dlg`. */
  cls: string;
  /** The id of the element that names the dialog. */
  labelledBy: string;
  /** An alert dialog: the ended layer announces itself. */
  alert?: boolean;
  /** Style sheets to load after `dialogs.css` and `settings.css`. */
  sheets?: string[];
  /** `Escape`. Without it the key does nothing (a step that cannot be left). */
  onEscape?: () => void;
  /** `Enter` in a text field of the box. */
  onEnter?: (input: HTMLInputElement) => void;
  /** Where the focus goes on close; the terminal when it is gone or not given. */
  returnTo?: HTMLElement | null;
}

/** An open layer. */
export interface Layer {
  backdrop: HTMLElement;
  box: HTMLElement;
  /** Take the layer away and give the focus back. Calling it again does nothing. */
  close(): void;
}

const open: Layer[] = [];

/** The controls a person can reach by Tab inside `box`, in reading order. */
export function stopsOf(box: HTMLElement): HTMLElement[] {
  const found = box.querySelectorAll<HTMLElement>("input, button, select, textarea, a[href]");
  return Array.from(found).filter((n) => {
    if (n instanceof HTMLInputElement && n.type === "hidden") return false;
    if ((n as HTMLButtonElement).disabled || n.hasAttribute("hidden")) return false;
    return n.getClientRects().length > 0;
  });
}

/** Give the keyboard to the terminal. The keyboard module is loaded here, not at start, so the desktop page never pays for the layers. */
async function toTerminal(): Promise<void> {
  const { focusInput } = await import("./keyboard.js");
  focusInput();
}

/** Open a layer. The sheets are loaded first; a sheet that fails to load leaves the dialog working without its look. */
export async function openLayer(options: LayerOptions): Promise<Layer> {
  try {
    await loadStyles("./dialogs.css");
    await loadStyles("./settings.css");
    for (const sheet of options.sheets ?? []) await loadStyles(sheet);
  } catch {
    // the dialog still works without its style sheets
  }
  const returnTo = options.returnTo === undefined ? (document.activeElement as HTMLElement | null) : options.returnTo;
  const backdrop = el("div", "dlg-backdrop", document.body);
  const box = el("div", `dlg settings ${options.cls}`, backdrop);
  box.setAttribute("role", options.alert ? "alertdialog" : "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-labelledby", options.labelledBy);

  let pop: () => void = () => {};
  let gone = false;
  const layer: Layer = {
    backdrop,
    box,
    close: () => {
      if (gone) return;
      gone = true;
      pop();
      backdrop.remove();
      const at = open.indexOf(layer);
      if (at >= 0) open.splice(at, 1);
      if (returnTo && returnTo.isConnected && returnTo !== document.body) {
        returnTo.focus();
        return;
      }
      const below = open[open.length - 1];
      if (below) stopsOf(below.box)[0]?.focus();
      else void toTerminal();
    },
  };
  open.push(layer);

  pop = pushKeyHandler((e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      options.onEscape?.();
    } else if (e.key === "Tab") {
      e.preventDefault();
      const items = stopsOf(box);
      if (items.length === 0) return true;
      const at = items.indexOf(document.activeElement as HTMLElement);
      const to = e.shiftKey ? (at <= 0 ? items.length - 1 : at - 1) : at < 0 || at === items.length - 1 ? 0 : at + 1;
      items[to]?.focus();
    } else if (e.key === "Enter" && e.target instanceof HTMLInputElement && options.onEnter && box.contains(e.target)) {
      if (e.target.type !== "checkbox" && e.target.type !== "radio") {
        e.preventDefault();
        options.onEnter(e.target);
      }
    }
    return true;
  });
  return layer;
}
