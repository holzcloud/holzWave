// The marks of the status line that depend on the clock (docs/design.md 6.30): a lost connection draws nothing for one
// second, the browser's own offline report outranks it, and the idle expiry counts down by the minute. The clock of the page is
// Playwright's, paused after the start, so a second and a minute are played in a moment and nothing waits.
import assert from "node:assert/strict";
import { actionTitles, leftover, marksOf, openMain, sessionRoute } from "./websession-lib.mjs";

export const backends = ["ws"];
export const backendNote = "the marks exist on the web backend only: the connection and the session come from the server";

/** Let the promises the last clock step started run to their end. */
const settled = (page) => page.evaluate(() => new Promise((done) => queueMicrotask(() => queueMicrotask(done))));
const emit = async (page, state, seconds) => {
  await page.evaluate(([s, n]) => window.__MOCK__.emit("hw-session", n === undefined ? { state: s } : { state: s, seconds: n }), [state, seconds]);
  await settled(page);
};
const seenOf = (page) => page.evaluate(() => window.__seen);
const liveOf = (page) => page.evaluate(() => document.querySelector('#status .web-mark[data-mark="expiring"]')?.getAttribute("aria-live") ?? null);
const sockets = (page) => page.evaluate(() => window.__WS_MOCK__.sockets.length);

export default async function ({ page, url, errors }) {
  await page.clock.install();
  const server = await openMain(page, url, [sessionRoute()]);
  await page.waitForFunction(() => window.__WS_MOCK__.sockets.length >= 1);
  // Every mark that was ever drawn, in the order it was drawn: a mark that shows for a moment cannot slip by.
  await page.evaluate(() => {
    window.__seen = [];
    new MutationObserver(() => {
      for (const m of document.querySelectorAll("#status .web-mark")) window.__seen.push(m.dataset.mark);
    }).observe(document.getElementById("status"), { childList: true, subtree: true });
  });
  const now = await page.evaluate(() => Date.now());
  await page.clock.pauseAt(new Date(now + 1000));
  assert.deepEqual(await marksOf(page), [], "a live session with a long time left shows nothing");

  // ---- a quick reconnect shows nothing at all ---------------------------------------------------------------------------
  await page.evaluate(() => window.__WS_MOCK__.drop(1006));
  await page.clock.runFor(300);
  assert.deepEqual(await marksOf(page), []);
  await page.clock.runFor(500);
  await settled(page);
  assert.equal(await sockets(page), 2, "the socket came back");
  assert.deepEqual(await marksOf(page), []);
  await page.clock.runFor(2000);
  assert.deepEqual(await seenOf(page), [], "no mark was drawn, not even for a moment");

  // ---- a longer break: nothing at 0.9 s, `reconnecting` at 1.1 s, gone after the return ---------------------------------
  await page.evaluate(() => {
    window.__WS_MOCK__.refuseOpen(3);
    window.__WS_MOCK__.drop(1006);
  });
  await page.clock.runFor(900);
  assert.deepEqual(await marksOf(page), [], "nothing at 0.9 s");
  await page.clock.runFor(200);
  assert.deepEqual(await marksOf(page), ["reconnecting"], "reconnecting after 1.1 s");
  assert.equal(await page.locator('#status .web-mark[data-mark="reconnecting"] .web-glyph').textContent(), "◌");
  for (let i = 0; i < 40 && (await marksOf(page)).length > 0; i++) {
    await page.clock.runFor(500);
    await settled(page);
  }
  assert.deepEqual(await marksOf(page), [], "gone after the connection is back");
  assert.ok((await seenOf(page)).includes("reconnecting"));
  assert.ok((await seenOf(page)).every((m) => m === "reconnecting"), "only that mark was drawn");

  // ---- offline outranks reconnecting, and needs no wait ---------------------------------------------------------------
  await page.evaluate(() => {
    window.__WS_MOCK__.refuseOpen(1000);
    window.__WS_MOCK__.drop(1006);
  });
  await page.clock.runFor(1500);
  assert.deepEqual(await marksOf(page), ["reconnecting"]);
  await page.evaluate(() => window.dispatchEvent(new Event("offline")));
  assert.deepEqual(await marksOf(page), ["offline"], "offline replaces reconnecting at once");
  assert.equal(await page.locator('#status .web-mark[data-mark="offline"]').getAttribute("data-tone"), "danger");
  assert.equal(await page.locator('#status .web-mark[data-mark="offline"] .web-glyph').textContent(), "!");
  await page.evaluate(() => window.dispatchEvent(new Event("online")));
  assert.deepEqual(await marksOf(page), ["reconnecting"], "back online, the connection is still down");
  await page.evaluate(() => {
    window.__WS_MOCK__.refuseOpen(0);
  });
  for (let i = 0; i < 80 && (await marksOf(page)).length > 0; i++) {
    await page.clock.runFor(1000);
    await settled(page);
  }
  assert.deepEqual(await marksOf(page), [], "the connection came back");
  await page.evaluate(() => window.dispatchEvent(new Event("offline")));
  assert.deepEqual(await marksOf(page), ["offline"], "the browser says there is no network");
  await page.evaluate(() => window.dispatchEvent(new Event("online")));
  assert.deepEqual(await marksOf(page), []);

  // ---- the idle expiry: five minutes, counting down by the minute -------------------------------------------------------
  await emit(page, "expiring", 240);
  assert.deepEqual(await marksOf(page), ["session ends in 4 min"]);
  assert.equal(await page.locator('#status .web-mark[data-mark="expiring"] .web-glyph').textContent(), "◔");
  assert.equal(await page.locator('#status .web-mark[data-mark="expiring"]').getAttribute("data-tone"), "warn");
  assert.equal(await liveOf(page), "polite", "announced when it first appears");
  await page.clock.runFor(59_000);
  assert.deepEqual(await marksOf(page), ["session ends in 4 min"], "it does not change before the minute is over");
  await page.clock.runFor(2000);
  assert.deepEqual(await marksOf(page), ["session ends in 3 min"], "61 s later, 3 min");
  assert.equal(await liveOf(page), "off", "not announced again every minute");
  await page.clock.runFor(120_000);
  assert.deepEqual(await marksOf(page), ["session ends in 1 min"]);
  assert.equal(await liveOf(page), "polite", "announced again at the last minute");
  await emit(page, "ok", 1500);
  assert.deepEqual(await marksOf(page), [], "activity ends it");
  await emit(page, "ok", 301);
  assert.deepEqual(await marksOf(page), [], "301 s is not shown");
  await emit(page, "ok", 300);
  assert.deepEqual(await marksOf(page), ["session ends in 5 min"]);
  await emit(page, "ok", 61);
  assert.deepEqual(await marksOf(page), ["session ends in 2 min"]);
  await emit(page, "ok", 60);
  assert.deepEqual(await marksOf(page), ["session ends in 1 min"]);
  await emit(page, "ok", 900);
  assert.deepEqual(await marksOf(page), []);

  // ---- the order of the marks: connection, expiry, lock ---------------------------------------------------------------
  await emit(page, "expiring", 120);
  await emit(page, "locked");
  assert.deepEqual(await marksOf(page), ["session ends in 2 min", "secrets locked"]);
  await page.evaluate(() => window.dispatchEvent(new Event("offline")));
  assert.deepEqual(await marksOf(page), ["offline", "session ends in 2 min", "secrets locked"]);
  await page.evaluate(() => window.dispatchEvent(new Event("online")));
  assert.ok((await actionTitles(page)).includes("Unlock…"));
  // The lock mark has its text button, a real button, in the tab order of the status line.
  assert.equal(await page.locator("#status button.web-act").count(), 1);
  await emit(page, "unlocked");
  await emit(page, "ok", 900);
  assert.deepEqual(await marksOf(page), []);
  assert.ok(!(await actionTitles(page)).includes("Unlock…"));

  // ---- the session ends while a mark is shown: only `session ended` remains ----------------------------------------------
  await emit(page, "expiring", 30);
  await emit(page, "ended", 0);
  assert.deepEqual(await marksOf(page), ["session ended"]);
  assert.equal(await page.locator('#status .web-mark[data-mark="ended"]').getAttribute("data-tone"), "danger");
  const l = leftover(server);
  assert.deepEqual(l.foreign, []);
  assert.deepEqual(l.unscripted, []);
  assert.deepEqual(errors, [], "the page reported errors");
}
