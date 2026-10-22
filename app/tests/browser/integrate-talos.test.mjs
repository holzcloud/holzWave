// Talos end to end, page side: the page draws exactly what the core produced. Every answer here is a golden file that the
// core's own test wrote (core/tests/integrate_e2e_talos.rs) from the real types, so a renamed member or a changed word fails
// there and here in the same change. The tool list is written out by hand from the verbs of the Talos module.
//
// ASSUMPTION: the goldens rest on fixtures of talosctl that are written from its documents, not recorded from the real
// tool; nothing here says what the real tool prints.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { LAUNCHED, openBlock, rowsShown } from "./integrate-lib.mjs";
import { allow, cardNode, installCore, textOf, wait } from "./integrate-confirm-lib.mjs";
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
  const cases = [
    the_nodes_golden_is_drawn_as_the_core_produced_it,
    services_status_and_contexts_goldens_are_drawn,
    the_role_is_said_in_words,
    a_truncated_stream_shows_the_rows_and_the_note,
    use_context_asks_first_with_the_files_name,
    node_actions_launch_or_ask,
    a_hostile_node_has_no_button_and_inert_text,
  ];
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

/** The cells of a table, row by row, as drawn (the cell of the actions is left out). */
const drawnRows = (page) =>
  page.locator(".block-integration .ig-table tbody tr").evaluateAll((trs) => trs.map((tr) => Array.from(tr.querySelectorAll("td.ig-cell:not(.ig-acts)")).map((td) => td.textContent)));

const tonesOf = (page) => page.locator(".block-integration .ig-row").evaluateAll((trs) => trs.map((tr) => tr.getAttribute("data-tone") ?? "normal"));

async function services_status_and_contexts_goldens_are_drawn(page, url) {
  // Services: the core's state and health words, and the unhealthy service is warn with the mark besides the colour.
  const services = golden("talos_services.json");
  await open(page, url, "services", { services });
  await rowsShown(page);
  assert.deepEqual(await head(page), services.view.columns.map((c) => c.label));
  assert.deepEqual(await drawnRows(page), services.view.rows.map((r) => r.cells), "every cell as the core wrote it");
  assert.deepEqual(await tonesOf(page), services.view.rows.map((r) => r.tone));
  const unhealthy = services.view.rows.findIndex((r) => r.cells[3] === "unhealthy");
  assert.ok(unhealthy >= 0 && services.view.rows[unhealthy].tone === "warn", "the golden has an unhealthy service that is warn");
  const first = page.locator(".block-integration .ig-row").nth(unhealthy).locator("td").first();
  assert.equal(await first.evaluate((n) => n.dataset.mark ?? null), "!", "the mark is the second cue");
  assert.equal(await first.evaluate((n) => getComputedStyle(n, "::before").content.includes("!")), true, "and it is drawn");
  assert.equal(await page.locator(".block-integration .ig-row[data-tone]").count(), 1, "no other row is marked");

  // Machine status: the node that is not ready is warn.
  const status = golden("talos_machine_status.json");
  await open(page, url, "machine_status", { machine_status: status });
  await rowsShown(page);
  assert.deepEqual(await head(page), status.view.columns.map((c) => c.label));
  assert.deepEqual(await drawnRows(page), status.view.rows.map((r) => r.cells));
  assert.deepEqual(await tonesOf(page), status.view.rows.map((r) => r.tone));
  assert.ok((await tonesOf(page)).includes("warn"));

  // Contexts: the current row is marked by the core's `*`, and each row can be made the default (it asks first).
  const contexts = golden("talos_contexts.json");
  await open(page, url, "contexts", { contexts });
  await rowsShown(page);
  assert.deepEqual(await head(page), contexts.view.columns.map((c) => c.label));
  assert.deepEqual(await drawnRows(page), contexts.view.rows.map((r) => r.cells));
  assert.equal((await drawnRows(page))[0][0], "*", "the current context is marked");
  assert.deepEqual(await page.locator(".ig-act").allTextContents(), ["Use context\u2026", "Use context\u2026"], "the ones that ask first end in an ellipsis");
  assert.deepEqual(await page.locator(".ig-act").evaluateAll((n) => n.map((b) => b.title)), ["Asks for confirmation", "Asks for confirmation"]);
}

/** The key-value row with this label. */
const kvRow = (page, label) => page.locator(".block-integration .ig-kvrow", { has: page.locator(".ig-k", { hasText: new RegExp(`^${label}$`) }) });

async function the_role_is_said_in_words(page, url) {
  // ASSUMPTION: the `roles` member is a guess from the documents; the words below are the core's for that guess.
  const reader = golden("talos_info.json");
  await open(page, url, "info", { info: reader });
  await page.locator(".block-integration .ig-kv").waitFor();
  const labels = await page.locator(".block-integration .ig-kvrow .ig-k").allTextContents();
  assert.deepEqual(labels, reader.view.rows.map((r) => r.label), "the core's rows in the core's order");
  assert.deepEqual(await page.locator(".block-integration .ig-kvrow .ig-v").allTextContents(), reader.view.rows.map((r) => r.value));
  const access = kvRow(page, "Access");
  assert.equal(await access.locator(".ig-v").textContent(), "read-only (os:reader)");
  assert.equal(await access.getAttribute("data-tone"), null, "a read-only role is not a warning");
  assert.equal(await access.getAttribute("data-mark"), null);
  const calm = await access.locator(".ig-v").evaluate((n) => getComputedStyle(n).color);

  // A role set with os:admin says full access, in the warn tone, with the mark and the words.
  const admin = golden("talos_info_admin.json");
  await open(page, url, "info", { info: admin });
  await page.locator(".block-integration .ig-kv").waitFor();
  const full = kvRow(page, "Access");
  assert.equal(await full.locator(".ig-v").textContent(), "full access (os:admin)");
  assert.equal(await full.getAttribute("data-tone"), "warn", "the core's tone");
  assert.equal(await full.getAttribute("data-mark"), "!", "the mark");
  assert.equal(await full.locator(".ig-v").evaluate((n) => getComputedStyle(n, "::before").content.includes("!")), true, "is drawn in front of the words");
  assert.notEqual(await full.locator(".ig-v").evaluate((n) => getComputedStyle(n).color), calm, "and the colour differs from the calm row");
  assert.equal(await page.locator(".block-integration .ig-kvrow[data-tone]").count(), 1, "only the access row is marked");
  assert.equal(await kvRow(page, "Roles").locator(".ig-v").textContent(), "os:admin");
}

async function a_truncated_stream_shows_the_rows_and_the_note(page, url) {
  // ASSUMPTION: the cut stream is a written variant of the members document.
  const cut = golden("talos_nodes_truncated.json");
  assert.ok(cut.view.note.startsWith("The output was cut off"), "the golden carries the core's note");
  await open(page, url, "nodes", { nodes: cut });
  await rowsShown(page);
  assert.deepEqual(await drawnRows(page), cut.view.rows.map((r) => r.cells), "the rows the core could read");
  const note = page.locator(".block-integration .ig-note");
  assert.equal(await note.count(), 1);
  assert.equal(await note.textContent(), cut.view.note, "the core's sentence, drawn under the table");
  assert.equal(await page.evaluate(() => {
    const table = document.querySelector(".block-integration .ig-table");
    const line = document.querySelector(".block-integration .ig-note");
    return Boolean(table.compareDocumentPosition(line) & Node.DOCUMENT_POSITION_FOLLOWING);
  }), true, "after the table, not before it");

  // An unknown shape is the text, drawn as text, with the core's note.
  const unknown = golden("talos_unknown_shape.json");
  await open(page, url, "nodes", { nodes: unknown });
  await page.locator(".block-integration pre.ig-text").waitFor();
  assert.equal(await page.locator(".block-integration pre.ig-text").textContent(), unknown.view.text);
  assert.equal(await page.locator(".block-integration .ig-table").count(), 0, "no table is made from text");
  assert.equal(await page.locator(".block-integration .ig-note").textContent(), unknown.view.note);

  // A tool that failed is the core's sentence on the status line and no table.
  const text = "Talos reported an error: rpc error: code = Unavailable desc = connection refused";
  await open(page, url, "nodes", {}, { faults: { nodes: { code: "failed", reason: "exit_status", text } } });
  await page.waitForFunction((t) => document.querySelector(".block-integration .ig-status")?.textContent === t, text);
  assert.equal(await page.locator(".block-integration .ig-table").count(), 0);
}

async function use_context_asks_first_with_the_files_name(page, url) {
  // The card is the core's own (the golden of `use_context` for the context demo-lab); the page asks for it, shows it, and
  // sends nothing until one real press.
  const card = golden("talos_card_use_context.json");
  await installCore(page, { cards: { "talos/use_context": card } });
  const nodes = golden("talos_nodes.json");
  await open(page, url, "nodes", { nodes, contexts: golden("talos_contexts.json") });
  await rowsShown(page);

  // Choose the other context of the talosconfig from the list; the chip follows the core's selection.
  await page.locator(".ig-chip").first().click();
  await page.waitForSelector(".ig-list .ig-opt");
  assert.deepEqual(await page.locator(".ig-opt .ig-opt-label").allTextContents(), ["demo-prod", "demo-lab", "Make default\u2026"], "the last entry asks first");
  await page.locator(".ig-opt", { hasText: "demo-lab" }).click();
  await page.waitForFunction(() => document.querySelector(".ig-chip")?.textContent === "context demo-lab");
  assert.deepEqual((await callsOf(page, "integration_select"))[0], { tool: "talos", pane: 2, selection: { context: "demo-lab" } });

  await page.locator(".ig-chip").first().click();
  await page.waitForSelector(".ig-list .ig-opt-default");
  assert.equal((await callsOf(page, "integration_prepare")).length, 0, "opening the list asks for no card");
  await page.locator(".ig-opt-default").click();
  await cardNode(page).waitFor();
  assert.deepEqual(await callsOf(page, "integration_prepare"), [{ tool: "talos", verb: "use_context", pane: 2, args: { context: "demo-lab" } }]);
  assert.equal(await textOf(page, ".ic-title"), "Talos \u00b7 Use context");
  const rows = (selector) => cardNode(page).locator(`${selector} .ic-row`).evaluateAll((r) => r.map((x) => [x.children[0].textContent, x.children[1].textContent]));
  assert.deepEqual(await rows(".ic-target"), [["context", "demo-lab"]], "the target; the sentence about the file is in the lead");
  assert.deepEqual(await rows(".ic-args"), [["program", "talosctl"], ["arg 1", "config"], ["arg 2", "context"], ["arg 3", "--"], ["arg 4", "demo-lab"]]);
  assert.match(await textOf(page, ".ic-effect"), /the talosconfig of this computer/, "the card says which file it writes");
  assert.equal(await cardNode(page).locator(".ic-guard").count(), 0, "no production guard");
  assert.equal(await textOf(page, ".ic-hash"), `hash ${card.hash.slice(0, 12)}`);
  assert.equal((await callsOf(page, "integration_confirm")).length, 0, "nothing is sent before the press");

  // Deny cancels the ticket and confirms nothing; a second card, allowed after the half second, sends the ticket and the hash.
  await wait(page, 600);
  await cardNode(page).locator(".ic-no").click();
  await page.waitForFunction(() => document.querySelector("section.ic") === null);
  assert.equal((await callsOf(page, "integration_confirm")).length, 0);
  assert.deepEqual(await callsOf(page, "integration_cancel"), [{ ticket: card.ticket }]);
  await page.locator(".ig-chip").first().click();
  await page.waitForSelector(".ig-list .ig-opt-default");
  await page.locator(".ig-opt-default").click();
  await cardNode(page).waitFor();
  await allow(page);
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_confirm").length === 1);
  assert.deepEqual(await callsOf(page, "integration_confirm"), [{ ticket: card.ticket, hash: card.hash }]);
}

async function node_actions_launch_or_ask(page, url) {
  const card = golden("talos_card_dashboard.json");
  const nodes = golden("talos_nodes.json");
  await installCore(page, { cards: { "talos/dashboard": card } });
  await open(page, url, "nodes", { nodes }, { launch: { pane: 3, view: LAUNCHED } });
  await rowsShown(page);

  // The core offers four actions on a valid node; the page draws a read action only for a verb that opens a terminal, so the two
  // views (Services, Machine status) have no button here and the other two do, the one that asks first ending in an ellipsis.
  assert.deepEqual(nodes.view.rows[0].actions.map((a) => a.verb), ["services", "machine_status", "dmesg", "dashboard"]);
  for (const i of [0, 1, 2]) {
    assert.deepEqual(await page.locator(`tr[data-i="${i}"] .ig-act`).allTextContents(), ["Kernel log", "Dashboard\u2026"], `row ${i}`);
  }

  // A read terminal launches beside the pane with the row's own argument and nothing else.
  await page.locator('tr[data-i="1"] .ig-act', { hasText: "Kernel log" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_launch").length === 1);
  assert.deepEqual((await callsOf(page, "integration_launch"))[0], {
    tool: "talos", verb: "dmesg", pane: 2, args: { node: "192.0.2.12" }, placement: "right",
  });
  await page.waitForSelector('.panehead[data-pane="3"]');
  assert.equal((await callsOf(page, "integration_prepare")).length, 0, "a read asks for no card");

  // An Exec verb asks first: the core's card is shown with the exact fields, and nothing is launched or confirmed by the press.
  await page.locator('tr[data-i="0"] .ig-act', { hasText: "Dashboard" }).click();
  await cardNode(page).waitFor();
  assert.deepEqual(await callsOf(page, "integration_prepare"), [{ tool: "talos", verb: "dashboard", pane: 2, args: { node: "192.0.2.11" } }]);
  assert.equal(await textOf(page, ".ic-title"), "Talos \u00b7 Dashboard");
  assert.match(await textOf(page, ".ic-effect"), /^Opens a session with your credentials\./);
  const rows = (selector) => cardNode(page).locator(`${selector} .ic-row`).evaluateAll((r) => r.map((x) => [x.children[0].textContent, x.children[1].textContent]));
  assert.deepEqual(await rows(".ic-target"), card.target);
  assert.deepEqual(await rows(".ic-args"), [["program", "talosctl"], ...card.argv.map((a, i) => [`arg ${i + 1}`, a])], "the exact argument list of the core");
  assert.equal(await cardNode(page).locator(".ic-guard").count(), 0);
  assert.equal((await callsOf(page, "integration_launch")).length, 1, "the press launched nothing");
  assert.equal((await callsOf(page, "integration_confirm")).length, 0, "and confirmed nothing");
  await wait(page, 600);
  await cardNode(page).locator(".ic-no").click();
  await page.waitForFunction(() => document.querySelector("section.ic") === null);
  assert.equal((await callsOf(page, "integration_confirm")).length, 0, "Deny starts nothing");
  assert.deepEqual(await callsOf(page, "integration_cancel"), [{ ticket: card.ticket }]);
}

async function a_hostile_node_has_no_button_and_inert_text(page, url) {
  // ASSUMPTION: the members stream is a written variant with hostile names; the core made the rows.
  const hostile = golden("talos_nodes_hostile.json");
  await open(page, url, "nodes", { nodes: hostile });
  await rowsShown(page);
  assert.deepEqual(await drawnRows(page), hostile.view.rows.map((r) => r.cells), "every cell as the core wrote it, hostile text included");
  assert.equal(await page.locator(".block-integration img").count(), 0, "markup in a host name is text");
  assert.equal(await page.locator(".block-integration [onerror]").count(), 0);
  assert.equal(await page.locator('tr[data-i="1"] .ig-act').count(), 0, "a hostile node: no button");
  assert.equal(await page.locator('tr[data-i="3"] .ig-act').count(), 0, "a mask and an option: no button");
  // A valid address with a hostile host name: the buttons carry the address, never the name.
  assert.deepEqual(await page.locator('tr[data-i="2"] .ig-act').allTextContents(), ["Kernel log", "Dashboard\u2026"]);
  await page.evaluate(() => {
    window.__IG__.launch = { pane: 3, view: window.__MOCK__.view() };
  });
  await page.locator('tr[data-i="2"] .ig-act', { hasText: "Kernel log" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_launch").length === 1);
  const sent = (await callsOf(page, "integration_launch"))[0];
  assert.deepEqual(sent.args, { node: "192.0.2.13" });
  assert.ok(!JSON.stringify(sent).includes("touch"), "the host name is not sent");
}
