// What the page knows about integration blocks (src/integratestate.ts, built into dist/): the reader of the core's view model,
// its caps held equal to the published schema, and the words. No DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  CAPS, MEMBERS, PICK_VERBS, ageWords, clearIntegrationContext, contextText, cutCell, entryTitle, integrationContextText, integrationProduction,
  nextSelection, pickerEntries, readFault, readLaunchAnswer, readRunAnswer, readTools, readView, refreshDue, setIntegrationContext,
  stateWords, viewVerbs,
} from "../../dist/integratestate.js";
import { blockContext, blockTitle, readBlocks } from "../../dist/blockstate.js";

const schema = JSON.parse(readFileSync(new URL("../../../docs/integration-view.schema.json", import.meta.url), "utf8"));
const at = (pointer) => pointer.split("/").slice(1).reduce((node, key) => node[key], schema);

const table = (over = {}) => ({
  kind: "table",
  title: "Pods",
  columns: [{ id: "name", label: "NAME" }, { id: "status", label: "STATUS" }],
  rows: [{ key: "a", cells: ["api-0", "Running"], tone: "normal", actions: [] }],
  truncated: null,
  note: null,
  fetched: 1800000000,
  ...over,
});

test("the caps and member names of the page are the ones of the published schema", () => {
  assert.equal(CAPS.columns, at("/oneOf/0/properties/columns/maxItems"));
  assert.equal(CAPS.rows, at("/oneOf/0/properties/rows/maxItems"));
  assert.equal(CAPS.rows, at("/oneOf/1/properties/rows/maxItems"));
  assert.equal(CAPS.cells, at("/$defs/tableRow/properties/cells/maxItems"));
  assert.equal(CAPS.cell, at("/$defs/text/maxLength"));
  assert.equal(CAPS.text, at("/oneOf/2/properties/text/maxLength"));
  assert.equal(CAPS.title, at("/oneOf/0/properties/title/maxLength"));
  assert.equal(CAPS.note, at("/oneOf/0/properties/note/maxLength"));
  assert.equal(CAPS.key, at("/$defs/tableRow/properties/key/maxLength"));
  assert.equal(CAPS.label, at("/$defs/column/properties/label/maxLength"));
  assert.equal(CAPS.label, at("/$defs/kvRow/properties/label/maxLength"));
  assert.equal(CAPS.actions, at("/$defs/tableRow/properties/actions/maxItems"));
  assert.equal(CAPS.actions, at("/oneOf/1/properties/actions/maxItems"));
  assert.equal(CAPS.args, at("/$defs/action/properties/args/maxProperties"));
  assert.equal(CAPS.argValue, at("/$defs/action/properties/args/additionalProperties/maxLength"));
  assert.equal(CAPS.argName, at("/$defs/action/properties/args/propertyNames/maxLength"));
  assert.equal(CAPS.truncation, at("/$defs/truncation/properties/note/maxLength"));
  assert.equal(CAPS.shown, 240, "the page shows at most 240 characters of a cell");
  assert.ok(CAPS.shown < CAPS.cell);
  assert.deepEqual([...MEMBERS.table].sort(), Object.keys(at("/oneOf/0/properties")).sort());
  assert.deepEqual([...MEMBERS.kv].sort(), Object.keys(at("/oneOf/1/properties")).sort());
  assert.deepEqual([...MEMBERS.text].sort(), Object.keys(at("/oneOf/2/properties")).sort());
  assert.deepEqual([...MEMBERS.row].sort(), Object.keys(at("/$defs/tableRow/properties")).sort());
  assert.deepEqual([...MEMBERS.action].sort(), Object.keys(at("/$defs/action/properties")).sort());
  assert.deepEqual([...MEMBERS.kvRow].sort(), Object.keys(at("/$defs/kvRow/properties")).sort());
  assert.deepEqual([...MEMBERS.tones].sort(), [...at("/$defs/tone/enum")].sort());
  assert.deepEqual([...MEMBERS.effects].sort(), [...at("/$defs/effect/enum")].sort());
});

test("a value that is not a view is no view", () => {
  for (const raw of [null, undefined, "x", 3, [], {}, { kind: "chart" }, { kind: "table" }, { kind: "text", title: "t" }]) {
    assert.equal(readView(raw), null, JSON.stringify(raw));
  }
});

test("a table is read as it is and the page keeps no member the contract does not have", () => {
  const v = readView({ ...table(), extra: "<img src=x onerror=alert(1)>" });
  assert.equal(v.kind, "table");
  assert.equal("extra" in v, false);
  assert.deepEqual(v.columns, [{ id: "name", label: "NAME" }, { id: "status", label: "STATUS" }]);
  assert.deepEqual(v.rows[0].cells, ["api-0", "Running"]);
  assert.equal(v.rows[0].tone, "normal");
  assert.equal(v.fetched, 1800000000);
  assert.equal(v.truncated, null);
});

test("a row is padded or cut to the columns and a cell that is no string is empty", () => {
  const v = readView(
    table({
      rows: [
        { key: "short", cells: ["only"] },
        { key: "long", cells: ["a", "b", "c", "d"] },
        { key: "odd", cells: ["a", 7] },
        { key: "nulled", cells: [null, { x: 1 }] },
      ],
    }),
  );
  assert.deepEqual(v.rows.map((r) => r.cells), [["only", ""], ["a", "b"], ["a", ""], ["", ""]]);
});

test("columns are cut at 64 and rows at 2000, and a row without a key or cells is dropped", () => {
  const columns = Array.from({ length: 70 }, (_, i) => ({ id: `c${i}`, label: `C${i}` }));
  const rows = Array.from({ length: 2100 }, (_, i) => ({ key: `r${i}`, cells: ["x"] }));
  const v = readView(table({ columns, rows }));
  assert.equal(v.columns.length, 64);
  assert.equal(v.rows.length, 2000);
  assert.equal(v.rows[0].cells.length, 64);
  const w = readView(table({ rows: [{ cells: ["a", "b"] }, { key: 3, cells: [] }, { key: "ok" }, null, "x", { key: "fine", cells: ["a", "b"] }] }));
  assert.deepEqual(w.rows.map((r) => r.key), ["fine"]);
});

test("a tone the contract does not have is the normal tone, and an action the contract does not have is dropped", () => {
  const good = { verb: "logs", label: "Logs", effect: "read", args: { pod: "api-0" } };
  const v = readView(
    table({
      rows: [
        {
          key: "a",
          cells: ["a", "b"],
          tone: "sparkle",
          actions: [
            good,
            { verb: "x", label: "X", effect: "nuke", args: {} },
            { verb: "x", label: "X", effect: "read", args: { a: 1 } },
            { verb: "x", label: "X", effect: "read", args: [] },
            { verb: 1, label: "X", effect: "read", args: {} },
            "logs",
          ],
        },
      ],
    }),
  );
  assert.equal(v.rows[0].tone, "normal");
  assert.deepEqual(v.rows[0].actions, [good]);
});

test("an action holds at most 8 arguments and 8 actions, and a long argument value drops the action", () => {
  const many = Object.fromEntries(Array.from({ length: 9 }, (_, i) => [`a${i}`, "v"]));
  const actions = Array.from({ length: 10 }, (_, i) => ({ verb: `v${i}`, label: "L", effect: "read", args: {} }));
  const v = readView(
    table({
      rows: [
        { key: "a", cells: ["a", "b"], actions: [{ verb: "x", label: "X", effect: "read", args: many }, { verb: "y", label: "Y", effect: "read", args: { p: "q".repeat(254) } }, ...actions] },
      ],
    }),
  );
  assert.equal(v.rows[0].actions.length, 8);
  assert.equal(v.rows[0].actions[0].verb, "v0");
});

test("a truncation note and a note are read as text, and a truncation without a number is none", () => {
  const v = readView(table({ truncated: { shown: 2000, note: "Showing the first 2000 of 2500 rows." }, note: "n" }));
  assert.deepEqual(v.truncated, { shown: 2000, note: "Showing the first 2000 of 2500 rows." });
  assert.equal(v.note, "n");
  assert.equal(readView(table({ truncated: { shown: "x", note: "n" } })).truncated, null);
  assert.equal(readView(table({ truncated: "yes" })).truncated, null);
});

test("a key-value list and a text are read", () => {
  const kv = readView({ kind: "kv", title: "Account", rows: [{ label: "Name", value: "holz", tone: "warn" }, { label: 3, value: "x" }, { label: "A" }], actions: [], note: null, fetched: null });
  assert.equal(kv.kind, "kv");
  assert.deepEqual(kv.rows, [{ label: "Name", value: "holz", tone: "warn" }]);
  const text = readView({ kind: "text", title: "Info", text: "a\nb\tc", format: "json", truncated: true, note: "cut" });
  assert.equal(text.text, "a\nb\tc", "line feeds and tabs are kept");
  assert.equal(text.format, "json");
  assert.equal(text.truncated, true);
  assert.equal(readView({ kind: "text", title: "Info", text: "x", format: "markdown" }).format, "plain");
  assert.equal(readView({ kind: "text", title: "t", text: "x".repeat(70000) }).text.length, 65536);
});

test("a cell is cut at 240 characters with an ellipsis and never in the middle of a character", () => {
  assert.equal(cutCell("short"), "short");
  const long = "x".repeat(300);
  const cut = cutCell(long);
  assert.equal(Array.from(cut).length, 240);
  assert.ok(cut.endsWith("…"));
  assert.equal(cutCell("x".repeat(240)), "x".repeat(240));
  const emoji = cutCell("\u{1F600}".repeat(300));
  assert.equal(Array.from(emoji).length, 240);
  assert.ok(!/[\uD800-\uDBFF]$/.test(emoji.slice(0, -1)));
  assert.equal(cutCell("abcdef", 4), "abc…");
});

test("a fault is read as code, reason and the core's sentence, and a plain string is a sentence", () => {
  assert.deepEqual(readFault({ code: "missing", reason: "not_found", text: "kubectl was not found on this computer. Install it or set where it is." }), {
    code: "missing", reason: "not_found", text: "kubectl was not found on this computer. Install it or set where it is.",
  });
  assert.deepEqual(readFault("something went wrong"), { code: "failed", reason: "", text: "something went wrong" });
  assert.equal(readFault({ code: "failed", reason: "x" }).text, "The request failed.");
  assert.equal(readFault(null).text, "The request failed.");
  assert.equal(readFault({ code: "failed", reason: "x", text: "y".repeat(900) }).text.length, 400);
});

test("a run answer has a view, a selection and a time", () => {
  const raw = { view: table(), selection: { selection: { context: "prod", namespace: "payments", node: null }, production: "Production" }, fetched: 1800000000 };
  const a = readRunAnswer(raw);
  assert.equal(a.view.kind, "table");
  assert.deepEqual(a.selection.selection, { context: "prod", namespace: "payments" });
  assert.equal(a.selection.production, true, "the flag is the core's, whatever the name");
  assert.equal(a.fetched, 1800000000);
  assert.equal(readRunAnswer({ ...raw, selection: { selection: {}, production: null } }).selection.production, false);
  assert.equal(readRunAnswer({ ...raw, view: { kind: "x" } }), null);
  assert.equal(readRunAnswer("x"), null);
  assert.deepEqual(readRunAnswer({ view: table() }).selection.selection, {}, "a missing selection is an empty one");
});

test("the tool entries are read entry by entry", () => {
  const tools = readTools([
    {
      id: "kube", label: "Kubernetes", state: "found", path: "/usr/bin/kubectl", binary: null, reason: null, version: "v1.31", searched: [],
      selects: ["context", "namespace", "mystery"],
      verbs: [
        { id: "pods", label: "Pods", effect: "read", output: "view", refresh_secs: 10, args: [] },
        { id: "logs", label: "Logs", effect: "read", output: "terminal", refresh_secs: null, args: [{ name: "pod" }] },
        { id: 7, label: "bad" },
      ],
    },
    { id: "nonesuch", label: "X", state: "found" },
    null,
    { id: "gam", label: "GAM", state: "not_found", searched: ["/a", 3, "/b"], verbs: [], selects: [] },
  ]);
  assert.deepEqual(tools.map((t) => t.id), ["kube", "gam"]);
  assert.deepEqual(tools[0].selects, ["context", "namespace"], "a select the page does not know is dropped");
  assert.deepEqual(tools[0].verbs.map((v) => v.id), ["pods", "logs"]);
  assert.equal(tools[0].verbs[0].refresh_secs, 10);
  assert.equal(tools[0].verbs[1].refresh_secs, null);
  assert.deepEqual(tools[1].searched, ["/a", "/b"]);
  assert.deepEqual(readTools("x"), []);
});

test("a launch answer has the pane and the layout", () => {
  const view = { panes: [[3, { x: 0, y: 0, w: 1, h: 1 }]], tabs: [], workspaces: [], focus: 3, dividers: [] };
  assert.deepEqual(readLaunchAnswer({ pane: 3, view }), { pane: 3, view });
  assert.equal(readLaunchAnswer({ pane: "3", view }), null);
  assert.equal(readLaunchAnswer({ pane: 3, view: null }), null);
  assert.equal(readLaunchAnswer(null), null);
});

test("the list verbs of the selections are the convention of the core", () => {
  assert.deepEqual(PICK_VERBS, { context: "contexts", namespace: "namespaces", subscription: "subscriptions", section: "sections", node: "nodes" });
});

test("a picker lists the keys of the list the core sent", () => {
  const v = readView(table({ columns: [{ id: "c", label: "CURRENT" }, { id: "n", label: "NAME" }, { id: "u", label: "USER" }], rows: [
    { key: "dev", cells: ["*", "dev", "token-user"] },
    { key: "<img src=x onerror=alert(1)>", cells: ["", "<img src=x onerror=alert(1)>", ""] },
  ] }));
  const entries = pickerEntries(v);
  assert.deepEqual(entries.map((e) => e.key), ["dev", "<img src=x onerror=alert(1)>"]);
  assert.equal(entries[0].label, "dev");
  assert.equal(entries[0].detail, "token-user");
  assert.deepEqual(pickerEntries(readView({ kind: "text", title: "t", text: "x" })), []);
});

test("choosing an entry changes one member of the selection and drops the one that belonged to the old value", () => {
  const sel = { context: "prod", namespace: "payments" };
  assert.deepEqual(nextSelection("kube", sel, "namespace", "web"), { context: "prod", namespace: "web" });
  assert.deepEqual(nextSelection("kube", sel, "context", "dev"), { context: "dev" }, "a namespace belongs to its cluster");
  assert.deepEqual(nextSelection("talos", { context: "a", node: "n1" }, "context", "b"), { context: "b" });
  assert.deepEqual(nextSelection("azure", { subscription: "s1" }, "subscription", "s2"), { subscription: "s2" });
  assert.deepEqual(nextSelection("kube", { profile: "p", context: "a" }, "namespace", "web"), { profile: "p", context: "a", namespace: "web" });
});

test("the header says the tool, the selection and the interval, and the production flag is its own", () => {
  const kube = { id: "kube", label: "Kubernetes", selects: ["context", "namespace"] };
  assert.equal(contextText(kube, { context: "prod", namespace: "payments" }, 10), "Kubernetes · prod · payments · 10 s");
  assert.equal(contextText(kube, { context: "prod" }, null), "Kubernetes · prod");
  assert.equal(contextText(kube, {}, null), "Kubernetes");
  assert.equal(contextText({ id: "azure", label: "Azure", selects: ["subscription"] }, { subscription: "holz-prod" }, null), "Azure · holz-prod");
  setIntegrationContext(2, "Kubernetes · prod · payments · 10 s", true);
  assert.equal(integrationContextText(2), "Kubernetes · prod · payments · 10 s");
  assert.equal(integrationProduction(2), true);
  assert.equal(integrationProduction(9), false);
  const b = { pane: 2, kind: "integration", source: "kube", path: "pods", source_label: "Kubernetes" };
  assert.equal(blockTitle(b), "pods");
  assert.equal(blockTitle({ ...b, path: "machine_status" }), "machine status");
  assert.equal(blockContext(b), "Kubernetes · prod · payments · 10 s");
  clearIntegrationContext(2);
  assert.equal(blockContext(b), "Kubernetes", "before the first answer the header says the tool");
  assert.equal(integrationProduction(2), false);
});

test("an integration block is listed only for one of the four tools and a plain verb id", () => {
  const list = readBlocks([
    { pane: 1, kind: "integration", source: "kube", path: "pods", source_label: "Kubernetes" },
    { pane: 2, kind: "integration", source: "local", path: "pods", source_label: "x" },
    { pane: 3, kind: "integration", source: "kube", path: "../pods", source_label: "x" },
    { pane: 4, kind: "integration", source: "kube", path: "", source_label: "x" },
    { pane: 5, kind: "integration", source: "gam", path: "sections", source_label: "GAM" },
  ]);
  assert.deepEqual(list.map((b) => b.pane), [1, 5]);
});

test("the words of each state", () => {
  assert.deepEqual(stateWords("reading"), { text: "Reading …", tone: "plain" });
  assert.deepEqual(stateWords("empty"), { text: "Nothing to show.", tone: "plain" });
  assert.equal(stateWords("failed").tone, "warn");
  assert.equal(stateWords("missing").tone, "warn");
  assert.equal(stateWords("unusable").tone, "warn");
  assert.equal(stateWords("unknown-word").text, "");
});

test("the age is said in seconds, minutes and hours", () => {
  assert.equal(ageWords(0), "Updated just now");
  assert.equal(ageWords(4), "Updated just now");
  assert.equal(ageWords(-3), "Updated just now");
  assert.equal(ageWords(12), "Updated 12 s ago");
  assert.equal(ageWords(59), "Updated 59 s ago");
  assert.equal(ageWords(60), "Updated 1 min ago");
  assert.equal(ageWords(180), "Updated 3 min ago");
  assert.equal(ageWords(7200), "Updated 2 h ago");
  assert.equal(ageWords(Number.NaN), "Updated just now");
});

test("a refresh is due only for a verb that asks for it, in view, shown, alive and with no request on the way", () => {
  const base = { secs: 10, last: 1000, now: 11000, hidden: false, shown: true, busy: false, alive: true };
  assert.equal(refreshDue(base), true);
  assert.equal(refreshDue({ ...base, now: 10999 }), false);
  assert.equal(refreshDue({ ...base, secs: null }), false, "a verb without an interval never ticks");
  assert.equal(refreshDue({ ...base, hidden: true }), false);
  assert.equal(refreshDue({ ...base, shown: false }), false);
  assert.equal(refreshDue({ ...base, busy: true }), false);
  assert.equal(refreshDue({ ...base, alive: false }), false);
  assert.equal(refreshDue({ ...base, secs: 0 }), false);
});

const verbOf = (id, label, effect, output, args = []) => ({ id, label, effect, output, refresh_secs: null, args });

test("a palette title is the tool's label, a colon and the verb's label", () => {
  assert.equal(entryTitle("Kubernetes", "Pods"), "Kubernetes: Pods");
  assert.equal(entryTitle("Talos", "Machine status"), "Talos: Machine status");
});

test("the view verbs are the read verbs with a view output, in the core's order", () => {
  const tool = {
    id: "kube", label: "Kubernetes", state: "found", path: null, binary: null, reason: null, version: null, searched: [], selects: [],
    verbs: [
      verbOf("contexts", "Contexts", "read", "view"),
      verbOf("use_context", "Use context", "local", "done", [{ name: "context", kind: "context", required: true }]),
      verbOf("shell", "Shell", "read", "terminal"),
      verbOf("pods", "Pods", "read", "view"),
      verbOf("delete_pod", "Delete pod", "write", "done", [{ name: "pod", kind: "pod", required: true }]),
    ],
  };
  assert.deepEqual(viewVerbs(tool).map((v) => v.id), ["contexts", "pods"]);
  assert.deepEqual(viewVerbs({ ...tool, verbs: [] }), []);
});
