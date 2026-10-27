// The unlock dialog (docs/design.md 6.28, "Unlock"): the saved passwords of a person are locked, and the passphrase or the
// recovery code opens them.
//
// The secret is read from its field once, the field is emptied before the request goes out, and nothing is ever put back
// into a field: a refusal leaves it empty. The server's answer is a status and never a sentence; the page chooses the words.
import { http } from "./bridge.js";
import { el } from "./dom.js";
import { failureOf, unlockMessage } from "./websessionstate.js";
import { openLayer } from "./weblayer.js";
import type { Layer } from "./weblayer.js";

/** The lead of each way to unlock. */
const LEAD = {
  passphrase: "Enter your passphrase to use your saved passwords and keys.",
  recovery: "Enter your recovery code to use your saved passwords and keys.",
} as const;
/** The dim sentence at the end of the dialog. */
const LOST_BOTH =
  "If you lost both, an admin can reset your secrets. That deletes your saved passwords and keys, and keeps your account, layout and history.";

/** The dialog on screen, if any: there is one at a time. */
let shown: { layer: Layer; end: () => void } | null = null;

/** Close the dialog if it is open, without a request: the secrets were unlocked somewhere else. */
export function closeUnlock(): void {
  shown?.end();
}

/**
 * Show the dialog. `unlocked` is called once after the server accepted the secret. A second call while the dialog is
 * open does nothing.
 */
export async function openUnlock(unlocked: () => void): Promise<void> {
  if (shown !== null) return;
  const layer = await openLayer({
    cls: "web-unlock",
    labelledBy: "web-unlock-title",
    returnTo: null,
    sheets: ["./websession.css"],
    onEscape: () => close(),
    onEnter: () => void submit(),
  });
  if (shown !== null) {
    layer.close();
    return;
  }
  const box = layer.box;

  const title = el("h2", "set-title", box);
  title.id = "web-unlock-title";
  title.textContent = "Your saved passwords are locked";
  const lead = el("p", "web-lead", box);

  const field = el("div", "web-field", box);
  const label = el("label", "label web-label", field);
  label.htmlFor = "web-unlock-field";
  const input = el("input", "web-input", field);
  input.id = "web-unlock-field";
  input.type = "password";
  input.autocomplete = "off";
  input.spellcheck = false;
  input.setAttribute("autocapitalize", "off");
  input.setAttribute("autocorrect", "off");
  const line = el("p", "web-err", box);
  line.id = "web-unlock-error";
  line.setAttribute("role", "alert");

  const actions = el("div", "dlg-actions", box);
  const notNow = el("button", "web-btn", actions);
  notNow.type = "button";
  notNow.textContent = "Not now";
  const go = el("button", "web-btn web-solid", actions);
  go.type = "button";
  go.textContent = "Unlock";
  const swap = el("button", "web-tb", box);
  swap.type = "button";
  el("p", "web-note", box).textContent = LOST_BOTH;

  let mode: "passphrase" | "recovery" = "passphrase";
  let busy = false;

  const showMode = (): void => {
    lead.textContent = LEAD[mode];
    label.textContent = mode === "passphrase" ? "Passphrase" : "Recovery code";
    swap.textContent = mode === "passphrase" ? "Use recovery code" : "Use passphrase";
    if (mode === "recovery") input.placeholder = "XXXX-XXXX-XXXX-XXXX-XXXX-XXXX";
    else input.removeAttribute("placeholder");
  };
  const clearError = (): void => {
    line.textContent = "";
    input.removeAttribute("aria-invalid");
    input.removeAttribute("aria-describedby");
  };

  const close = (): void => {
    input.value = "";
    shown = null;
    layer.close();
  };
  shown = { layer, end: close };

  const submit = async (): Promise<void> => {
    if (busy) return;
    const secret = mode === "recovery" ? input.value.trim() : input.value;
    if (secret === "") return;
    const body = mode === "recovery" ? { recovery_code: secret } : { passphrase: secret };
    // Read once, now; the field is empty before the request goes out.
    input.value = "";
    busy = true;
    box.setAttribute("aria-busy", "true");
    input.readOnly = true;
    go.textContent = "Unlock…";
    clearError();
    try {
      await http("POST", "/auth/unlock", body);
    } catch (e) {
      const { status, body: answer } = failureOf(e);
      busy = false;
      box.removeAttribute("aria-busy");
      input.readOnly = false;
      go.textContent = "Unlock";
      line.textContent = unlockMessage(status, answer);
      input.setAttribute("aria-invalid", "true");
      input.setAttribute("aria-describedby", "web-unlock-error");
      input.focus();
      return;
    }
    close();
    unlocked();
  };

  notNow.addEventListener("click", close);
  go.addEventListener("click", () => void submit());
  swap.addEventListener("click", () => {
    if (busy) return;
    // The field is swapped, so what was typed in it goes: a secret of one kind is not carried into the field of the other.
    input.value = "";
    mode = mode === "passphrase" ? "recovery" : "passphrase";
    clearError();
    showMode();
    input.focus();
  });
  // A press on the backdrop does nothing: a stray click must not throw the dialog away.
  showMode();
  input.focus();
}
