// The server as a source of sessions in the Connections dialog (docs/design.md 6.34): the row above the saved connections in each state
// of the link, the list of running sessions with Attach and End, New session, and the host's refusals as bounded text. The stub plays
// the host; nothing here talks to a real server.
import assert from "node:assert/strict";
import { ADDRESS, ROWS, SIGNED_IN, linkCalls, open, openDialog, rowButtons, sessionCalls, sessionTitles, status } from "./serversessions-lib.mjs";

export const backends = ["tauri"];
export const backendNote = "the server row and its sessions are the desktop's: the browser build has no command for them yet";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await statesCase(page, url);
  await listCase(page, url);
  await actionsCase(page, url);
  await refusalCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

async function statesCase(page, url) {
  // No link at all: not signed in, Sign in… only, no list. The row sits above the saved connections.
  await open(page, url, { view: { ...SIGNED_IN, state: "signed_out", url: "" }, rows: ROWS });
  await openDialog(page, "not signed in");
  const order = await page.evaluate(() => {
    const box = document.querySelector(".dlg.conns");
    const server = box.querySelector(".cx-server");
    const body = box.querySelector(".cx-body");
    return { before: Boolean(server.compareDocumentPosition(body) & Node.DOCUMENT_POSITION_FOLLOWING), legend: server.querySelector("legend").textContent };
  });
  assert.deepEqual(order, { before: true, legend: "Server" }, "the group `Server` is above the saved connections");
  assert.deepEqual(await rowButtons(page), ["Sign in…"]);
  assert.equal(await page.locator(".conns .cx-sess").count(), 0);
  assert.equal(await page.locator(".conns .cx-dot").getAttribute("data-s"), "out", "a hollow dot");
  // Sign in… opens the dialog of the sign-in.
  await page.locator(".conns .cx-srv-acts button", { hasText: "Sign in…" }).click();
  await page.getByRole("dialog", { name: "Sign in to the server" }).waitFor();
  await page.keyboard.press("Escape");
  await page.getByRole("dialog", { name: "Sign in to the server" }).waitFor({ state: "detached" });

  // Signed in with remote sessions on: name, address in mono, the word, Sign out and New session.
  await open(page, url);
  await openDialog(page);
  assert.equal(await page.locator(".conns .cx-srv-name").textContent(), "holz.example.net");
  assert.equal(await page.locator(".conns .cx-srv-addr").textContent(), ADDRESS);
  assert.deepEqual(await rowButtons(page), ["Sign out", "New session"]);
  assert.equal(await page.locator(".conns .cx-dot").getAttribute("data-s"), "in");
  assert.equal(await page.locator(".conns .cx-dot").evaluate((n) => getComputedStyle(n).backgroundColor !== "rgba(0, 0, 0, 0)"), true, "a filled dot");
  assert.equal((await linkCalls(page, "server_refresh")).length, 1, "the link is asked again once when the dialog opens");
  // Sign out asks with a second press.
  const signOut = page.locator(".conns .cx-srv-acts button").first();
  await signOut.click();
  assert.equal(await signOut.textContent(), "Click again to sign out");
  assert.equal((await linkCalls(page, "server_sign_out")).length, 0, "the first press signs nothing out");
  await signOut.click();
  await page.waitForFunction(() => document.querySelector(".conns .cx-srv-state")?.textContent === "not signed in");
  assert.equal((await linkCalls(page, "server_sign_out")).length, 1);
  assert.deepEqual(await rowButtons(page), ["Sign in…"]);
  assert.equal(await page.locator(".conns .cx-srv-addr").textContent(), ADDRESS, "the address stays");

  // Remote sessions off on the server: the sentence in the dim colour, no list and no New session.
  await open(page, url, { view: { ...SIGNED_IN, remote_on: false } });
  await openDialog(page);
  assert.deepEqual(await rowButtons(page), ["Sign out"]);
  assert.equal(await page.locator(".conns .cx-help").textContent(), "Remote sessions are off on this server.");
  assert.equal(await page.locator(".conns .cx-sess").count(), 0);
  assert.equal((await sessionCalls(page, "server_sessions")).length, 0, "a list that is not drawn is not asked for");
  const dim = await page.evaluate(() => {
    const probe = document.createElement("span");
    probe.style.color = "var(--hw-dim)";
    document.body.append(probe);
    const want = getComputedStyle(probe).color;
    probe.remove();
    return getComputedStyle(document.querySelector(".conns .cx-help")).color === want;
  });
  assert.ok(dim, "the sentence is dim");

  // Locked: the buttons give way to the locked line.
  await open(page, url, { view: { ...SIGNED_IN, locked: true } });
  await openDialog(page);
  assert.deepEqual(await rowButtons(page), []);
  assert.equal(await page.locator(".conns .cx-srv-acts .cx-quiet").textContent(), "Locked: unlock to connect");

  // Signing in: the dotted ring and no button.
  await open(page, url, { view: { ...SIGNED_IN, state: "signing_in" } });
  await openDialog(page, "signing in…");
  assert.deepEqual(await rowButtons(page), []);
  assert.equal(await page.locator(".conns .cx-dot").evaluate((n) => getComputedStyle(n).borderTopStyle), "dotted");
}

async function listCase(page, url) {
  const hostile = { id: 9, title: "<img src=x onerror=window.__PWNED=1>", cwd: "/srv/x", cols: 100, rows: 30, started: 1_700_000_300, attached: 2, held: true };
  await open(page, url, { rows: [ROWS[1], ROWS[0], hostile] });
  await openDialog(page);
  await page.waitForSelector(".conns .cx-sess");
  assert.deepEqual(await sessionTitles(page), ["cargo build", "Session 2", hostile.title], "oldest first; an untitled one is Session N");
  const first = page.locator(".conns .cx-sess").first();
  assert.equal(await first.locator(".cx-sess-dir").textContent(), "~/holzWave");
  assert.equal(await first.locator(".cx-sess-size").textContent(), "120×40");
  assert.match(await first.locator(".cx-sess-start").textContent(), /^(\d\d:\d\d|[A-Z][a-z]{2} \d\d:\d\d|\d{4}-\d\d-\d\d)$/);
  assert.equal(await first.locator(".cx-sess-att").textContent(), "1 attached");
  assert.equal(await first.locator(".cx-sess-writer").textContent(), "held");
  assert.equal(await first.locator(".cx-sq").getAttribute("data-shape"), "filled");
  const second = page.locator(".conns .cx-sess").nth(1);
  assert.equal(await second.locator(".cx-sess-writer").textContent(), "free");
  assert.equal(await second.locator(".cx-sq").evaluate((n) => getComputedStyle(n).borderTopStyle), "dashed", "free is a dashed square");
  assert.deepEqual(await first.locator("button").allTextContents(), ["Attach", "End"]);
  assert.equal(await page.locator(".conns .cx-help").textContent(),
    "A session keeps running on the server with no window open. It ends when the server restarts or after the idle limit of the server with nobody connected.");
  // A hostile title is text: no element was made from it and nothing ran.
  assert.equal(await page.locator(".conns .cx-sess img").count(), 0);
  assert.equal(await page.evaluate(() => window.__PWNED), undefined);
}

async function actionsCase(page, url) {
  // Attach opens the session, applies the view, closes the dialog and gives the keyboard to the terminal.
  await open(page, url);
  await page.evaluate(() => window.__MOCK__.remoteSessions.opened({ ...window.__MOCK__.view(), focus: 2 }));
  await openDialog(page);
  await page.waitForSelector(".conns .cx-sess");
  const attach = page.locator(".conns .cx-sess").nth(1).locator("button", { hasText: "Attach" });
  // Two presses in a row open one pane.
  await attach.dblclick();
  await page.waitForSelector(".dlg.conns", { state: "detached" });
  assert.deepEqual(await sessionCalls(page, "server_session_open"), [{ session: 7 }]);
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime", "the keyboard is in the terminal");

  // New session makes one and shows it.
  await openDialog(page);
  await page.locator(".conns .cx-srv-acts button", { hasText: "New session" }).click();
  await page.waitForSelector(".dlg.conns", { state: "detached" });
  assert.equal((await sessionCalls(page, "server_session_new")).length, 1);

  // End asks with a second press; the row leaves after the list is read again.
  await openDialog(page);
  await page.waitForFunction(() => document.querySelectorAll(".conns .cx-sess").length === 3);
  const end = page.locator('.conns .cx-sess[data-session="4"] button', { hasText: /End|Click again to end/ });
  await end.click();
  assert.equal(await end.textContent(), "Click again to end");
  assert.equal((await sessionCalls(page, "server_session_end")).length, 0);
  await end.click();
  await page.waitForFunction(() => document.querySelector('.conns .cx-sess[data-session="4"]') === null);
  assert.deepEqual(await sessionCalls(page, "server_session_end"), [{ session: 4 }]);
  assert.ok(await page.locator(".dlg.conns").isVisible(), "ending a session keeps the dialog open");
}

async function refusalCase(page, url) {
  const sentence = "The server has no room for another session.";
  await open(page, url, { fails: { server_session_new: sentence, server_session_open: "<b>" + "x".repeat(400) } });
  await openDialog(page);
  await page.waitForSelector(".conns .cx-sess");
  await page.locator(".conns .cx-srv-acts button", { hasText: "New session" }).click();
  await page.waitForFunction((s) => document.querySelector(".conns .set-status")?.textContent === s, sentence);
  assert.ok(await page.locator(".dlg.conns").isVisible(), "a refusal changes nothing");
  await page.locator(".conns .cx-sess").first().locator("button", { hasText: "Attach" }).click();
  await page.waitForFunction(() => document.querySelector(".conns .set-status")?.textContent.startsWith("<b>"));
  const shown = await status(page);
  assert.equal(shown.length, 200, "a long refusal is bounded");
  assert.equal(await page.locator(".conns .set-status b").count(), 0, "a refusal is text");
  await page.keyboard.press("Escape");
}
