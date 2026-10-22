// With no integration data on a desktop nothing is added (design.md 6.11): the status line has the segments it always had, a pane
// header has the children it always had, and the sidebar has its three sections. The core is asked once and its empty answer
// draws nothing.
import assert from "node:assert/strict";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await page.addInitScript(() => localStorage.setItem("holzwave.sidebar", "1"));
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".panehead");
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_context").length >= 1);
  await page.waitForTimeout(300);
  assert.deepEqual(
    await page.locator("#status > *").evaluateAll((n) => n.map((x) => x.className)),
    ["seg label mode", "seg label notice", "seg label conns", "seg label panes", "seg label renderer", "seg label encoding", "seg label version"],
  );
  for (const head of await page.locator(".panehead").all()) {
    assert.deepEqual(await head.evaluate((n) => Array.from(n.children).map((c) => c.className)), ["ph-dot", "label ph-title", "label ph-sub", "ph-ctx", "close ph-close"]);
  }
  assert.deepEqual(await page.locator("#sidebar .sb-section > .sb-head > span:first-child").allTextContents(), ["Hosts", "History", "Widgets"]);
  assert.equal(await page.locator(".cx-section").count(), 0);
  assert.deepEqual(errors, [], "the page reported errors");
}
