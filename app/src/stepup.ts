// `Confirm it is you` (docs/design.md 6.29): every change of an account goes through `withStepUp(run)`. It runs the change; when
// the server answers that a recent sign-in is needed, it shows one layer, sends the proof and repeats the change exactly
// once. A cancel sends nothing more and rejects with `StepUpCancelled`. Callers that need it at the same time share one layer.
//
// The password is read from its field once, the field is emptied at every submit, and nothing is ever put back. The only
// request function is `http` of the bridge. Every server string (the account name, the provider's name) is put in with
// `textContent`; the provider link is a path built from an id that passed a strict pattern, and a hostile id gives no link.
import { http } from "./bridge.js";
import { el } from "./dom.js";
import { decodeRequestOptions, encodeAssertion } from "./loginpasskey.js";
import { FAILED, messageFor, UNREACHABLE } from "./loginstate.js";
import { leadParts, needsStepUp, NO_METHOD, providerPath, readStepUpMe, STEP_UP_TITLE, stepUpMethods } from "./stepupstate.js";
import type { Method } from "./stepupstate.js";
import { openLayer } from "./weblayer.js";
import { failureOf } from "./websessionstate.js";

/** The person closed the layer: nothing was confirmed and nothing more was sent. */
export class StepUpCancelled extends Error {
  constructor() {
    super("cancelled");
    this.name = "StepUpCancelled";
  }
}

/** The layer that is open, shared by every caller that needs it at the same time. */
let pending: Promise<void> | null = null;

/**
 * Run `run`. When it fails because a recent sign-in is needed, ask for it once and run `run` again, once: a second refusal
 * is the caller's. Any other failure is the caller's at once.
 */
export async function withStepUp<T>(run: () => Promise<T>): Promise<T> {
  try {
    return await run();
  } catch (e) {
    if (!needsStepUp(e)) throw e;
  }
  pending ??= confirmIt().finally(() => {
    pending = null;
  });
  await pending;
  return run();
}

/** Show the layer; resolves when the server accepted the proof, rejects with `StepUpCancelled` when the person leaves. */
function confirmIt(): Promise<void> {
  const opener = document.activeElement as HTMLElement | null;
  return new Promise<void>((resolve, reject) => {
    void build(opener, resolve, reject);
  });
}

async function build(opener: HTMLElement | null, resolve: () => void, reject: (e: unknown) => void): Promise<void> {
  // What the account has decides what the layer offers. A failed lookup is said in the layer, with `Cancel` only.
  let me: ReturnType<typeof readStepUpMe> = null;
  try {
    me = readStepUpMe(await http("GET", "/api/me"));
  } catch {
    me = null;
  }
  const methods: Method[] = me === null ? [] : stepUpMethods(me);

  let cancel: () => void = () => {};
  const layer = await openLayer({
    cls: "web-step",
    labelledBy: "web-step-title",
    returnTo: opener,
    sheets: ["./websession.css"],
    onEscape: () => cancel(),
    onEnter: () => void submitPassword(),
  });
  const box = layer.box;
  const title = el("h2", "set-title", box);
  title.id = "web-step-title";
  title.textContent = STEP_UP_TITLE;

  const hasPassword = methods.some((m) => m.kind === "password");
  const hasPasskey = methods.some((m) => m.kind === "passkey");
  const provider = methods.find((m): m is Extract<Method, { kind: "provider" }> => m.kind === "provider");
  const providerHref = provider ? providerPath(provider.id) : null;

  const lead = el("p", "web-lead", box);
  let input: HTMLInputElement | null = null;
  const line = el("p", "web-err", box);
  line.id = "web-step-error";
  line.setAttribute("role", "alert");

  if (me === null) {
    line.textContent = UNREACHABLE;
  } else if (!hasPassword && !hasPasskey && providerHref === null) {
    lead.textContent = NO_METHOD;
  } else {
    const parts = leadParts(hasPassword || hasPasskey ? methods : []);
    lead.append(parts.before);
    el("b", "web-name", lead).textContent = me.name;
    lead.append(parts.after);
  }

  if (hasPassword) {
    const field = el("div", "web-field");
    lead.after(field);
    const label = el("label", "label web-label", field);
    label.htmlFor = "web-step-field";
    label.textContent = "Password";
    input = el("input", "web-input", field);
    input.id = "web-step-field";
    input.type = "password";
    input.autocomplete = "current-password";
    input.spellcheck = false;
  }

  const actions = el("div", "dlg-actions", box);
  const cancelButton = el("button", "web-btn", actions);
  cancelButton.type = "button";
  cancelButton.textContent = "Cancel";
  let confirmButton: HTMLButtonElement | null = null;
  if (hasPassword) {
    confirmButton = el("button", "web-btn web-solid", actions);
    confirmButton.type = "button";
    confirmButton.textContent = "Confirm";
  } else if (hasPasskey) {
    confirmButton = el("button", "web-btn web-solid", actions);
    confirmButton.type = "button";
    confirmButton.textContent = "Use a passkey";
  } else if (providerHref !== null && provider) {
    // The provider signs the person in again; the page leaves like a sign-in does. The link is a path, never an address.
    const link = el("a", "web-btn web-solid", actions);
    link.href = providerHref;
    link.textContent = `Sign in again with ${provider.name}`;
  }
  let passkeyButton: HTMLButtonElement | null = null;
  if (hasPassword && hasPasskey) {
    passkeyButton = el("button", "web-tb", box);
    passkeyButton.type = "button";
    passkeyButton.textContent = "Use a passkey";
  }

  let busy = false;
  const working = (on: boolean): void => {
    busy = on;
    if (on) box.setAttribute("aria-busy", "true");
    else box.removeAttribute("aria-busy");
    if (input) input.readOnly = on;
    if (hasPassword && confirmButton) confirmButton.textContent = on ? "Confirm…" : "Confirm";
  };
  const fail = (text: string): void => {
    line.textContent = text;
    if (input) {
      input.setAttribute("aria-invalid", "true");
      input.setAttribute("aria-describedby", "web-step-error");
      input.focus();
    }
  };
  const clearError = (): void => {
    line.textContent = "";
    input?.removeAttribute("aria-invalid");
    input?.removeAttribute("aria-describedby");
  };
  const done = (): void => {
    if (input) input.value = "";
    layer.close();
    resolve();
  };
  cancel = (): void => {
    if (input) input.value = "";
    layer.close();
    reject(new StepUpCancelled());
  };

  async function submitPassword(): Promise<void> {
    if (busy || input === null) return;
    const secret = input.value;
    if (secret === "") return;
    // Read once, now; the field is empty before the request goes out.
    input.value = "";
    working(true);
    clearError();
    try {
      await http("POST", "/auth/step-up", { password: secret });
    } catch (e) {
      working(false);
      const { status, body } = failureOf(e);
      fail(messageFor(status, body));
      return;
    }
    done();
  }

  async function submitPasskey(): Promise<void> {
    if (busy) return;
    working(true);
    clearError();
    try {
      const begin = (await http("POST", "/auth/step-up/begin", {})) as { options?: unknown } | undefined;
      const credentials = navigator.credentials as CredentialsContainer | undefined;
      if (credentials === undefined) throw new TypeError("no credentials");
      const options = decodeRequestOptions(begin?.options) as unknown as PublicKeyCredentialRequestOptions;
      const assertion = await credentials.get({ publicKey: options });
      if (assertion === null) {
        working(false);
        return;
      }
      await http("POST", "/auth/step-up", encodeAssertion(assertion));
    } catch (e) {
      working(false);
      const name = (e as { name?: unknown } | null)?.name;
      // A prompt the person closed is not a failure.
      if (name === "NotAllowedError" || name === "AbortError") return;
      // An answer of the server says its status; anything else (a malformed option, a browser without credentials) is a failed proof.
      if (typeof (e as { status?: unknown } | null)?.status !== "number") fail(FAILED);
      else {
        const { status, body } = failureOf(e);
        fail(messageFor(status, body));
      }
      return;
    }
    done();
  }

  cancelButton.addEventListener("click", () => cancel());
  confirmButton?.addEventListener("click", () => void (hasPassword ? submitPassword() : submitPasskey()));
  passkeyButton?.addEventListener("click", () => void submitPasskey());
  (input ?? confirmButton ?? cancelButton).focus();
}
