// The chip in the header of a pane (design.md 6.11): the context holzWave set for a terminal it gave one to, with the production
// word when the core says so. A pane the core did not list has none and its header is as before; an integration block names its
// context in its own header and has no second one.
import assert from "node:assert/strict";
import { answer, podsView } from "./integrate-lib.mjs";
import { layout, openContext, paneInfo } from "./integrate-context-lib.mjs";

const head = (id) => `.panehead[data-pane="${id}"]`;

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const block = { pane: 3, kind: "integration", source: "kube", path: "pods", source_label: "Kubernetes" };
  await openContext(page, url, {
    panes: [paneInfo(2, { view: "shell", context: "prod-eu" }), paneInfo(3, { view: "pods", context: "prod-eu" })],
    blocks: [block],
    runs: { pods: answer(podsView()) },
    sidebar: false,
  }, layout(1, [1, 2, 3]));
  await page.waitForSelector(`${head(2)} .ph-chip`);

  // A listed terminal has the chip, between the title and the context, and says who set it.
  const chip = page.locator(`${head(2)} .ph-chip`);
  assert.equal(await chip.locator(".ph-chip-text").textContent(), "⎈ prod-eu");
  assert.equal(await chip.getAttribute("title"), "Set by holzWave for this pane");
  assert.equal(await chip.locator(".ph-chip-prod").count(), 0);
  const kids = await page.locator(head(2)).evaluate((n) => Array.from(n.children).map((c) => c.className));
  assert.deepEqual(kids, ["ph-dot", "label ph-title", "label ph-sub", "ph-chip", "ph-ctx", "close ph-close"]);

  // A pane the core did not list has none, and its header is the one it always had.
  assert.equal(await page.locator(`${head(1)} .ph-chip`).count(), 0);
  assert.deepEqual(await page.locator(head(1)).evaluate((n) => Array.from(n.children).map((c) => c.className)), ["ph-dot", "label ph-title", "label ph-sub", "ph-ctx", "close ph-close"]);

  // An integration block names its context in its own header; the chip would say it twice.
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="3"] .ph-ctx')?.textContent?.startsWith("Kubernetes"));
  assert.equal(await page.locator(`${head(3)} .ph-chip`).count(), 0);

  // Production is the core's word: it comes with the underline and the mark, and a name that says production does not make it.
  await page.evaluate(() => {
    window.__CX__.panes = [
      { pane: 2, tool: "kube", view: "shell", context: "production", namespace: null, production: "Production", held: false, status: null },
      { pane: 3, tool: "kube", view: "pods", context: "x", namespace: null, production: null, held: false, status: null },
    ];
    window.__MOCK__.emit("integration", { seq: 1, kind: "context", tool: "kube", pane: 2, code: null });
  });
  const word = page.locator(`${head(2)} .ph-chip-prod`);
  await word.waitFor();
  assert.equal(await word.textContent(), "production");
  assert.match(await word.evaluate((n) => getComputedStyle(n).textDecorationLine), /underline/);
  assert.equal(await word.evaluate((n) => getComputedStyle(n, "::before").content), '"! "');

  // A pane named like production but not flagged is a name.
  await page.evaluate(() => {
    window.__CX__.panes = [{ pane: 2, tool: "kube", view: "shell", context: "production", namespace: null, production: null, held: false, status: null }];
    window.__MOCK__.emit("integration", { seq: 2, kind: "context", tool: "kube", pane: 2, code: null });
  });
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-chip-prod') === null);
  assert.equal(await chip.locator(".ph-chip-text").textContent(), "⎈ production");

  // The pane left the list (its selection is gone): the chip goes with it.
  await page.evaluate(() => {
    window.__CX__.panes = [];
    window.__MOCK__.emit("integration", { seq: 3, kind: "context", tool: "kube", pane: 2, code: null });
  });
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-chip') === null);
  assert.deepEqual(errors, [], "the page reported errors");
}
