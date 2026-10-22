// On a phone (app_info names ios or android) the Context section, the status segments and the pane chips are not drawn, and the
// core is not asked for the context.
import assert from "node:assert/strict";
import { layout, openContext, paneInfo, ROWS } from "./integrate-context-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await openContext(page, url, { os: "android", rows: ROWS, panes: [paneInfo(2)] }, layout(1, [1, 2]));
  await page.waitForSelector('.panehead[data-pane="2"]');
  await page.waitForTimeout(600);
  assert.equal(await page.evaluate(() => window.__MOCK__.callsOf("integration_context").length), 0, "a phone asks nothing");
  assert.equal(await page.evaluate(() => window.__MOCK__.callsOf("integration_tools").length), 0);
  assert.equal(await page.locator(".cx-section").count(), 0);
  assert.equal(await page.locator("#status .ctx-seg").count(), 0);
  assert.equal(await page.locator(".ph-chip").count(), 0);
  // The sidebar's other sections are as they were.
  const heads = await page.locator("#sidebar .sb-section > .sb-head > span:first-child").allTextContents();
  assert.deepEqual(heads, ["Hosts", "History", "Widgets"]);

  assert.deepEqual(errors, [], "the page reported errors");
}
