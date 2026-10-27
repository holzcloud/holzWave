// The people dialog (docs/design.md 6.29): an administrator sees who has an account on this server, invites people, and changes an
// account: the role, disable and enable, lift a sign-in lock-out, reset the secrets and delete. It shows a name, a role, a state and
// the last sign-in of each person, and nothing else of anyone: no secret, no terminal, no command, no layout, and no control that
// suggests it. Every change goes through `withStepUp`, so a refusal that asks for a recent sign-in shows `Confirm it is you` once
// and the change is repeated once. `Delete` and `Reset secrets` ask first, and the last active administrator cannot be demoted,
// disabled or deleted from the menu.
//
// The invitation link is a secret that is shown once. It stands in one element only, put there with `textContent`, and that element
// is emptied and removed whenever the layer ends: `Done`, `Escape`, a closed dialog or a new invitation. It is never put in the
// address, the title, the status line, an attribute or the storage of the browser. Every string the server sends, a name above all,
// is put in with `textContent` in an element of its own, so none of it is ever markup. The only request function is `http` of the
// bridge.
import { registerAction } from "./actions.js";
import { http } from "./bridge.js";
import { CHANGE_FAILED } from "./accountstate.js";
import { el } from "./dom.js";
import { UNREACHABLE } from "./loginstate.js";
import {
  ADMIN_ONLY,
  DEFAULT_DAYS,
  INVITE_DAYS,
  LIST_NOTE,
  confirmFor,
  inviteBody,
  inviteNote,
  lastSigninText,
  menuFor,
  outcomeOf,
  readAccounts,
  readInvitation,
  requestFor,
  stateWord,
} from "./peoplestate.js";
import type { Confirmation, Entry, People, Role, Row } from "./peoplestate.js";
import { StepUpCancelled, withStepUp } from "./stepup.js";
import { openLayer } from "./weblayer.js";
import type { Layer } from "./weblayer.js";
import { sessionAccount } from "./websession.js";
import { failureOf } from "./websessionstate.js";

/** The dialog on screen, if any: there is one at a time. */
let shown: Layer | null = null;
let registered = false;
/** Gives each description of a menu its own id. */
let sequence = 0;

/** The outcome of a change: it was made, the person cancelled the confirmation, or the server refused it. */
type Result<T> = { ok: true; value: T } | { ok: false; cancelled: true } | { ok: false; cancelled: false; status: number; body?: Record<string, unknown> };

/** Run a change through the step-up rule and say how it ended; it never throws. */
async function change<T>(run: () => Promise<T>): Promise<Result<T>> {
  try {
    return { ok: true, value: await withStepUp(run) };
  } catch (e) {
    if (e instanceof StepUpCancelled) return { ok: false, cancelled: true };
    return { ok: false, cancelled: false, ...failureOf(e) };
  }
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

/** How the dialog names a person: the display name, or the username when there is none. */
const nameOf = (row: Row): string => (row.displayName === "" ? row.username : row.displayName);

/** Open the dialog; a second call while it is open does nothing. */
export async function openPeople(): Promise<void> {
  if (shown !== null) return;
  /** What ends the invitation layer that is open, if one is. */
  let endInvite: (() => void) | null = null;
  /** The invitation layer is open or being built: a second press of `Invite` does nothing. */
  let inviteOpen = false;
  let dialogOpen = true;
  /** What ends the question that is open, if one is. */
  let endAsk: (() => void) | null = null;
  /** The menu that is open: its row, its button and its element. */
  let menu: { id: string; button: HTMLButtonElement; node: HTMLElement; tr: HTMLElement } | null = null;
  /** A change is on its way: nothing else is started until it ends. */
  let busy = false;

  const layer = await openLayer({
    cls: "people",
    labelledBy: "ppl-title",
    returnTo: null,
    sheets: ["./websession.css", "./people.css"],
    onEscape: () => {
      // The menu is closed first and the dialog second.
      if (menu !== null) closeMenu(true);
      else closeAll();
    },
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
  const invite = el("button", "web-btn ppl-invite", head);
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
    endAsk?.();
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

  /** Ends the listeners of the last drawing. */
  let drawn = new AbortController();

  /** Draw the rows of the last reading. */
  function draw(): void {
    if (people === null) return;
    closeMenu(false);
    drawn.abort();
    drawn = new AbortController();
    tbody.textContent = "";
    const now = Date.now();
    for (const row of people.rows) {
      const tr = el("tr", "ppl-row", tbody);
      tr.dataset.id = row.id;
      const name = el("td", "ppl-cell", tr);
      el("span", "ppl-name", name).textContent = nameOf(row);
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
      const cell = el("td", "ppl-cell ppl-act", tr);
      const more = el("button", "ppl-more", cell);
      more.type = "button";
      more.textContent = "⋯";
      more.setAttribute("aria-label", `Actions for ${nameOf(row)}`);
      more.setAttribute("aria-haspopup", "menu");
      more.setAttribute("aria-expanded", "false");
      more.addEventListener("click", () => {
        if (busy) return;
        if (menu?.id === row.id) closeMenu(true);
        else openMenu(row, more, tr, cell);
      });
    }
  }

  /** The button of a row's menu, to put the focus back on it. */
  const buttonOf = (id: string): HTMLButtonElement | null => {
    for (const tr of tbody.querySelectorAll<HTMLElement>("tr")) if (tr.dataset.id === id) return tr.querySelector<HTMLButtonElement>(".ppl-more");
    return null;
  };

  /** Put the focus on the button of the row, or on the first control when the row is gone. */
  const refocus = (id: string): void => {
    (buttonOf(id) ?? (invite.hidden ? close : invite)).focus();
  };

  /** Take the dialog back to the one sentence and `Close`: the person may not be here, or the server cannot be read. */
  function lockOut(text: string): void {
    closeMenu(false);
    endAsk?.();
    endInvite?.();
    say(text);
    invite.hidden = true;
    body.hidden = true;
    close.focus();
  }

  /** Ask the server for the accounts and draw them. When that fails only `Close` is left. */
  async function load(): Promise<boolean> {
    try {
      const read = readAccounts(await http("GET", "/api/admin/accounts"));
      if (read === null) throw new Error("not a list of accounts");
      people = read;
    } catch (e) {
      lockOut(outcomeOf(failureOf(e).status, failureOf(e).body).kind === "locked" ? ADMIN_ONLY : UNREACHABLE);
      return false;
    }
    draw();
    return true;
  }

  // ---- the menu of a row --------------------------------------------------------------------------------------------------

  /** Take the menu away. With `refocus` the keyboard goes back to its button. */
  function closeMenu(back: boolean): void {
    if (menu === null) return;
    const open = menu;
    menu = null;
    open.node.remove();
    open.tr.classList.remove("ppl-open");
    open.button.setAttribute("aria-expanded", "false");
    if (back) open.button.focus();
  }

  function openMenu(row: Row, button: HTMLButtonElement, tr: HTMLElement, cell: HTMLElement): void {
    closeMenu(false);
    if (people === null) return;
    const entries = menuFor(row, people.admins);
    const node = el("div", "ppl-menu", cell);
    node.setAttribute("role", "menu");
    node.setAttribute("aria-label", `Actions for ${nameOf(row)}`);
    const why = el("p", "ppl-why");
    why.id = `ppl-why-${++sequence}`;
    const items: HTMLButtonElement[] = [];
    let held = false;
    for (const entry of entries) {
      // `Reset secrets` and `Delete` come after a hairline.
      if (entry.id === "reset") el("div", "ppl-sep", node).setAttribute("role", "separator");
      const item = el("button", entry.danger ? "ppl-item ppl-danger" : "ppl-item", node);
      item.type = "button";
      item.setAttribute("role", "menuitem");
      item.textContent = entry.label;
      if (entry.reason !== null) {
        // Held back, not removed: the entry stays in the list, dim, and the reason is attached to it.
        item.setAttribute("aria-disabled", "true");
        item.setAttribute("aria-describedby", why.id);
        why.textContent = entry.reason;
        held = true;
      }
      item.addEventListener("click", () => void choose(entry, row, button));
      items.push(item);
    }
    if (held) node.append(why);
    node.addEventListener("keydown", (e) => {
      const at = items.indexOf(document.activeElement as HTMLButtonElement);
      let to = -1;
      if (e.key === "ArrowDown") to = at < 0 || at === items.length - 1 ? 0 : at + 1;
      else if (e.key === "ArrowUp") to = at <= 0 ? items.length - 1 : at - 1;
      else if (e.key === "Home") to = 0;
      else if (e.key === "End") to = items.length - 1;
      if (to < 0) return;
      e.preventDefault();
      items[to]?.focus();
    });
    menu = { id: row.id, button, node, tr };
    tr.classList.add("ppl-open");
    button.setAttribute("aria-expanded", "true");
    items[0]?.focus();
  }

  // A press anywhere else in the dialog closes the menu without moving the focus.
  box.addEventListener("pointerdown", (e) => {
    if (menu !== null && e.target instanceof Node && !menu.node.contains(e.target) && !menu.button.contains(e.target)) closeMenu(false);
  });

  /** An entry was chosen. One that is held back does nothing; one that asks first opens its question. */
  async function choose(entry: Entry, row: Row, button: HTMLButtonElement): Promise<void> {
    if (entry.reason !== null || busy) return;
    const ask = confirmFor(entry.id, nameOf(row));
    if (ask !== null) {
      closeMenu(false);
      await openAsk(ask, entry, row, button);
      return;
    }
    const r = await send(entry, row, true);
    await after(r, row);
  }

  /** Send the change of an entry. While it is on its way the entries of the open menu are disabled. */
  async function send(entry: Entry, row: Row, inMenu: boolean): Promise<Result<unknown>> {
    busy = true;
    box.setAttribute("aria-busy", "true");
    say("");
    if (inMenu) for (const item of menu?.node.querySelectorAll("button") ?? []) item.disabled = true;
    const request = requestFor(entry.id, row);
    const r = await change(() => http(request.method, request.path, request.body));
    busy = false;
    box.removeAttribute("aria-busy");
    return r;
  }

  /** What a change that ended means: read the list again, say a sentence, or leave the dialog as it was. */
  async function after(r: Result<unknown>, row: Row): Promise<void> {
    closeMenu(false);
    if (!dialogOpen) return;
    if (r.ok) {
      if (await load()) refocus(row.id);
      return;
    }
    if (r.cancelled) return refocus(row.id);
    const o = outcomeOf(r.status, r.body);
    if (o.kind === "locked") return lockOut(ADMIN_ONLY);
    if (o.kind === "gone") {
      // The person is gone: the list is read again, with no sentence.
      if (await load()) refocus(row.id);
      return;
    }
    say(o.text);
    refocus(row.id);
  }

  // ---- the question of `Delete` and `Reset secrets` -----------------------------------------------------------------------

  async function openAsk(ask: Confirmation, entry: Entry, row: Row, button: HTMLButtonElement): Promise<void> {
    if (endAsk !== null) return;
    let closed = false;
    let working = false;
    const asking = await openLayer({
      cls: "people-ask",
      labelledBy: "ppl-ask-title",
      returnTo: button,
      sheets: ["./websession.css", "./people.css"],
      onEscape: () => end(),
    });
    function end(): void {
      if (closed || working) return;
      closed = true;
      endAsk = null;
      asking.close();
    }
    endAsk = end;
    if (!dialogOpen) return end();
    const heading = el("h2", "set-title", asking.box);
    heading.id = "ppl-ask-title";
    heading.textContent = ask.title;
    el("p", "ppl-ask-text", asking.box).textContent = ask.text;
    const buttons = el("div", "dlg-actions", asking.box);
    const cancel = el("button", "web-btn", buttons);
    cancel.type = "button";
    cancel.textContent = "Cancel";
    // The destructive button has the danger look (as an armed button has, 6.27) and is not the one that has the focus.
    const go = el("button", "web-btn ppl-go", buttons);
    go.type = "button";
    go.textContent = ask.button;
    go.dataset.armed = "true";
    cancel.addEventListener("click", end);
    go.addEventListener("click", () => {
      if (working || busy) return;
      working = true;
      cancel.disabled = true;
      go.disabled = true;
      void (async () => {
        const r = await send(entry, row, false);
        working = false;
        closed = true;
        endAsk = null;
        asking.close();
        await after(r, row);
      })();
    });
    cancel.focus();
  }

  // ---- the invitation -----------------------------------------------------------------------------------------------------

  invite.addEventListener("click", () => {
    if (inviteOpen || busy) return;
    inviteOpen = true;
    void openInvite();
  });

  async function openInvite(): Promise<void> {
    closeMenu(false);
    let working = false;
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
    for (const [value, text] of [
      ["member", "Member"],
      ["admin", "Administrator"],
    ] as const) {
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
      if (working) return;
      const role: Role = radios.get("admin")?.checked === true ? "admin" : "member";
      const picked = INVITE_DAYS.find((n) => dayRadios.get(n)?.checked === true) ?? DEFAULT_DAYS;
      const chosen = inviteBody(role, picked);
      working = true;
      create.disabled = true;
      create.textContent = "Create link…";
      inviteBox.setAttribute("aria-busy", "true");
      err.textContent = "";
      void (async () => {
        const r = await change(() => http("POST", "/api/admin/invitations", chosen));
        working = false;
        inviteBox.removeAttribute("aria-busy");
        if (closed) return;
        create.disabled = false;
        create.textContent = "Create link";
        if (!r.ok) {
          if (r.cancelled) return void create.focus();
          const o = outcomeOf(r.status, r.body);
          if (o.kind === "locked") return lockOut(ADMIN_ONLY);
          err.textContent = o.kind === "say" ? o.text : CHANGE_FAILED;
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
