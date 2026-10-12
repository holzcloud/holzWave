// The in-place name editor for tabs, panes and workspaces: a text field laid over the label it renames. Enter and a
// click elsewhere confirm, Escape cancels. The field is the page's own: its keys never reach the terminal, and what
// is typed goes to the core, which cleans it. A name from the core is only ever shown with `textContent`.
import { focusInput } from "./keyboard.js";

/** The longest name the editor takes; the core cuts to the same limit. */
const MAX_NAME = 128;

let open: HTMLInputElement | null = null;

/** Whether the name editor is open. */
export function isRenaming(): boolean {
  return open !== null;
}

/**
 * Open the editor over `anchor` with `initial` as its text (the empty text when the label is automatic, with
 * `placeholder` showing what that label is). `commit` gets the text when it was confirmed and changed. A second call
 * closes the first editor, confirming it.
 */
export function startRename(anchor: HTMLElement, initial: string, placeholder: string, commit: (name: string) => void): void {
  open?.blur();
  const box = anchor.getBoundingClientRect();
  const input = document.createElement("input");
  input.type = "text";
  input.className = "rename-input";
  input.maxLength = MAX_NAME;
  input.value = initial;
  input.placeholder = placeholder;
  input.spellcheck = false;
  input.autocomplete = "off";
  input.setAttribute("aria-label", "Name");
  input.style.left = `${box.left}px`;
  input.style.top = `${box.top}px`;
  input.style.minWidth = `${Math.max(box.width, 140)}px`;
  input.style.height = `${box.height}px`;
  let done = false;
  const finish = (confirmed: boolean): void => {
    if (done) return;
    done = true;
    if (open === input) open = null;
    const text = input.value;
    input.remove();
    focusInput();
    if (confirmed && text !== initial) commit(text);
  };
  input.addEventListener("keydown", (e) => {
    // Nothing typed here is a shortcut or terminal input.
    e.stopPropagation();
    if (e.isComposing) return;
    if (e.key === "Enter") {
      e.preventDefault();
      finish(true);
    } else if (e.key === "Escape") {
      e.preventDefault();
      finish(false);
    }
  });
  input.addEventListener("keyup", (e) => e.stopPropagation());
  input.addEventListener("blur", () => finish(true));
  document.body.appendChild(input);
  open = input;
  input.focus();
  input.select();
}
