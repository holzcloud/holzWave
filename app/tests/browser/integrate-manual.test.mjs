// A view whose verb has no interval is read when it opens and when the person asks, and never by itself (design.md 6.25).
import assert from "node:assert/strict";
import { BLOCK, answer, listView, openBlock, rowsShown } from "./integrate-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await page.clock.install();
  await openBlock(page, url, { blocks: [{ ...BLOCK, path: "contexts" }], runs: { contexts: answer(listView("Contexts", ["prod", "dev"])) } });
  await rowsShown(page);
  assert.equal(await page.evaluate(() => window.__IG__.started), 1);
  // The header names no interval for a view that does not read by itself.
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-ctx')?.textContent === "Kubernetes · prod · payments");
  assert.equal(await page.locator('.panehead[data-pane="2"] .ph-title').textContent(), "contexts");
  await page.clock.runFor(120000);
  assert.equal(await page.evaluate(() => window.__IG__.started), 1, "no read by itself");
  await page.locator(".ig-refresh").click();
  await page.waitForFunction(() => window.__IG__.started === 2);
  // The age advances while nothing is read.
  await page.clock.runFor(30000);
  await page.waitForFunction(() => /^Updated \d+ (s|min) ago$/.test(document.querySelector(".ig-status")?.textContent ?? ""));
  // The event of the core holds no data: for this pane it makes the block ask; for another pane it does nothing; a gap or a
  // change of the tools asks the core again.
  const reads = () => page.evaluate(() => window.__IG__.started);
  const tools = () => page.evaluate(() => window.__MOCK__.callsOf("integration_tools").length);
  await page.waitForFunction(() => window.__MOCK__.listening("integration") === 1);
  const r0 = await reads();
  await page.evaluate(() => window.__MOCK__.emit("integration", { seq: 1, kind: "context", tool: "kube", pane: 9, code: null }));
  await page.evaluate(() => window.__MOCK__.emit("integration", { seq: 2, kind: "context", tool: "kube", pane: 2, code: null }));
  await page.waitForFunction((n) => window.__IG__.started === n + 1, r0);
  const t0 = await tools();
  await page.evaluate(() => window.__MOCK__.emit("integration", { seq: 3, kind: "tools", tool: null, pane: null, code: null }));
  await page.waitForFunction((n) => window.__MOCK__.callsOf("integration_tools").length === n + 1, t0);
  const r1 = await reads();
  await page.evaluate(() => window.__MOCK__.emit("integration", { seq: 9, kind: "profiles", tool: null, pane: null, code: null }));
  await page.waitForFunction((n) => window.__IG__.started === n + 1, r1);
  assert.equal(await tools(), t0 + 2, "a gap asks for the tools again");
  assert.deepEqual(errors, [], "the page reported errors");
}
