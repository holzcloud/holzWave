// The refresh of the integration block (design.md 6.25): the interval of the core's verb, only while the window is visible and
// the block shown, never two requests at once, and nothing after the pane is gone. The clock of the page is Playwright's.
import assert from "node:assert/strict";
import { FOCUSED, LAYOUT, openBlock, rowsShown } from "./integrate-lib.mjs";

const started = (page) => page.evaluate(() => window.__IG__.started);

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await page.clock.install();
  await openBlock(page, url, {}, FOCUSED);
  await rowsShown(page);
  assert.equal(await started(page), 1, "one read when the block opens");

  // The pods verb has an interval of 10 s: the block asks again by itself, once for each interval.
  await page.clock.runFor(9000);
  assert.equal(await started(page), 1, "not before the interval");
  await page.clock.runFor(2000);
  assert.equal(await started(page), 2);
  await page.clock.runFor(10000);
  assert.equal(await started(page), 3);

  // A window that is hidden costs nothing, and asks again when it is visible.
  await page.evaluate(() => {
    Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
  });
  await page.clock.runFor(60000);
  assert.equal(await started(page), 3, "no read while the window is hidden");
  await page.evaluate(() => {
    delete document.hidden;
  });
  await page.clock.runFor(2000);
  assert.equal(await started(page), 4);

  // A block that is not shown (its tab is not) asks nothing.
  await page.evaluate(() => {
    document.querySelector(".block-integration").style.display = "none";
  });
  await page.clock.runFor(60000);
  assert.equal(await started(page), 4, "no read while the block is not shown");
  await page.evaluate(() => {
    document.querySelector(".block-integration").style.display = "";
  });

  // A slow answer is never asked twice: the next read starts after it ended.
  await page.evaluate(() => {
    window.__IG__.delay = 25000;
    window.__IG__.maxInflight = 0;
  });
  await page.clock.runFor(2000);
  const slow = await started(page);
  assert.equal(slow, 5);
  await page.clock.runFor(20000);
  assert.equal(await started(page), slow, "still waiting for the first slow answer");
  await page.clock.runFor(20000);
  assert.equal(await page.evaluate(() => window.__IG__.maxInflight), 1, "never two requests of one block at once");
  await page.evaluate(() => {
    window.__IG__.delay = 0;
  });

  // The button and the key ask at once.
  const before = await started(page);
  await page.clock.runFor(30000);
  const now = await started(page);
  await page.locator(".ig-refresh").click();
  await page.waitForFunction((n) => window.__IG__.started > n, now);
  const afterButton = await started(page);
  await page.evaluate(() => document.getElementById("ime").focus());
  await page.keyboard.press("r");
  await page.waitForFunction((n) => window.__IG__.started > n, afterButton);
  assert.ok(now >= before);

  // The pane goes: nothing is asked any more.
  await page.evaluate(
    (layout) => {
      window.__IG__.blocks = [];
      window.__MOCK__.emit("layout", layout);
    },
    { ...LAYOUT, panes: [[1, { x: 0, y: 0, w: 1, h: 1 }]], dividers: [], tabs: [{ title: "pods", active: true, focus: 1, panes: 1 }], workspaces: [{ name: "main", active: true, panes: 1 }] },
  );
  await page.waitForFunction(() => document.querySelector(".block-integration") === null);
  const gone = await started(page);
  await page.clock.runFor(120000);
  assert.equal(await started(page), gone, "a closed pane asks nothing");

  assert.deepEqual(errors, [], "the page reported errors");
}
