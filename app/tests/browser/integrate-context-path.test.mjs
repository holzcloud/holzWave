// A tool that is missing or cannot be used (design.md 6.11): the row says so in words with a second cue and offers the path field
// of the block; the core checks the path and its sentence is shown when it refuses one.
import assert from "node:assert/strict";
import { ALL_TOOLS, KUBE_FULL, KUBE_ROW, callsOf, openContext, row, sectionShown } from "./integrate-context-lib.mjs";

const kube = '.cx-row[data-tool="kube"]';
const MISSING = { ...KUBE_FULL, state: "not_found", path: null, searched: ["/usr/local/bin", "/opt/homebrew/bin"] };

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const rows = [row("kube", { state: "not_found" }), row("talos", { state: "not_found" })];
  await openContext(page, url, { tools: [MISSING, ...ALL_TOOLS.slice(1)], rows, toolsAfterSet: ALL_TOOLS });
  await sectionShown(page);

  // The section is never an empty heading: a missing tool is a row, with the word and the button.
  assert.equal(await page.locator(".cx-row").count(), 2);
  assert.equal(await page.locator(`${kube} .cx-word[data-kind=missing]`).textContent(), "not found");
  const word = page.locator(`${kube} .cx-word[data-kind=missing]`);
  assert.match(await word.evaluate((n) => getComputedStyle(n).textDecorationLine), /underline/, "a second cue besides the colour");
  assert.equal(await word.evaluate((n) => getComputedStyle(n, "::before").content), '"! "');
  assert.equal(await page.locator(`${kube} .cx-act[data-act=path]`).textContent(), "Set path");
  assert.equal(await page.locator(`${kube} .cx-sel`).textContent(), "—");

  // The button opens the field of the block (the same component), under the row.
  await page.locator(`${kube} .cx-act[data-act=path]`).click();
  const field = page.locator(`${kube} .ig-path`);
  assert.equal(await field.getAttribute("placeholder"), "Absolute path of the program");
  assert.equal(await page.locator(`${kube} .ig-pathbtn`).allTextContents().then((t) => t.join("|")), "Set path|Check again");

  // A refused path is the core's sentence, and the field stays.
  await page.evaluate(() => {
    window.__CX__.setFault = { code: "refused", reason: "path", text: "That file is not kubectl." };
  });
  await field.fill("/tmp/x");
  await page.locator(`${kube} .ig-pathbtn`, { hasText: "Set path" }).click();
  await page.waitForFunction(() => document.querySelector(".cx-line")?.textContent === "That file is not kubectl.");
  assert.equal(await page.locator(".cx-line").getAttribute("data-tone"), "warn");
  assert.equal(await field.inputValue(), "/tmp/x", "what was typed survives");

  // An accepted path: the core is told the tool and the path and nothing else, the tools are the core's new list, the row asks again.
  await page.evaluate((r) => {
    window.__CX__.setFault = null;
    window.__CX__.rows = r;
  }, [KUBE_ROW]);
  await field.fill("/opt/bin/kubectl");
  await field.press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_binary_set").length === 2);
  assert.deepEqual((await callsOf(page, "integration_binary_set")).at(-1), { tool: "kube", path: "/opt/bin/kubectl" });
  await page.waitForFunction(() => document.querySelector('.cx-row[data-tool="kube"] .cx-sel')?.textContent === "prod-eu · payments");
  assert.equal(await page.locator(`${kube} .cx-word[data-kind=missing]`).count(), 0);
  assert.equal(await page.locator(`${kube} .ig-path`).count(), 0, "the field goes when the tool is found");

  // Escape in the field gives the keyboard back to the terminal.
  await page.evaluate((r) => {
    window.__CX__.rows = r;
    window.__MOCK__.emit("integration", { seq: 2, kind: "context", tool: "kube", pane: null, code: null });
  }, rows);
  await page.waitForSelector(`${kube} .cx-act[data-act=path]`);
  await page.locator(`${kube} .cx-act[data-act=path]`).click();
  await page.locator(`${kube} .ig-path`).press("Escape");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime");

  // A tool that cannot be used shows the core's reason, not an invented sentence.
  await page.evaluate(([tools, r]) => {
    window.__CX__.tools = tools;
    window.__CX__.rows = r;
    window.__MOCK__.emit("integration", { seq: 3, kind: "tools", tool: null, pane: null, code: null });
  }, [[{ ...MISSING, state: "unusable", reason: "kubectl cannot be used: writable by everyone." }, ...ALL_TOOLS.slice(1)], rows]);
  await page.waitForFunction(() => document.querySelector(".cx-reason")?.textContent === "kubectl cannot be used: writable by everyone.");
  assert.equal(await page.locator(`${kube} .cx-word[data-kind=missing]`).count(), 0, "no word of the page's own for it");
  assert.equal(await page.locator(`${kube} .cx-act[data-act=path]`).count(), 1);
  assert.deepEqual(errors, [], "the page reported errors");
}
