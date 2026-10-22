// Kubernetes end to end, the page's half (design.md 6.25 and 6.26): the page answers the core's commands with the golden files
// that core/tests/integrate_e2e_kube.rs writes from the real types and shows that it draws exactly what the core produced. A
// change of the contract on either side fails one of the two tests.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { KUBE, openBlock, rowsShown } from "./integrate-lib.mjs";
import { cardNode, openConfirm, textOf } from "./integrate-confirm-lib.mjs";
import { ALL_TOOLS, layout, openContext, sectionShown } from "./integrate-context-lib.mjs";

/** A golden of the core, as the page receives it. */
const golden = (name) => JSON.parse(readFileSync(new URL(`../fixtures/integration/${name}`, import.meta.url), "utf8"));

const ctx = (page) => page.locator('.panehead[data-pane="2"] .ph-ctx');

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await the_pods_golden_is_drawn_as_the_core_produced_it(page, url);
  await the_chips_switch_the_selection_for_this_pane(page, url);
  await make_default_asks_first_with_the_files_name(page, url);
  await the_context_section_sets_the_pick(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

async function the_pods_golden_is_drawn_as_the_core_produced_it(page, url) {
  const pods = golden("kube_pods.json");
  await openBlock(page, url, { tools: [KUBE], runs: { pods } });
  await rowsShown(page);

  // The header of the pane names the tool, the selection the core resolved, the interval, and the core's production word.
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-ctx')?.textContent === "Kubernetes · prod-eu · payments · 10 s");
  assert.equal(await page.locator(".ig-prod").textContent(), "production");
  assert.equal(await ctx(page).getAttribute("data-tone"), "warn");
  assert.deepEqual(await page.locator(".ig-chip").allTextContents(), ["context prod-eu", "namespace payments"]);

  // The table is the core's: its columns, one row for each pod, the cells as the core wrote them.
  const head = await page.locator(".block-integration .ig-table thead th:not(.ig-th-act)").allTextContents();
  assert.deepEqual(head, pods.view.columns.map((c) => c.label), "the columns of the core's view");
  const rows = await page.locator(".block-integration .ig-table tbody tr").evaluateAll((trs) => trs.map((tr) => Array.from(tr.querySelectorAll("td:not(.ig-act-cell)")).map((td) => td.textContent)));
  assert.equal(rows.length, pods.view.rows.length, "one row for each pod");
  assert.deepEqual(rows.map((r) => r[0]), pods.view.rows.map((r) => r.cells[0]), "the names of the pods");

  // The status words are the core's words, and the rows the core marked warn or bad carry a mark besides the colour.
  const statusAt = head.indexOf("STATUS");
  assert.ok(statusAt >= 0, "a STATUS column");
  const words = rows.map((r) => r[statusAt]);
  assert.deepEqual(words, pods.view.rows.map((r) => r.cells[statusAt]));
  assert.deepEqual(words, ["Running", "ContainerCreating", "CrashLoopBackOff"], "the recorded pods: one running, one starting, one in back-off");
  const tones = await page.locator(".block-integration .ig-row").evaluateAll((trs) => trs.map((tr) => tr.getAttribute("data-tone")));
  assert.deepEqual(tones.map((t) => t ?? "normal"), pods.view.rows.map((r) => r.tone));
  // A row the core called warn or danger has the mark `!` in front of its name, as well as the colour; a danger row also underlines it.
  for (const [i, row] of pods.view.rows.entries()) {
    const name = page.locator(".block-integration .ig-row").nth(i).locator("td").first();
    const cue = await name.evaluate((n) => ({ mark: n.dataset.mark ?? null, shown: getComputedStyle(n, "::before").content, line: getComputedStyle(n).textDecorationLine }));
    assert.equal(cue.mark, row.tone === "normal" ? null : "!", `row ${i} (${row.cells[0]}): the mark follows the core's tone ${row.tone}`);
    assert.equal(cue.shown.includes("!"), row.tone !== "normal", `row ${i}: the mark is drawn`);
    assert.equal(cue.line.includes("underline"), row.tone === "danger", `row ${i}: only a danger row is underlined`);
  }

  // A valid row has the three actions the core offered, in the core's order; the ones that ask first end in an ellipsis.
  const first = pods.view.rows[0];
  assert.deepEqual(first.actions.map((a) => a.verb), ["exec", "logs", "delete_pod"]);
  assert.deepEqual(await page.locator('tr[data-i="0"] .ig-act').allTextContents(), ["Exec…", "Logs", "Delete pod…"]);

  // The core was asked with the facts of the pane and nothing else.
  const asked = await page.evaluate(() => window.__MOCK__.callsOf("integration_run")[0]);
  assert.deepEqual(asked, { tool: "kube", verb: "pods", pane: 2, args: {} });
}

const calls = (page, cmd) => page.evaluate((c) => window.__MOCK__.callsOf(c), cmd);

/** The goldens a Kubernetes pane reads: the pods, the contexts and the namespaces, as the core wrote them. */
const runs = () => ({ pods: golden("kube_pods.json"), contexts: golden("kube_contexts.json"), namespaces: golden("kube_namespaces.json") });
const names = (view) => view.rows.map((r) => r.key);

async function the_chips_switch_the_selection_for_this_pane(page, url) {
  const all = runs();
  await openBlock(page, url, { tools: [KUBE], runs: all, selectProduction: null });
  await rowsShown(page);

  // The context chip lists the contexts of the core's golden, in its order, and ends with the page's own entry.
  await page.locator('.ig-chip[data-select="context"]').click();
  await page.waitForSelector(".ig-list .ig-opt");
  assert.deepEqual(await page.locator(".ig-opt .ig-opt-label").allTextContents(), [...names(all.contexts.view), "Make default…"]);
  assert.ok(names(all.contexts.view).includes("kind-test"), "the recorded kubeconfig has the context the test switches to");
  assert.deepEqual((await calls(page, "integration_run")).at(-1), { tool: "kube", verb: "contexts", pane: 2, args: {} });

  // Choosing one sends the selection of this pane, and nothing else, and the page reads the pods again.
  const before = (await calls(page, "integration_run")).length;
  await page.locator(".ig-opt", { hasText: "kind-test" }).click();
  await page.waitForFunction((n) => window.__MOCK__.callsOf("integration_run").length > n, before);
  assert.deepEqual(await calls(page, "integration_select"), [{ tool: "kube", pane: 2, selection: { context: "kind-test" } }]);
  assert.deepEqual((await calls(page, "integration_run")).at(-1), { tool: "kube", verb: "pods", pane: 2, args: {} });
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-ctx')?.textContent === "Kubernetes · kind-test · 10 s");
  assert.equal(await page.locator(".ig-prod").isVisible(), false, "the core said the new selection is not production");

  // The namespace chip does the same with the namespaces of the core's golden.
  await page.locator('.ig-chip[data-select="namespace"]').click();
  await page.waitForSelector(".ig-list .ig-opt");
  assert.deepEqual(await page.locator(".ig-opt .ig-opt-label").allTextContents(), names(all.namespaces.view), "no Make default for a namespace: the tool lists no verb for it");
  assert.deepEqual((await calls(page, "integration_run")).at(-1), { tool: "kube", verb: "namespaces", pane: 2, args: {} });
  const target = names(all.namespaces.view).at(-1);
  await page.locator(".ig-opt", { hasText: target }).last().click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_select").length === 2);
  assert.deepEqual((await calls(page, "integration_select")).at(-1), { tool: "kube", pane: 2, selection: { context: "kind-test", namespace: target } });
}

async function make_default_asks_first_with_the_files_name(page, url) {
  const all = runs();
  const card = golden("kube_card_use_context.json");
  await openConfirm(page, url, { cards: { "kube/use_context": card } }, { tools: [KUBE], runs: all, selectProduction: null });
  await rowsShown(page);
  await page.locator('.ig-chip[data-select="context"]').click();
  await page.waitForSelector(".ig-list .ig-opt");
  await page.locator(".ig-opt", { hasText: "kind-test" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_select").length === 1);

  // The last entry asks the core for the card of the verb that writes the tool's file, with the context the pane uses now.
  await page.locator('.ig-chip[data-select="context"]').click();
  await page.waitForSelector(".ig-list .ig-opt");
  await page.locator(".ig-list .ig-opt-default").click();
  await cardNode(page).waitFor();
  assert.deepEqual(await calls(page, "integration_prepare"), [{ tool: "kube", verb: "use_context", pane: 2, args: { context: "kind-test" } }]);

  // The card shows the core's own words: the file it writes, the exact list, no production guard.
  const changes = card.target.find(([label]) => label === "changes")[1];
  assert.equal(changes, "the kubeconfig files named by KUBECONFIG, by default ~/.kube/config");
  assert.equal(await textOf(page, ".ic-title"), "Kubernetes · Use context");
  assert.equal(await textOf(page, ".ic-effect"), `Changes the tool's own configuration. It writes: ${changes}`);
  const argv = await cardNode(page).locator(".ic-args .ic-row").evaluateAll((rows) => rows.map((r) => r.children[1].textContent));
  assert.deepEqual(argv, [card.program, ...card.argv], "every argument of the golden, in its order");
  assert.equal(await cardNode(page).locator(".ic-guard").count(), 0, "kind-test is no production context");
  assert.equal((await calls(page, "integration_confirm")).length, 0, "nothing was confirmed: the card only asks");
  assert.equal((await calls(page, "integration_select")).length, 1, "choosing the entry selects nothing");
  await cardNode(page).locator(".ic-no").click({ force: true });
  await page.waitForFunction(() => document.querySelector("section.ic") === null);
  assert.deepEqual(await calls(page, "integration_cancel"), [{ ticket: card.ticket }]);
}

async function the_context_section_sets_the_pick(page, url) {
  const all = runs();
  await openContext(page, url, { tools: ALL_TOOLS, runs: all }, layout(1, [1, 2]));
  await sectionShown(page);
  const kube = '.cx-row[data-tool="kube"]';

  // A plain shell has the focus: the choice is the pick of the tool for new blocks, and the line says so.
  await page.locator(`${kube} .cx-value[data-select="context"]`).click();
  await page.locator(`${kube} .ig-list .ig-opt`).first().waitFor();
  assert.deepEqual((await calls(page, "integration_run")).at(-1), { tool: "kube", verb: "contexts", pane: null, args: {} });
  await page.locator(`${kube} .ig-list .ig-opt`, { hasText: "kind-test" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_select").length === 1);
  assert.deepEqual(await calls(page, "integration_select"), [{ tool: "kube", pane: null, selection: { context: "kind-test" } }]);
  assert.equal(await page.locator(".cx-line").textContent(), "New blocks start with kind-test.");
}
