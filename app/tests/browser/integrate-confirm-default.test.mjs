// Make default (docs/design.md 6.25, 6.11 and 6.26): the list under a chip of the block and under a value of the Context section ends with
// `Make default…` when the core's list has a Local verb whose first argument has that kind of value. The page finds the verb from the
// list and names no tool; choosing the entry asks for the card with the value the pane has now, and nothing changes before it is allowed.
import assert from "node:assert/strict";
import { KUBE, TICKET, HASH, allow, calls, cardNode, cardOf, gone, openConfirm, outcome, textOf } from "./integrate-confirm-lib.mjs";
import { ALL_TOOLS, KUBE_FULL, listView, openContext, paneInfo, row } from "./integrate-context-lib.mjs";
import { answer } from "./integrate-lib.mjs";
import { installCore } from "./integrate-confirm-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await make_default_is_offered_where_a_local_verb_exists_and_asks_first(page, url);
  await the_section_offers_it_for_its_values(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const verb = (id, label, effect, args = []) => ({ id, label, effect, output: effect === "read" ? "view" : "done", refresh_secs: null, exit_is_state: false, args });
const arg = (name, kind) => ({ name, kind, required: true });
const CHANGES = "the kubeconfig files named by KUBECONFIG, by default ~/.kube/config";

/** The Kubernetes tool with a Local verb for the context under a name the page cannot know, and none for the namespace. */
const KUBE_RENAMED = { ...KUBE, verbs: [...KUBE.verbs.filter((v) => v.id !== "use_context"), verb("switch_ctx", "Switch context", "local", [arg("context", "context")])] };
const KUBE_BOTH = { ...KUBE_RENAMED, verbs: [...KUBE_RENAMED.verbs, verb("set_namespace", "Set namespace", "local", [arg("namespace", "namespace")])] };

const localCard = (over = {}) =>
  cardOf({
    verb: "switch_ctx", effect: "local", argv: ["config", "use-context", "--", "prod"], target: [["context", "prod"], ["changes", CHANGES]], ...over,
  });

async function make_default_is_offered_where_a_local_verb_exists_and_asks_first(page, url) {
  const runs = {
    pods: answer({ kind: "table", title: "Pods", columns: [{ id: "name", label: "NAME" }], rows: [{ key: "api-0", cells: ["api-0"], tone: "normal", actions: [] }], truncated: null, note: null, fetched: null }),
    contexts: answer(listView("Contexts", ["prod", "dev"])),
    namespaces: answer(listView("Namespaces", ["payments", "default"])),
  };
  await openConfirm(page, url, { cards: { "kube/switch_ctx": localCard() } }, { tools: [KUBE_RENAMED], runs });
  await page.waitForSelector(".block-integration .ig-table tbody tr");

  // The context chip: the list ends with the entry, after the entries of the core.
  await page.locator('.ig-chip[data-select="context"]').click();
  await page.waitForSelector(".ig-list .ig-opt");
  const options = await page.locator(".ig-list .ig-opt").evaluateAll((o) => o.map((x) => x.textContent));
  assert.equal(options.at(-1), "Make default…", JSON.stringify(options));
  assert.equal(options.filter((t) => t === "Make default…").length, 1);
  assert.equal(options.length, 3, "two entries of the core and the one of the page");
  assert.equal(await page.locator(".ig-list .ig-opt").last().getAttribute("title"), "Asks for confirmation");

  // Choosing it asks for the card of the Local verb with the value the pane has now, and runs nothing.
  await page.locator(".ig-list .ig-opt").last().click();
  await cardNode(page).waitFor();
  assert.deepEqual(await calls(page, "integration_prepare"), [{ tool: "kube", verb: "switch_ctx", pane: 2, args: { context: "prod" } }]);
  assert.equal(await textOf(page, ".ic-title"), "Kubernetes · Switch context");
  assert.equal(await textOf(page, ".ic-effect"), `Changes the tool's own configuration. It writes: ${CHANGES}`);
  const rows = await cardNode(page).locator(".ic-target .ic-row").evaluateAll((r) => r.map((x) => [x.children[0].textContent, x.children[1].textContent]));
  assert.deepEqual(rows, [["context", "prod"]], "the changes sentence is in the effect line, not twice");
  assert.equal((await calls(page, "integration_confirm")).length, 0);
  assert.equal((await calls(page, "integration_select")).length, 0, "choosing the entry selects nothing");
  await allow(page);
  await page.waitForFunction(() => document.querySelector(".ic-line")?.textContent?.startsWith("Done:"));
  assert.deepEqual(await calls(page, "integration_confirm"), [{ ticket: TICKET, hash: HASH }]);

  // The namespace chip: this tool has no Local verb for a namespace, so the list has no entry.
  await gone(page);
  await page.locator('.ig-chip[data-select="namespace"]').click();
  await page.waitForSelector(".ig-list .ig-opt");
  assert.deepEqual(await page.locator(".ig-list .ig-opt").evaluateAll((o) => o.map((x) => x.textContent.includes("Make default"))), [false, false]);
  assert.equal(await page.locator(".ig-list .ig-opt-default").count(), 0);

  // With a verb for the namespace too the entry is there and prepares that verb.
  await openConfirm(page, url, { cards: { "kube/set_namespace": cardOf({ verb: "set_namespace", effect: "local", argv: ["config", "set-context", "--current", "--namespace=payments"], target: [["namespace", "payments"], ["changes", CHANGES]] }) } }, { tools: [KUBE_BOTH], runs });
  await page.waitForSelector(".block-integration .ig-table tbody tr");
  await page.locator('.ig-chip[data-select="namespace"]').click();
  await page.waitForSelector(".ig-list .ig-opt");
  await page.locator(".ig-list .ig-opt-default").click();
  await cardNode(page).waitFor();
  assert.deepEqual((await calls(page, "integration_prepare")).at(-1), { tool: "kube", verb: "set_namespace", pane: 2, args: { namespace: "payments" } });
}

async function the_section_offers_it_for_its_values(page, url) {
  const tools = [
    { ...KUBE_FULL, verbs: [...KUBE_FULL.verbs, verb("use_context", "Use context", "local", [arg("context", "context")])] },
    { ...ALL_TOOLS[1], verbs: [...ALL_TOOLS[1].verbs, verb("use_context", "Use context", "local", [arg("context", "context")])] },
    { ...ALL_TOOLS[2], verbs: [...ALL_TOOLS[2].verbs, verb("set_subscription", "Set subscription", "local", [arg("subscription", "subscription")])] },
    ALL_TOOLS[3],
  ];
  const runs = {
    contexts: answer(listView("Contexts", ["prod-eu", "dev"])),
    subscriptions: answer(listView("Subscriptions", ["holz-prod", "holz-dev"])),
    sections: answer(listView("Sections", ["work", "home"])),
  };
  await installCore(page, {
    cards: {
      "kube/use_context": cardOf({ verb: "use_context", effect: "local", argv: ["config", "use-context", "--", "prod-eu"], target: [["context", "prod-eu"], ["changes", CHANGES]] }),
      "azure/set_subscription": cardOf({ tool: "azure", verb: "set_subscription", program: "az", effect: "local", argv: ["account", "set", "--subscription=holz-prod"], target: [["subscription", "holz-prod"], ["changes", "the Azure CLI's own configuration"]] }),
    },
  });
  await openContext(page, url, { tools, runs });
  await page.waitForSelector(".cx-row");
  const entries = async (tool, kind) => {
    await page.locator(`.cx-row[data-tool="${tool}"] .cx-value[data-select="${kind}"]`).click();
    await page.waitForSelector(".ig-list .ig-opt");
    const out = await page.locator(".ig-list .ig-opt").evaluateAll((o) => o.map((x) => x.textContent));
    await page.keyboard.press("Escape");
    await page.waitForFunction(() => document.querySelector(".ig-list") === null);
    return out;
  };
  assert.equal((await entries("kube", "context")).at(-1), "Make default…");
  assert.equal((await entries("talos", "context")).at(-1), "Make default…");
  assert.equal((await entries("azure", "subscription")).at(-1), "Make default…");
  assert.ok(!(await entries("gam", "section")).includes("Make default…"), "GAM has no Local verb");

  // Azure: the entry prepares the Local verb with the value the row shows; the card says what it writes.
  await page.locator('.cx-row[data-tool="azure"] .cx-value[data-select="subscription"]').click();
  await page.waitForSelector(".ig-list .ig-opt");
  await page.locator(".ig-list .ig-opt-default").click();
  await cardNode(page).waitFor();
  assert.deepEqual((await calls(page, "integration_prepare")).at(-1), { tool: "azure", verb: "set_subscription", pane: null, args: { subscription: "holz-prod" } });
  assert.equal(await textOf(page, ".ic-title"), "Azure · Set subscription");
  assert.equal(await textOf(page, ".ic-effect"), "Changes the tool's own configuration. It writes: the Azure CLI's own configuration");
  assert.equal((await calls(page, "integration_confirm")).length, 0);
  await cardNode(page).locator(".ic-no").click();
  await gone(page);
  assert.equal((await calls(page, "integration_select")).length, 0);
}
