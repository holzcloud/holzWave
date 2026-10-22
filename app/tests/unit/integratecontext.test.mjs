// What the page says about the context of the tools (src/integratestate.ts, built into dist/): the reader of the core's context
// answer, the words of a row, the text of a selection and, further down, the reach of a choice, the status segments and the
// pane chip. No DOM is needed. The fixtures are the ones the browser tests use, so the two kinds of test cannot drift apart.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { glyphOf, reachPane, reachWords, readContext, rowWords, selectionOf, selectionText, valueOf } from "../../dist/integratestate.js";

const golden = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));

const row = (tool, over = {}) => ({
  tool, state: "found", context: null, namespace: null, node: null, production: null, section: null, stale: false, subscription: null, ...over,
});

test("the reader keeps the rows and panes of the golden answer", () => {
  const read = readContext(golden.answers.integration.context);
  assert.deepEqual(read.rows.map((r) => [r.tool, r.state, r.context, r.namespace, r.stale]), [
    ["kube", "found", "dev", "default", false],
    ["azure", "signed_out", null, null, true],
  ]);
  assert.deepEqual(read.panes.map((p) => [p.pane, p.tool, p.view, p.context, p.namespace, p.production, p.held, p.status]), [
    [3, "kube", "pods", "dev", "default", "Production", true, 1],
  ]);
});

test("the reader drops what is not the contract", () => {
  assert.deepEqual(readContext(null), { rows: [], panes: [] });
  assert.deepEqual(readContext("rows"), { rows: [], panes: [] });
  assert.deepEqual(readContext({ rows: "x", panes: 4 }), { rows: [], panes: [] });
  const read = readContext({
    rows: [
      row("kube", { context: "prod-eu" }),
      row("rm", { context: "x" }),
      { tool: "talos", state: "weird", context: 4, node: ["a"] },
      "kube",
      null,
      row("gam", { section: "work", stale: "yes" }),
    ],
    panes: [
      { pane: 3, tool: "kube", view: "shell", context: "prod-eu", namespace: null, production: null, held: false, status: null },
      { pane: "3", tool: "kube", view: "shell" },
      { pane: 4, tool: "ssh", view: "shell" },
      { pane: 5, tool: "kube", view: 7 },
      { pane: -1, tool: "kube", view: "shell" },
      { pane: 1.5, tool: "kube", view: "shell" },
    ],
  });
  assert.deepEqual(read.rows.map((r) => r.tool), ["kube", "talos", "gam"], "an unknown tool and a non-object are gone");
  const talos = read.rows[1];
  assert.equal(talos.state, "unknown", "an unknown state word is not trusted");
  assert.equal(talos.context, null, "a member that is no text is empty");
  assert.equal(talos.node, null);
  assert.equal(read.rows[2].stale, false, "a flag is true or it is false");
  assert.deepEqual(read.panes.map((p) => p.pane), [3], "a pane needs an integer id, a known tool and a view");
});

test("a value is cut where the contract stops and a production name may be a flag", () => {
  const long = "x".repeat(400);
  const read = readContext({ rows: [row("kube", { context: long, production: true }), row("azure", { subscription: "s", production: "" })], panes: [] });
  assert.ok(read.rows[0].context.length <= 253, "an argument value is at most 253 characters");
  assert.equal(read.rows[0].production, "", "a flag without a name still means production");
  assert.equal(read.rows[1].production, null, "an empty name is no production");
});

test("the selection is text, per tool, and a dash when there is none", () => {
  assert.equal(selectionText(row("kube", { context: "prod-eu", namespace: "payments" })), "prod-eu · payments");
  assert.equal(selectionText(row("kube", { context: "prod-eu" })), "prod-eu");
  assert.equal(selectionText(row("talos", { context: "talos-prod", node: "node-1" })), "talos-prod · node-1");
  assert.equal(selectionText(row("azure", { subscription: "holz-prod" })), "holz-prod");
  assert.equal(selectionText(row("gam", { section: "work" })), "work");
  assert.equal(selectionText(row("kube")), "—");
  // A member the tool does not select is not shown, whatever the core sent.
  assert.equal(selectionText(row("azure", { subscription: "holz-prod", namespace: "x", context: "y" })), "holz-prod");
});

test("a name that looks like markup stays those characters and a long one is cut", () => {
  const hostile = "<img src=x onerror=alert(1)>";
  assert.equal(selectionText(row("kube", { context: hostile, namespace: "a" })), `${hostile} · a`);
  const cut = selectionText(row("kube", { context: "c".repeat(200) }));
  assert.ok(Array.from(cut).length <= 60, "cut for the narrow column");
  assert.ok(cut.endsWith("…"));
});

test("the words of a row: production, not found, signed out and stale", () => {
  const words = (r) => rowWords(r).map((w) => [w.kind, w.text]);
  assert.deepEqual(words(row("kube", { context: "dev" })), []);
  assert.deepEqual(words(row("kube", { context: "prod-eu", production: "Production" })), [["production", "production"]]);
  assert.deepEqual(words(row("talos", { state: "not_found" })), [["missing", "not found"]]);
  assert.deepEqual(words(row("azure", { state: "signed_out" })), [["signed_out", "signed out"]]);
  assert.deepEqual(words(row("kube", { context: "dev", stale: true })), [["stale", "stale"]]);
  // Production comes first; stale is only said when there is a value that can be old.
  assert.deepEqual(words(row("kube", { context: "p", production: "", stale: true })), [["production", "production"], ["stale", "stale"]]);
  assert.deepEqual(words(row("azure", { state: "signed_out", stale: true })), [["signed_out", "signed out"]], "no value, nothing is stale");
  assert.deepEqual(words(row("kube", { state: "unknown" })), [], "an unknown state has no word of its own");
});

test("the production word is the core's flag and never comes from a name", () => {
  const words = rowWords(row("kube", { context: "production", namespace: "prod", production: null }));
  assert.deepEqual(words, [], "a context called production is just a name");
  assert.deepEqual(rowWords(row("kube", { context: "dev", production: "Production" })).map((w) => w.kind), ["production"]);
});

test("each tool has its mark", () => {
  assert.deepEqual(["kube", "talos", "azure", "gam"].map(glyphOf), ["⎈", "◇", "az", "gam"]);
});

// ---- the reach of a choice ------------------------------------------------------------------------------------------------

const pane = (id, over = {}) => ({ pane: id, tool: "kube", view: "pods", context: "prod-eu", namespace: null, production: null, held: false, status: null, ...over });

test("the reach words say which pane a choice changed or what new blocks start with", () => {
  assert.equal(reachWords(3, "prod-eu"), "Pane 3 now uses prod-eu.");
  assert.equal(reachWords(null, "prod-eu"), "New blocks start with prod-eu.");
  assert.equal(reachWords(12, "<b>x</b>"), "Pane 12 now uses <b>x</b>.", "a value is text and stays those characters");
  const long = reachWords(null, "v".repeat(300));
  assert.ok(Array.from(long).length < 140, "a long value is cut in the sentence");
});

test("a choice follows the focused pane only when that pane belongs to the tool", () => {
  const panes = [pane(3), pane(4, { view: "shell" }), pane(5, { tool: "talos", view: "nodes" })];
  assert.equal(reachPane("kube", 3, panes, null), 3, "an integration block of the tool in the core's list");
  assert.equal(reachPane("kube", 4, panes, null), 4, "a terminal a launch opened for the tool");
  assert.equal(reachPane("kube", 5, panes, null), null, "a pane of another tool");
  assert.equal(reachPane("kube", 1, panes, null), null, "a pane the core did not list");
  assert.equal(reachPane("kube", null, panes, null), null, "no focus");
  assert.equal(reachPane("kube", 2, [], { kind: "integration", source: "kube" }), 2, "a block of the tool that has no selection yet");
  assert.equal(reachPane("kube", 2, [], { kind: "integration", source: "talos" }), null, "a block of another tool");
  assert.equal(reachPane("kube", 2, [], { kind: "files", source: "kube" }), null, "a block of another kind");
});

test("the selection of a row is what the tool selects and nothing else", () => {
  assert.deepEqual(selectionOf(row("kube", { context: "prod-eu", namespace: "payments", subscription: "x" })), { context: "prod-eu", namespace: "payments" });
  assert.deepEqual(selectionOf(row("azure", { subscription: "holz-prod" })), { subscription: "holz-prod" });
  assert.deepEqual(selectionOf(row("talos")), {});
  assert.equal(valueOf(row("kube", { context: "prod-eu" }), "context"), "prod-eu");
  assert.equal(valueOf(row("kube", { context: "prod-eu" }), "namespace"), undefined);
});
