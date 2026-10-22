// The Shell button of a Kubernetes row (design.md 6.11): a terminal in a new tab with the context of the row, the core's sentence
// when it refuses, and the note that a shell's own startup file can replace the context while such a shell is focused.
import assert from "node:assert/strict";
import { KUBE_ROW, callsOf, layout, openContext, paneInfo, row, sectionShown } from "./integrate-context-lib.mjs";

const kube = '.cx-row[data-tool="kube"]';

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const launched = layout(5, [1, 5]);
  await openContext(page, url, { launch: { pane: 5, view: launched } });
  await sectionShown(page);

  // Only the Kubernetes row has the button, and only because the core listed a read verb `shell` that opens a terminal.
  assert.equal(await page.locator(".cx-act[data-act=shell]").count(), 1);
  assert.equal(await page.locator(`${kube} .cx-act[data-act=shell]`).textContent(), "Shell");
  assert.equal(await page.locator(".cx-note").count(), 0);

  await page.locator(`${kube} .cx-act[data-act=shell]`).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_launch").length === 1);
  assert.deepEqual((await callsOf(page, "integration_launch"))[0], { tool: "kube", verb: "shell", pane: 1, args: {}, placement: "tab" });
  await page.waitForSelector('.panehead[data-pane="5"]');

  // The new pane is the focused one, and the core lists it as a terminal of the tool: the chip is for the next task, the note is here.
  await page.evaluate(() => {
    window.__CX__.panes = [{ pane: 5, tool: "kube", view: "shell", context: "prod-eu", namespace: "payments", production: null, held: false, status: null }];
    window.__MOCK__.emit("integration", { seq: 1, kind: "context", tool: "kube", pane: 5, code: null });
  });
  await page.waitForFunction(() => document.querySelector(".cx-note")?.textContent === "A shell's own startup file can replace this.");
  assert.equal(await page.locator(`${kube} .cx-note`).count(), 1, "under the Kubernetes row");

  // The note goes when the focus leaves such a shell.
  const back = layout(1, [1, 5]);
  await page.evaluate((v) => {
    window.__MOCK__.setView(v);
    window.__MOCK__.emit("layout", v);
  }, back);
  await page.waitForFunction(() => document.querySelector(".cx-note") === null);

  // A refusal is the core's sentence in the line under the section, and no pane is made.
  await page.evaluate(() => {
    window.__CX__.launchFault = { code: "refused", reason: "no_room", text: "There is no room for another pane here." };
  });
  await page.locator(`${kube} .cx-act[data-act=shell]`).click();
  await page.waitForFunction(() => document.querySelector(".cx-line")?.textContent === "There is no room for another pane here.");
  assert.equal(await page.locator(".cx-line").getAttribute("data-tone"), "warn");
  assert.deepEqual(errors, [], "the page reported errors");
}
