// The status line segments (design.md 6.11): `⎈ context/namespace` for Kubernetes and `az subscription` for Azure, from the one
// answer the section reads, so they follow the focused pane first, then the pick, then what the tool is set to (the core
// resolves that for the pane it is asked about). Production is a word in the segment; the segment is a button that brings the
// section into view.
import assert from "node:assert/strict";
import { layout, openContext, row, sectionShown } from "./integrate-context-lib.mjs";

const seg = (tool) => `#status .ctx-seg[data-tool="${tool}"]`;
const part = (tool, cls) => `${seg(tool)} .${cls}`;

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const rows = [
    row("kube", { context: "prod-eu", namespace: "payments" }),
    row("talos", { context: "talos-prod", node: "node-1" }),
    row("azure", { subscription: "holz-prod" }),
    row("gam", { section: "work" }),
  ];
  const own = [row("kube", { context: "dev", namespace: "default", production: "Production" }), ...rows.slice(1)];
  await openContext(page, url, { rows, rowsByPane: { 2: own }, sidebar: false }, layout(1, [1, 2]));
  await page.waitForSelector(seg("kube"));

  // Where they sit: after the pane count, before the renderer; only Kubernetes and Azure have one.
  const order = await page.locator("#status > *").evaluateAll((n) => n.map((x) => x.className.replace("seg label ", "")));
  assert.deepEqual(order, ["mode", "notice", "conns", "panes", "ctx-seg", "ctx-seg", "renderer", "encoding", "version"]);
  assert.equal(await page.locator(part("kube", "ctx-text")).textContent(), "⎈ prod-eu/payments");
  assert.equal(await page.locator(part("azure", "ctx-text")).textContent(), "az holz-prod");
  assert.equal(await page.locator("#status .ctx-seg").count(), 2);
  assert.equal(await page.locator(seg("kube")).evaluate((n) => n.tagName), "BUTTON");
  assert.equal(await page.locator(seg("kube")).getAttribute("aria-label"), "Kubernetes context prod-eu, namespace payments");
  assert.equal(await page.locator(part("kube", "ctx-prod")).count(), 0, "no production word without the core's flag");

  // The pane that has the focus decides: pane 2 has its own selection, and it is a production one.
  await page.evaluate((v) => {
    window.__MOCK__.setView(v);
    window.__MOCK__.emit("layout", v);
  }, layout(2, [1, 2]));
  await page.waitForFunction(() => document.querySelector('#status .ctx-seg[data-tool="kube"] .ctx-text')?.textContent === "⎈ dev/default");
  // Production: the word in the segment, with an underline and the mark besides its colour.
  const word = page.locator(part("kube", "ctx-prod"));
  assert.equal(await word.textContent(), "PRODUCTION");
  assert.match(await word.evaluate((n) => getComputedStyle(n).textDecorationLine), /underline/);
  assert.equal(await word.evaluate((n) => getComputedStyle(n, "::before").content), '"! "');
  assert.equal(await page.locator(seg("kube")).getAttribute("aria-label"), "Kubernetes context dev, namespace default, production");

  // Names are shown as they are, not in capitals: a name is case sensitive.
  await page.evaluate(() => {
    window.__CX__.rowsByPane[2] = [{ tool: "kube", state: "found", context: "Prod-EU", namespace: "payments", production: null, stale: false, node: null, section: null, subscription: null }];
    window.__MOCK__.emit("integration", { seq: 1, kind: "context", tool: "kube", pane: null, code: null });
  });
  await page.waitForFunction(() => document.querySelector('#status .ctx-seg[data-tool="kube"] .ctx-text')?.textContent === "⎈ Prod-EU/payments");
  assert.equal(await page.locator(part("kube", "ctx-text")).evaluate((n) => getComputedStyle(n).textTransform), "none");

  // Between the focus moving and the core answering, the segment is dimmed and says so, because it may still be the old pane's.
  await page.evaluate(() => {
    window.__CX__.delay = 600;
  });
  await page.evaluate((v) => {
    window.__MOCK__.setView(v);
    window.__MOCK__.emit("layout", v);
  }, layout(1, [1, 2]));
  await page.waitForFunction(() => document.querySelector('#status .ctx-seg[data-tool="kube"] .ctx-stale')?.textContent === "stale");
  assert.equal(await page.locator(seg("kube")).getAttribute("data-stale"), "true");
  await page.waitForFunction(() => document.querySelector('#status .ctx-seg[data-tool="kube"] .ctx-stale') === null);
  assert.equal(await page.locator(part("kube", "ctx-text")).textContent(), "⎈ prod-eu/payments");

  // No value, no segment: Azure signed out has none.
  await page.evaluate(() => {
    window.__CX__.delay = 0;
    window.__CX__.rows = window.__CX__.rows.map((r) => (r.tool === "azure" ? { ...r, state: "signed_out", subscription: null } : r));
    window.__MOCK__.emit("integration", { seq: 2, kind: "context", tool: "azure", pane: null, code: null });
  });
  await page.waitForFunction(() => document.querySelector('#status .ctx-seg[data-tool="azure"]') === null);

  // The sidebar was hidden; pressing a segment shows it and brings the section into view with the keyboard on its first value.
  assert.equal(await page.locator("#sidebar").isHidden(), true);
  await page.locator(seg("kube")).click();
  await page.waitForSelector("#sidebar:not([hidden])");
  await sectionShown(page);
  assert.equal(await page.evaluate(() => document.activeElement?.dataset.select), "context");
  assert.equal(await page.locator(".cx-section").isVisible(), true);

  // One answer feeds both: the section's row and the segment say the same thing.
  const sectionText = await page.locator('.cx-row[data-tool="kube"] .cx-sel').textContent();
  const segmentText = await page.locator(part("kube", "ctx-text")).textContent();
  assert.equal(segmentText, `⎈ ${sectionText.replace(" · ", "/")}`);
  assert.deepEqual(errors, [], "the page reported errors");
}
