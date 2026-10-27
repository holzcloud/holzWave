// The people dialog (docs/design.md 6.29) on the main page against a scripted server: who may open it, the table with its state
// words, and the invitation whose link is shown once. The row menu, the confirmations, the last administrator and the failures are
// in the second half of this file.
import assert from "node:assert/strict";
import { ACCOUNT, actionTitles, errorText, installClipboard, openMain, session, sessionRoute, terminalHasFocus } from "./websession-lib.mjs";
import { STEP, clean, confirmWith, fixture, focusedName, holdsSecret, layer, meRoute, runAction, stepUpRoute } from "./account-lib.mjs";

export const backends = ["ws"];
export const backendNote = "the dialog exists on the web backend only: http() throws on the Tauri backend";

const LINK = "https://holz.example.net/#invite=hwi_7Qk2mXe9aBcDeFgHiJkLmNoPqRsTuVwXyZ012345678";
const TOKEN = "hwi_7Qk2mXe9aBcDeFgHiJkLmNoPqRsTuVwXyZ012345678";
const STEP_PW = "step up password";

// A time relative to now, taken when the fixture is made: the files of a full run are loaded long before they run.
const iso = (secondsAgo) => new Date(Date.now() - secondsAgo * 1000).toISOString();

/** The accounts of the scripted server. Holz is the only active administrator; Dora is an administrator who is disabled. */
export function people(over = {}) {
  const base = (id, username, display_name, role, state, last) => ({ id, username, display_name, role, state, last_signin: last === null ? null : iso(last), created_at: iso(9e6) });
  return {
    admins: 1,
    accounts: [
      base("acc1", "holz", "Holz", "admin", "active", 300),
      base("a2", "ben", "Ben Ito", "member", "active", 30 * 3600),
      base("a3", "chris", "Chris Roy", "member", "locked_out", 5 * 3600),
      base("a4", "dana", "Dana Wu", "member", "disabled", 21 * 86_400),
      base("a5", "eli", "Eli Sato", "member", "active", null),
      base("a6", "dora", "Dora Vos", "admin", "disabled", 4 * 86_400),
    ],
    ...over,
  };
}

/** The route for the list: it answers what `state` is at that moment, so a test changes the people between requests. */
export const listRoute = (state) => ({ path: "/api/admin/accounts", body: () => structuredClone({ admins: state.admins, accounts: state.accounts }) });
export const dialog = (page) => page.getByRole("dialog", { name: "People", exact: true });
export const inviteLayer = (page) => page.getByRole("dialog", { name: "Invite", exact: true });
export const linkLayer = (page) => page.getByRole("dialog", { name: "Invitation link", exact: true });
export const statusLine = (page) => page.locator(".people .set-status").textContent();

/** Wait until the entry is registered: it is, a moment after the page is up, when the first lookup of the session has ended. */
export const entryReady = (page) =>
  page.waitForFunction(async () => (await import(new URL("actions.js", location.href).href)).listActions().some((a) => a.id === "people.open"));

/** Open the main page as an administrator with the scripted server and the dialog. */
export async function openPeople(page, url, state, table = [], sessionOver = {}) {
  await installClipboard(page);
  const server = await openMain(page, url, [sessionRoute(sessionOver), meRoute({ me: fixture() }), listRoute(state), ...table]);
  await entryReady(page);
  assert.equal(await runAction(page, "people.open"), true, "the action runs");
  await dialog(page).waitFor();
  await page.waitForSelector(".people tbody tr");
  return server;
}

/** The table as a person reads it. */
export const tableOf = (page) =>
  page.evaluate(() => ({
    head: Array.from(document.querySelectorAll(".people thead th")).map((th) => th.textContent),
    rows: Array.from(document.querySelectorAll(".people tbody tr")).map((tr) => ({
      name: tr.querySelector(".ppl-name")?.textContent,
      user: tr.querySelector(".ppl-user")?.textContent,
      role: tr.querySelector(".ppl-role")?.textContent,
      glyph: tr.querySelector(".ppl-glyph")?.textContent,
      word: tr.querySelector(".ppl-word")?.textContent,
      last: tr.querySelector(".ppl-last")?.textContent,
      button: tr.querySelector("button")?.getAttribute("aria-label"),
    })),
  }));

export default async function ({ page, url, errors }) {
  // ---- a member never sees the entry --------------------------------------------------------------------------------------------
  {
    await installClipboard(page);
    const member = session({ account: { ...ACCOUNT, role: "member" } });
    const server = await openMain(page, url, [{ path: "/api/session", body: member }, listRoute({ admins: 1, accounts: [] })]);
    await page.waitForFunction(async () => (await import(new URL("actions.js", location.href).href)).listActions().some((a) => a.id === "account.open"));
    await page.waitForFunction(() => true);
    while (server.count("GET", "/api/session") < 1) await page.waitForTimeout(50);
    await page.waitForTimeout(300); // the lookup of the session has ended: it is the one that decides whether the entry exists
    const titles = await actionTitles(page);
    assert.ok(titles.includes("Account…"), "the web build has its account entry");
    assert.ok(!titles.includes("People…"), "a member does not see People…");
    assert.equal(await runAction(page, "people.open"), false, "there is no such action for a member");
    assert.equal(server.count("GET", "/api/admin/accounts"), 0, "a member never asks for the list");
    assert.equal(await page.getByRole("dialog").count(), 0);
  }

  // ---- an administrator: the entry, the table, the four columns, the state words and glyphs ---------------------------------------
  {
    const state = people();
    const server = await openPeople(page, url, state);
    assert.ok((await actionTitles(page)).includes("People…"), "an administrator sees People…");
    assert.equal(server.count("GET", "/api/admin/accounts"), 1, "the list is read once when the dialog opens");
    assert.equal(await dialog(page).getAttribute("aria-modal"), "true");
    assert.equal(await page.locator(".people .set-status").getAttribute("role"), "status");
    assert.equal(await statusLine(page), "", "the status line is empty at first");
    const t = await tableOf(page);
    assert.deepEqual(t.head, ["Name", "Role", "State", "Last sign-in", ""]);
    assert.deepEqual(
      t.rows.map((r) => [r.name, r.user, r.role, r.glyph, r.word, r.last]),
      [
        ["Holz", "holz", "admin", "●", "active", "5 min ago"],
        ["Ben Ito", "ben", "member", "●", "active", "yesterday"],
        ["Chris Roy", "chris", "member", "!", "locked out", "5 h ago"],
        ["Dana Wu", "dana", "member", "○", "disabled", "3 weeks ago"],
        ["Eli Sato", "eli", "member", "●", "active", "never"],
        ["Dora Vos", "dora", "admin", "○", "disabled", "4 days ago"],
      ],
    );
    assert.equal(await page.locator(".people thead th").count(), 5, "real header cells");
    assert.equal(await page.locator('.people [data-state="locked_out"] .ppl-word').count(), 1, "the locked out state has its own marker");
    assert.equal(
      await page.locator(".ppl-glyph").first().getAttribute("aria-hidden"),
      "true",
      "the glyph is a second cue for the eye; the word is what is read out",
    );
    const note = await page.locator(".people .ppl-note").textContent();
    assert.equal(note, "This list shows names, roles and states only. It never shows anyone's secrets, terminals, commands or layout.");
    assert.equal(await dialog(page).getByRole("button", { name: "Invite", exact: true }).count(), 1);
    assert.equal(await dialog(page).getByRole("button", { name: "Close", exact: true }).count(), 1);
    // The dialog is one at a time, and Escape gives the keyboard back to the terminal.
    await runAction(page, "people.open");
    assert.equal(await page.getByRole("dialog", { name: "People", exact: true }).count(), 1, "one dialog at a time");
    await page.keyboard.press("Escape");
    await dialog(page).waitFor({ state: "detached" });
    await page.waitForFunction(() => document.activeElement?.id === "ime");
    assert.equal(await terminalHasFocus(page), true);
    await clean(page, server, [], "the table");
  }

  // ---- a hostile name is text in the table ---------------------------------------------------------------------------------------
  {
    const evil = `<img src=x onerror="window.__pwned=1">`;
    const state = people();
    state.accounts[1].display_name = evil;
    state.accounts[1].username = "<b>ben</b>";
    const server = await openPeople(page, url, state);
    const t = await tableOf(page);
    assert.equal(t.rows[1].name, evil);
    assert.equal(t.rows[1].user, "<b>ben</b>");
    assert.equal(await page.locator(".people img, .people b").count(), 0, "no element was made from a name");
    assert.equal(await page.evaluate(() => window.__pwned), undefined);
    await clean(page, server, [], "a hostile name");
  }

  // ---- the answer that is not of the shape: only Close is left -----------------------------------------------------------------
  {
    const state = people({ admins: "many" });
    await installClipboard(page);
    const server = await openMain(page, url, [sessionRoute(), listRoute(state)]);
    await entryReady(page);
    await runAction(page, "people.open");
    await dialog(page).waitFor();
    await page.waitForFunction(() => document.querySelector(".people .set-status")?.textContent !== "");
    assert.equal(await statusLine(page), "Could not reach the server. Try again.");
    assert.equal(await page.locator(".people tbody tr").count(), 0);
    assert.equal(await page.locator(".people").getByRole("button").count(), 1, "only Close is left");
    await page.getByRole("button", { name: "Close", exact: true }).click();
    await dialog(page).waitFor({ state: "detached" });
    await clean(page, server, [], "a bad answer");
  }

  // ---- the invitation: the form, the defaults, the step-up round trip, the link shown once -----------------------------------------
  {
    const state = people();
    const server = await openPeople(page, url, state, [
      { method: "POST", path: "/api/admin/invitations", answers: [STEP, { status: 200, body: { link: LINK, expires_at: iso(-3 * 86_400) } }] },
      stepUpRoute,
    ]);
    await dialog(page).getByRole("button", { name: "Invite", exact: true }).click();
    await inviteLayer(page).waitFor();
    assert.equal(await inviteLayer(page).getAttribute("aria-modal"), "true");
    assert.equal(await inviteLayer(page).getByLabel("Member", { exact: true }).isChecked(), true, "member is selected at first");
    assert.equal(await inviteLayer(page).getByLabel("Administrator", { exact: true }).isChecked(), false);
    const dayChoices = inviteLayer(page).getByRole("group", { name: "Days", exact: true }).getByRole("radio");
    assert.deepEqual(await dayChoices.evaluateAll((all) => all.map((r) => [r.labels[0].textContent, r.checked])), [["1", false], ["3", false], ["7", true], ["14", false], ["30", false]], "7 days at first");
    assert.equal(await inviteLayer(page).getByLabel("Username").count(), 0, "no username field: role and expiry only");
    assert.equal(await inviteLayer(page).locator("input[type=text], input[type=password], input[type=email]").count(), 0);
    await inviteLayer(page).getByLabel("Administrator", { exact: true }).check();
    await inviteLayer(page).getByRole("radio", { name: "3", exact: true }).check();
    await inviteLayer(page).getByRole("button", { name: "Create link", exact: true }).click();
    await confirmWith(page, STEP_PW);
    await linkLayer(page).waitFor();
    const posts = server.requests("POST", "/api/admin/invitations");
    assert.equal(posts.length, 2, "refused once for a recent sign-in, then repeated once");
    assert.deepEqual(posts[0].json, { role: "admin", days: 3 });
    assert.deepEqual(posts[1].json, { role: "admin", days: 3 });
    assert.equal(posts[1].headers["x-csrf-token"], "csrf-token-1");
    assert.equal(server.count("POST", "/auth/step-up"), 1);
    // The link, in mono, selectable, with its words.
    const shown = linkLayer(page).locator(".ppl-link-text");
    assert.equal(await shown.textContent(), LINK);
    assert.match(await shown.evaluate((n) => getComputedStyle(n).fontFamily), /mono/i, "the link is in the mono face");
    assert.notEqual(await shown.evaluate((n) => getComputedStyle(n).userSelect), "none", "the link can be selected");
    const words = await linkLayer(page).locator(".ppl-link-note").textContent();
    assert.ok(words.startsWith("Shown once."), words);
    assert.equal(words, "Shown once. Copy it now and send it to the person. It works for 3 days, as an administrator.");
    assert.equal(await linkLayer(page).getByRole("button", { name: "Copy link", exact: true }).count(), 1);
    assert.equal(await linkLayer(page).getByRole("button", { name: "Done", exact: true }).count(), 1);
    // Copy puts the link on the clipboard and nowhere else.
    await linkLayer(page).getByRole("button", { name: "Copy link", exact: true }).click();
    await page.waitForFunction(() => window.__copied.length === 1);
    assert.deepEqual(await page.evaluate(() => window.__copied), [LINK]);
    // The link is in no other place of the page: not the address, the title, the status line, an attribute or the storage.
    assert.equal(await statusLine(page), "");
    assert.deepEqual(await holdsSecret(page, [TOKEN]).then((found) => found.filter((f) => !f.startsWith("markup"))), [], "the link is in no address, title, cookie or storage");
    const attributeHits = await page.evaluate((token) => Array.from(document.querySelectorAll("*")).flatMap((n) => n.getAttributeNames().filter((a) => n.getAttribute(a).includes(token)).map((a) => `${n.tagName}[${a}]`)), TOKEN);
    assert.deepEqual(attributeHits, [], "no attribute holds the link");
    assert.equal(server.count("GET", "/api/admin/accounts"), 1, "the list is not read again with the link");
    // Done removes the link, and opening the dialog again shows none.
    await linkLayer(page).getByRole("button", { name: "Done", exact: true }).click();
    await linkLayer(page).waitFor({ state: "detached" });
    assert.deepEqual(await holdsSecret(page, [TOKEN, LINK]), [], "no link is left after Done");
    await page.keyboard.press("Escape");
    await dialog(page).waitFor({ state: "detached" });
    await runAction(page, "people.open");
    await dialog(page).waitFor();
    assert.deepEqual(await holdsSecret(page, [TOKEN, LINK]), [], "no link in the dialog opened again");
    assert.equal(await page.locator(".ppl-link-text").count(), 0);
    await clean(page, server, [STEP_PW], "the invitation");
  }

  // ---- the link is emptied when the layer or the dialog is left by another way, and a double click sends one request ----------------
  {
    const state = people();
    const server = await openPeople(page, url, state, [{ method: "POST", path: "/api/admin/invitations", status: 200, delay: 150, body: { link: LINK, expires_at: iso(-7 * 86_400) } }]);
    await dialog(page).getByRole("button", { name: "Invite", exact: true }).click();
    await inviteLayer(page).waitFor();
    await inviteLayer(page).getByRole("button", { name: "Create link", exact: true }).dblclick();
    await linkLayer(page).waitFor();
    assert.equal(server.count("POST", "/api/admin/invitations"), 1, "a double click sent one request");
    assert.deepEqual(server.requests("POST", "/api/admin/invitations")[0].json, { role: "member", days: 7 });
    assert.equal(await linkLayer(page).locator(".ppl-link-note").textContent(), "Shown once. Copy it now and send it to the person. It works for 7 days, as a member.");
    // Escape leaves the layer and empties the link.
    await page.keyboard.press("Escape");
    await linkLayer(page).waitFor({ state: "detached" });
    assert.deepEqual(await holdsSecret(page, [TOKEN, LINK]), [], "Escape took the link away");
    assert.equal(await page.locator(".ppl-link-text").count(), 0);
    assert.equal(await dialog(page).count(), 1, "the dialog is still open under the layer that closed");
    // Another invitation shows its own link; the first one is not there.
    await dialog(page).getByRole("button", { name: "Invite", exact: true }).click();
    await inviteLayer(page).waitFor();
    assert.equal(await page.locator(".ppl-link-text").count(), 0, "the form of another invitation has no link");
    await inviteLayer(page).getByRole("button", { name: "Cancel", exact: true }).click();
    await inviteLayer(page).waitFor({ state: "detached" });
    assert.equal(server.count("POST", "/api/admin/invitations"), 1, "Cancel sends nothing");
    assert.equal(await focusedName(page), "Invite", "the focus is back on the button that opened the layer");
    await clean(page, server, [], "a double click");
  }

  await second({ page, url });
  assert.deepEqual(errors, [], "the page reported errors");
}

// ---- the second half: the row menu, the confirmations, the last administrator and the failures -------------------------------------

const ONLY = "The only active administrator.";
const ONE_ADMIN = "There must be one active administrator.";
const ADMIN_ONLY = "Only administrators can open this.";
const UNREACHABLE = "Could not reach the server. Try again.";
const CHANGE_FAILED = "That change was not made. Try again.";
const DELETE_TEXT = "They can no longer sign in and their terminals stop. Their files stay on the server, and nobody can open their saved passwords and keys again.";
const RESET_TEXT = "Their saved passwords and keys are deleted. Their account, layout and history stay.";

/** What `menuFor` is expected to give for each row of `people()`: the entries, and which of them are held back. */
const MENU = {
  Holz: { entries: [["Make member", true], ["Disable", true], ["Reset secrets", false], ["Delete", true]], why: ONLY },
  "Ben Ito": { entries: [["Make admin", false], ["Disable", false], ["Reset secrets", false], ["Delete", false]], why: "" },
  "Chris Roy": { entries: [["Make admin", false], ["Disable", false], ["Unlock sign-in", false], ["Reset secrets", false], ["Delete", false]], why: "" },
  "Dana Wu": { entries: [["Make admin", false], ["Enable", false], ["Reset secrets", false], ["Delete", false]], why: "" },
  "Eli Sato": { entries: [["Make admin", false], ["Disable", false], ["Reset secrets", false], ["Delete", false]], why: "" },
  "Dora Vos": { entries: [["Make member", false], ["Enable", false], ["Reset secrets", false], ["Delete", false]], why: "" },
};

const more = (page, name) => dialog(page).getByRole("button", { name: `Actions for ${name}`, exact: true });
const menuOpen = (page) => page.locator('.people [role="menu"]');
const find = (state, id) => state.accounts.find((a) => a.id === id);
/** A route answer that changes the scripted people and then says 204. */
const mutate = (fn) => ({ reply: () => (fn(), { status: 204 }) });
const countAdmins = (state) => state.accounts.filter((a) => a.role === "admin" && a.state !== "disabled").length;

/** The entries of the open menu, as a person reads them. */
const menuOf = (page) =>
  page.evaluate(() => ({
    label: document.querySelector('.people [role="menu"]')?.getAttribute("aria-label"),
    entries: Array.from(document.querySelectorAll('.people [role="menu"] [role="menuitem"]')).map((i) => [i.textContent, i.getAttribute("aria-disabled") === "true"]),
    why: document.querySelector(".people [role=menu] .ppl-why")?.textContent ?? "",
    separators: document.querySelectorAll('.people [role="menu"] [role="separator"]').length,
  }));

/** Open the menu of a row with the pointer and press an entry. */
async function choose(page, name, label) {
  await more(page, name).click();
  await menuOpen(page).waitFor();
  await page.getByRole("menuitem", { name: label, exact: true }).click();
}

/** Every control of the layers on screen, as `role:name`: a button, a radio or a menu entry. */
const controlsOf = (page) =>
  page.evaluate(() =>
    Array.from(document.querySelectorAll(".dlg-backdrop")).flatMap((layer) =>
      Array.from(layer.querySelectorAll("button, input, select, textarea, a[href], [role='menuitem'], [tabindex]"))
        .filter((n) => n.getClientRects().length > 0)
        .map((n) => {
          const role = n.getAttribute("role") ?? (n instanceof HTMLInputElement ? n.type : n.tagName.toLowerCase());
          const name = n.getAttribute("aria-label") ?? (n.labels?.length ? n.labels[0].textContent : n.textContent);
          return `${role}:${name.trim()}`;
        }),
    ),
  );

export async function second({ page, url }) {
  // ---- the menu of every row: only the entries that apply, the last administrator held back with its reason -----------------------
  {
    const state = people();
    const server = await openPeople(page, url, state);
    assert.equal(await page.getByRole("button", { name: /^Actions for / }).count(), 6, "one actions button for each row");
    const seen = {};
    for (const row of state.accounts) {
      const button = more(page, row.display_name);
      assert.equal(await button.getAttribute("aria-haspopup"), "menu");
      assert.equal(await button.getAttribute("aria-expanded"), "false");
      assert.equal(await button.textContent(), "⋯");
      await button.click();
      await menuOpen(page).waitFor();
      assert.equal(await button.getAttribute("aria-expanded"), "true");
      assert.equal(await page.locator(".people .ppl-open").count(), 1, "the row of the open menu is marked");
      const menu = await menuOf(page);
      assert.equal(menu.label, `Actions for ${row.display_name}`);
      assert.equal(menu.separators, 1, "one hairline, before Reset secrets and Delete");
      seen[row.display_name] = { entries: menu.entries, why: menu.why };
      assert.equal(await focusedName(page), menu.entries[0][0], "the first entry has the focus");
      await page.keyboard.press("Escape");
      await menuOpen(page).waitFor({ state: "detached" });
      assert.equal(await dialog(page).count(), 1, "Escape closed the menu and not the dialog");
      assert.equal(await focusedName(page), `Actions for ${row.display_name}`, "the focus is back on the button of the row");
    }
    console.log("Menu table:", JSON.stringify(seen));
    assert.deepEqual(seen, MENU);
    assert.equal(server.count("GET", "/api/admin/accounts"), 1);
    await clean(page, server, [], "the menus");
  }

  // ---- the keys: Enter opens with the first entry, the arrows move, Tab reaches every entry, Escape closes the menu first --------------
  {
    const state = people();
    const server = await openPeople(page, url, state);
    await more(page, "Chris Roy").focus();
    await page.keyboard.press("Enter");
    await menuOpen(page).waitFor();
    assert.equal(await focusedName(page), "Make admin");
    const order = [];
    for (let i = 0; i < 5; i++) {
      order.push(await focusedName(page));
      await page.keyboard.press("ArrowDown");
    }
    assert.deepEqual(order, ["Make admin", "Disable", "Unlock sign-in", "Reset secrets", "Delete"]);
    assert.equal(await focusedName(page), "Make admin", "the arrows go round");
    await page.keyboard.press("ArrowUp");
    assert.equal(await focusedName(page), "Delete");
    await page.keyboard.press("Home");
    assert.equal(await focusedName(page), "Make admin");
    await page.keyboard.press("End");
    assert.equal(await focusedName(page), "Delete");
    await page.keyboard.press("Home");
    const tabbed = [];
    for (let i = 0; i < 5; i++) {
      tabbed.push(await focusedName(page));
      await page.keyboard.press("Tab");
    }
    assert.deepEqual(tabbed, ["Make admin", "Disable", "Unlock sign-in", "Reset secrets", "Delete"], "Tab reaches every entry");
    // Space opens it too; Escape closes the menu first and the dialog second.
    await page.keyboard.press("Escape");
    await menuOpen(page).waitFor({ state: "detached" });
    assert.equal(await dialog(page).count(), 1);
    await page.keyboard.press("Space");
    await menuOpen(page).waitFor();
    await page.keyboard.press("Escape");
    await menuOpen(page).waitFor({ state: "detached" });
    await page.keyboard.press("Escape");
    await dialog(page).waitFor({ state: "detached" });
    await page.waitForFunction(() => document.activeElement?.id === "ime");
    assert.equal(await terminalHasFocus(page), true, "the keyboard is back with the terminal");
    assert.equal(server.log.filter((l) => l.method !== "GET").length, 0, "no key sent a change");
    await clean(page, server, [], "the keys");
  }

  // ---- Make admin, Disable, Enable and Unlock sign-in: the step-up round trip, then the list is read again -------------------------
  {
    const state = people();
    const server = await openPeople(page, url, state, [
      { method: "POST", path: "/api/admin/accounts/a2/role", answers: [STEP, mutate(() => ((find(state, "a2").role = "admin"), (state.admins = countAdmins(state))))] },
      { method: "POST", path: "/api/admin/accounts/a2/disable", answers: [STEP, mutate(() => ((find(state, "a2").state = "disabled"), (state.admins = countAdmins(state))))] },
      { method: "POST", path: "/api/admin/accounts/a2/enable", answers: [STEP, mutate(() => ((find(state, "a2").state = "active"), (state.admins = countAdmins(state))))] },
      { method: "POST", path: "/api/admin/accounts/a3/unlock-signin", answers: [STEP, mutate(() => (find(state, "a3").state = "active"))] },
      stepUpRoute,
    ]);
    const roleOf = (name) => page.locator(".people tbody tr", { hasText: name }).locator(".ppl-role").textContent();
    const wordOf = (name) => page.locator(".people tbody tr", { hasText: name }).locator(".ppl-word").textContent();

    await choose(page, "Ben Ito", "Make admin");
    await confirmWith(page, STEP_PW);
    await page.waitForFunction(() => document.querySelector('.people tr[data-id="a2"] .ppl-role')?.textContent === "admin");
    assert.equal(await roleOf("Ben Ito"), "admin", "the list was read again and shows the new role");
    assert.equal(server.count("GET", "/api/admin/accounts"), 2);
    assert.deepEqual(server.requests("POST", "/api/admin/accounts/a2/role").map((r) => r.json), [{ role: "admin" }, { role: "admin" }], "refused once, repeated once");
    assert.equal(await focusedName(page), "Actions for Ben Ito", "the focus returns to the button of the row");
    assert.equal(await statusLine(page), "", "no sentence for a change that was made");
    await more(page, "Ben Ito").click();
    assert.deepEqual((await menuOf(page)).entries.map((e) => e[0]), ["Make member", "Disable", "Reset secrets", "Delete"], "an administrator now: Make member, and two administrators hold nothing back");
    assert.deepEqual((await menuOf(page)).entries.map((e) => e[1]), [false, false, false, false]);
    await page.keyboard.press("Escape");

    await choose(page, "Ben Ito", "Disable");
    await confirmWith(page, STEP_PW);
    await page.waitForFunction(() => document.querySelector('.people tr[data-id="a2"] .ppl-word')?.textContent === "disabled");
    assert.equal(await wordOf("Ben Ito"), "disabled");
    assert.deepEqual(server.requests("POST", "/api/admin/accounts/a2/disable").map((r) => r.json), [{}, {}]);

    await choose(page, "Ben Ito", "Enable");
    await confirmWith(page, STEP_PW);
    await page.waitForFunction(() => document.querySelector('.people tr[data-id="a2"] .ppl-word')?.textContent === "active");
    assert.deepEqual(server.requests("POST", "/api/admin/accounts/a2/enable").map((r) => r.json), [{}, {}]);

    await choose(page, "Chris Roy", "Unlock sign-in");
    await confirmWith(page, STEP_PW);
    await page.waitForFunction(() => document.querySelector('.people tr[data-id="a3"] .ppl-word')?.textContent === "active");
    assert.equal(await page.locator('.people [data-state="locked_out"]').count(), 0, "nobody is locked out now");
    assert.deepEqual(server.requests("POST", "/api/admin/accounts/a3/unlock-signin").map((r) => r.json), [{}, {}]);
    assert.equal(server.count("POST", "/auth/step-up"), 4, "one layer for each change");
    assert.equal(server.count("GET", "/api/admin/accounts"), 5, "the list was read again after each change");
    await clean(page, server, [STEP_PW], "the plain changes");
  }

  // ---- Delete and Reset secrets ask first: the question, the safe button, Escape and Cancel send nothing, the button sends once --------
  {
    const state = people();
    const server = await openPeople(page, url, state, [
      { method: "DELETE", path: "/api/admin/accounts/a2", status: 204, delay: 150, answers: undefined, reply: () => ((state.accounts = state.accounts.filter((a) => a.id !== "a2")), { status: 204 }) },
      { method: "POST", path: "/api/admin/accounts/a3/reset-secrets", answers: [STEP, mutate(() => {})] },
      stepUpRoute,
    ]);
    const asking = (name) => page.getByRole("dialog", { name, exact: true });
    await choose(page, "Ben Ito", "Delete");
    await asking("Delete Ben Ito?").waitFor();
    assert.equal(await asking("Delete Ben Ito?").locator(".ppl-ask-text").textContent(), DELETE_TEXT);
    assert.equal(await asking("Delete Ben Ito?").getAttribute("aria-modal"), "true");
    assert.equal(await focusedName(page), "Cancel", "the safe button has the focus, not the destructive one");
    assert.equal(await asking("Delete Ben Ito?").getByRole("button", { name: "Delete", exact: true }).getAttribute("data-armed"), "true", "the destructive button has the danger look");
    assert.equal(server.count("DELETE", "/api/admin/accounts/a2"), 0, "nothing is sent before the answer");
    await page.keyboard.press("Escape");
    await asking("Delete Ben Ito?").waitFor({ state: "detached" });
    assert.equal(await dialog(page).count(), 1, "Escape ended the question and not the dialog");
    assert.equal(await focusedName(page), "Actions for Ben Ito");
    await choose(page, "Ben Ito", "Delete");
    await asking("Delete Ben Ito?").waitFor();
    await asking("Delete Ben Ito?").getByRole("button", { name: "Cancel", exact: true }).click();
    await asking("Delete Ben Ito?").waitFor({ state: "detached" });
    assert.equal(server.count("DELETE", "/api/admin/accounts/a2"), 0, "Cancel sends nothing");
    assert.equal(await focusedName(page), "Actions for Ben Ito");
    // The button sends one request, also for a double click.
    await choose(page, "Ben Ito", "Delete");
    await asking("Delete Ben Ito?").waitFor();
    await asking("Delete Ben Ito?").getByRole("button", { name: "Delete", exact: true }).dblclick();
    await page.waitForFunction(() => !document.querySelector('.people tr[data-id="a2"]'));
    assert.equal(server.count("DELETE", "/api/admin/accounts/a2"), 1, "a double click sent one request");
    assert.equal(await asking("Delete Ben Ito?").count(), 0, "the question is gone");
    assert.equal(await page.locator(".people tbody tr").count(), 5, "the person is not in the list any more");
    assert.equal(server.count("GET", "/api/admin/accounts"), 2);

    // Reset secrets: its own question, refused once for a recent sign-in, then done once more.
    await choose(page, "Chris Roy", "Reset secrets");
    await asking("Reset secrets of Chris Roy?").waitFor();
    assert.equal(await asking("Reset secrets of Chris Roy?").locator(".ppl-ask-text").textContent(), RESET_TEXT);
    assert.equal(await focusedName(page), "Cancel");
    await asking("Reset secrets of Chris Roy?").getByRole("button", { name: "Reset secrets", exact: true }).click();
    await confirmWith(page, STEP_PW);
    while (server.count("POST", "/api/admin/accounts/a3/reset-secrets") < 2) await page.waitForTimeout(25);
    assert.deepEqual(server.requests("POST", "/api/admin/accounts/a3/reset-secrets").map((r) => r.json), [{}, {}], "refused once for a recent sign-in, then done");
    await page.waitForFunction(() => document.querySelectorAll(".dlg-backdrop").length === 1);
    assert.equal(await page.locator('.people tr[data-id="a3"]').count(), 1, "the account stays");
    assert.equal(await focusedName(page), "Actions for Chris Roy");
    await clean(page, server, [STEP_PW], "the questions");
  }

  // ---- a hostile name is text in the menu and in the question --------------------------------------------------------------------------
  {
    const evil = `<img src=x onerror="window.__pwned=1">`;
    const state = people();
    find(state, "a2").display_name = evil;
    const server = await openPeople(page, url, state, [{ method: "DELETE", path: "/api/admin/accounts/a2", status: 204 }]);
    await more(page, evil).click();
    assert.equal((await menuOf(page)).label, `Actions for ${evil}`);
    await page.getByRole("menuitem", { name: "Delete", exact: true }).click();
    const asking = page.getByRole("dialog", { name: `Delete ${evil}?`, exact: true });
    await asking.waitFor();
    assert.equal(await asking.locator("h2").textContent(), `Delete ${evil}?`);
    assert.equal(await page.locator(".people img, .dlg img").count(), 0, "no element was made from a name");
    assert.equal(await page.evaluate(() => window.__pwned), undefined);
    await page.keyboard.press("Escape");
    await clean(page, server, [], "a hostile name in the menu");
  }

  // ---- the last active administrator: the three entries are held back and a click on one does nothing -------------------------------
  {
    const state = people();
    const server = await openPeople(page, url, state, [
      { method: "POST", path: "/api/admin/accounts/a5/disable", status: 409, body: { code: "conflict", field: "role" } },
    ]);
    await more(page, "Holz").click();
    for (const label of ["Make member", "Disable", "Delete"]) {
      const entry = page.getByRole("menuitem", { name: label, exact: true });
      assert.equal(await entry.getAttribute("aria-disabled"), "true", `${label} is held back`);
      assert.equal(await entry.getAttribute("aria-describedby"), await page.locator(".people [role=menu] .ppl-why").getAttribute("id"), "and the reason is attached to it");
    }
    assert.equal(await page.locator(".people [role=menu] .ppl-why").textContent(), ONLY, "the reason is text under the entries");
    assert.equal(await page.getByRole("menuitem", { name: "Reset secrets", exact: true }).getAttribute("aria-disabled"), null);
    // Playwright counts aria-disabled as not enabled, so the press is forced.
    for (const label of ["Delete", "Make member", "Disable"]) await page.getByRole("menuitem", { name: label, exact: true }).click({ force: true });
    assert.equal(await page.getByRole("dialog", { name: /^Delete / }).count(), 0, "no question for an entry that is held back");
    assert.equal(server.log.filter((l) => l.method !== "GET").length, 0, "no request for an entry that is held back");
    assert.equal(await menuOpen(page).count(), 1, "the menu stays open");
    await page.keyboard.press("Escape");
    // A race: the server refuses because another administrator acted at the same moment. The sentence, and the list as it was.
    await choose(page, "Eli Sato", "Disable");
    await page.waitForFunction(() => document.querySelector(".people .set-status")?.textContent !== "");
    assert.equal(await statusLine(page), ONE_ADMIN);
    assert.equal(await page.locator(".people tbody tr").count(), 6, "the list is as it was");
    assert.equal(server.count("GET", "/api/admin/accounts"), 1, "the list was not read again");
    assert.equal(server.count("POST", "/auth/step-up"), 0);
    assert.equal(await page.locator('.people tr[data-id="a5"] .ppl-word').textContent(), "active");
    assert.equal(await focusedName(page), "Actions for Eli Sato");
    await clean(page, server, [], "the last administrator");
  }

  // ---- the failures: gone, not an administrator, unreachable, cancelled, and one request for a double click --------------------------
  {
    const state = people();
    const server = await openPeople(page, url, state, [
      { method: "POST", path: "/api/admin/accounts/a2/disable", status: 404, body: { code: "not_found" } },
      { method: "POST", path: "/api/admin/accounts/a4/enable", abort: true },
      { method: "POST", path: "/api/admin/accounts/a5/role", answers: [STEP] },
      { method: "POST", path: "/api/admin/accounts/a6/enable", status: 204, delay: 200 },
      stepUpRoute,
    ]);
    // 404: the person is gone: the list is read again and there is no sentence.
    await choose(page, "Ben Ito", "Disable");
    await page.waitForFunction(() => true);
    while (server.count("GET", "/api/admin/accounts") < 2) await page.waitForTimeout(25);
    assert.equal(await statusLine(page), "");
    // An aborted request: the sentence for a server that cannot be reached, the list kept.
    await choose(page, "Dana Wu", "Enable");
    await page.waitForFunction(() => document.querySelector(".people .set-status")?.textContent !== "");
    assert.equal(await statusLine(page), UNREACHABLE);
    assert.equal(await page.locator(".people tbody tr").count(), 6);
    assert.equal(await focusedName(page), "Actions for Dana Wu");
    // A cancelled step-up: nothing more is sent and no text is shown. The status line is empty again before.
    await choose(page, "Eli Sato", "Make admin");
    await page.getByRole("dialog", { name: "Confirm it is you" }).waitFor();
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    await page.getByRole("dialog", { name: "Confirm it is you" }).waitFor({ state: "detached" });
    assert.equal(server.count("POST", "/api/admin/accounts/a5/role"), 1, "a cancel sends no further request");
    assert.equal(await statusLine(page), "", "and says nothing");
    assert.equal(await focusedName(page), "Actions for Eli Sato");
    assert.equal(server.count("POST", "/auth/step-up"), 0);
    // A double click on an entry sends one request.
    await more(page, "Dora Vos").click();
    await page.getByRole("menuitem", { name: "Enable", exact: true }).dblclick();
    while (server.count("GET", "/api/admin/accounts") < 3) await page.waitForTimeout(25);
    assert.equal(server.count("POST", "/api/admin/accounts/a6/enable"), 1, "a double click sent one request");
    await clean(page, server, [], "the failures");
  }

  // ---- a role that was taken away: the dialog says so at once and offers nothing else ------------------------------------------------
  {
    const state = people();
    const server = await openPeople(page, url, state, [{ method: "POST", path: "/api/admin/accounts/a2/disable", status: 403, body: { code: "forbidden" } }]);
    await choose(page, "Ben Ito", "Disable");
    await page.waitForFunction(() => document.querySelector(".people .set-status")?.textContent !== "");
    assert.equal(await statusLine(page), ADMIN_ONLY);
    assert.equal(await page.locator(".people tbody tr:visible").count(), 0, "the table is gone");
    assert.equal(await page.locator(".people").getByRole("button").count(), 1, "only Close is left");
    assert.equal(server.count("POST", "/auth/step-up"), 0, "a 403 that is no step-up shows no layer");
    await page.getByRole("button", { name: "Close", exact: true }).click();
    await dialog(page).waitFor({ state: "detached" });
    await clean(page, server, [], "a role taken away");
  }
  {
    // The same when the list itself is refused.
    await installClipboard(page);
    const server = await openMain(page, url, [sessionRoute(), { path: "/api/admin/accounts", status: 403, body: { code: "forbidden" } }]);
    await entryReady(page);
    await runAction(page, "people.open");
    await dialog(page).waitFor();
    await page.waitForFunction(() => document.querySelector(".people .set-status")?.textContent !== "");
    assert.equal(await statusLine(page), ADMIN_ONLY);
    assert.equal(await page.locator(".people").getByRole("button").count(), 1, "only Close is left");
    await clean(page, server, [], "the list refused");
  }

  // ---- the controls: exactly these, and nothing that reaches another person's data ------------------------------------------------------
  {
    const state = people();
    const server = await openPeople(page, url, state, [
      { method: "POST", path: "/api/admin/invitations", status: 200, body: { link: LINK, expires_at: iso(-86_400) } },
    ]);
    const seen = new Set();
    const take = async () => (await controlsOf(page)).forEach((c) => seen.add(c));
    await take();
    for (const name of ["Dana Wu", "Dora Vos", "Holz"]) {
      await more(page, name).click();
      await take();
      await page.keyboard.press("Escape");
    }
    await more(page, "Chris Roy").click();
    await take();
    await page.getByRole("menuitem", { name: "Delete", exact: true }).click();
    await page.getByRole("dialog", { name: "Delete Chris Roy?" }).waitFor();
    await take();
    await page.keyboard.press("Escape");
    await choose(page, "Chris Roy", "Reset secrets");
    await page.getByRole("dialog", { name: "Reset secrets of Chris Roy?" }).waitFor();
    await take();
    await page.keyboard.press("Escape");
    await dialog(page).getByRole("button", { name: "Invite", exact: true }).click();
    await inviteLayer(page).waitFor();
    await take();
    await inviteLayer(page).getByRole("button", { name: "Create link", exact: true }).click();
    await linkLayer(page).waitFor();
    await take();
    const controls = [...seen].sort();
    console.log("Controls:", JSON.stringify(controls));
    const names = state.accounts.map((a) => `button:Actions for ${a.display_name}`);
    assert.deepEqual(
      controls,
      [
        ...names,
        "button:Cancel",
        "button:Close",
        "button:Copy link",
        "button:Create link",
        "button:Delete",
        "button:Done",
        "button:Invite",
        "button:Reset secrets",
        "menuitem:Delete",
        "menuitem:Disable",
        "menuitem:Enable",
        "menuitem:Make admin",
        "menuitem:Make member",
        "menuitem:Reset secrets",
        "menuitem:Unlock sign-in",
        ...["1", "3", "7", "14", "30"].map((d) => `radio:${d}`),
        "radio:Administrator",
        "radio:Member",
      ].sort(),
    );
    assert.equal(controls.filter((c) => /password|scrollback|command|layout|export|audit|impersonat|file|terminal|key/i.test(c.split(":")[1]) && !/^(button|menuitem):Reset secrets$/.test(c)).length, 0, "nothing in the dialog names another person's data");
    await page.keyboard.press("Escape");
    await clean(page, server, [], "the controls");
  }
}
