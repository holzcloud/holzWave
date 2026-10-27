// The login page (docs/design.md 6.28): the page a person sees before they have a session. It signs in with a passkey,
// a single sign-on provider or a password and an authenticator code; it sets up the first administrator from the token the
// operator printed; it redeems an invitation, including the authenticator step and the recovery code.
//
// The page talks to its own server through `http` of the bridge and to nothing else: no socket, no other origin. The
// sentences come from loginstate.ts, the words of design 6.28; the server sends none. Every text from the server (a host
// name, a provider name) is put in with `textContent`. The token of a link arrives in the address fragment, is read once,
// removed from the address at once and kept in a variable of this module's closure until the step ends: it is never in a
// field's attribute, the title, a status line, Web Storage or a later address. A password is cleared from its field as
// soon as the request is sent.
import { http, HttpError } from "./bridge.js";
import {
  CODE_MISMATCH,
  COPIED,
  ENDED,
  FAILED,
  INVITE_GONE,
  PASSKEY_NOT_SAVED,
  SECRET_RULE,
  SETUP_GONE,
  USERNAME_TAKEN,
  messageFor,
  parseFragment,
  readPeek,
  readRecovery,
  readSession,
  readTotpSetup,
} from "./loginstate.js";
import type { FragmentKind, Role, SessionView } from "./loginstate.js";
import { decodeCreationOptions, decodeRequestOptions, encodeAssertion, encodeAttestation } from "./loginpasskey.js";
import { showRecovery, showSecret } from "./secretsteps.js";

const mount = document.getElementById("login");

// ---- small building blocks -------------------------------------------------------------------------------------------

function h<K extends keyof HTMLElementTagNameMap>(tag: K, cls: string, parent?: Element, text?: string): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  parent?.append(node);
  return node;
}

let ids = 0;
const nextId = (): string => `lg${++ids}`;

/** The result of one request: the parsed answer, or the status and body of the failure (status 0: nothing arrived). */
type Reply = { ok: true; data: unknown } | { ok: false; status: number; body: Record<string, unknown> | undefined };

async function post(path: string, body: unknown): Promise<Reply> {
  try {
    return { ok: true, data: await http("POST", path, body) };
  } catch (e) {
    if (e instanceof HttpError) return { ok: false, status: e.status, body: e.body };
    return { ok: false, status: 0, body: undefined };
  }
}

/** The text of a failed reply. */
const failure = (r: Reply & { ok: false }): string => messageFor(r.status, r.body);

const goHome = (): void => location.replace("/");

/** A person who closed the passkey prompt: not a failure. */
const cancelled = (e: unknown): boolean => typeof e === "object" && e !== null && (e as { name?: unknown }).name === "NotAllowedError";

/** The line under a form: the one place for the sentence of the current state. */
interface Line {
  el: HTMLElement;
  error(text: string): void;
  note(text: string): void;
  clear(): void;
}

function makeLine(parent: Element): Line {
  const el = h("div", "lg-line", parent);
  el.id = nextId();
  el.setAttribute("role", "status");
  const set = (text: string, tone: "error" | "plain"): void => {
    el.setAttribute("role", tone === "error" ? "alert" : "status");
    el.dataset.tone = tone;
    el.textContent = text;
  };
  return {
    el,
    error: (text) => set(text, "error"),
    note: (text) => set(text, "plain"),
    clear: () => {
      el.textContent = "";
      el.setAttribute("role", "status");
      delete el.dataset.tone;
    },
  };
}

interface Field {
  wrap: HTMLElement;
  input: HTMLInputElement;
  error(text: string): void;
  clearError(): void;
}

interface FieldOptions {
  label: string;
  autocomplete: string;
  type?: string;
  inputmode?: string;
  hint?: string;
  readonly?: boolean;
  value?: string;
}

function field(parent: Element, o: FieldOptions): Field {
  const id = nextId();
  const wrap = h("div", "lg-f", parent);
  const label = h("label", "lg-label label", wrap, o.label);
  label.htmlFor = id;
  const input = h("input", "lg-in", wrap);
  input.id = id;
  input.type = o.type ?? "text";
  input.setAttribute("autocomplete", o.autocomplete);
  if (o.inputmode) input.inputMode = o.inputmode;
  input.spellcheck = false;
  input.setAttribute("autocapitalize", "none");
  input.setAttribute("autocorrect", "off");
  if (o.readonly) input.readOnly = true;
  if (o.value !== undefined) input.value = o.value;
  const described: string[] = [];
  if (o.hint) {
    const hint = h("div", "lg-hint", wrap, o.hint);
    hint.id = `${id}-h`;
    described.push(hint.id);
  }
  const err = h("div", "lg-err", wrap);
  err.id = `${id}-e`;
  err.hidden = true;
  if (described.length > 0) input.setAttribute("aria-describedby", described.join(" "));
  return {
    wrap,
    input,
    error: (text) => {
      err.textContent = text;
      err.hidden = false;
      err.setAttribute("role", "alert");
      input.setAttribute("aria-invalid", "true");
      input.setAttribute("aria-describedby", [...described, err.id].join(" "));
    },
    clearError: () => {
      err.textContent = "";
      err.hidden = true;
      err.removeAttribute("role");
      input.removeAttribute("aria-invalid");
      if (described.length > 0) input.setAttribute("aria-describedby", described.join(" "));
      else input.removeAttribute("aria-describedby");
    },
  };
}

interface ButtonOptions {
  solid?: boolean;
  type?: "button" | "submit";
}

function button(parent: Element, label: string, o: ButtonOptions = {}): HTMLButtonElement {
  const b = h("button", `lg-btn lg-full${o.solid ? " lg-solid" : ""}`, parent, label);
  b.type = o.type ?? "button";
  return b;
}

/** The working state of a step: the pressed button keeps its label and adds `…`, the fields are read-only, the box is busy. */
function busyState(box: HTMLElement, inputs: HTMLInputElement[], pressed: HTMLElement, idle: string): { on(): void; off(): void; active(): boolean } {
  let active = false;
  return {
    on: () => {
      active = true;
      box.setAttribute("aria-busy", "true");
      for (const i of inputs) i.readOnly = true;
      pressed.textContent = `${idle}…`;
    },
    off: () => {
      active = false;
      box.setAttribute("aria-busy", "false");
      for (const i of inputs) i.readOnly = false;
      pressed.textContent = idle;
    },
    active: () => active,
  };
}

/** The page frame: the box (the `main` landmark) with the wordmark and the server, and the line below it. */
function frame(session: SessionView): { box: HTMLElement; below: HTMLElement } {
  if (mount === null) throw new Error("the page has no mount");
  mount.textContent = "";
  const box = h("main", "lg-box", mount);
  box.setAttribute("aria-busy", "false");
  const brand = h("div", "lg-brand", box);
  h("h1", "lg-mark", brand, "holzWave");
  const host = h("span", "lg-host", brand, session.host);
  host.title = "The server";
  const below = h("div", "lg-below", mount);
  below.setAttribute("role", "status");
  return { box, below };
}

const hasPasskeys = (): boolean => typeof PublicKeyCredential !== "undefined" && typeof navigator.credentials !== "undefined";

// ---- sign in ---------------------------------------------------------------------------------------------------------

interface Notice {
  text: string;
  tone: "error" | "plain";
}

function signInScreen(session: SessionView, notice?: Notice, username = ""): void {
  const { box } = frame(session);
  const passkey = session.passkey && hasPasskeys();
  const above = passkey || session.providers.length > 0;
  let passkeyButton: HTMLButtonElement | undefined;
  let line: Line | undefined;

  if (passkey) {
    passkeyButton = button(box, "Sign in with a passkey", { solid: true });
    passkeyButton.addEventListener("click", () => {
      if (line) void signInWithPasskey(box, passkeyButton as HTMLButtonElement, line);
    });
  }
  for (const p of session.providers) {
    const link = h("a", "lg-btn lg-full", box, `Continue with ${p.name}`);
    link.href = `/auth/oidc/${p.id}/start`;
  }
  if (above && session.password) h("div", "lg-or label", box, "or");

  let user: Field | undefined;
  let pass: Field | undefined;
  let form: HTMLFormElement | undefined;
  let submit: HTMLButtonElement | undefined;
  if (session.password) {
    form = h("form", "lg-form", box);
    form.noValidate = true;
    user = field(form, { label: "Username", autocomplete: "username", value: username });
    pass = field(form, { label: "Password", autocomplete: "current-password", type: "password" });
    submit = button(form, "Continue", { solid: !passkey, type: "submit" });
  }
  line = makeLine(box);
  if (notice) {
    if (notice.tone === "error") line.error(notice.text);
    else line.note(notice.text);
  }

  if (form && user && pass && submit) {
    const u = user;
    const p = pass;
    const go = submit;
    const l = line;
    const state = busyState(box, [u.input, p.input], go, "Continue");
    form.addEventListener("submit", (e) => {
      e.preventDefault();
      if (state.active()) return;
      void (async () => {
        l.clear();
        u.clearError();
        p.clearError();
        const name = u.input.value;
        state.on();
        const pending = post("/auth/password", { username: name, password: p.input.value });
        // The password leaves the field as soon as the request is on its way.
        p.input.value = "";
        const r = await pending;
        state.off();
        if (r.ok) {
          const next = (r.data as { next?: unknown } | null)?.next;
          if (next === "totp") return codeScreen(session, name);
          if (next === "done") return goHome();
          l.error(FAILED);
        } else l.error(failure(r));
        p.input.setAttribute("aria-invalid", "true");
        p.input.setAttribute("aria-describedby", l.el.id);
        p.input.focus();
      })();
    });
    p.input.addEventListener("input", () => p.clearError());
    (username === "" ? u.input : p.input).focus();
  } else passkeyButton?.focus();
}

async function signInWithPasskey(box: HTMLElement, pressed: HTMLButtonElement, line: Line): Promise<void> {
  if (box.getAttribute("aria-busy") === "true") return;
  line.clear();
  box.setAttribute("aria-busy", "true");
  pressed.textContent = "Sign in with a passkey…";
  try {
    const begin = await post("/auth/passkey/begin", {});
    if (!begin.ok) return line.error(failure(begin));
    let publicKey: Record<string, unknown>;
    try {
      publicKey = decodeRequestOptions((begin.data as { options?: unknown } | null)?.options);
    } catch {
      return line.error(FAILED);
    }
    let credential: Credential | null;
    try {
      credential = await navigator.credentials.get({ publicKey: publicKey as unknown as PublicKeyCredentialRequestOptions });
    } catch (e) {
      // A prompt the person closed is not a failure: the box returns to idle with no message.
      if (cancelled(e)) return;
      return line.error(FAILED);
    }
    let body: unknown;
    try {
      body = encodeAssertion(credential);
    } catch {
      return line.error(FAILED);
    }
    const finish = await post("/auth/passkey/finish", body);
    if (!finish.ok) return line.error(failure(finish));
    goHome();
  } finally {
    box.setAttribute("aria-busy", "false");
    pressed.textContent = "Sign in with a passkey";
    if (pressed.isConnected) pressed.focus();
  }
}

function codeScreen(session: SessionView, username: string): void {
  const { box } = frame(session);
  const lead = h("p", "lg-lead", box);
  lead.append("Signing in as ");
  h("b", "lg-name", lead, username);
  lead.append(". Enter the six-digit code of your authenticator app.");

  const form = h("form", "lg-form", box);
  form.noValidate = true;
  const code = field(form, { label: "Code", autocomplete: "one-time-code", inputmode: "numeric" });
  const go = button(form, "Sign in", { solid: true, type: "submit" });
  const line = makeLine(box);
  const back = h("button", "lg-tb", h("div", "", box), "Back");
  back.type = "button";
  back.addEventListener("click", () => signInScreen(session, undefined, username));

  const state = busyState(box, [code.input], go, "Sign in");
  form.addEventListener("submit", (e) => {
    e.preventDefault();
    if (state.active()) return;
    void (async () => {
      line.clear();
      code.clearError();
      state.on();
      const r = await post("/auth/totp", { code: code.input.value.replace(/\s+/g, "") });
      state.off();
      if (r.ok) {
        const next = (r.data as { next?: unknown } | null)?.next;
        if (next === "done") return goHome();
        line.error(FAILED);
      } else line.error(failure(r));
      code.input.setAttribute("aria-invalid", "true");
      code.input.setAttribute("aria-describedby", line.el.id);
      code.input.focus();
    })();
  });
  code.input.addEventListener("input", () => {
    code.input.removeAttribute("aria-invalid");
    code.input.removeAttribute("aria-describedby");
  });
  code.input.focus();
}

// ---- first administrator and invitation ------------------------------------------------------------------------------

interface Enrolment {
  kind: FragmentKind;
  /** The route of the two flows; both take the same steps. */
  route: string;
  title: string;
  gone: string;
}

const SETUP: Enrolment = { kind: "setup", route: "/auth/bootstrap", title: "Set up this server", gone: SETUP_GONE };
const INVITE: Enrolment = { kind: "invite", route: "/auth/invite/redeem", title: "You were invited", gone: INVITE_GONE };

/** A box with a title and one error line instead of a form: a link or token that is no longer good. */
function endScreen(session: SessionView, title: string, text: string): void {
  const { box } = frame(session);
  h("h2", "lg-title", box, title);
  makeLine(box).error(text);
}

function titleRow(box: HTMLElement, title: string, role?: Role): void {
  const row = h("div", "lg-title", box);
  h("h2", "lg-h2", row, title);
  if (role !== undefined) h("span", "lg-chip label", row, role);
}

function detailsScreen(session: SessionView, which: Enrolment, token: string, peek?: { role: Role; username?: string }): void {
  const { box } = frame(session);
  titleRow(box, which.title, peek?.role);
  if (which.kind === "setup") h("p", "lg-lead", box, "There is no administrator yet. The account you make here becomes the first one.");

  const form = h("form", "lg-form", box);
  form.noValidate = true;
  const tokenField =
    which.kind === "setup"
      ? field(form, { label: "Setup token", autocomplete: "off", type: "password", hint: "The token the server printed when you started it.", value: token })
      : undefined;
  const user = field(form, { label: "Username", autocomplete: "username", value: peek?.username ?? "", readonly: peek?.username !== undefined });
  const display = field(form, { label: "Display name", autocomplete: "name" });

  const passkeyOffered = hasPasskeys();
  const group = h("fieldset", "lg-f lg-radiogroup", form);
  group.setAttribute("role", "radiogroup");
  h("legend", "lg-label label", group, "Sign-in method");
  const options = h("div", "lg-radio", group);
  const radios = new Map<string, HTMLInputElement>();
  const choices: Array<[string, string]> = passkeyOffered
    ? [["passkey", "Passkey"], ["password", "Password and authenticator app"]]
    : [["password", "Password and authenticator app"]];
  const groupName = nextId();
  for (const [value, text] of choices) {
    const label = h("label", "lg-option", options);
    const radio = h("input", "", label);
    radio.type = "radio";
    radio.name = groupName;
    radio.value = value;
    h("span", "", label, text);
    radios.set(value, radio);
  }
  const first = radios.get(passkeyOffered ? "passkey" : "password");
  if (first) first.checked = true;
  const chosen = (): string => (radios.get("password")?.checked ? "password" : "passkey");

  const pass = field(form, { label: "Password", autocomplete: "new-password", type: "password" });
  const syncPassword = (): void => {
    pass.wrap.hidden = chosen() !== "password";
  };
  syncPassword();
  for (const r of radios.values()) r.addEventListener("change", syncPassword);

  const go = button(form, "Continue", { solid: true, type: "submit" });
  const line = makeLine(box);
  const inputs = [user.input, display.input, pass.input, ...(tokenField ? [tokenField.input] : [])];
  const state = busyState(box, inputs, go, "Continue");

  const clearErrors = (): void => {
    line.clear();
    user.clearError();
    display.clearError();
    pass.clearError();
    tokenField?.clearError();
  };

  /** A failed first step: the sentence of its kind, at the field it concerns. */
  const refused = (r: Reply & { ok: false }): void => {
    const where = typeof r.body?.field === "string" ? r.body.field : "";
    if (r.status === 410) return endScreen(session, which.title, which.gone);
    if (r.status === 409 && where === "username") {
      user.error(USERNAME_TAKEN);
      user.input.focus();
      return;
    }
    if (r.status === 400 && where === "password") {
      pass.error(SECRET_RULE);
      pass.input.focus();
      return;
    }
    line.error(r.status === 400 || r.status === 401 ? FAILED : failure(r));
    const target = r.status === 400 ? ({ username: user, display_name: display, token: tokenField } as Record<string, Field | undefined>)[where] : undefined;
    (target ?? user).input.focus();
  };

  form.addEventListener("submit", (e) => {
    e.preventDefault();
    if (state.active()) return;
    void (async () => {
      clearErrors();
      const method = chosen();
      const body: Record<string, unknown> = {
        step: "begin",
        token: tokenField ? tokenField.input.value.trim() : token,
        username: user.input.value,
        display_name: display.input.value,
        method,
      };
      if (method === "password") body.password = pass.input.value;
      state.on();
      const pending = post(which.route, body);
      pass.input.value = "";
      const r = await pending;
      if (!r.ok) {
        state.off();
        return refused(r);
      }
      const answer = r.data as { next?: unknown; options?: unknown } | null;
      if (method === "password") {
        state.off();
        const setup = answer?.next === "totp_setup" ? readTotpSetup(r.data) : null;
        if (setup === null) return line.error(FAILED);
        return authenticatorScreen(session, which, setup);
      }
      // The passkey path: the browser makes the credential, the server finishes the account.
      try {
        if (answer?.next !== "passkey_register") return line.error(FAILED);
        let publicKey: Record<string, unknown>;
        try {
          publicKey = decodeCreationOptions(answer.options);
        } catch {
          return line.error(PASSKEY_NOT_SAVED);
        }
        let credential: Credential | null;
        try {
          credential = await navigator.credentials.create({ publicKey: publicKey as unknown as PublicKeyCredentialCreationOptions });
        } catch (err) {
          if (cancelled(err)) return;
          return line.error(PASSKEY_NOT_SAVED);
        }
        let attestation: unknown;
        try {
          attestation = encodeAttestation(credential);
        } catch {
          return line.error(PASSKEY_NOT_SAVED);
        }
        const done = await post(which.route, { step: "finish", ...(attestation as Record<string, unknown>) });
        if (!done.ok) {
          if (done.status === 410) return endScreen(session, which.title, which.gone);
          return line.error(done.status === 400 || done.status === 401 ? PASSKEY_NOT_SAVED : failure(done));
        }
        const recovery = readRecovery(done.data);
        if (recovery === null) return line.error(PASSKEY_NOT_SAVED);
        recoveryScreen(session, recovery);
      } finally {
        if (box.isConnected && box.contains(go)) state.off();
      }
    })();
  });
  const firstField = tokenField && token === "" ? tokenField : user.input.readOnly ? display : user;
  firstField.input.focus();
}

function authenticatorScreen(session: SessionView, which: Enrolment, setup: { secret: string; otpauth: string }): void {
  const { box, below } = frame(session);
  titleRow(box, which.title);
  h("p", "lg-lead", box, "Add this key to your authenticator app, then enter the code it shows.");
  const copied = (): void => {
    below.textContent = COPIED;
  };
  const clearSecret = showSecret(box, setup.secret, setup.otpauth, copied);

  const form = h("form", "lg-form", box);
  form.noValidate = true;
  const code = field(form, { label: "Code", autocomplete: "one-time-code", inputmode: "numeric" });
  const go = button(form, "Continue", { solid: true, type: "submit" });
  const line = makeLine(box);
  const state = busyState(box, [code.input], go, "Continue");

  form.addEventListener("submit", (e) => {
    e.preventDefault();
    if (state.active()) return;
    void (async () => {
      line.clear();
      code.clearError();
      state.on();
      const r = await post(which.route, { step: "finish", code: code.input.value.replace(/\s+/g, "") });
      state.off();
      if (!r.ok) {
        if (r.status === 410) return endScreen(session, which.title, which.gone);
        if (r.status === 401) {
          code.input.value = "";
          code.error(CODE_MISMATCH);
          code.input.focus();
          return;
        }
        return line.error(failure(r));
      }
      const recovery = readRecovery(r.data);
      if (recovery === null) return line.error(FAILED);
      clearSecret();
      recoveryScreen(session, recovery);
    })();
  });
}

function recoveryScreen(session: SessionView, code: string): void {
  const { box, below } = frame(session);
  h("h2", "lg-title", box, "Save your recovery code");
  h(
    "p",
    "lg-lead",
    box,
    "If you forget how you unlock your saved passwords, this code unlocks them. It is shown once and cannot be shown again.",
  );
  showRecovery(box, code, goHome, () => {
    below.textContent = COPIED;
  });
}

// ---- start -----------------------------------------------------------------------------------------------------------

/** Take the fragment and the two flags out of the address at once; what they carried stays in memory. */
function readAddress(): { fragment: ReturnType<typeof parseFragment>; oidcFailed: boolean; ended: boolean } {
  const params = new URLSearchParams(location.search);
  const fragment = parseFragment(location.hash);
  const oidcFailed = params.get("error") === "oidc";
  const ended = params.get("ended") === "1";
  if (location.hash !== "" || oidcFailed || ended) {
    if (oidcFailed) params.delete("error");
    if (ended) params.delete("ended");
    const rest = params.toString();
    history.replaceState(null, "", location.pathname + (rest === "" ? "" : `?${rest}`));
  }
  return { fragment, oidcFailed, ended };
}

async function start(): Promise<void> {
  if (mount === null) return;
  const { fragment, oidcFailed, ended } = readAddress();

  let session: SessionView = { state: "anonymous", host: location.host, bootstrap: false, passkey: false, password: true, providers: [] };
  let unreachable: string | undefined;
  try {
    const read = readSession(await http("GET", "/api/session"));
    if (read === null) unreachable = messageFor(0);
    else session = read;
  } catch (e) {
    unreachable = e instanceof HttpError ? messageFor(e.status, e.body) : messageFor(0);
  }

  if (unreachable === undefined && session.state !== "anonymous") return goHome();

  if (unreachable === undefined && session.bootstrap) {
    return detailsScreen(session, SETUP, fragment?.kind === "setup" ? fragment.token : "");
  }
  if (unreachable === undefined && fragment?.kind === "invite") {
    const token = fragment.token;
    const peek = await post(INVITE.route, { step: "peek", token });
    if (!peek.ok) return endScreen(session, INVITE.title, peek.status === 410 ? INVITE.gone : failure(peek));
    const read = readPeek(peek.data);
    if (read === null) return endScreen(session, INVITE.title, FAILED);
    return detailsScreen(session, INVITE, token, read);
  }

  let notice: Notice | undefined;
  if (unreachable !== undefined) notice = { text: unreachable, tone: "error" };
  else if (oidcFailed) notice = { text: FAILED, tone: "error" };
  else if (ended) notice = { text: ENDED, tone: "plain" };
  signInScreen(session, notice);
}

void start();
