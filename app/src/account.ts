// The account dialog (docs/design.md 6.29): a person changes their own password, authenticator, passkeys, unlock passphrase and
// recovery code, and ends their sessions. Every change goes through `withStepUp`, so a refusal that asks for a recent sign-in
// shows `Confirm it is you` once and the change is repeated once.
//
// Nothing secret outlives its step. A secret that a person types is read from its field once and the field is emptied at
// once; nothing is ever put back. A secret the server sends (the authenticator key, the new recovery code) is shown by the
// shared once-only steps, which empty their elements when the step ends. Every string the server sends (a device, a passkey's
// name) is put in with `textContent` in an element of its own, so none of it is ever markup. The only request function is
// `http` of the bridge; the dialog makes no `call` of the core.
import { http } from "./bridge.js";
import { agoText, changeMessage, CHANGE_FAILED, groupStates, readMe, REPLACES_OLD, SAVED, sessionRows, usedText } from "./accountstate.js";
import type { Group, Me } from "./accountstate.js";
import { buildDevicesGroup } from "./devices.js";
import { armed, el } from "./dom.js";
import { decodeCreationOptions, encodeAttestation } from "./loginpasskey.js";
import { PASSKEY_NOT_SAVED, readRecovery, readTotpSetup, UNREACHABLE } from "./loginstate.js";
import { showRecovery, showSecret } from "./secretsteps.js";
import { StepUpCancelled, withStepUp } from "./stepup.js";
import { openLayer } from "./weblayer.js";
import type { Layer } from "./weblayer.js";
import { requestUnlock } from "./websession.js";
import { failureOf } from "./websessionstate.js";

/** The dialog on screen, if any: there is one at a time. */
let shown: Layer | null = null;

/** The outcome of a change: it was made, the person cancelled the confirmation, or the server refused it. */
type Outcome<T> = { ok: true; value: T } | { ok: false; cancelled: true } | { ok: false; cancelled: false; status: number; body?: Record<string, unknown> };

/** Run a change through the step-up rule and say how it ended; it never throws. */
async function change<T>(run: () => Promise<T>): Promise<Outcome<T>> {
  try {
    return { ok: true, value: await withStepUp(run) };
  } catch (e) {
    if (e instanceof StepUpCancelled) return { ok: false, cancelled: true };
    return { ok: false, cancelled: false, ...failureOf(e) };
  }
}

/** The ids of the elements that name a field to the reader of the screen. */
let sequence = 0;

/** Open the dialog; a second call while it is open does nothing. */
export async function openAccount(): Promise<void> {
  if (shown !== null) return;
  // A form that is open: the next key `Enter` in a field presses its `Save`.
  let current: { submit: () => void; close: (refocus: boolean) => void } | null = null;
  /** A once-only step (the new recovery code) is on screen: the dialog cannot be left until its checkbox is ticked. */
  let onceOnly = false;
  let busy = false;

  const layer = await openLayer({
    cls: "account",
    labelledBy: "acc-title",
    returnTo: null,
    sheets: ["./websession.css", "./account.css", "./devices.css"],
    onEscape: () => {
      if (!onceOnly) closeAll();
    },
    onEnter: () => current?.submit(),
  });
  if (shown !== null) {
    layer.close();
    return;
  }
  shown = layer;
  const box = layer.box;
  let me: Me | null = null;

  const title = el("h2", "set-title", box);
  title.id = "acc-title";
  title.textContent = "Account";
  const status = el("p", "set-status", box);
  status.setAttribute("role", "status");
  const groups = el("div", "acc-groups", box);
  // The Devices group follows the groups of the account (docs/design.md 6.35); it reads its own list and is not drawn again with them.
  const devicesArea = el("div", "acc-devices", box);
  const actions = el("div", "dlg-actions", box);
  const close = el("button", "", actions);
  close.type = "button";
  close.textContent = "Close";

  const say = (text: string): void => {
    status.textContent = text;
  };

  const devices = buildDevicesGroup(devicesArea, "person", {
    box,
    say,
    isOpen: () => shown === layer,
    fallback: () => close.focus(),
  });

  function closeAll(): void {
    current?.close(false);
    devices.end();
    layer.close();
    shown = null;
  }
  close.addEventListener("click", () => {
    if (!onceOnly) closeAll();
  });

  // ---- reading the account ------------------------------------------------------------------------------------------------

  /** Ask the server who the person is and what they have, and draw it. */
  async function reload(): Promise<boolean> {
    try {
      const read = readMe(await http("GET", "/api/me"));
      if (read === null) throw new Error("not an account");
      me = read;
    } catch {
      say(UNREACHABLE);
      return false;
    }
    current?.close(false);
    draw();
    return true;
  }

  /** The main button of a group, to put the focus back on it after a change. */
  const focusGroup = (id: string): void => {
    groups.querySelector<HTMLElement>(`[data-group="${id}"] .acc-main`)?.focus();
  };

  // ---- the parts every form is made of ------------------------------------------------------------------------------------

  interface Form {
    body: HTMLElement;
    err: HTMLElement;
    save: HTMLButtonElement;
    cancel: HTMLButtonElement;
    inputs: HTMLInputElement[];
  }

  /** Open the form of a group in the place of its row. Only one form is open at a time. */
  function openForm(id: string, saveLabel: string, submit: () => Promise<void>): Form {
    current?.close(false);
    const fs = groups.querySelector<HTMLElement>(`[data-group="${id}"]`)!;
    const row = fs.querySelector<HTMLElement>(".acc-row")!;
    row.hidden = true;
    say("");
    const body = el("div", "acc-form");
    row.after(body);
    const err = el("p", "web-err", body);
    err.id = `acc-err-${++sequence}`;
    err.setAttribute("role", "alert");
    const buttons = el("div", "acc-buttons", body);
    const save = el("button", "acc-save", buttons);
    save.type = "button";
    save.textContent = saveLabel;
    const cancel = el("button", "", buttons);
    cancel.type = "button";
    cancel.textContent = "Cancel";
    const form: Form = { body, err, save, cancel, inputs: [] };
    const end = (refocus: boolean): void => {
      for (const input of form.inputs) input.value = "";
      stepClear?.();
      stepClear = null;
      body.remove();
      row.hidden = false;
      current = null;
      if (refocus) focusGroup(id);
    };
    current = { submit: () => void submit(), close: end };
    save.addEventListener("click", () => void submit());
    cancel.addEventListener("click", () => end(true));
    return form;
  }

  /** What empties the secret elements of the step that is open (the authenticator key, the recovery code). */
  let stepClear: (() => void) | null = null;

  /** A labelled field in a form, before its error line. */
  function field(form: Form, label: string, kind: { type: string; autocomplete: string; inputmode?: string; maxLength?: number }): HTMLInputElement {
    const wrap = el("div", "web-field");
    form.body.insertBefore(wrap, form.err);
    const input = el("input", "web-input", wrap);
    input.id = `acc-f-${++sequence}`;
    const l = el("label", "label web-label");
    l.htmlFor = input.id;
    l.textContent = label;
    wrap.insertBefore(l, input);
    input.type = kind.type;
    input.setAttribute("autocomplete", kind.autocomplete);
    input.spellcheck = false;
    input.setAttribute("autocapitalize", "off");
    input.setAttribute("autocorrect", "off");
    if (kind.inputmode) input.inputMode = kind.inputmode;
    if (kind.maxLength) input.maxLength = kind.maxLength;
    form.inputs.push(input);
    return input;
  }

  const working = (form: Form, on: boolean, label: string): void => {
    busy = on;
    if (on) box.setAttribute("aria-busy", "true");
    else box.removeAttribute("aria-busy");
    for (const input of form.inputs) input.readOnly = on;
    form.save.textContent = on ? `${label}…` : label;
  };

  const clearError = (form: Form): void => {
    form.err.textContent = "";
    for (const input of form.inputs) {
      input.removeAttribute("aria-invalid");
      input.removeAttribute("aria-describedby");
    }
  };

  /** Say a refusal in the form, under its fields, and mark the field it concerns. */
  const refuse = (form: Form, text: string, input?: HTMLInputElement): void => {
    form.err.textContent = text;
    const target = input ?? form.inputs[0];
    if (target) {
      target.setAttribute("aria-invalid", "true");
      target.setAttribute("aria-describedby", form.err.id);
      target.focus();
    }
  };

  /** A change was made: say so, close the form, read the account again and put the focus back. */
  async function made(id: string): Promise<void> {
    current?.close(false);
    say(SAVED);
    await reload();
    focusGroup(id);
  }

  // ---- the groups ---------------------------------------------------------------------------------------------------------

  /** Ends the listeners of the armed buttons of the last drawing. */
  let drawn = new AbortController();

  function draw(): void {
    if (me === null) return;
    drawn.abort();
    drawn = new AbortController();
    groups.textContent = "";
    for (const g of groupStates(me)) drawGroup(me, g);
    drawSessions(me);
  }

  function drawGroup(account: Me, g: Group): void {
    const fs = el("fieldset", "set-group acc-group", groups);
    fs.setAttribute("role", "group");
    fs.dataset.group = g.id;
    el("legend", "label", fs).textContent = g.legend;
    const row = el("div", "acc-row", fs);
    el("span", "acc-name", row).textContent = g.name;
    const state = el("span", "acc-state", row);
    state.dataset.tone = g.tone;
    state.textContent = g.state;
    const main = el("button", "acc-main", row);
    main.type = "button";
    main.textContent = g.button.label;
    main.addEventListener("click", () => {
      if (busy) return;
      if (g.button.action === "unlock") void requestUnlock(() => void reload());
      else if (g.id === "password") passwordForm();
      else if (g.id === "totp") void totpForm(main);
      else if (g.id === "passkeys") passkeyForm();
      else if (g.id === "passphrase") passphraseForm();
      else if (g.id === "recovery") recoveryForm();
    });
    if (g.id === "passkeys") drawPasskeys(account, fs);
  }

  // ---- password -----------------------------------------------------------------------------------------------------------

  function passwordForm(): void {
    const submit = async (): Promise<void> => {
      if (busy) return;
      const old = currentPassword.value;
      const next = newPassword.value;
      if (old === "") return void currentPassword.focus();
      if (next === "") return void newPassword.focus();
      // Read once, now; both fields are empty before the request goes out.
      currentPassword.value = "";
      newPassword.value = "";
      working(form, true, "Save");
      clearError(form);
      const r = await change(() => http("POST", "/api/me/password", { current: old, new: next }));
      working(form, false, "Save");
      if (r.ok) return made("password");
      if (r.cancelled) return void currentPassword.focus();
      const m = changeMessage("password", r.status, r.body);
      refuse(form, m.text, m.field === "new" ? newPassword : currentPassword);
    };
    const form = openForm("password", "Save", submit);
    const currentPassword = field(form, "Current password", { type: "password", autocomplete: "current-password" });
    const newPassword = field(form, "New password", { type: "password", autocomplete: "new-password" });
    currentPassword.focus();
  }

  // ---- authenticator ------------------------------------------------------------------------------------------------------

  async function totpForm(opener: HTMLElement): Promise<void> {
    // The key comes first; asking for it is a change, so it may ask for a recent sign-in.
    busy = true;
    say("");
    const begun = await change(() => http("POST", "/api/me/totp", { step: "begin" }));
    busy = false;
    if (!begun.ok) {
      if (!begun.cancelled) say(changeMessage("totp", begun.status, begun.body).text);
      opener.focus();
      return;
    }
    const setup = readTotpSetup(begun.value);
    if (setup === null) {
      say(CHANGE_FAILED);
      return;
    }
    const submit = async (): Promise<void> => {
      if (busy) return;
      const code = codeInput.value.replace(/\s+/g, "");
      if (code === "") return void codeInput.focus();
      codeInput.value = "";
      working(form, true, "Save");
      clearError(form);
      const r = await change(() => http("POST", "/api/me/totp", { step: "finish", code }));
      working(form, false, "Save");
      if (r.ok) return made("totp");
      if (r.cancelled) return void codeInput.focus();
      const m = changeMessage("totp", r.status, r.body);
      refuse(form, m.text, codeInput);
    };
    const form = openForm("totp", "Save", submit);
    // The key and the link are shown by the shared step; it empties them when the step ends.
    const host = el("div", "acc-secret");
    form.body.insertBefore(host, form.err);
    const clear = showSecret(host, setup.secret, setup.otpauth);
    stepClear = () => {
      clear();
      host.remove();
    };
    const codeInput = field(form, "Code", { type: "text", autocomplete: "one-time-code", inputmode: "numeric", maxLength: 12 });
  }

  // ---- passkeys -----------------------------------------------------------------------------------------------------------

  function drawPasskeys(account: Me, fs: HTMLElement): void {
    if (account.passkeys.length === 0) return;
    const list = el("ul", "acc-list", fs);
    const now = Date.now();
    for (const key of account.passkeys) {
      const item = el("li", "acc-item", list);
      el("span", "acc-name", item).textContent = key.name;
      el("span", "acc-meta", item).textContent = usedText(key.lastUsedMs, now);
      armed(el("button", "acc-danger", item), "Remove", "Click again to remove", box, drawn.signal, async () => {
        const r = await change(() => http("DELETE", `/api/me/passkeys/${encodeURIComponent(key.id)}`));
        if (r.ok) {
          say(SAVED);
          await reload();
          focusGroup("passkeys");
        } else if (!r.cancelled) say(changeMessage("passkey-remove", r.status, r.body).text);
      });
    }
  }

  function passkeyForm(): void {
    const submit = async (): Promise<void> => {
      if (busy) return;
      const name = nameInput.value.trim();
      if (name === "") return void nameInput.focus();
      working(form, true, "Save");
      clearError(form);
      try {
        const begun = await change(() => http("POST", "/api/me/passkeys", { step: "begin", name }));
        if (!begun.ok) {
          if (!begun.cancelled) refuse(form, changeMessage("passkey-add", begun.status, begun.body).text, nameInput);
          return;
        }
        const credentials = navigator.credentials as CredentialsContainer | undefined;
        if (credentials === undefined) throw new TypeError("no credentials");
        const options = decodeCreationOptions((begun.value as { options?: unknown } | undefined)?.options);
        const created = await credentials.create({ publicKey: options as unknown as PublicKeyCredentialCreationOptions });
        if (created === null) return;
        const finishBody = { step: "finish", ...encodeAttestation(created) };
        const finished = await change(() => http("POST", "/api/me/passkeys", finishBody));
        if (!finished.ok) {
          if (!finished.cancelled) refuse(form, changeMessage("passkey-add", finished.status, finished.body).text, nameInput);
          return;
        }
        working(form, false, "Save");
        await made("passkeys");
      } catch (e) {
        const n = (e as { name?: unknown } | null)?.name;
        // A prompt that the person closed is not a failure.
        if (n !== "NotAllowedError" && n !== "AbortError") refuse(form, PASSKEY_NOT_SAVED, nameInput);
      } finally {
        if (busy) working(form, false, "Save");
      }
    };
    const form = openForm("passkeys", "Save", submit);
    const nameInput = field(form, "Name", { type: "text", autocomplete: "off", maxLength: 64 });
    nameInput.focus();
  }

  // ---- unlock passphrase --------------------------------------------------------------------------------------------------

  function passphraseForm(): void {
    const submit = async (): Promise<void> => {
      if (busy) return;
      const secret = input.value;
      if (secret === "") return void input.focus();
      input.value = "";
      working(form, true, "Save");
      clearError(form);
      const r = await change(() => http("PUT", "/api/me/passphrase", { passphrase: secret }));
      working(form, false, "Save");
      if (r.ok) return made("passphrase");
      if (r.cancelled) return void input.focus();
      refuse(form, changeMessage("passphrase", r.status, r.body).text, input);
    };
    const form = openForm("passphrase", "Save", submit);
    const input = field(form, "New passphrase", { type: "password", autocomplete: "new-password" });
    input.focus();
  }

  // ---- recovery code ------------------------------------------------------------------------------------------------------

  function recoveryForm(): void {
    const submit = async (): Promise<void> => {
      if (busy) return;
      working(form, true, "Make code");
      clearError(form);
      const r = await change(() => http("POST", "/api/me/recovery", {}));
      working(form, false, "Make code");
      if (!r.ok) {
        if (!r.cancelled) refuse(form, changeMessage("recovery", r.status, r.body).text);
        return;
      }
      const code = readRecovery(r.value);
      if (code === null) return refuse(form, CHANGE_FAILED);
      // The code is shown once, in the step of the sign-in page; the dialog cannot be left until it was confirmed.
      form.body.textContent = "";
      onceOnly = true;
      close.disabled = true;
      const clear = showRecovery(form.body, code, () => {
        onceOnly = false;
        close.disabled = false;
        void made("recovery");
      });
      stepClear = () => {
        clear();
        onceOnly = false;
        close.disabled = false;
      };
    };
    const form = openForm("recovery", "Make code", submit);
    form.body.insertBefore(el("p", "acc-note"), form.err).textContent = REPLACES_OLD;
    form.save.focus();
  }

  // ---- sessions -----------------------------------------------------------------------------------------------------------

  function drawSessions(account: Me): void {
    const fs = el("fieldset", "set-group acc-group", groups);
    fs.setAttribute("role", "group");
    fs.dataset.group = "sessions";
    el("legend", "label", fs).textContent = "Sessions";
    const list = el("ul", "acc-list", fs);
    const now = Date.now();
    for (const row of sessionRows(account)) {
      const item = el("li", "acc-item acc-session", list);
      if (row.current) el("span", "acc-dot", item).textContent = "●";
      el("span", "acc-name", item).textContent = row.device;
      el("span", "acc-addr", item).textContent = row.addr;
      el("span", "acc-meta", item).textContent = `· ${row.current ? "now" : agoText((now - row.lastSeenMs) / 1000)}`;
      if (row.current) {
        el("span", "acc-state acc-this", item).textContent = "this session";
      } else {
        const out = el("button", "acc-main", item);
        out.type = "button";
        out.textContent = "Sign out";
        out.addEventListener("click", () => {
          if (busy) return;
          void (async () => {
            const r = await change(() => http("DELETE", `/api/me/sessions/${encodeURIComponent(row.id)}`));
            if (r.ok) {
              say(SAVED);
              await reload();
              focusGroup("sessions");
            } else if (!r.cancelled) say(changeMessage("session", r.status, r.body).text);
          })();
        });
      }
    }
    const everywhere = el("div", "acc-row acc-end", fs);
    armed(el("button", "acc-danger", everywhere), "Sign out everywhere", "Click again to sign out everywhere", box, drawn.signal, async () => {
      const r = await change(() => http("DELETE", "/api/me/sessions"));
      // Every session of the account is over, this one too: the window shows the sign-in page.
      if (r.ok) location.replace("/");
      else if (!r.cancelled) say(changeMessage("session", r.status, r.body).text);
    });
  }

  await reload();
  await devices.load();
  (groups.querySelector<HTMLElement>(".acc-main") ?? close).focus();
}
