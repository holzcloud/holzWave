// GAM7 end to end on the page: the answers are the files the core wrote (core/tests/integrate_e2e_gam.rs writes them from
// a fake `gam`), and the page must draw exactly what the core produced, as inert text. The mock core answers the commands of
// the block with those files; nothing here is typed by hand except the tool's own entry, which is the core's list for GAM7.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { openBlock } from "./integrate-lib.mjs";

/** An answer file of the core, as the page receives it. */
const golden = (name) => JSON.parse(readFileSync(new URL(`../fixtures/integration/${name}`, import.meta.url), "utf8"));

const verb = (id, label, args = []) => ({ id, label, effect: "read", output: "view", refresh_secs: null, exit_is_state: false, args });

/** The entry of GAM7 in the core's tool list (integration_tools): three views, one flag, one selection field, no Local verb. */
const GAM = {
  id: "gam", label: "GAM7", state: "found", path: "/home/u/bin/gam7/gam", place: "fixed_dir", binary: null, reason: null,
  version: "7.46.02", searched: [], selects: ["section"],
  verbs: [verb("sections", "Sections"), verb("domain", "Domain"), verb("users", "Users", [{ kind: "flag", name: "as_json", required: false }])],
};

const BLOCK = (path) => ({ pane: 2, kind: "integration", source: "gam", path, source_label: "GAM7" });

/** Open the page with a GAM7 block on `path` whose reads are answered with `runs` (verb id to answer file). */
const open = (page, url, path, runs, extra = {}) =>
  openBlock(page, url, { tools: [GAM], blocks: [BLOCK(path)], runs, ...extra });

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const cases = [the_users_golden_is_drawn];
  for (const run of cases) {
    try {
      await run(page, url);
    } catch (e) {
      throw new Error(`${run.name}: ${e instanceof Error ? e.message : e}`, { cause: e });
    }
  }
  assert.deepEqual(errors, [], "the page reported errors");
}

async function the_users_golden_is_drawn(page, url) {
  const users = golden("gam_users.json");
  await open(page, url, "users", { users });
  await page.waitForSelector(".block-integration .ig-table tbody tr");

  // The header says what the pane reads and from where: the tool and the section the core resolved.
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-ctx')?.textContent === "GAM7 · work");

  // The columns are the golden's, in its order, and there is one row per user.
  const head = await page.locator(".block-integration .ig-table thead th").allTextContents();
  assert.deepEqual(head, users.view.columns.map((c) => c.label), "the columns of the core's answer");
  const rows = await page.locator(".block-integration .ig-table tbody tr").evaluateAll((trs) => trs.map((tr) => [...tr.querySelectorAll("td")].map((td) => td.textContent)));
  assert.deepEqual(rows, users.view.rows.map((r) => r.cells), "every cell is the core's, row for row");
  assert.equal(rows.length, 5);

  // The multi-line cell of the directory is one cell on one line: no line break reaches the page.
  assert.equal(rows[1][4], "Line one, with a comma line two with \"quotes\"");
  assert.ok(rows.every((r) => r.every((c) => !c.includes("\n"))), "no cell holds a line break");
  // The suspended user is dim, as the core said.
  assert.equal(await page.locator(".block-integration tr.ig-row").nth(2).getAttribute("data-tone"), "dim");

  // The core was asked for the one view of this pane and nothing else.
  const calls = await page.evaluate(() => window.__MOCK__.callsOf("integration_run"));
  assert.deepEqual(calls[0], { tool: "gam", verb: "users", pane: 2, args: {} });
}
