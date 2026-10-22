// The palette entries of the integrations: one entry for each view of each tool the core listed, one for each terminal verb that
// needs no argument, and two rebindable actions for the Context section and the profiles dialog. The entries are built from the
// core's own tool list; the page names no tool. A mock core answers `integration_tools` and records `block_open`,
// `integration_launch` and `integration_prepare`.
import assert from "node:assert/strict";
import { ALL_TOOLS, KUBE, LAYOUT, openContext } from "./integrate-context-lib.mjs";
import { callsOf } from "./lib.mjs";

const verb = (id, label, effect, output, args = []) => ({ id, label, effect, output, refresh_secs: null, exit_is_state: false, args });

/** The tool list with one tool that has the view verb `pods` and nothing else. */
const PODS_ONLY = [{ ...KUBE, verbs: [verb("pods", "Pods", "read", "view")] }];

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await the_palette_opens_a_kubernetes_pods_block(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** Open the page against the mock core and wait until it has drawn and the tool list is registered. */
async function open(page, url, config = {}) {
  await openContext(page, url, { sidebar: false, rows: [], ...config });
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(async () => {
    window.__registry = await import(new URL("actions.js", location.href).href);
  });
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_tools").length >= 1);
  await page.evaluate(() => document.getElementById("term").focus());
  await page.waitForFunction(() => document.activeElement?.id === "ime");
}

/** The ids of the registered integration actions, in registration order. */
const ids = (page) => page.evaluate(() => window.__registry.listActions().map((a) => a.id).filter((id) => id.startsWith("integration.")));

/** The title of a registered action by id, or null. */
const titleOf = (page, id) => page.evaluate((i) => window.__registry.listActions().find((a) => a.id === i)?.title ?? null, id);

/** Open the palette, type `query` and press Enter: the best match runs. */
async function runFromPalette(page, query) {
  await page.keyboard.press("Meta+k");
  await page.waitForFunction(() => document.activeElement?.classList.contains("palette-input") === true);
  await page.keyboard.type(query);
  await page.keyboard.press("Enter");
}

async function the_palette_opens_a_kubernetes_pods_block(page, url) {
  await open(page, url, { tools: PODS_ONLY });
  await page.waitForFunction(() => window.__registry.listActions().some((a) => a.id === "integration.open.kube.pods"));
  assert.deepEqual(await ids(page), ["integration.open.kube.pods"]);
  assert.equal(await titleOf(page, "integration.open.kube.pods"), "Kubernetes: Pods");

  // The palette lists it with its title.
  await page.keyboard.press("Meta+k");
  await page.waitForFunction(() => document.activeElement?.classList.contains("palette-input") === true);
  await page.keyboard.type("Kubernetes: Pods");
  const first = await page.locator(".palette-row .palette-title").first().textContent();
  assert.equal(first, "Kubernetes: Pods");
  await page.keyboard.press("Enter");

  await page.waitForFunction(() => window.__MOCK__.callsOf("block_open").length === 1);
  const calls = await callsOf(page, "block_open");
  assert.deepEqual(calls[0], { kind: "integration", source: "kube", path: "pods", placement: "tab" });
  void runFromPalette;
  void ALL_TOOLS;
  void LAYOUT;
}
