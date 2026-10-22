// The palette entries of the integrations: one entry for each view of each tool the core listed, one for each terminal verb that
// needs no argument, and two rebindable actions for the Context section and the profiles dialog. The entries are built from the
// core's own tool list; the page names no tool. A mock core answers `integration_tools` and records `block_open`,
// `integration_launch` and `integration_prepare`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { BLOCK, LAUNCHED } from "./integrate-lib.mjs";
import { KUBE, ROWS, layout, openContext } from "./integrate-context-lib.mjs";
import { cardOf, installCore } from "./integrate-confirm-lib.mjs";
import { installProfiles } from "./integrate-profiles-lib.mjs";
import { callsOf, emit } from "./lib.mjs";

const verb = (id, label, effect, output, args = []) => ({ id, label, effect, output, refresh_secs: null, exit_is_state: false, args });
const tool = (id, label, selects, verbs, extra = {}) => ({
  id, label, state: "found", path: `/usr/local/bin/${id}`, place: "path", binary: null, reason: null, version: "1.0", searched: [], selects, verbs, ...extra,
});
const arg = (name, kind) => [{ name, kind, required: true }];

/** The four tools as the core lists them: views, terminals, and the verbs that need an argument or change something. */
const FULL = [
  tool("kube", "Kubernetes", ["context", "namespace"], [
    verb("contexts", "Contexts", "read", "view"),
    verb("namespaces", "Namespaces", "read", "view"),
    verb("pods", "Pods", "read", "view"),
    verb("shell", "Shell with the current context", "read", "terminal"),
    verb("logs", "Logs", "read", "terminal", arg("pod", "pod")),
    verb("exec", "Exec in pod", "exec", "terminal", arg("pod", "pod")),
    verb("delete_pod", "Delete pod", "write", "done", arg("pod", "pod")),
    verb("use_context", "Use context", "local", "done", arg("context", "context")),
  ]),
  tool("talos", "Talos", ["context", "node"], [
    verb("nodes", "Nodes", "read", "view"),
    verb("services", "Services", "read", "view"),
    verb("machine_status", "Machine status", "read", "view"),
    verb("info", "Info", "read", "view"),
    verb("contexts", "Contexts", "read", "view"),
    verb("dashboard", "Dashboard", "exec", "terminal"),
    verb("dmesg", "Kernel messages", "read", "terminal"),
    verb("service_logs", "Service logs", "read", "terminal", arg("service", "service")),
    verb("reboot", "Reboot", "write", "done"),
    verb("use_context", "Use context", "local", "done", arg("context", "context")),
  ]),
  tool("azure", "Azure", ["subscription"], [
    verb("account", "Account", "read", "view"),
    verb("subscriptions", "Subscriptions", "read", "view"),
    verb("resource_groups", "Resource groups", "read", "view"),
    verb("login", "Sign in", "exec", "terminal"),
    verb("use_subscription", "Use subscription", "local", "done", arg("subscription", "subscription")),
  ]),
  tool("gam", "GAM7", ["section"], [
    verb("users", "Users", "read", "view"),
    verb("domain", "Domain", "read", "view"),
    verb("sections", "Sections", "read", "view"),
    verb("delete_user", "Delete user", "write", "done", arg("user", "user")),
  ]),
];

const VIEW_ENTRIES = [
  ["integration.open.kube.contexts", "Kubernetes: Contexts"],
  ["integration.open.kube.namespaces", "Kubernetes: Namespaces"],
  ["integration.open.kube.pods", "Kubernetes: Pods"],
  ["integration.open.talos.nodes", "Talos: Nodes"],
  ["integration.open.talos.services", "Talos: Services"],
  ["integration.open.talos.machine_status", "Talos: Machine status"],
  ["integration.open.talos.info", "Talos: Info"],
  ["integration.open.talos.contexts", "Talos: Contexts"],
  ["integration.open.azure.account", "Azure: Account"],
  ["integration.open.azure.subscriptions", "Azure: Subscriptions"],
  ["integration.open.azure.resource_groups", "Azure: Resource groups"],
  ["integration.open.gam.users", "GAM7: Users"],
  ["integration.open.gam.domain", "GAM7: Domain"],
  ["integration.open.gam.sections", "GAM7: Sections"],
];
const RUN_ENTRIES = [
  ["integration.run.kube.shell", "Kubernetes: Shell with the current context"],
  ["integration.run.talos.dashboard", "Talos: Dashboard"],
  ["integration.run.talos.dmesg", "Talos: Kernel messages"],
  ["integration.run.azure.login", "Azure: Sign in"],
];

/** The tool list with one tool that has the view verb `pods` and nothing else. */
const PODS_ONLY = [{ ...KUBE, verbs: [verb("pods", "Pods", "read", "view")] }];

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const cases = [
    the_palette_opens_a_kubernetes_pods_block,
    every_view_and_argument_less_terminal_verb_has_an_entry,
    a_missing_tool_keeps_its_entries,
    a_tools_event_re_registers,
    mobile_has_no_integration_entries,
    a_terminal_entry_launches_or_asks_by_effect,
    the_two_integration_actions_run_by_id,
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

/** Open the page against the mock core and wait until it has drawn and the tool list is registered. */
async function open(page, url, config = {}, view) {
  await openContext(page, url, { sidebar: false, rows: [], ...config }, view);
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 1);
  await page.evaluate(async () => {
    window.__registry = await import(new URL("actions.js", location.href).href);
  });
  // A phone asks nothing for the list: only a page that has the entries waits for it.
  if (config.os !== "ios" && config.os !== "android") await page.waitForFunction(() => window.__MOCK__.callsOf("integration_tools").length >= 1);
  await page.evaluate(() => document.getElementById("term").focus());
  await page.waitForFunction(() => document.activeElement?.id === "ime");
}

/** The ids of the registered palette entries of the tools, in registration order (not the two core actions). */
const ids = (page) =>
  page.evaluate(() => window.__registry.listActions().map((a) => a.id).filter((id) => id.startsWith("integration.open.") || id.startsWith("integration.run.")));

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
}

/** The entries wait until the page has registered `count` integration actions. */
const registered = (page, count) =>
  page.waitForFunction((n) => window.__registry.listActions().filter((a) => a.id.startsWith("integration.open.") || a.id.startsWith("integration.run.")).length === n, count);

/** The ids and titles of the integration entries, in registration order. */
const entries = (page) =>
  page.evaluate(() =>
    window.__registry.listActions().filter((a) => a.id.startsWith("integration.open.") || a.id.startsWith("integration.run.")).map((a) => [a.id, a.title]),
  );

async function every_view_and_argument_less_terminal_verb_has_an_entry(page, url) {
  await open(page, url, { tools: FULL });
  await registered(page, 18);
  const found = await entries(page);
  assert.deepEqual(found.filter(([id]) => id.startsWith("integration.open.")), VIEW_ENTRIES, "one entry for each view, with the core's labels");
  assert.deepEqual(found.filter(([id]) => id.startsWith("integration.run.")), RUN_ENTRIES, "one entry for each terminal that needs no argument");
  assert.equal(new Set(found.map(([id]) => id)).size, found.length, "the ids are unique");
  // A verb that needs an argument, and a verb that changes something, has no entry.
  for (const title of ["Kubernetes: Logs", "Kubernetes: Exec in pod", "Kubernetes: Delete pod", "Kubernetes: Use context", "Talos: Reboot", "Talos: Service logs", "Azure: Use subscription", "GAM7: Delete user"]) {
    assert.ok(!found.some(([, t]) => t === title), `no entry for ${title}`);
  }
  // The palette lists them: the title narrows to the entry and the entry runs the view.
  await runFromPalette(page, "Talos: Machine status");
  await page.waitForFunction(() => window.__MOCK__.callsOf("block_open").length === 1);
  assert.deepEqual((await callsOf(page, "block_open"))[0], { kind: "integration", source: "talos", path: "machine_status", placement: "tab" });
}

async function a_missing_tool_keeps_its_entries(page, url) {
  const MISSING = "kubectl was not found on this computer. Install it or set where it is.";
  const tools = [{ ...KUBE, state: "not_found", path: null, searched: ["/usr/local/bin"], verbs: [verb("pods", "Pods", "read", "view")] }];
  await open(page, url, { tools, faults: { pods: { code: "missing", reason: "not_found", text: MISSING } } }, layout(1, [1]));
  await registered(page, 1);
  assert.deepEqual(await entries(page), [["integration.open.kube.pods", "Kubernetes: Pods"]], "a tool that is not installed keeps its entry");
  // The mock core opens the block the way the real one does: a second pane that the block list names.
  await page.evaluate((block) => {
    window.__MOCK__.on("block_open", () => {
      window.__CX__.blocks = [block];
      window.__MOCK__.setView({
        workspaces: [{ name: "main", active: true, panes: 2 }],
        tabs: [{ title: "pods", active: true, focus: 2, panes: 2 }],
        focus: 2,
        panes: [[1, { x: 0, y: 0, w: 0.5, h: 1 }], [2, { x: 0.5, y: 0, w: 0.5, h: 1 }]],
        dividers: [{ split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0.5 }],
        pane_names: [],
        windows: 1,
      });
      return window.__MOCK__.view();
    });
  }, BLOCK);
  await runFromPalette(page, "Kubernetes: Pods");
  await page.locator(".block-integration input.ig-path").waitFor();
  assert.equal(await page.locator(".block-integration .ig-status").textContent(), MISSING, "the block says the tool was not found");
  assert.deepEqual(await page.locator(".block-integration .ig-pathbtn").allTextContents(), ["Set path", "Check again"], "and offers the path field");
}

async function a_tools_event_re_registers(page, url) {
  await open(page, url, { tools: [FULL[0]] });
  await registered(page, 4);
  assert.deepEqual((await entries(page)).map(([id]) => id), [...VIEW_ENTRIES.slice(0, 3), RUN_ENTRIES[0]].map(([id]) => id));
  // A tool leaves the list and another arrives: the event makes the page ask again.
  await page.evaluate((azure) => {
    window.__CX__.tools = [azure];
  }, FULL[2]);
  await emit(page, "integration", { seq: 1, kind: "tools", tool: null, pane: null, code: null });
  await registered(page, 4);
  assert.deepEqual(
    (await entries(page)).map(([id]) => id),
    ["integration.open.azure.account", "integration.open.azure.subscriptions", "integration.open.azure.resource_groups", "integration.run.azure.login"],
    "the entries of the tool that left are gone and the new tool's are there",
  );
  // The list shrinks to nothing.
  await page.evaluate(() => {
    window.__CX__.tools = [];
  });
  await emit(page, "integration", { seq: 2, kind: "tools", tool: null, pane: null, code: null });
  await registered(page, 0);
  assert.deepEqual(await ids(page), []);
}

async function mobile_has_no_integration_entries(page, url) {
  await open(page, url, { tools: FULL, os: "ios" });
  // Nothing asks for the list on a phone, so ask for it here; the entries are still not made.
  await page.evaluate(async () => {
    const t = await import(new URL("integratetools.js", location.href).href);
    await t.loadTools(true);
  });
  assert.equal((await callsOf(page, "integration_tools")).length >= 1, true, "the list arrived");
  await page.evaluate(() => new Promise((r) => setTimeout(r, 300)));
  assert.deepEqual(await entries(page), [], "a phone has no integration entries");
  assert.equal(await titleOf(page, "integration.context"), null, "and no handler of the two actions: there is no sidebar to show");
  await emit(page, "integration", { seq: 1, kind: "tools", tool: null, pane: null, code: null });
  await page.evaluate(() => new Promise((r) => setTimeout(r, 300)));
  assert.deepEqual(await entries(page), []);
}

async function a_terminal_entry_launches_or_asks_by_effect(page, url) {
  const card = (tool, verbId, program) => cardOf({ tool, verb: verbId, effect: "exec", program, argv: ["x"], target: [["context", "talos-prod"]] });
  await installCore(page, { cards: { "talos/dashboard": card("talos", "dashboard", "talosctl"), "azure/login": card("azure", "login", "az") } });
  await open(page, url, { tools: FULL, launch: { pane: 3, view: LAUNCHED } });
  await registered(page, 18);

  // A read verb launches at once, beside nothing: a new tab, with the focused pane named.
  await runFromPalette(page, "Kubernetes: Shell with the current context");
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_launch").length === 1);
  assert.deepEqual((await callsOf(page, "integration_launch"))[0], { tool: "kube", verb: "shell", pane: 1, args: {}, placement: "tab" });
  assert.equal((await callsOf(page, "integration_prepare")).length, 0, "a read verb asks nothing");
  await page.waitForFunction(async () => (await import(new URL("state.js", location.href).href)).state.view?.panes.length === 3);

  // A second read verb, on the new focus.
  await runFromPalette(page, "Talos: Kernel messages");
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_launch").length === 2);
  assert.deepEqual((await callsOf(page, "integration_launch"))[1], { tool: "talos", verb: "dmesg", pane: 3, args: {}, placement: "tab" });

  // An Exec verb asks first through the card and launches nothing by itself.
  await runFromPalette(page, "Talos: Dashboard");
  await page.locator("section.ic").waitFor();
  const prepared = await callsOf(page, "integration_prepare");
  assert.deepEqual(prepared, [{ tool: "talos", verb: "dashboard", pane: 3, args: {} }]);
  assert.equal((await callsOf(page, "integration_launch")).length, 2, "an Exec verb does not launch before the card is allowed");
  assert.equal((await callsOf(page, "integration_confirm")).length, 0, "and confirms nothing by itself");
  await page.locator("section.ic .ic-no").click();
  await page.waitForFunction(() => document.querySelector("section.ic") === null);

  await runFromPalette(page, "Azure: Sign in");
  await page.locator("section.ic").waitFor();
  assert.deepEqual((await callsOf(page, "integration_prepare")).at(-1), { tool: "azure", verb: "login", pane: 3, args: {} });
  assert.equal((await callsOf(page, "integration_launch")).length, 2);
  await page.locator("section.ic .ic-no").click();
}

/** The key binding rows the core sends, from the golden file the page and the core are both held to. */
const GOLDEN = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));
const coreRow = (id) => GOLDEN.settings_view.keybindings.find((r) => r.action === id);

async function the_two_integration_actions_run_by_id(page, url) {
  // The core's two rows have no chord, and the page registers actions with the core's titles under the core's ids.
  for (const id of ["integration.context", "integration.profiles"]) {
    const row = coreRow(id);
    assert.ok(row, `the core lists ${id}`);
    assert.equal(row.chord, null, `${id} has no default chord`);
  }
  await installProfiles(page, { profiles: [], notes: [] });
  // The sidebar starts hidden: the first action shows it with the Context section in view.
  await open(page, url, { sidebar: false, rows: ROWS });
  await page.waitForSelector(".cx-row", { state: "attached" });
  for (const id of ["integration.context", "integration.profiles"]) {
    assert.equal(await titleOf(page, id), coreRow(id).title, `${id}: the page and the core name it the same`);
  }
  assert.equal(await page.locator(".cx-section").isVisible(), false, "the sidebar is hidden to begin with");
  await page.evaluate(() => window.__registry.runAction("integration.context"));
  await page.locator(".cx-section").waitFor({ state: "visible" });
  assert.equal(
    await page.evaluate(() => document.activeElement?.classList.contains("cx-value")),
    true,
    "and the keyboard is on the first value of the section",
  );

  // The second action opens the profiles dialog without the section's button.
  await page.evaluate(() => document.getElementById("term").focus());
  await page.evaluate(() => window.__registry.runAction("integration.profiles"));
  await page.waitForSelector(".dlg.ip-box");
  assert.equal((await callsOf(page, "integration_profiles")).length, 1, "the dialog read the profiles");
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => document.querySelector(".dlg.ip-box") === null);
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime", "the keyboard goes back to the terminal");

  // Both are in the palette.
  await page.keyboard.press("Meta+k");
  await page.waitForFunction(() => document.activeElement?.classList.contains("palette-input") === true);
  await page.keyboard.type("Integration profiles");
  assert.equal(await page.locator(".palette-row .palette-title").first().textContent(), "Integration profiles\u2026");
  await page.keyboard.press("Escape");
}
