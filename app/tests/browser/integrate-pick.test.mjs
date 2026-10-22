// The chips, the pickers, the production word, the actions of a row and the keys of the integration block (design.md 6.25).
import assert from "node:assert/strict";
import { FOCUSED, HOSTILE, KUBE, LAUNCHED, answer, listView, logs, openBlock, podsView, rowsShown } from "./integrate-lib.mjs";

const ctx = (page) => page.locator('.panehead[data-pane="2"] .ph-ctx');

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const view = podsView();
  view.rows[0].actions = [logs("api-0"), { verb: "delete_pod", label: "Delete pod", effect: "write", args: { pod: "api-0" } }, { verb: "use_context", label: "Use", effect: "local", args: { context: "dev" } }];
  await openBlock(
    page,
    url,
    {
      selectProduction: null,
      runs: {
        pods: answer(view),
        contexts: answer(listView("Contexts", ["prod", "dev", HOSTILE])),
        namespaces: answer(listView("Namespaces", ["payments", "default"])),
      },
      launch: { pane: 3, view: LAUNCHED },
    },
    FOCUSED,
  );
  await rowsShown(page);

  // One chip for each selection of the tool, with the current value, and a popup that is a list.
  assert.deepEqual(await page.locator(".ig-chip").allTextContents(), ["context prod", "namespace payments"]);
  assert.equal(await page.locator(".ig-chip").first().getAttribute("aria-haspopup"), "listbox");
  assert.equal(await page.locator(".ig-chip").first().getAttribute("aria-expanded"), "false");

  // Production is a word with the mark and an underline, from the core's flag; the header carries it too.
  assert.equal(await page.locator(".ig-prod").textContent(), "production");
  assert.equal(await page.locator(".ig-prod").isVisible(), true);
  assert.equal(await page.locator(".ig-prod").evaluate((n) => getComputedStyle(n).textDecorationLine), "underline");
  assert.equal(await page.locator(".ig-prod").evaluate((n) => getComputedStyle(n, "::before").content), '"! "');
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-ctx')?.getAttribute("data-tone") === "warn");
  assert.equal(await ctx(page).evaluate((n) => getComputedStyle(n).textDecorationLine), "underline");
  assert.equal(await ctx(page).textContent(), "Kubernetes · prod · payments · 10 s");

  // A press asks the core for the list of this pane and shows it; the entries are text and the current one is marked.
  await page.locator(".ig-chip").first().click();
  await page.waitForSelector(".ig-list .ig-opt");
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("integration_run").at(-1)), { tool: "kube", verb: "contexts", pane: 2, args: {} });
  assert.equal(await page.locator(".ig-list").getAttribute("role"), "listbox");
  assert.deepEqual(await page.locator(".ig-opt .ig-opt-label").allTextContents(), ["prod", "dev", HOSTILE]);
  assert.equal(await page.locator(".ig-list img").count(), 0);
  assert.deepEqual(await page.locator(".ig-opt").evaluateAll((n) => n.map((o) => o.getAttribute("aria-selected"))), ["true", "false", "false"]);
  assert.equal(await page.locator(".ig-chip").first().getAttribute("aria-expanded"), "true");

  // Escape closes it and gives the keyboard back to the terminal.
  await page.locator(".ig-opt").nth(1).focus();
  await page.keyboard.press("Escape");
  assert.equal(await page.locator(".ig-list").count(), 0);
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime");

  // Choosing sets this pane's selection (the namespace belongs to the old cluster) and reads again.
  await page.locator(".ig-chip").first().click();
  await page.waitForSelector(".ig-list .ig-opt");
  const runs = await page.evaluate(() => window.__MOCK__.callsOf("integration_run").length);
  await page.locator(".ig-opt").nth(1).click();
  await page.waitForFunction((n) => window.__MOCK__.callsOf("integration_run").length > n, runs);
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("integration_select")), [{ tool: "kube", pane: 2, selection: { context: "dev" } }]);
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("integration_run").at(-1)), { tool: "kube", verb: "pods", pane: 2, args: {} });
  assert.equal(await page.locator(".ig-list").count(), 0, "the list closes");
  await page.waitForFunction(() => document.querySelector(".ig-chip")?.textContent === "context dev");
  assert.equal(await page.evaluate(() => document.activeElement?.classList.contains("ig-chip")), true, "the keyboard is back on the chip");
  assert.deepEqual(await page.locator(".ig-chip").allTextContents(), ["context dev", "namespace —"]);
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-ctx')?.textContent === "Kubernetes · dev · 10 s");

  // The core says the new selection is no production one: neither the word nor the header mark remain.
  assert.equal(await page.locator(".ig-prod").isVisible(), false);
  assert.equal(await ctx(page).getAttribute("data-tone"), null);

  // A list that cannot be read shows the core's sentence in the list.
  await page.evaluate(() => {
    window.__IG__.faults = { namespaces: { code: "failed", reason: "exit_status", text: "Kubernetes reported an error: forbidden" } };
  });
  await page.locator(".ig-chip").nth(1).click();
  await page.waitForSelector(".ig-list-msg");
  assert.equal(await page.locator(".ig-list-msg").textContent(), "Kubernetes reported an error: forbidden");
  assert.equal(await page.locator(".ig-list-msg").getAttribute("data-tone"), "warn");
  await page.locator(".ig-chip").nth(1).click();
  assert.equal(await page.locator(".ig-list").count(), 0, "a second press closes it");
  await page.evaluate(() => {
    window.__IG__.faults = {};
  });

  // The core refuses a selection: its sentence is on the status line and nothing else changes.
  await page.evaluate(() => {
    window.__IG__.selectFault = { code: "refused", reason: "selection", text: "The selection was refused: the context is not known." };
  });
  await page.locator(".ig-chip").first().click();
  await page.waitForSelector(".ig-list .ig-opt");
  await page.locator(".ig-opt").first().click();
  await page.waitForFunction(() => document.querySelector(".ig-status")?.textContent === "The selection was refused: the context is not known.");
  await page.evaluate(() => {
    window.__IG__.selectFault = null;
  });
  await page.locator(".ig-refresh").click();
  await page.waitForFunction(() => /^Updated/.test(document.querySelector(".ig-status")?.textContent ?? ""));

  // A row shows the buttons of its actions that only read, and a press launches beside the pane with the row's own arguments.
  assert.deepEqual(await page.locator(".ig-act").allTextContents(), ["Logs"], "an action that changes something is not drawn here");
  assert.equal(await page.locator("tr[data-i='1'] .ig-act").count(), 0);
  await page.locator(".ig-act").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_launch").length === 1);
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.callsOf("integration_launch")[0]), {
    tool: "kube", verb: "logs", pane: 2, args: { pod: "api-0" }, placement: "right",
  });
  await page.waitForSelector('.panehead[data-pane="3"]');

  // A launch the core refuses is its sentence on the status line.
  await page.evaluate(() => {
    window.__IG__.launchFault = { code: "refused", reason: "needs_card", text: "This action changes something or opens a session, so it needs an approval first." };
  });
  await page.locator(".ig-act").click();
  await page.waitForFunction(() => document.querySelector(".ig-status")?.textContent?.startsWith("This action changes something"));

  // The keyboard is on the integration pane again (the launch moved the focus to the new terminal).
  await page.evaluate((layout) => {
    window.__MOCK__.emit("layout", { ...layout, focus: 2 });
    document.getElementById("ime").focus();
  }, LAUNCHED);
  await page.waitForFunction(() => document.querySelector(".block-integration.focused") !== null);

  // Keys: Down, Up, End and Home move the selected row; r reads again; the selected row survives a read.
  const marks = () => page.locator(".ig-row").evaluateAll((n) => n.map((r) => r.getAttribute("aria-selected")));
  assert.deepEqual(await marks(), ["true", "false"]);
  await page.keyboard.press("ArrowDown");
  assert.deepEqual(await marks(), ["false", "true"]);
  await page.keyboard.press("ArrowUp");
  assert.deepEqual(await marks(), ["true", "false"]);
  await page.keyboard.press("End");
  assert.deepEqual(await marks(), ["false", "true"]);
  const before = await page.evaluate(() => window.__MOCK__.callsOf("integration_run").length);
  await page.keyboard.press("r");
  await page.waitForFunction((n) => window.__MOCK__.callsOf("integration_run").length > n, before);
  await page.waitForFunction(() => /^Updated/.test(document.querySelector(".ig-status")?.textContent ?? ""));
  assert.deepEqual(await marks(), ["false", "true"], "the row with the same key stays selected");
  await page.keyboard.press("Home");
  assert.deepEqual(await marks(), ["true", "false"]);

  assert.deepEqual(errors, [], "the page reported errors");
}
