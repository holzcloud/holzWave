// The people dialog (docs/design.md 6.29): an administrator sees who has an account on this server and invites people. It shows a
// name, a role, a state and the last sign-in of each person, and nothing else of anyone: no secret, no terminal, no command, no
// layout, and no control that suggests it. Every change goes through `withStepUp`, so a refusal that asks for a recent sign-in shows
// `Confirm it is you` once and the change is repeated once.
//
// The invitation link is a secret that is shown once. It stands in one element only, put there with `textContent`, and that element
// is emptied and removed whenever the layer ends: `Done`, `Escape`, a closed dialog or a new invitation. It is never put in the
// address, the title, the status line, an attribute or the storage of the browser. Every string the server sends is put in with
// `textContent` in an element of its own. The only request function is `http` of the bridge.
import { registerAction } from "./actions.js";
import { http } from "./bridge.js";
import { CHANGE_FAILED } from "./accountstate.js";
import { el } from "./dom.js";
import { UNREACHABLE, messageFor } from "./loginstate.js";
import { DEFAULT_DAYS, INVITE_DAYS, LIST_NOTE, inviteBody, inviteNote, lastSigninText, readAccounts, readInvitation, stateWord } from "./peoplestate.js";
import type { People, Role } from "./peoplestate.js";
import { StepUpCancelled, withStepUp } from "./stepup.js";
import { openLayer } from "./weblayer.js";
import type { Layer } from "./weblayer.js";
import { sessionAccount } from "./websession.js";
import { failureOf } from "./websessionstate.js";

/** The dialog on screen, if any: there is one at a time. */
let shown: Layer | null = null;
let registered = false;

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

/** The sentence of a change that did not happen. `status` is 0 when nothing arrived. */
function refusalText(status: number, body?: Record<string, unknown>): string {
  if (status === 0 || status === 429 || status === 503 || status >= 500) return messageFor(status, body);
  return CHANGE_FAILED;
}

/**
 * Register the palette entry `People…` when the person who is signed in is an administrator. It runs once the first lookup of the
 * session has ended; on the desktop there is no account, so nothing is registered.
 */
export function initPeople(): void {
  if (registered || sessionAccount()?.role !== "admin") return;
  registered = true;
  registerAction({ id: "people.open", title: "People…", run: () => void openPeople() });
}

/** Select the text of `node`: what a copy falls back to when the browser does not give the clipboard. */
function selectText(node: Node): void {
  const selection = getSelection();
  if (selection === null) return;
  const range = document.createRange();
  range.selectNodeContents(node);
  selection.removeAllRanges();
  selection.addRange(range);
}

/** Open the dialog; a second call while it is open does nothing. */
export async function openPeople(): Promise<void> {
  if (shown !== null) return;
  /** What ends the invitation layer that is open, if one is. */
  let endInvite: (() => void) | null = null;
  /** The invitation layer is open or being built: a second press of `Invite` does nothing. */
  let inviteOpen = false;
  let dialogOpen = true;

  const layer = await openLayer({
    cls: "people",
    labelledBy: "ppl-title",
    returnTo: null,
    sheets: ["./websession.css", "./people.css"],
    onEscape: () => closeAll(),
  });
  if (shown !== null) {
    layer.close();
    return;
  }
  shown = layer;
  const box = layer.box;
  let people: People | null = null;

  const head = el("div", "ppl-head", box);
  const title = el("h2", "set-title", head);
  title.id = "ppl-title";
  title.textContent = "People";
  const invite = el("button", "ppl-invite", head);
  invite.type = "button";
  invite.textContent = "Invite";
  const status = el("p", "set-status", box);
  status.setAttribute("role", "status");
  const body = el("div", "ppl-body", box);
  const actions = el("div", "dlg-actions", box);
  const close = el("button", "", actions);
  close.type = "button";
  close.textContent = "Close";

  const say = (text: string): void => {
    status.textContent = text;
  };

  function closeAll(): void {
    dialogOpen = false;
    endInvite?.();
    layer.close();
    shown = null;
  }
  close.addEventListener("click", closeAll);

  // ---- the table ----------------------------------------------------------------------------------------------------------

  const table = el("table", "ppl-table", body);
  const headRow = el("tr", "", el("thead", "", table));
  for (const name of ["Name", "Role", "State", "Last sign-in", ""]) {
    const th = el("th", "label", headRow);
    th.scope = "col";
    th.textContent = name;
  }
  const tbody = el("tbody", "", table);
  el("p", "ppl-note", body).textContent = LIST_NOTE;

  /** Draw the rows of the last reading. */
  function draw(): void {
    if (people === null) return;
    tbody.textContent = "";
    const now = Date.now();
    for (const row of people.rows) {
      const tr = el("tr", "ppl-row", tbody);
      tr.dataset.id = row.id;
      const name = el("td", "ppl-cell", tr);
      el("span", "ppl-name", name).textContent = row.displayName === "" ? row.username : row.displayName;
      el("span", "ppl-user", name).textContent = row.username;
      el("td", "ppl-cell ppl-role", tr).textContent = row.role;
      const state = el("td", "ppl-cell ppl-state", tr);
      const word = stateWord(row.state);
      if (word !== null) {
        state.dataset.state = row.state;
        state.dataset.tone = word.tone;
        const glyph = el("span", "ppl-glyph", state);
        glyph.textContent = word.glyph;
        glyph.setAttribute("aria-hidden", "true");
        el("span", "ppl-word", state).textContent = word.word;
      }
      el("td", "ppl-cell ppl-last", tr).textContent = lastSigninText(row.lastSigninMs, now);
      el("td", "ppl-cell ppl-act", tr);
    }
  }

  /** Ask the server for the accounts and draw them. When that fails only `Close` is left. */
  async function load(): Promise<boolean> {
    try {
      const read = readAccounts(await http("GET", "/api/admin/accounts"));
      if (read === null) throw new Error("not a list of accounts");
      people = read;
    } catch {
      say(UNREACHABLE);
      invite.hidden = true;
      body.hidden = true;
      close.focus();
      return false;
    }
    draw();
    return true;
  }

  // ---- the invitation -----------------------------------------------------------------------------------------------------

  invite.addEventListener("click", () => {
    if (inviteOpen) return;
    inviteOpen = true;
    void openInvite();
  });

  async function openInvite(): Promise<void> {
    let busy = false;
    let closed = false;
    /** The one element that holds a link; empty and removed when the layer ends. */
    let linkText: HTMLElement | null = null;
    const inviteLayer = await openLayer({
      cls: "people-invite",
      labelledBy: "ppl-inv-title",
      returnTo: invite,
      sheets: ["./websession.css", "./people.css"],
      onEscape: () => end(),
    });
    const end = (): void => {
      if (closed) return;
      closed = true;
      inviteOpen = false;
      if (linkText !== null) linkText.textContent = "";
      linkText?.remove();
      linkText = null;
      endInvite = null;
      inviteLayer.close();
    };
    if (!dialogOpen) return end();
    endInvite = end;
    const inviteBox = inviteLayer.box;
    const heading = el("h2", "set-title", inviteBox);
    heading.id = "ppl-inv-title";
    heading.textContent = "Invite";
    const form = el("div", "ppl-form", inviteBox);

    const roles = el("fieldset", "ppl-roles", form);
    roles.setAttribute("role", "group");
    el("legend", "label web-label", roles).textContent = "Role";
    const radios = new Map<Role, HTMLInputElement>();
    for (const [value, text] of [["member", "Member"], ["admin", "Administrator"]] as const) {
      const option = el("label", "ppl-option", roles);
      const radio = el("input", "", option);
      radio.type = "radio";
      radio.name = "ppl-role";
      radio.value = value;
      radio.checked = value === "member";
      option.append(text);
      radios.set(value, radio);
    }
    const dayGroup = el("fieldset", "ppl-roles ppl-days", form);
    dayGroup.setAttribute("role", "group");
    el("legend", "label web-label", dayGroup).textContent = "Days";
    const dayRadios = new Map<number, HTMLInputElement>();
    const dayRow = el("div", "ppl-options", dayGroup);
    for (const n of INVITE_DAYS) {
      const option = el("label", "ppl-option", dayRow);
      const radio = el("input", "", option);
      radio.type = "radio";
      radio.name = "ppl-days";
      radio.value = String(n);
      radio.checked = n === DEFAULT_DAYS;
      option.append(String(n));
      dayRadios.set(n, radio);
    }
    const err = el("p", "web-err", form);
    err.setAttribute("role", "alert");
    const buttons = el("div", "dlg-actions", form);
    const cancel = el("button", "web-btn", buttons);
    cancel.type = "button";
    cancel.textContent = "Cancel";
    const create = el("button", "web-btn web-solid", buttons);
    create.type = "button";
    create.textContent = "Create link";
    cancel.addEventListener("click", end);

    create.addEventListener("click", () => {
      if (busy) return;
      const role: Role = radios.get("admin")?.checked === true ? "admin" : "member";
      const picked = INVITE_DAYS.find((n) => dayRadios.get(n)?.checked === true) ?? DEFAULT_DAYS;
      const chosen = inviteBody(role, picked);
      busy = true;
      create.disabled = true;
      create.textContent = "Create link…";
      inviteBox.setAttribute("aria-busy", "true");
      err.textContent = "";
      void (async () => {
        const r = await change(() => http("POST", "/api/admin/invitations", chosen));
        busy = false;
        inviteBox.removeAttribute("aria-busy");
        if (closed) return;
        create.disabled = false;
        create.textContent = "Create link";
        if (!r.ok) {
          if (!r.cancelled) err.textContent = refusalText(r.status, r.body);
          create.focus();
          return;
        }
        const link = readInvitation(r.value);
        if (link === null) {
          err.textContent = CHANGE_FAILED;
          create.focus();
          return;
        }
        showLink(link, chosen.days, chosen.role);
      })();
    });

    /** Replace the form with the link, shown once. */
    function showLink(link: string, count: number, role: Role): void {
      form.remove();
      heading.textContent = "Invitation link";
      const view = el("div", "ppl-view", inviteBox);
      el("p", "ppl-link-note", view).textContent = inviteNote(count, role);
      const field = el("div", "ppl-linkfield", view);
      el("span", "label web-label", field).textContent = "Link";
      const row = el("div", "ppl-linkrow", field);
      const boxed = el("div", "ppl-link-box", row);
      const text = el("span", "ppl-link-text", boxed);
      text.textContent = link;
      linkText = text;
      const copy = el("button", "web-btn", row);
      copy.type = "button";
      copy.textContent = "Copy link";
      copy.addEventListener("click", () => {
        const clipboard = navigator.clipboard as Clipboard | undefined;
        if (clipboard === undefined || typeof clipboard.writeText !== "function") {
          selectText(text);
          return;
        }
        clipboard.writeText(link).catch(() => {
          if (text.isConnected) selectText(text);
        });
      });
      const done = el("div", "dlg-actions", view);
      const ok = el("button", "web-btn web-solid", done);
      ok.type = "button";
      ok.textContent = "Done";
      ok.addEventListener("click", end);
      copy.focus();
    }

    (radios.get("member") ?? create).focus();
  }

  await load();
  if (!invite.hidden) invite.focus();
}
