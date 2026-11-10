// The device features of the web page (docs/design.md 6.35): the dialog `Authorize device`, where a person decides whether an app
// that asked to sign in may have a device of their own; the small layer that asks for a typed code; the group `Devices`, where a
// person sees their devices and ends one (and where an administrator sees everybody's); and the group `Device access`, the two
// switches of the administrator. Every word is a constant of devicestate.ts, which is the record's table in code.
//
// Nothing here is secret except what it is told never to keep: the code of a request is a request for approval and not a credential,
// it stands in one element of the dialog and in the body of two requests, and it is kept nowhere else (no storage, no address, no
// log line). A token never reaches the page at all: the lists carry none. Every change goes through `withStepUp`, so a refusal that
// asks for a recent sign-in shows `Confirm it is you` once and the change is repeated once. Every string the server sends is put in
// with `textContent`; the only request function is `http` of the bridge.
import { http } from "./bridge.js";
import { armed } from "./account.js";
import { CHANGE_FAILED, SAVED } from "./accountstate.js";
import {
  ACCESS_HELP,
  ACCESS_LEGEND,
  APPROVE,
  APPROVE_SENTENCE,
  APPROVED,
  AUTHORIZE_TITLE,
  CANCEL,
  CLOSE,
  CODE_LABEL,
  DENIED,
  DENY,
  DEVICES_LEGEND,
  LIST_HEADS,
  LOCKED_LINE,
  LOOK_UP,
  NOT_VALID,
  NO_DEVICES,
  REVOKE,
  REVOKE_ARMED,
  ROW_ADDRESS,
  ROW_DEVICE,
  ROW_PLATFORM,
  ROW_REQUESTED,
  SWITCHES,
  SWITCH_OFF,
  SWITCH_ON,
  accessBody,
  codeMessage,
  deviceChangeMessage,
  lastSeenText,
  metaText,
  normaliseDeviceCode,
  orderDevices,
  readAccess,
  readDevices,
  readLookup,
  requestedText,
} from "./devicestate.js";
import type { Access, DeviceRow, Lookup, SwitchKey } from "./devicestate.js";
import { el } from "./dom.js";
import { UNREACHABLE } from "./loginstate.js";
import { StepUpCancelled, withStepUp } from "./stepup.js";
import { openLayer } from "./weblayer.js";
import type { Layer } from "./weblayer.js";
import { failureOf } from "./websessionstate.js";

/** The style sheets of every layer of this file: the web dialogs' own, the switch of the vault, and the rules of this file. */
const SHEETS = ["./websession.css", "./vault.css", "./devices.css"];

/** The outcome of a change: it was made, the person cancelled the confirmation, or the server refused it. */
export type Changed<T> = { ok: true; value: T } | { ok: false; cancelled: true } | { ok: false; cancelled: false; status: number; body?: Record<string, unknown> };

/** Run a change through the step-up rule and say how it ended; it never throws. */
export async function change<T>(run: () => Promise<T>): Promise<Changed<T>> {
  try {
    return { ok: true, value: await withStepUp(run) };
  } catch (e) {
    if (e instanceof StepUpCancelled) return { ok: false, cancelled: true };
    return { ok: false, cancelled: false, ...failureOf(e) };
  }
}

// ---- the dialog `Authorize device` ------------------------------------------------------------------------------------------

/** The dialog or the small layer that is on screen: there is one at a time. */
let shown: { end: () => void } | null = null;

/** Whether one of the layers of this file is open. */
export function authorizeOpen(): boolean {
  return shown !== null;
}

/** What the status line of the dialog says: its words, and the second cue besides colour. */
type Tone = "approved" | "denied" | "bad";

const GLYPH: Record<Tone, string> = { approved: "●", denied: "○", bad: "!" };

/**
 * Open the dialog for a code that is well formed (`normaliseDeviceCode` made it): the lookup comes first, with a recent sign-in, and
 * the dialog shows who asks and from where; `Approve` and `Deny` decide it. A code that is not valid says so and shows nothing else.
 * A second call while a dialog is open does nothing.
 */
export async function openAuthorize(code: string): Promise<void> {
  if (shown !== null) return;
  const clean = normaliseDeviceCode(code);
  if (clean === null) return;
  let closed = false;
  let working = false;
  const layer = await openLayer({
    cls: "device-authorize",
    labelledBy: "dev-title",
    returnTo: null,
    sheets: SHEETS,
    onEscape: () => end(),
  });
  if (shown !== null) {
    layer.close();
    return;
  }
  function end(): void {
    if (closed) return;
    closed = true;
    shown = null;
    layer.close();
  }
  shown = { end };
  const box = layer.box;
  box.setAttribute("aria-busy", "true");
  const title = el("h2", "set-title dev-title", box);
  title.id = "dev-title";
  title.textContent = AUTHORIZE_TITLE;
  // The heading takes the first focus: nothing that decides is pre-selected, and the dialog is read from its name.
  title.tabIndex = -1;
  const body = el("div", "dev-body", box);
  const status = el("p", "set-status dev-status", box);
  status.setAttribute("role", "status");
  const actions = el("div", "dlg-actions", box);
  title.focus();

  const say = (tone: Tone | null, text: string): void => {
    status.textContent = "";
    if (tone === null) {
      status.textContent = text;
      delete status.dataset.tone;
      return;
    }
    status.dataset.tone = tone;
    const glyph = el("span", "dev-glyph", status);
    glyph.textContent = GLYPH[tone];
    glyph.setAttribute("aria-hidden", "true");
    el("span", "dev-word", status).textContent = text;
  };

  /** After a result the two buttons give way to `Close`. */
  const finish = (): void => {
    actions.textContent = "";
    const close = el("button", "web-btn", actions);
    close.type = "button";
    close.textContent = CLOSE;
    close.addEventListener("click", end);
    close.focus();
  };

  const draw = (who: Lookup): void => {
    const now = Date.now();
    const rows = el("dl", "dev-kv", body);
    const pairs: Array<[string, string, boolean]> = [
      [ROW_DEVICE, who.name, false],
      [ROW_PLATFORM, who.platform, false],
      [ROW_ADDRESS, who.addr, true],
      [ROW_REQUESTED, requestedText(who.requestedMs, now), false],
    ];
    for (const [label, value, mono] of pairs) {
      el("dt", "dev-k", rows).textContent = label;
      el("dd", mono ? "dev-v dev-mono" : "dev-v", rows).textContent = value;
    }
    const numbers = el("div", "dev-code", body);
    numbers.setAttribute("role", "group");
    numbers.setAttribute("aria-label", CODE_LABEL);
    numbers.textContent = clean;
    el("p", "dev-sentence", body).textContent = APPROVE_SENTENCE;
    const deny = el("button", "web-btn", actions);
    deny.type = "button";
    deny.textContent = DENY;
    const approve = el("button", "web-btn web-solid", actions);
    approve.type = "button";
    approve.textContent = APPROVE;
    const decide = (path: "approve" | "deny", ok: string, tone: Tone, pressed: HTMLButtonElement): void => {
      if (working) return;
      working = true;
      box.setAttribute("aria-busy", "true");
      say(null, "");
      deny.disabled = true;
      approve.disabled = true;
      void (async () => {
        const r = await change(() => http("POST", `/api/device/${path}`, { code: clean }));
        working = false;
        box.removeAttribute("aria-busy");
        if (closed) return;
        if (r.ok) {
          say(tone, ok);
          return finish();
        }
        deny.disabled = false;
        approve.disabled = false;
        if (r.cancelled) return void pressed.focus();
        // A code that was decided or ran out in between is as gone as one that never was.
        if (r.status === 404 || r.status === 410) {
          say("bad", NOT_VALID);
          body.textContent = "";
          return finish();
        }
        say("bad", codeMessage(r.status, r.body));
        pressed.focus();
      })();
    };
    approve.addEventListener("click", () => decide("approve", APPROVED, "approved", approve));
    deny.addEventListener("click", () => decide("deny", DENIED, "denied", deny));
  };

  working = true;
  const looked = await change(() => http("POST", "/api/device/lookup", { code: clean }));
  working = false;
  if (closed) return;
  box.removeAttribute("aria-busy");
  // The person did not confirm who they are: nothing was asked and nothing is decided, so there is nothing to show.
  if (!looked.ok && looked.cancelled) return end();
  const who = looked.ok ? readLookup(looked.value) : null;
  if (who === null) {
    say("bad", looked.ok ? NOT_VALID : codeMessage(looked.status, looked.body));
    return finish();
  }
  draw(who);
  title.focus();
}

// ---- the layer of a typed code ------------------------------------------------------------------------------------------

/**
 * The palette entry `Authorize device…`: one field for a code typed from the screen of the app (the dash optional, the case
 * ignored). A code that is not well formed is refused here, before any request, with the one sentence; a well formed one opens the
 * dialog above.
 */
export async function openTypedCode(): Promise<void> {
  if (shown !== null) return;
  let closed = false;
  const layer = await openLayer({
    cls: "device-code",
    labelledBy: "dev-code-title",
    returnTo: null,
    sheets: SHEETS,
    onEscape: () => end(),
    onEnter: () => submit(),
  });
  if (shown !== null) {
    layer.close();
    return;
  }
  function end(): void {
    if (closed) return;
    closed = true;
    shown = null;
    layer.close();
  }
  shown = { end };
  const box = layer.box;
  const title = el("h2", "set-title", box);
  title.id = "dev-code-title";
  title.textContent = AUTHORIZE_TITLE;
  const field = el("div", "web-field", box);
  const label = el("label", "label web-label", field);
  label.htmlFor = "dev-code-field";
  label.textContent = CODE_LABEL;
  const input = el("input", "web-input dev-input", field);
  input.id = "dev-code-field";
  input.type = "text";
  input.maxLength = 32;
  input.setAttribute("autocomplete", "off");
  input.setAttribute("autocapitalize", "characters");
  input.setAttribute("autocorrect", "off");
  input.spellcheck = false;
  const err = el("p", "web-err", box);
  err.id = "dev-code-error";
  err.setAttribute("role", "alert");
  const actions = el("div", "dlg-actions", box);
  const cancel = el("button", "web-btn", actions);
  cancel.type = "button";
  cancel.textContent = CANCEL;
  const go = el("button", "web-btn web-solid", actions);
  go.type = "button";
  go.textContent = LOOK_UP;

  function submit(): void {
    if (closed) return;
    const code = normaliseDeviceCode(input.value);
    if (code === null) {
      err.textContent = NOT_VALID;
      input.setAttribute("aria-invalid", "true");
      input.setAttribute("aria-describedby", err.id);
      input.focus();
      return;
    }
    input.value = "";
    end();
    void openAuthorize(code);
  }
  input.addEventListener("input", () => {
    err.textContent = "";
    input.removeAttribute("aria-invalid");
    input.removeAttribute("aria-describedby");
  });
  cancel.addEventListener("click", end);
  go.addEventListener("click", submit);
  input.focus();
}

// ---- what the groups need from the dialog that holds them -----------------------------------------------------------------------

/** What a group asks of its dialog. */
export interface GroupHost {
  /** The dialog's box: the scope in which an armed button is disarmed again. */
  box: HTMLElement;
  /** The line of the dialog that says how a change ended. */
  say(text: string): void;
  /** Whether the dialog is still open: an answer that comes later draws nothing. */
  isOpen(): boolean;
  /** A refused change that the dialog owns (an administrator whose role was taken away); true when it was handled. */
  refused?(status: number, body?: Record<string, unknown>): boolean;
  /** Where the focus goes when the control that had it is gone. */
  fallback?(): void;
}

/** What a group offers its dialog. */
export interface Group {
  /** Ask the server and draw; false when the server could not be read (nothing is drawn then, and the dialog's line says so). */
  load(): Promise<boolean>;
  /** End the listeners of the last drawing. */
  end(): void;
}

// ---- the group `Devices` ------------------------------------------------------------------------------------------------------

/**
 * The group `Devices`: `person` is the list of the signed-in person, `admin` the list of everybody with the column `Owner`. Each row has
 * `Revoke`, which asks with a second press. The list is read from the server after every change: the page never edits its own copy.
 */
export function buildDevicesGroup(parent: Element, kind: "person" | "admin", host: GroupHost): Group {
  const admin = kind === "admin";
  const group = el("fieldset", "set-group dev-group", parent);
  group.setAttribute("role", "group");
  group.dataset.group = "devices";
  group.dataset.kind = kind;
  el("legend", "label", group).textContent = DEVICES_LEGEND;
  const list = el("div", "dev-list", group);
  let drawn = new AbortController();
  let working = false;
  let sequence = 0;

  const draw = (rows: DeviceRow[]): void => {
    drawn.abort();
    drawn = new AbortController();
    list.textContent = "";
    if (rows.length === 0) {
      el("p", "dev-empty", list).textContent = NO_DEVICES;
      return;
    }
    const now = Date.now();
    const ordered = orderDevices(rows);
    if (admin) {
      const table = el("table", "dev-table", list);
      const head = el("tr", "", el("thead", "", table));
      for (const name of [...LIST_HEADS, ""]) {
        const th = el("th", "label", head);
        th.scope = "col";
        th.textContent = name;
      }
      const tbody = el("tbody", "", table);
      for (const row of ordered) {
        const tr = el("tr", "dev-row", tbody);
        tr.dataset.id = row.id;
        const name = el("td", "dev-cell dev-name", tr);
        name.textContent = row.name;
        el("td", "dev-cell dev-owner", tr).textContent = row.owner ?? "";
        el("td", "dev-cell dev-platform", tr).textContent = row.platform;
        el("td", "dev-cell dev-seen", tr).textContent = lastSeenText(row.lastSeenMs, now);
        const act = el("td", "dev-cell dev-act", tr);
        revokeButton(el("button", "dev-revoke", act), row, name);
      }
      return;
    }
    const ul = el("ul", "dev-items", list);
    for (const row of ordered) {
      const li = el("li", "dev-item", ul);
      li.dataset.id = row.id;
      const name = el("span", "dev-name", li);
      name.textContent = row.name;
      el("span", "dev-meta", li).textContent = metaText(row, now);
      revokeButton(el("button", "dev-revoke", li), row, name);
    }
  };

  const path = (row: DeviceRow): string => `/api/${admin ? "admin" : "me"}/devices/${encodeURIComponent(row.id)}`;

  function revokeButton(button: HTMLButtonElement, row: DeviceRow, name: HTMLElement): void {
    // The words of the button change when it is armed, so it keeps them as its name; the device it concerns is its description.
    name.id = `dev-n-${++sequence}`;
    button.setAttribute("aria-describedby", name.id);
    armed(button, REVOKE, REVOKE_ARMED, host.box, drawn.signal, async () => {
      if (working) return;
      working = true;
      host.box.setAttribute("aria-busy", "true");
      host.say("");
      const r = await change(() => http("DELETE", path(row)));
      working = false;
      host.box.removeAttribute("aria-busy");
      if (!host.isOpen()) return;
      if (r.ok || (!r.cancelled && r.status === 404)) {
        // Whatever the server holds now is what the list shows; a device that is gone already is gone.
        if (r.ok && !admin) host.say(SAVED);
        if (await load()) group.querySelector<HTMLElement>(".dev-revoke")?.focus();
        if (group.querySelector(".dev-revoke") === null) host.fallback?.();
        return;
      }
      if (r.cancelled) return void button.focus();
      if (host.refused?.(r.status, r.body) === true) return;
      host.say(deviceChangeMessage(r.status, r.body));
      button.focus();
    });
  }

  async function load(): Promise<boolean> {
    let rows: DeviceRow[] | null;
    try {
      rows = readDevices(await http("GET", `/api/${admin ? "admin" : "me"}/devices`), admin);
    } catch (e) {
      const f = failureOf(e);
      if (host.isOpen() && host.refused?.(f.status, f.body) !== true) host.say(UNREACHABLE);
      return false;
    }
    if (!host.isOpen()) return false;
    if (rows === null) {
      host.say(UNREACHABLE);
      return false;
    }
    draw(rows);
    return true;
  }

  return { load, end: () => drawn.abort() };
}

// ---- the group `Device access` ------------------------------------------------------------------------------------------------

/**
 * The group `Device access`: one switch for the sync target and one for remote sessions, the ones of the Android Security row at the
 * desktop's size. The page draws only what the server answers: a press asks for a fresh sign-in, posts the change and draws the
 * answer, so a refusal leaves the switch where the server has it. A switch that the operator's configuration has turned off for good
 * is drawn `OFF`, cannot be operated and says why.
 */
export function buildAccessGroup(parent: Element, host: GroupHost): Group {
  const group = el("fieldset", "set-group dev-group dev-access", parent);
  group.setAttribute("role", "group");
  group.dataset.group = "device-access";
  el("legend", "label", group).textContent = ACCESS_LEGEND;
  const rows = el("div", "dev-switches", group);
  const help = el("p", "dev-help", group);
  help.textContent = ACCESS_HELP;
  let working = false;
  let sequence = 0;

  const draw = (access: Access): void => {
    const hadFocus = group.contains(document.activeElement) ? (document.activeElement as HTMLElement).dataset.switch : undefined;
    rows.textContent = "";
    for (const { key, label } of SWITCHES) {
      const state = access[key];
      const id = `dev-sw-${++sequence}`;
      const row = el("div", "dev-sw-row", rows);
      row.dataset.switch = key;
      const name = el("span", "dev-sw-label", row);
      name.id = `${id}-l`;
      name.textContent = label;
      const toggle = el("button", "vault-switch dev-switch", row);
      toggle.type = "button";
      toggle.dataset.switch = key;
      toggle.dataset.on = String(state.on);
      toggle.setAttribute("role", "switch");
      toggle.setAttribute("aria-checked", String(state.on));
      toggle.setAttribute("aria-labelledby", name.id);
      el("span", "vault-switch-word", toggle).textContent = state.on ? SWITCH_ON : SWITCH_OFF;
      el("i", "", toggle);
      if (state.locked) {
        toggle.dataset.locked = "true";
        toggle.setAttribute("aria-disabled", "true");
        const why = el("p", "dev-dim", rows);
        why.id = `${id}-w`;
        why.textContent = LOCKED_LINE;
        toggle.setAttribute("aria-describedby", why.id);
      }
      toggle.addEventListener("click", () => void press(key, state));
      if (hadFocus === key) toggle.focus();
    }
  };

  async function press(key: SwitchKey, state: { on: boolean; locked: boolean }): Promise<void> {
    // A switch that the configuration has closed is not operable; the server refuses it as well.
    if (working || state.locked) return;
    working = true;
    group.setAttribute("aria-busy", "true");
    host.say("");
    const r = await change(() => http("POST", "/api/admin/device-access", accessBody(key, !state.on)));
    working = false;
    group.removeAttribute("aria-busy");
    if (!host.isOpen()) return;
    if (r.ok) {
      const next = readAccess(r.value);
      if (next === null) return void host.say(CHANGE_FAILED);
      return draw(next);
    }
    if (r.cancelled) return;
    if (host.refused?.(r.status, r.body) === true) return;
    host.say(deviceChangeMessage(r.status, r.body));
    // The configuration may have closed the switch since the group was drawn: read what the server holds.
    if (r.status === 409) await load();
  }

  async function load(): Promise<boolean> {
    let access: Access | null;
    try {
      access = readAccess(await http("GET", "/api/admin/device-access"));
    } catch (e) {
      const f = failureOf(e);
      if (host.isOpen() && host.refused?.(f.status, f.body) !== true) host.say(UNREACHABLE);
      return false;
    }
    if (!host.isOpen()) return false;
    if (access === null) {
      host.say(UNREACHABLE);
      return false;
    }
    draw(access);
    return true;
  }

  return { load, end: () => {} };
}
