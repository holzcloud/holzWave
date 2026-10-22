// Kubernetes end to end, the page's half (design.md 6.25 and 6.26): the page answers the core's commands with the golden files
// that core/tests/integrate_e2e_kube.rs writes from the real types and shows that it draws exactly what the core produced. A
// change of the contract on either side fails one of the two tests.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { KUBE, openBlock, rowsShown } from "./integrate-lib.mjs";

/** A golden of the core, as the page receives it. */
const golden = (name) => JSON.parse(readFileSync(new URL(`../fixtures/integration/${name}`, import.meta.url), "utf8"));

const ctx = (page) => page.locator('.panehead[data-pane="2"] .ph-ctx');

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await the_pods_golden_is_drawn_as_the_core_produced_it(page, url);
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
