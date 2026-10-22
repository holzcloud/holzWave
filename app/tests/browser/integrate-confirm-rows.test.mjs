// Row actions of every effect (docs/design.md sections 6.25 and 6.26): an action that only reads and opens a terminal launches as
// before; an action of any other effect asks the core for a card with the row's own arguments, ends in an ellipsis and says so in its
// title, and runs nothing until the card is allowed. An action the core's list does not back up is not drawn.
import assert from "node:assert/strict";
import { KUBE, TICKET, HASH, calls, cardNode, cardOf, gone, openConfirm, outcome, textOf, allow, wait } from "./integrate-confirm-lib.mjs";
import { answer, logs, podsView } from "./integrate-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await row_actions_of_every_effect_prepare_a_card(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const act = (verb, label, effect, args) => ({ verb, label, effect, args });

async function row_actions_of_every_effect_prepare_a_card(page, url) {
  const rows = [
    {
      key: "api-0", cells: ["api-0", "1/1", "Running", "0", "3d"], tone: "normal",
      actions: [logs("api-0"), act("exec", "Exec", "exec", { pod: "api-0" }), act("delete_pod", "Delete", "write", { pod: "api-0" })],
    },
    {
      key: "odd-1", cells: ["odd-1", "1/1", "Running", "0", "3d"], tone: "normal",
      actions: [act("nuke", "Nuke", "write", { pod: "odd-1" }), act("delete_pod", "Quiet delete", "read", { pod: "odd-1" }), act("logs", "Fake logs", "exec", { pod: "odd-1" })],
    },
  ];
  await openConfirm(page, url, {}, { tools: [KUBE], runs: { pods: answer(podsView({ rows })) }, launch: null });
  await page.waitForSelector(".block-integration .ig-table tbody tr");

  // The labels and titles: an action that asks first ends in an ellipsis and says so; the core's list backs up every drawn button.
  const first = await page.locator('.ig-row[data-i="0"] .ig-act').evaluateAll((b) => b.map((x) => [x.textContent, x.title]));
  assert.deepEqual(first, [["Logs", ""], ["Exec…", "Asks for confirmation"], ["Delete…", "Asks for confirmation"]]);
  assert.equal(await page.locator('.ig-row[data-i="1"] .ig-act').count(), 0, "an unknown verb or an effect the core's list does not give is not drawn");

  // A press prepares a card with the action's own arguments and the block's pane; nothing runs.
  const reads = async () => (await calls(page, "integration_run")).filter((c) => c.verb === "pods").length;
  const before = await reads();
  await page.locator('.ig-row[data-i="0"] .ig-act', { hasText: "Delete…" }).click();
  await cardNode(page).waitFor();
  assert.deepEqual(await calls(page, "integration_prepare"), [{ tool: "kube", verb: "delete_pod", pane: 2, args: { pod: "api-0" } }]);
  assert.equal(await textOf(page, ".ic-title"), "Kubernetes · Delete pod");
  assert.equal((await calls(page, "integration_confirm")).length, 0, "nothing runs before the card is allowed");
  assert.equal((await calls(page, "integration_launch")).length, 0);
  assert.equal(await reads(), before, "no new read yet");

  // Allowing it runs it once, and the block reads again.
  await allow(page);
  await page.waitForFunction(() => document.querySelector(".ic-line")?.textContent?.startsWith("Done:"));
  assert.deepEqual(await calls(page, "integration_confirm"), [{ ticket: TICKET, hash: HASH }]);
  await page.waitForFunction(async (n) => window.__MOCK__.callsOf("integration_run").filter((c) => c.verb === "pods").length > n, before);
  await gone(page);

  // `Logs` still launches a terminal without a card.
  await page.locator('.ig-row[data-i="0"] .ig-act', { hasText: "Logs" }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_launch").length === 1);
  assert.equal((await calls(page, "integration_prepare")).length, 1, "a read verb is not prepared");
  assert.deepEqual((await calls(page, "integration_launch"))[0], { tool: "kube", verb: "logs", pane: 2, args: { pod: "api-0" }, placement: "right" });
}
