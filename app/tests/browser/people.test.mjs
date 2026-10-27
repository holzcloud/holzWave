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

const NOW = Date.now();
const iso = (secondsAgo) => new Date(NOW - secondsAgo * 1000).toISOString();

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

  assert.deepEqual(errors, [], "the page reported errors");
}
