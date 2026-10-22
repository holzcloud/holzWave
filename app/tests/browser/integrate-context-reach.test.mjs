// The reach of a choice in the Context section (design.md 6.11): it sets the selection of the focused pane when that pane is an
// integration block of the tool or a terminal a launch opened for it, else the pick of the tool, and the line under the section
// says which. Nothing but a selection is written, and the namespace is a second target on the same row.
import assert from "node:assert/strict";
import { answer, listView } from "./integrate-lib.mjs";
import { callsOf, layout, openContext, paneInfo, row, sectionShown } from "./integrate-context-lib.mjs";

const kube = '.cx-row[data-tool="kube"]';

/** Show a layout the way the core sends it, with the rows and panes the core would answer for it; `mark` is the namespace of those rows, which says they arrived. */
async function focusPane(page, id, ids, panes, mark) {
  await page.evaluate(([v, panes, rows, id]) => {
    window.__CX__.panes = panes;
    window.__CX__.rowsByPane[id] = rows;
    window.__MOCK__.setView(v);
    window.__MOCK__.emit("layout", v);
    // The core says a selection changed (the panes it lists changed), as it does when a pane gets or loses one.
    window.__SEQ__ = (window.__SEQ__ ?? 0) + 1;
    window.__MOCK__.emit("integration", { seq: window.__SEQ__, kind: "context", tool: null, pane: id, code: null });
  }, [layout(id, ids), panes, [row("kube", { context: "prod-eu", namespace: mark })], id]);
  await page.waitForFunction((m) => document.querySelector('.cx-row[data-tool="kube"] .cx-sel')?.textContent.endsWith(m), mark);
}

async function choose(page, select, entry) {
  await page.locator(`${kube} .cx-value[data-select="${select}"]`).click();
  await page.locator(`${kube} .ig-list .ig-opt`).first().waitFor();
  await page.locator(`${kube} .ig-list .ig-opt`, { hasText: entry }).click();
}

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const runs = {
    contexts: answer(listView("Contexts", ["prod-eu", "dev"])),
    namespaces: answer(listView("Namespaces", ["payments", "default"])),
  };
  await openContext(page, url, { runs }, layout(1, [1, 2, 3]));
  await sectionShown(page);

  // A plain shell has the focus: the choice is the pick, and the line says what new blocks start with.
  await page.locator(`${kube} .cx-value[data-select="context"]`).click();
  await page.locator(`${kube} .ig-list .ig-opt`).first().waitFor();
  assert.deepEqual(await page.locator(`${kube} .ig-list .ig-opt`).allTextContents(), ["●prod-eu", "○dev", "Make default…"]);
  assert.equal(await page.locator(`${kube} .ig-list`).getAttribute("role"), "listbox");
  assert.deepEqual((await callsOf(page, "integration_run")).at(-1), { tool: "kube", verb: "contexts", pane: null, args: {} });
  await page.locator(`${kube} .ig-list .ig-opt`, { hasText: "dev" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_select").length === 1);
  assert.deepEqual((await callsOf(page, "integration_select"))[0], { tool: "kube", pane: null, selection: { context: "dev" } }, "the namespace of the old context is dropped");
  assert.equal(await page.locator(".cx-line").textContent(), "New blocks start with dev.");
  assert.equal(await page.locator(`${kube} .ig-list`).count(), 0, "the list closes");
  assert.equal(await page.evaluate(() => document.activeElement?.dataset.select), "context", "the keyboard returns to the value");

  // A terminal that a launch opened for the tool has the focus: the choice is for that pane, and the namespace is a second target.
  await focusPane(page, 3, [1, 2, 3], [paneInfo(3)], "n3");
  await choose(page, "namespace", "default");
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_select").length === 2);
  assert.deepEqual((await callsOf(page, "integration_select"))[1], { tool: "kube", pane: 3, selection: { context: "prod-eu", namespace: "default" } });
  assert.deepEqual((await callsOf(page, "integration_run")).at(-1), { tool: "kube", verb: "namespaces", pane: 3, args: {} });
  assert.equal(await page.locator(".cx-line").textContent(), "Pane 3 now uses default.");

  // An integration block of the tool in the core's list: the same.
  await focusPane(page, 2, [1, 2, 3], [paneInfo(2, { view: "pods" })], "n2");
  await choose(page, "context", "dev");
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_select").length === 3);
  assert.equal((await callsOf(page, "integration_select"))[2].pane, 2);
  assert.equal(await page.locator(".cx-line").textContent(), "Pane 2 now uses dev.");

  // A pane of another tool has the focus: the pick again.
  await focusPane(page, 2, [1, 2, 3], [paneInfo(2, { tool: "talos", view: "nodes" })], "n2b");
  await choose(page, "context", "dev");
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_select").length === 4);
  assert.equal((await callsOf(page, "integration_select"))[3].pane, null);
  assert.equal(await page.locator(".cx-line").textContent(), "New blocks start with dev.");

  // Escape closes the list and gives the keyboard back to the terminal.
  await page.locator(`${kube} .cx-value[data-select="context"]`).click();
  await page.locator(`${kube} .ig-list .ig-opt`).first().waitFor();
  await page.keyboard.press("Escape");
  assert.equal(await page.locator(`${kube} .ig-list`).count(), 0);
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime");
  assert.equal((await callsOf(page, "integration_select")).length, 4, "closing a list selects nothing");
  assert.deepEqual(errors, [], "the page reported errors");
}
