// The question before a link the program marked opens (docs/design.md 6.31): when the text over the link names another place
// than its address, the click does not open at once. A modal alert dialog of the shape of the close confirmation: the question as
// its title, the text the person sees (cut at 120 characters) and the address the link goes to (wrapped, never cut),
// and two buttons, `Cancel` focused first and `Open`. Enter on Cancel, Escape and a click outside are Cancel and call nothing.
// Every string is put in with `textContent`, never as markup. The opening is the caller's `onOpen`, called synchronously from
// the click or the key that confirms, because a browser lets a new tab open only from the gesture itself: nothing is awaited
// between the press on `Open` and `onOpen`.
import { el, loadStyles } from "./dom.js";
import { focusInput } from "./keyboard.js";
import { pushKeyHandler } from "./keystack.js";
import { cutShown } from "./links.js";

let open = false;

/** A press on the backdrop this soon after the dialog opened is the tail of the click that asked, not a Cancel. */
const SETTLE_MS = 500;

/** The question is on screen: a second link asks nothing until it is answered. */
export function isAskingLink(): boolean {
  return open;
}

/**
 * Ask whether to open `target`, a link whose visible text is `shown`. `onOpen` runs only when the person confirms, and runs
 * synchronously from that click or key. One question at a time: while one is open another call does nothing. Focus is given
 * back to the terminal's input when the question is answered.
 */
export function askOpenLink(shown: string, target: string, onOpen: () => void): void {
  if (open) return;
  open = true;
  void loadStyles("./dialogs.css")
    .catch(() => {
      // the dialog still works without its style sheet
    })
    .then(() => build(shown, target, onOpen));
}

function build(shown: string, target: string, onOpen: () => void): void {
  const backdrop = el("div", "dlg-backdrop", document.body);
  const box = el("div", "dlg dlg-link", backdrop);
  box.setAttribute("role", "alertdialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-labelledby", "dlg-link-title");
  box.setAttribute("aria-describedby", "dlg-link-text dlg-link-target");

  const title = el("h2", "dlg-title", box);
  title.id = "dlg-link-title";
  title.textContent = "Open this link?";

  const textLabel = el("div", "dlg-lbl", box);
  textLabel.textContent = "The text says";
  const text = el("div", "dlg-val dlg-shown", box);
  text.id = "dlg-link-text";
  text.textContent = cutShown(shown);

  const targetLabel = el("div", "dlg-lbl", box);
  targetLabel.textContent = "The link goes to";
  const where = el("div", "dlg-val dlg-target", box);
  where.id = "dlg-link-target";
  where.textContent = target;

  const actions = el("div", "dlg-actions", box);
  const cancel = el("button", "dlg-cancel", actions);
  cancel.type = "button";
  cancel.textContent = "Cancel";
  const ok = el("button", "dlg-open", actions);
  ok.type = "button";
  ok.textContent = "Open";

  let pop = (): void => {};
  const finish = (confirmed: boolean): void => {
    if (!open) return;
    open = false;
    pop();
    backdrop.remove();
    try {
      if (confirmed) onOpen(); // still inside the click or the key: a browser allows the tab
    } finally {
      focusInput();
    }
  };
  pop = pushKeyHandler((e) => {
    // A Cmd chord belongs to the system menu (quit, hide): not the terminal's, so it is consumed, and its default is left alone.
    if (e.metaKey) return true;
    // A held Enter or Space that was down when the dialog opened must not also answer it.
    if (e.repeat) {
      e.preventDefault();
      return true;
    }
    const focused = document.activeElement;
    if (e.key === "Escape") finish(false);
    else if (e.key === "Enter") finish(focused === ok);
    else if (e.key === " ") {
      if (focused === ok) finish(true);
      else if (focused === cancel) finish(false);
    } else if (e.key === "Tab") (focused === ok ? cancel : ok).focus();
    e.preventDefault();
    return true;
  });
  cancel.addEventListener("click", () => finish(false));
  ok.addEventListener("click", () => finish(true));
  const shownAt = performance.now();
  backdrop.addEventListener("pointerdown", (e) => {
    if (e.target === backdrop && performance.now() - shownAt > SETTLE_MS) finish(false);
  });
  cancel.focus();
}
