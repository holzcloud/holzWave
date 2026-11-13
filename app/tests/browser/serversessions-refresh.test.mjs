// When the Connections dialog reads the server's sessions (docs/design.md 6.34): when it opens, after each action, every five seconds
// while it is open and never after it closed; and a list that cannot be read is said as `unreachable`. The stub plays the host. The
// browser build's row (`This server`) is the same code with no Sign out, so only its buttons differ.
import assert from "node:assert/strict";
import { currentBackend } from "./lib.mjs";
import { SIGNED_IN, open, openDialog, rowButtons, sessionCalls } from "./serversessions-lib.mjs";

export const backends = ["tauri", "ws"];

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(8000);
  await timerCase(page, url);
  await unreadableCase(page, url);
  await lockedCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const lists = async (page) => (await sessionCalls(page, "server_sessions")).length;

async function timerCase(page, url) {
  await open(page, url);
  assert.equal(await lists(page), 0, "nothing is asked before the dialog opens");
  await openDialog(page);
  await page.waitForSelector(".conns .cx-sess");
  assert.equal(await lists(page), 1, "the list is read when the dialog opens");
  // Every five seconds while it is open.
  await page.waitForFunction(() => window.__MOCK__.remoteSessions.calls.filter((c) => c[0] === "server_sessions").length === 2, null, { timeout: 7000 });
  // After an action.
  const end = page.locator('.conns .cx-sess[data-session="7"] button', { hasText: /End|Click again/ });
  await end.click();
  await end.click();
  await page.waitForFunction(() => document.querySelector('.conns .cx-sess[data-session="7"]') === null);
  assert.equal(await lists(page), 3, "the list is read again after End");
  // The timer stops when the dialog closes.
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.conns", { state: "detached" });
  const closed = await lists(page);
  await page.waitForTimeout(5600);
  assert.equal(await lists(page), closed, "no read after the dialog closed");
}

async function unreadableCase(page, url) {
  await open(page, url, { view: SIGNED_IN, fails: { server_sessions: "The server cannot be reached." } });
  await openDialog(page, "unreachable");
  assert.equal(await page.locator(".conns .cx-dot").getAttribute("data-s"), "down", "hollow with a dash");
  const web = currentBackend() === "ws";
  assert.deepEqual(await rowButtons(page), web ? [] : ["Sign out"], "Sign out still makes sense on the desktop; New session does not");
  assert.equal(await page.locator(".conns .cx-sess").count(), 0);
  // The list comes back: the row says so on the next read.
  await page.evaluate(() => window.__MOCK__.remoteSessions.fails("server_sessions", null));
  await page.waitForFunction(() => document.querySelector(".conns .cx-srv-state")?.textContent === "signed in", null, { timeout: 7000 });
  assert.deepEqual(await rowButtons(page), web ? ["New session"] : ["Sign out", "New session"]);
}

/** While the store of secrets is locked the list is never asked for: the host would refuse it, and on Android that opens the unlock sheet. */
async function lockedCase(page, url) {
  if (currentBackend() === "ws") return; // the browser build has no store of secrets
  await open(page, url, { view: { ...SIGNED_IN, locked: true } });
  await page.evaluate(() => window.__registry.runAction("connections.open"));
  await page.waitForSelector(".conns .cx-srv-acts .cx-quiet");
  await page.waitForTimeout(5600);
  assert.equal(await lists(page), 0, "no list is read while locked, not even by the timer");
  assert.equal(await page.locator(".conns .cx-srv-state").textContent(), "signed in", "never unreachable");
  assert.equal(await page.locator(".conns .cx-sess").count(), 0);
  assert.equal(await page.locator(".conns .cx-help").count(), 0, "neither the help line nor the off line");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.conns", { state: "detached" });

  // The store locks between the read of the link and the read of the list: the row says the locked line, not unreachable.
  await open(page, url, { fails: { server_sessions: "secrets are locked" } });
  await page.evaluate(() => window.__registry.runAction("connections.open"));
  await page.waitForSelector(".conns .cx-srv-acts .cx-quiet");
  assert.equal(await page.locator(".conns .cx-srv-acts .cx-quiet").textContent(), "Locked: unlock to connect");
  assert.notEqual(await page.locator(".conns .cx-srv-state").textContent(), "unreachable");
  assert.deepEqual(await rowButtons(page), []);
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.conns", { state: "detached" });
}
