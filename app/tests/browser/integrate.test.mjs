// The integration block (design.md section 6.25): a table from the core, drawn as text, with the header of its pane.
import assert from "node:assert/strict";
import { HOSTILE, openBlock, rowsShown } from "./integrate-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await openBlock(page, url);
  await rowsShown(page);

  const head = await page.locator(".block-integration .ig-table thead th:not(.ig-th-act)").allTextContents();
  assert.deepEqual(head, ["NAME", "READY", "STATUS", "RESTARTS", "AGE"], "a header row with the core's labels");
  assert.equal(await page.locator(".block-integration .ig-table tbody tr").count(), 2);
  assert.equal(await page.locator(".block-integration .ig-table").getAttribute("role"), "table");

  // A pod named like an image with an error handler is those characters, and no element is made.
  const names = await page.locator(".block-integration .ig-table tbody tr td:first-child").allTextContents();
  assert.equal(names[0], "api-0");
  assert.equal(names[1], HOSTILE);
  assert.equal(await page.locator(".block-integration img").count(), 0);
  assert.equal(await page.locator(".block-integration [onerror]").count(), 0);

  // The pane says what it shows and where it reads from.
  assert.equal(await page.locator('.panehead[data-pane="2"] .ph-title').textContent(), "pods");
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-ctx')?.textContent === "Kubernetes · prod · payments · 10 s");

  // The core was asked with the facts of the pane and nothing else.
  const first = await page.evaluate(() => window.__MOCK__.callsOf("integration_run")[0]);
  assert.deepEqual(first, { tool: "kube", verb: "pods", pane: 2, args: {} });
  assert.deepEqual(errors, [], "the page reported errors");
}
