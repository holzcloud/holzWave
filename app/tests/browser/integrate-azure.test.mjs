// Azure end to end on the page: the answers of the core, written by `core/tests/integrate_e2e_azure.rs` from a fake `az` and read here
// from `app/tests/fixtures/integration/`, are what the mock core answers, so the page draws exactly what the core produced. Signed out is
// a state a person can act on: the word in the Context section, the rows and the `Sign in…` action in the block, and no status segment
// (the status line draws a value only when the core has one).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { layout, openContext, sectionShown } from "./integrate-context-lib.mjs";
import { allow, cardNode, installCore, textOf } from "./integrate-confirm-lib.mjs";
import { callsOf } from "./lib.mjs";

/** A golden file of the core's answers. */
const golden = (name) => JSON.parse(readFileSync(new URL(`../fixtures/integration/${name}`, import.meta.url), "utf8"));

/** The answer of `integration_run` that a golden holds. */
const runOf = (g) => ({ view: g.view, selection: g.selection, fetched: g.fetched });

const BLOCK = (verb) => ({ pane: 2, kind: "integration", source: "azure", path: verb, source_label: "Azure" });
const TWO_PANES = {
  workspaces: [{ name: "main", active: true, panes: 2 }],
  tabs: [{ title: "azure", active: true, focus: 2, panes: 2 }],
  focus: 2,
  panes: [[1, { x: 0, y: 0, w: 0.5, h: 1 }], [2, { x: 0.5, y: 0, w: 0.5, h: 1 }]],
  dividers: [{ split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0.5 }],
  pane_names: [],
  windows: 1,
};

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const cases = [
    the_signed_out_account_is_a_state_in_the_block_and_the_section,
    the_signed_in_account_has_its_rows_and_no_sign_in,
    the_subscription_chip_switches_for_this_pane,
    the_status_segment_names_the_subscription,
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

/**
 * Open the page against the mock core with the tool list and the rows of a golden, and `runs` (the answers of the views by verb id,
 * each from a golden), wait until the entries are registered, and give the keyboard to the terminal. `config` is merged into the
 * set-up of `openContext`.
 */
async function open(page, url, g, runs, config = {}) {
  const verb = Object.keys(runs)[0];
  await openContext(page, url, { tools: [g.tool], rows: g.rows, runs, sidebar: true, ...config }, layout(1, [1]));
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 1);
  await page.evaluate(async () => {
    window.__registry = await import(new URL("actions.js", location.href).href);
  });
  await page.waitForFunction((id) => window.__registry.listActions().some((a) => a.id === id), `integration.open.azure.${verb}`);
  await page.evaluate(() => document.getElementById("term").focus());
  await page.waitForFunction(() => document.activeElement?.id === "ime");
}

/** Open the palette, type `query` and press Enter: the best match runs. */
async function runFromPalette(page, query) {
  await page.keyboard.press("Meta+k");
  await page.waitForFunction(() => document.activeElement?.classList.contains("palette-input") === true);
  await page.keyboard.type(query);
  await page.keyboard.press("Enter");
}

/** Make the mock core open the block the way the real one does: a second pane that the block list names. */
async function coreOpensTheBlock(page, verb) {
  await page.evaluate(([block, view]) => {
    window.__MOCK__.on("block_open", () => {
      window.__CX__.blocks = [block];
      window.__MOCK__.setView(view);
      return window.__MOCK__.view();
    });
  }, [BLOCK(verb), TWO_PANES]);
}

/** Enter the block of a view through the palette, as a person does, and wait until it has drawn its rows (`shape`: its selector). */
async function enterBlock(page, title, verb, shape = ".ig-kv") {
  await coreOpensTheBlock(page, verb);
  await runFromPalette(page, title);
  await page.waitForFunction(() => window.__MOCK__.callsOf("block_open").length === 1);
  assert.deepEqual((await callsOf(page, "block_open"))[0], { kind: "integration", source: "azure", path: verb, placement: "tab" });
  await page.waitForSelector(`.block-integration ${shape}`);
}

const azureRow = '.cx-row[data-tool="azure"]';
const rowsOf = (page) => page.locator(".block-integration .ig-kvrow").evaluateAll((lines) => lines.map((l) => [l.querySelector(".ig-k").textContent, l.querySelector(".ig-v").textContent, l.dataset.tone ?? "normal", l.dataset.mark ?? ""]));

async function the_signed_out_account_is_a_state_in_the_block_and_the_section(page, url) {
  const g = golden("azure_account_signed_out.json");
  assert.equal(g.view.kind, "kv", "the core's answer is a key-value model");
  await open(page, url, g, { account: runOf(g) });
  await sectionShown(page);

  // The Context section says it in a word with an underline and the mark besides its colour, and shows no value.
  const word = page.locator(`${azureRow} .cx-word`);
  assert.deepEqual(await word.allTextContents(), ["signed out"]);
  assert.match(await word.evaluate((n) => getComputedStyle(n).textDecorationLine), /underline/);
  assert.equal(await word.evaluate((n) => getComputedStyle(n, "::before").content), '"! "');
  assert.equal(await page.locator(`${azureRow} .cx-sel`).textContent(), "—", "no subscription is shown for a signed-out account");

  // The status line draws a segment only when the core has a value: there is none for a signed-out account.
  assert.equal(await page.locator('#status .ctx-seg[data-tool="azure"]').count(), 0);

  // The block: the core's rows, the warn tone with the mark `!`, the sign-in action, and no fault.
  await enterBlock(page, "Azure: Account", "account");
  assert.deepEqual(await rowsOf(page), [["State", "Signed out", "warn", "!"]]);
  const value = page.locator(".block-integration .ig-kvrow .ig-v");
  assert.equal(await value.evaluate((n) => getComputedStyle(n, "::before").content), '"! "', "the mark is the second cue");
  const buttons = page.locator(".block-integration .ig-kvacts .ig-act");
  assert.deepEqual(await buttons.allTextContents(), ["Sign in…"], "the one action is the core's, and it asks first");
  assert.equal(await buttons.getAttribute("title"), "Asks for confirmation");
  assert.notEqual(await page.locator(".block-integration .ig-status").getAttribute("data-tone"), "warn", "signed out is not a fault");
  assert.equal(await page.locator(".block-integration .ig-status").getAttribute("data-tone"), null);

  // The page asked for the account with the facts of the pane, and nothing was sent for the sign-in.
  assert.deepEqual((await callsOf(page, "integration_run")).at(-1), { tool: "azure", verb: "account", pane: 2, args: {} });
  assert.equal((await callsOf(page, "integration_prepare")).length, 0, "nothing is asked before the press");
  assert.equal((await callsOf(page, "integration_launch")).length, 0);
}

async function the_signed_in_account_has_its_rows_and_no_sign_in(page, url) {
  const g = golden("azure_account_signed_in.json");
  const guid = "00000000-0000-4000-8000-000000000002";
  await open(page, url, g, { account: runOf(g) });
  await sectionShown(page);

  // The Context section and the status line say the same thing, from the same row of the core.
  assert.equal(await page.locator(`${azureRow} .cx-sel`).textContent(), guid);
  assert.equal(await page.locator(`${azureRow} .cx-word`).count(), 0, "a signed-in account has no word");
  assert.equal(await page.locator('#status .ctx-seg[data-tool="azure"] .ctx-text').textContent(), `az ${guid}`);
  assert.equal(await page.locator('#status .ctx-seg[data-tool="azure"]').getAttribute("aria-label"), `Azure subscription ${guid}`);

  await enterBlock(page, "Azure: Account", "account");
  assert.deepEqual(await rowsOf(page), [
    ["State", "Signed in", "normal", ""],
    ["User", "user@example.com", "normal", ""],
    ["Subscription", "Item 1", "normal", ""],
    ["Subscription ID", guid, "normal", ""],
    ["Tenant ID", "00000000-0000-4000-8000-000000000001", "normal", ""],
    ["Environment", "AzureCloud", "normal", ""],
    ["Subscription state", "Enabled", "normal", ""],
  ]);
  assert.equal(await page.locator(".block-integration .ig-kvacts").count(), 0, "no sign-in action when signed in");
}

const guid = (n) => `00000000-0000-4000-8000-00000000000${n}`;
const TENANT = guid(1);
const FIRST = guid(2);
const SECOND = guid(3);
const THIRD = guid(4);
const choice = azureRow + ' .cx-value[data-select="subscription"]';
const cellsOf = (page) => page.locator(".block-integration .ig-table tbody tr").evaluateAll((rows) => rows.map((r) => Array.from(r.querySelectorAll(".ig-cell:not(.ig-acts)")).map((c) => c.textContent)));

async function the_subscription_chip_switches_for_this_pane(page, url) {
  const account = golden("azure_account_signed_in.json");
  const subs = golden("azure_subscriptions.json");
  const card = golden("azure_card_set_subscription.json");
  const done = { kind: "ran", pane: null, note: "Make default done.", view: null };
  await installCore(page, { cards: { "azure/set_subscription": card }, steps: [{ answer: done }] });
  await open(page, url, account, { subscriptions: runOf(subs), account: runOf(account) });
  await sectionShown(page);
  await enterBlock(page, "Azure: Subscriptions", "subscriptions", ".ig-table tbody tr");

  // The block draws the core's table: the default starred, the disabled one in the warn tone with its state in words and the mark.
  assert.deepEqual(await page.locator(".block-integration .ig-table thead th:not(.ig-th-act)").allTextContents(), ["Default", "Name", "ID", "State", "Tenant"]);
  assert.deepEqual(await cellsOf(page), [
    ["*", "Item 1", FIRST, "Enabled", TENANT],
    ["", "Item 2", SECOND, "Enabled", TENANT],
    ["", "Item 3", THIRD, "Disabled", TENANT],
  ]);
  const rows = page.locator(".block-integration .ig-table tbody tr");
  assert.equal(await rows.nth(2).getAttribute("data-tone"), "warn");
  assert.equal(await rows.nth(2).locator(".ig-cell").first().getAttribute("data-mark"), "!", "the mark is the second cue besides the colour");
  assert.equal(await rows.nth(0).getAttribute("data-tone"), null);
  assert.deepEqual(await page.locator(".block-integration .ig-table .ig-act").allTextContents(), ["Make default…", "Make default…", "Make default…"]);

  // The chip of the Context section opens the list the core read for this pane; the current value is marked.
  await page.locator(choice).click();
  const options = page.locator(`${azureRow} .ig-list .ig-opt:not(.ig-opt-default)`);
  await options.first().waitFor();
  assert.deepEqual(await options.locator(".ig-opt-label").allTextContents(), [FIRST, SECOND, THIRD]);
  assert.deepEqual(await options.evaluateAll((o) => o.map((x) => x.getAttribute("aria-selected"))), ["true", "false", "false"]);
  assert.equal(await options.nth(2).locator(".ig-opt-detail").textContent(), `Item 3 · Disabled · ${TENANT}`);
  assert.equal(await page.locator(`${azureRow} .ig-opt-default .ig-opt-label`).textContent(), "Make default…");
  assert.deepEqual((await callsOf(page, "integration_run")).at(-1), { tool: "azure", verb: "subscriptions", pane: 2, args: {} });

  // A choice is for the block that has the focus, and writes nothing else: one call to select, for that pane.
  await options.nth(1).click();
  await page.waitForFunction((n) => window.__MOCK__.callsOf("integration_select").length === n, 1);
  const selected = (await callsOf(page, "integration_select"))[0];
  assert.equal(selected.tool, "azure");
  assert.equal(selected.pane, 2, "this pane, not the pick of the tool");
  assert.equal(selected.selection.subscription, SECOND);
  await page.waitForFunction((t) => document.querySelector(".cx-line")?.textContent === t, `Pane 2 now uses ${SECOND}.`);
  assert.equal((await callsOf(page, "integration_prepare")).length, 0, "choosing asks no card");

  // `Make default…` of a row asks first: the card is the core's, field by field, and nothing runs before the press.
  await rows.nth(1).locator(".ig-act").click();
  await cardNode(page).waitFor();
  assert.deepEqual(await callsOf(page, "integration_prepare"), [{ tool: "azure", verb: "set_subscription", pane: 2, args: { subscription: SECOND } }]);
  assert.equal(await textOf(page, ".ic-title"), "Azure · Make default");
  const pairs = (selector) => cardNode(page).locator(`${selector} .ic-row`).evaluateAll((r) => r.map((x) => [x.children[0].textContent, x.children[1].textContent]));
  assert.deepEqual(await pairs(".ic-target"), card.target.filter(([name]) => name !== "changes"));
  const changes = card.target.find(([name]) => name === "changes")[1];
  assert.match(await textOf(page, ".ic-effect"), new RegExp(`It writes: ${changes.replace(/[()]/g, "\\$&")}$`), "the card names the configuration it writes");
  assert.deepEqual(await pairs(".ic-args"), [["program", "az"], ...card.argv.map((a, i) => [`arg ${i + 1}`, a])]);
  assert.equal(await cardNode(page).locator(".ic-guard").count(), 0, "no guard without a production profile");
  assert.equal(await textOf(page, ".ic-hash"), `hash ${card.hash.slice(0, 12)}`);
  assert.equal((await callsOf(page, "integration_confirm")).length, 0, "nothing runs before the press");
  await allow(page);
  await page.waitForFunction(() => document.querySelector(".ic-line")?.textContent === "Done: Make default done.");
  assert.deepEqual(await callsOf(page, "integration_confirm"), [{ ticket: card.ticket, hash: card.hash }], "one press, the ticket and the hash of the core");
  await page.keyboard.press("Escape");

  // The last entry of the list asks for the same card for the value the row shows.
  await page.locator(choice).click();
  await page.locator(`${azureRow} .ig-opt-default`).click();
  await cardNode(page).waitFor();
  assert.deepEqual((await callsOf(page, "integration_prepare")).at(-1), { tool: "azure", verb: "set_subscription", pane: 2, args: { subscription: FIRST } });
  await cardNode(page).locator(".ic-no").click({ force: true });
}

async function the_status_segment_names_the_subscription(page, url) {
  const groups = golden("azure_groups.json");
  await open(page, url, groups, { groups: runOf(groups) });
  await sectionShown(page);

  // The segment says what the core's row says: the subscription of the focused pane, and production as a word besides the colour.
  const segment = '#status .ctx-seg[data-tool="azure"]';
  assert.equal(await page.locator(`${segment} .ctx-text`).textContent(), `az ${FIRST}`);
  const word = page.locator(`${segment} .ctx-prod`);
  assert.equal(await word.textContent(), "PRODUCTION");
  assert.match(await word.evaluate((n) => getComputedStyle(n).textDecorationLine), /underline/);
  assert.equal(await word.evaluate((n) => getComputedStyle(n, "::before").content), '"! "');
  assert.equal(await page.locator(segment).getAttribute("aria-label"), `Azure subscription ${FIRST}, production`);
  assert.equal(await page.locator(`${azureRow} .cx-sel`).textContent(), FIRST, "the section and the segment say the same thing");
  assert.deepEqual(await page.locator(`${azureRow} .cx-word`).allTextContents(), ["production"]);

  // The groups of that subscription: four columns, no action, a state other than Succeeded in words and in the warn tone.
  await enterBlock(page, "Azure: Resource groups", "groups", ".ig-table tbody tr");
  assert.deepEqual(await page.locator(".block-integration .ig-table thead th").allTextContents(), ["Name", "Location", "State", "Tags"]);
  assert.deepEqual(await cellsOf(page), [
    ["rg-demo-one", "westeurope", "Succeeded", "1"],
    ["rg-demo-two", "northeurope", "Succeeded", "0"],
    ["rg-demo-three", "westeurope", "Deleting", "0"],
  ]);
  assert.equal(await page.locator(".block-integration .ig-act").count(), 0, "a group has no action");
  const last = page.locator(".block-integration .ig-table tbody tr").nth(2);
  assert.equal(await last.getAttribute("data-tone"), "warn");
  assert.equal(await last.locator(".ig-cell").first().getAttribute("data-mark"), "!");
}
