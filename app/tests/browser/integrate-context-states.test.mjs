// The words of the rows (design.md 6.11): production, signed out and stale each have a word, an underline or a dimmed value, and a
// choice the core refuses is its sentence in the line under the section; nothing changes.
import assert from "node:assert/strict";
import { answer, listView } from "./integrate-lib.mjs";
import { callsOf, openContext, row, sectionShown } from "./integrate-context-lib.mjs";

const kube = '.cx-row[data-tool="kube"]';

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const rows = [
    row("kube", { context: "prod-eu", namespace: "payments", production: "Production" }),
    row("talos", { context: "talos-prod", node: "node-1", stale: true }),
    row("azure", { state: "signed_out", stale: true }),
    row("gam", { section: "work" }),
  ];
  const runs = { contexts: answer(listView("Contexts", ["prod-eu", "dev"])) };
  await openContext(page, url, { rows, runs, selectFault: { code: "refused", reason: "invalid", text: "Context dev is not valid for this profile." } });
  await sectionShown(page);

  // Production is the core's flag: the word, an underline and the mark are all there besides the colour.
  const prod = page.locator(`${kube} .cx-word[data-kind=production]`);
  assert.equal(await prod.textContent(), "production");
  assert.match(await prod.evaluate((n) => getComputedStyle(n).textDecorationLine), /underline/);
  assert.equal(await prod.evaluate((n) => getComputedStyle(n, "::before").content), '"! "');
  assert.equal(await page.locator(`.cx-row:not([data-tool=kube]) .cx-word[data-kind=production]`).count(), 0);

  // Signed out is a word with the same cues, and no value is said to be stale when there is none.
  const out = page.locator('.cx-row[data-tool="azure"] .cx-word');
  assert.deepEqual(await out.allTextContents(), ["signed out"]);
  assert.match(await out.evaluate((n) => getComputedStyle(n).textDecorationLine), /underline/);
  assert.equal(await out.evaluate((n) => getComputedStyle(n, "::before").content), '"! "');

  // A stale value is dimmed and says so.
  const talos = page.locator('.cx-row[data-tool="talos"]');
  assert.equal(await talos.locator(".cx-word[data-kind=stale]").textContent(), "stale");
  assert.equal(await talos.getAttribute("data-stale"), "true");
  const dim = await talos.locator(".cx-sel").evaluate((n) => getComputedStyle(n).color);
  const ink = await page.locator('.cx-row[data-tool="gam"] .cx-sel').evaluate((n) => getComputedStyle(n).color);
  assert.notEqual(dim, ink, "dimmed besides the word");

  // A refused choice shows the core's sentence, changes nothing, and the list closes.
  await page.locator(`${kube} .cx-value[data-select="context"]`).click();
  await page.locator(`${kube} .ig-list .ig-opt`).first().waitFor();
  await page.locator(`${kube} .ig-list .ig-opt`, { hasText: "dev" }).click();
  await page.waitForFunction(() => document.querySelector(".cx-line")?.textContent === "Context dev is not valid for this profile.");
  assert.equal(await page.locator(".cx-line").getAttribute("data-tone"), "warn");
  assert.equal(await page.locator(`${kube} .cx-sel`).textContent(), "prod-eu · payments");
  assert.equal((await callsOf(page, "integration_select")).length, 1);

  // A list that cannot be read shows the core's sentence in place of the entries.
  await page.evaluate(() => {
    window.__CX__.faults = { contexts: { code: "failed", reason: "", text: "kubectl could not read the contexts." } };
  });
  await page.locator(`${kube} .cx-value[data-select="context"]`).click();
  await page.waitForFunction(() => document.querySelector('.cx-row[data-tool="kube"] .ig-list-msg')?.textContent === "kubectl could not read the contexts.");
  assert.deepEqual(errors, [], "the page reported errors");
}
