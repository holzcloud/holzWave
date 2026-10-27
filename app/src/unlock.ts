// The unlock dialog (docs/design.md 6.28, "Unlock"): the saved passwords of a person are locked, and the passphrase opens them.
//
// The passphrase is read from its field once, the field is emptied before the request goes out, and nothing is ever put back
// into a field: a refusal leaves it empty. The server's answer is a status and never a sentence; the page chooses the words.
import { http } from "./bridge.js";
import { el } from "./dom.js";
import { unlockMessage } from "./websessionstate.js";
import { openLayer } from "./weblayer.js";
import type { Layer } from "./weblayer.js";

/** The dialog on screen, if any: there is one at a time. */
let shown: Layer | null = null;

/** The status of a failed request: an `HttpError` has it, a rejection of anything else is the network failing. */
function statusOf(e: unknown): { status: number; body?: Record<string, unknown> } {
  const status = typeof (e as { status?: unknown })?.status === "number" ? (e as { status: number }).status : 0;
  const body = (e as { body?: unknown })?.body;
  return typeof body === "object" && body !== null ? { status, body: body as Record<string, unknown> } : { status };
}

/**
 * Show the dialog. `unlocked` is called once after the server accepted the passphrase. A second call while the dialog is
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
  shown = layer;
  const box = layer.box;

  const title = el("h2", "set-title", box);
  title.id = "web-unlock-title";
  title.textContent = "Your saved passwords are locked";
  el("p", "web-lead", box).textContent = "Enter your passphrase to use your saved passwords and keys.";

  const field = el("div", "web-field", box);
  const label = el("label", "label web-label", field);
  label.htmlFor = "web-unlock-field";
  label.textContent = "Passphrase";
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

  let busy = false;
  const close = (): void => {
    input.value = "";
    shown = null;
    layer.close();
  };

  const submit = async (): Promise<void> => {
    if (busy) return;
    const secret = input.value;
    if (secret === "") return;
    // Read once, now; the field is empty before the request goes out.
    input.value = "";
    busy = true;
    box.setAttribute("aria-busy", "true");
    input.readOnly = true;
    go.textContent = "Unlock…";
    line.textContent = "";
    input.removeAttribute("aria-invalid");
    input.removeAttribute("aria-describedby");
    try {
      await http("POST", "/auth/unlock", { passphrase: secret });
    } catch (e) {
      const { status, body } = statusOf(e);
      busy = false;
      box.removeAttribute("aria-busy");
      input.readOnly = false;
      go.textContent = "Unlock";
      line.textContent = unlockMessage(status, body);
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
  // A press on the backdrop does nothing: a stray click must not throw the dialog away.
  input.focus();
}
