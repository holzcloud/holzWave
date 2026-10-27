// The two once-only steps of design 6.28, built as functions so the login page and the account dialog show them the same
// way: the authenticator secret with its link, and the recovery code.
//
// What these steps show is a secret for as long as the step lasts. Each function puts the secret into its own element with
// `textContent` (never markup), puts it nowhere else (no attribute, no title, no storage, no console), and returns a
// `clear()` that empties the elements and removes them; the caller runs it when the step ends. The module imports nothing
// but the pure helpers: it does not know the login page.
import { groupRecovery, groupsOfFour, TICK } from "./loginstate.js";

function make<K extends keyof HTMLElementTagNameMap>(tag: K, cls: string, parent: Element, text?: string): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  node.className = cls;
  if (text !== undefined) node.textContent = text;
  parent.append(node);
  return node;
}

function button(parent: Element, label: string): HTMLButtonElement {
  const b = make("button", "lg-btn lg-small", parent, label);
  b.type = "button";
  return b;
}

/** Select the visible text of `node`: what a copy falls back to when the clipboard is not available. */
function selectText(node: Node): void {
  const selection = getSelection();
  if (selection === null) return;
  const range = document.createRange();
  range.selectNodeContents(node);
  selection.removeAllRanges();
  selection.addRange(range);
}

/** Put `text` on the clipboard on the press of a button; when the browser refuses, select `shown` instead. */
function copyOnPress(trigger: HTMLElement, text: () => string, shown: HTMLElement, onCopied?: () => void): void {
  trigger.addEventListener("click", () => {
    const clipboard = navigator.clipboard as Clipboard | undefined;
    if (clipboard === undefined || typeof clipboard.writeText !== "function") {
      selectText(shown);
      return;
    }
    clipboard.writeText(text()).then(
      () => onCopied?.(),
      () => {
        if (shown.isConnected) selectText(shown);
      },
    );
  });
}

/**
 * The authenticator step: the label `Key` over the secret in groups of four with `Copy`, the label `Link` over the
 * `otpauth://` link with `Copy link`. `Copy` puts the key without its spaces on the clipboard, `Copy link` the whole link.
 * `onCopied` is called after a successful copy. Focus goes to `Copy`. Returns `clear()`.
 */
export function showSecret(parent: HTMLElement, secret: string, otpauth: string, onCopied?: () => void): () => void {
  const root = make("div", "lg-step", parent);

  const key = make("div", "lg-f", root);
  make("span", "lg-label label", key, "Key");
  const keyRow = make("div", "lg-row2", key);
  const keyText = make("div", "lg-secret", keyRow, groupsOfFour(secret));
  const copyKey = button(keyRow, "Copy");
  copyOnPress(copyKey, () => secret, keyText, onCopied);

  const link = make("div", "lg-f", root);
  make("span", "lg-label label", link, "Link");
  const linkRow = make("div", "lg-row2", link);
  const linkBox = make("div", "lg-in", linkRow);
  const linkText = make("span", "lg-tx", linkBox, otpauth);
  const copyLink = button(linkRow, "Copy link");
  copyOnPress(copyLink, () => otpauth, linkText, onCopied);

  copyKey.focus();
  return () => {
    keyText.textContent = "";
    linkText.textContent = "";
    root.remove();
  };
}

/**
 * The recovery step: the code in groups of four joined by hyphens with `Copy`, the checkbox `I have saved this code`, and
 * `Continue`, which stays disabled until the box is ticked, with the line `Tick the box to continue.` under it.
 *
 * The step cannot be left by accident: `Escape` is swallowed at the document while it is shown, there is no handler for a
 * click outside, a press of the browser's Back button is answered by putting the step's own history entry back, and a
 * reload or a close asks the browser to confirm until the box is ticked. `Continue` removes the code from the page (not
 * hides it), ends the guards and then calls `onContinue`. Focus goes to `Copy`. Returns `clear()`, which does the same
 * without calling `onContinue`.
 */
export function showRecovery(parent: HTMLElement, code: string, onContinue: () => void, onCopied?: () => void): () => void {
  const grouped = groupRecovery(code);
  const root = make("div", "lg-step", parent);

  const row = make("div", "lg-row2", root);
  const block = make("div", "lg-secret lg-code", row, grouped);
  block.setAttribute("role", "group");
  block.setAttribute("aria-label", "Recovery code");
  const copy = button(row, "Copy");
  copyOnPress(copy, () => grouped, block, onCopied);

  const check = make("label", "lg-chk", root);
  const box = make("input", "", check);
  box.type = "checkbox";
  make("span", "", check, "I have saved this code");

  const go = make("button", "lg-btn lg-full", root, "Continue");
  go.type = "button";
  go.disabled = true;
  const note = make("div", "lg-note", root, TICK);

  const leaveWarning = (e: BeforeUnloadEvent): void => {
    e.preventDefault();
    e.returnValue = "";
  };
  const swallowEscape = (e: KeyboardEvent): void => {
    if (e.key !== "Escape") return;
    e.preventDefault();
    e.stopPropagation();
  };
  // The step owns one history entry; Back lands on the entry below it, and the step puts its own back.
  const guardBack = (): void => history.pushState(null, "", location.href);

  let warned = true;
  const stopWarning = (): void => {
    if (!warned) return;
    warned = false;
    window.removeEventListener("beforeunload", leaveWarning);
  };
  let live = true;
  const stop = (): void => {
    live = false;
    stopWarning();
    document.removeEventListener("keydown", swallowEscape, true);
    window.removeEventListener("popstate", guardBack);
  };

  window.addEventListener("beforeunload", leaveWarning);
  document.addEventListener("keydown", swallowEscape, true);
  history.pushState(null, "", location.href);
  window.addEventListener("popstate", guardBack);

  box.addEventListener("change", () => {
    go.disabled = !box.checked;
    note.textContent = box.checked ? "" : TICK;
    if (box.checked) stopWarning();
    else if (live && !warned) {
      warned = true;
      window.addEventListener("beforeunload", leaveWarning);
    }
  });

  const clear = (): void => {
    stop();
    block.textContent = "";
    root.remove();
  };
  go.addEventListener("click", () => {
    if (go.disabled) return;
    clear();
    onContinue();
  });

  copy.focus();
  return clear;
}
