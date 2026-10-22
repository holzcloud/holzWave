// Talos end to end, page side: the page draws exactly what the core produced. Every answer here is a golden file that the
// core's own test wrote (core/tests/integrate_e2e_talos.rs) from the real types, so a renamed member or a changed word fails
// there and here in the same change. The tool list is written out by hand from the verbs of the Talos module.
//
// ASSUMPTION: the goldens rest on fixtures of talosctl that are written from its documents, not recorded from the real
// tool; nothing here says what the real tool prints.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { openBlock, rowsShown } from "./integrate-lib.mjs";
import { callsOf } from "./lib.mjs";

/** A golden file of the core, parsed. */
const golden = (name) => JSON.parse(readFileSync(new URL(`../fixtures/integration/${name}`, import.meta.url), "utf8"));

const verb = (id, label, effect, output, args = []) => ({ id, label, effect, output, refresh_secs: null, exit_is_state: false, args });
const NODE_ARG = [{ name: "node", kind: "node", required: false }];

/** Talos as the core lists it: the eight verbs of the module, in its order. */
const TALOS = {
  id: "talos", label: "Talos", state: "found", path: "/usr/local/bin/talosctl", place: "path", binary: null, reason: null,
  version: "v1.12.0", searched: [], selects: ["context", "node"],
  verbs: [
    verb("contexts", "Contexts", "read", "view"),
    verb("nodes", "Nodes", "read", "view"),
    verb("services", "Services", "read", "view", NODE_ARG),
    verb("info", "Info", "read", "view"),
    verb("machine_status", "Machine status", "read", "view", NODE_ARG),
    verb("use_context", "Use context", "local", "done", [{ name: "context", kind: "context", required: true }]),
    verb("dmesg", "Kernel log", "read", "terminal", NODE_ARG),
    verb("dashboard", "Dashboard", "exec", "terminal", NODE_ARG),
  ],
};

/** The block of pane 2 for a view of Talos. */
const block = (path) => ({ pane: 2, kind: "integration", source: "talos", path, source_label: "Talos" });

/** The page with Talos listed and a block of `path` whose reads are answered with `runs`. */
const open = (page, url, path, runs, extra = {}) => openBlock(page, url, { tools: [TALOS], blocks: [block(path)], runs, ...extra });

const head = (page) => page.locator(".block-integration .ig-table thead th:not(.ig-th-act)").allTextContents();
const column = (page, n) => page.locator(`.block-integration .ig-table tbody tr td:nth-child(${n})`).allTextContents();

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const cases = [the_nodes_golden_is_drawn_as_the_core_produced_it];
  for (const run of cases) {
    try {
      await run(page, url);
    } catch (e) {
      throw new Error(`${run.name}: ${e instanceof Error ? e.message : e}`, { cause: e });
    }
  }
  assert.deepEqual(errors, [], "the page reported errors");
}

async function the_nodes_golden_is_drawn_as_the_core_produced_it(page, url) {
  const nodes = golden("talos_nodes.json");
  await open(page, url, "nodes", { nodes });
  await rowsShown(page);

  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="2"] .ph-ctx')?.textContent === "Talos · talos-prod");
  assert.deepEqual(await head(page), ["NODE", "HOSTNAME", "TYPE", "ADDRESSES"], "the core's column labels");
  assert.equal(await page.locator(".block-integration .ig-table tbody tr").count(), nodes.view.rows.length, "one row for each row of the golden");
  for (const [i, name] of nodes.view.columns.entries()) {
    assert.deepEqual(await column(page, i + 1), nodes.view.rows.map((r) => r.cells[i]), `column ${name.label} holds the core's cells`);
  }
  assert.deepEqual(await page.locator(".ig-chip").allTextContents(), ["context talos-prod", "node —"], "the chips of Talos");
  // The page asked with the facts of the pane and nothing else.
  assert.deepEqual((await callsOf(page, "integration_run"))[0], { tool: "talos", verb: "nodes", pane: 2, args: {} });
}
