// The history dialog opened by the shell of a pane (the opt-in key binding of `wsh init`): the core sends `history-open`
// {pane} to the window that has the pane, the page opens the same dialog as the chord, and a pick goes to the pane the
// event names. A payload that is not a pane id, an event for another window and a second event while the dialog is open open
// nothing. The stub plays the core.
import assert from "node:assert/strict";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const hits = [{ cmd: "kubectl get pods -n kube-system", cwd: "/home/holz/infra", host: "local", last: Math.floor(Date.now() / 1000) - 7200, count: 14, exit: 0, marks: [] }];
  await page.addInitScript((h) => {
    window.__MOCK__.on("history_search", () => structuredClone(h));
    window.__MOCK__.on("history_delete", () => 1);
    window.__MOCK__.on("history_clear", () => null);
    window.__MOCK__.on("paste", () => "done");
  }, hits);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 1);

  // The page listens for the event, once.
  assert.equal(await page.evaluate(() => window.__MOCK__.listening("history-open")), 1);
  const dialogs = () => page.locator(".history").count();
  const frames = () => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => done(true)))));

  // An event for a pane opens the dialog although another pane (2) has the focus; a pick goes to the pane of the event.
  await page.evaluate(() => window.__MOCK__.emit("history-open", { pane: 1 }));
  await page.waitForSelector(".history .history-row");
  assert.equal(await dialogs(), 1);
  await page.keyboard.press("Enter");
  await page.waitForSelector(".history", { state: "detached" });
  const pasted = await page.evaluate(() => window.__MOCK__.callsOf("paste").at(-1));
  assert.equal(pasted.pane, 1, "into the pane that asked, not into the one the page has focused");
  assert.equal(pasted.text, "kubectl get pods -n kube-system");
  assert.equal(pasted.allow_unsafe, false);
  assert.ok(!pasted.text.includes("\n"), "nothing runs: no line break goes with the command");

  // The chord still pastes into the focused pane.
  await page.keyboard.press("Meta+Shift+H");
  await page.waitForSelector(".history .history-row");
  await page.keyboard.press("Enter");
  await page.waitForSelector(".history", { state: "detached" });
  assert.equal((await page.evaluate(() => window.__MOCK__.callsOf("paste").at(-1))).pane, 2);

  // What is not a pane id opens nothing.
  const pastes = await page.evaluate(() => window.__MOCK__.callsOf("paste").length);
  for (const bad of [null, [], "1", {}, { pane: -1 }, { pane: 1.5 }, { pane: 2 ** 60 }, { pane: "1" }, [{ pane: 1 }]]) {
    await page.evaluate((payload) => window.__MOCK__.emit("history-open", payload), bad);
    await frames();
    assert.equal(await dialogs(), 0, `the payload ${JSON.stringify(bad)} opened the dialog`);
  }

  // An event addressed to another window is not for this page.
  await page.evaluate(() => window.__MOCK__.emitTo("other", "history-open", { pane: 1 }));
  await frames();
  assert.equal(await dialogs(), 0, "an event of another window opened the dialog");

  // A second event while it is open adds no second dialog; Escape closes it and a later event opens it again.
  await page.evaluate(() => window.__MOCK__.emit("history-open", { pane: 1 }));
  await page.waitForSelector(".history .history-row");
  await page.evaluate(() => window.__MOCK__.emit("history-open", { pane: 2 }));
  await frames();
  assert.equal(await dialogs(), 1, "one dialog at a time");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".history", { state: "detached" });
  await page.evaluate(() => window.__MOCK__.emit("history-open", { pane: 1 }));
  await page.waitForSelector(".history .history-row");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".history", { state: "detached" });
  assert.equal(await page.evaluate(() => window.__MOCK__.callsOf("paste").length), pastes, "nothing was pasted by a refused or closed dialog");
  assert.deepEqual(errors, [], "the page reported errors");
}
