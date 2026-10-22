// The Context section of the sidebar (design.md 6.11): one row for each tool the core listed, with the mark, the name and the
// selection as text, between Hosts and History, drawn from the one summary the core sends and asked as the core says.
import assert from "node:assert/strict";
import { KUBE_FULL, KUBE_ROW, callsOf, openContext, row, sectionShown } from "./integrate-context-lib.mjs";

const HOSTILE = "<img src=x onerror=alert(1)>";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await openContext(page, url, { tools: [KUBE_FULL], rows: [KUBE_ROW] });
  await sectionShown(page);

  // Where it sits: between Hosts and History, under the label Context.
  const heads = await page.locator("#sidebar .sb-section > .sb-head > span:first-child").allTextContents();
  assert.deepEqual(heads, ["Hosts", "Context", "History", "Widgets"]);
  assert.equal(await page.locator(".cx-row").count(), 1, "one row for the one tool the core listed");
  assert.equal(await page.locator(".cx-row .cx-mark").textContent(), "⎈");
  assert.equal(await page.locator(".cx-row .cx-label").textContent(), "Kubernetes");
  assert.equal(await page.locator(".cx-row .cx-sel").textContent(), "prod-eu · payments");
  assert.equal(await page.locator("#sidebar").innerText().then((t) => t.includes("⌘⇧C")), false, "no chord is bound, so none is drawn");

  // The core is asked with the focused pane and the flag, and nothing else.
  const first = (await callsOf(page, "integration_context"))[0];
  assert.deepEqual(first, { pane: 1, refresh: false });

  // A name that looks like markup is those characters, and no element is made from it.
  await page.evaluate((h) => {
    window.__CX__.rows = [{ tool: "kube", state: "found", context: h, namespace: h, node: null, production: null, section: null, stale: false, subscription: null }];
    window.__MOCK__.emit("integration", { seq: 1, kind: "context", tool: "kube", pane: null, code: null });
  }, HOSTILE);
  await page.waitForFunction((h) => document.querySelector(".cx-row .cx-sel")?.textContent === `${h} · ${h}`, HOSTILE);
  assert.equal(await page.locator("#sidebar img").count(), 0);
  assert.equal(await page.locator("#sidebar [onerror]").count(), 0);
  assert.deepEqual(errors, [], "the page reported errors");
}
