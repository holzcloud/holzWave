// Outcomes of a card (docs/design.md section 6.26), part one: a request that changed (the card is read again, with its half second and its
// two presses) and any other refusal of the core (kept above the buttons, the card unarmed again).
import assert from "node:assert/strict";
import { LAYOUT, PROD, TICKET, allow, calls, cardNode, cardOf, gone, modeOf, openConfirm, outcome, prodCard, request, show, textOf, ticketFault, wait } from "./integrate-confirm-lib.mjs";

const OTHER_TICKET = "fedcba9876543210fedcba9876543210";
const OTHER_HASH = "aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await a_changed_request_is_drawn_again(page, url);
  await other_refusals_keep_the_card(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const waitBlock = (page) => page.waitForSelector(".block-integration .ig-table tbody tr");

async function a_changed_request_is_drawn_again(page, url) {
  const second = prodCard({ ticket: OTHER_TICKET, hash: OTHER_HASH, target: [["context", "prod-eu"], ["namespace", "billing"], ["pod", "api-0"]] });
  await openConfirm(page, url, {
    cards: { "kube/delete_pod": [prodCard(), second] },
    steps: [{ fault: ticketFault("changed", "The request is not the one the card showed.") }, { answer: { kind: "ran", pane: null, note: "pod gone", view: null } }],
  });
  await waitBlock(page);
  await show(page);
  await allow(page);
  assert.equal(await modeOf(page), "armed");
  await allow(page);

  // The core refused the second press: the card is read again for the same action, drawn unarmed, with the sentence above the buttons.
  await page.waitForFunction((hash) => document.querySelector(".ic-hash")?.textContent === `hash ${hash}`, OTHER_HASH.slice(0, 12));
  assert.equal(await modeOf(page), "ask", "a fresh card is not armed");
  assert.equal(await textOf(page, ".ic-line"), "The request changed. Nothing ran. Read the card again.");
  assert.equal(await textOf(page, ".ic-yes"), "Allow once");
  const target = await cardNode(page).locator(".ic-target .ic-row").evaluateAll((rows) => rows.map((r) => r.children[1].textContent));
  assert.deepEqual(target, ["prod-eu", "billing", "api-0"], "the card shows the new request, not the old one");
  const prepared = await calls(page, "integration_prepare");
  assert.equal(prepared.length, 2, "one more prepare, for the same action");
  assert.deepEqual(prepared[1], prepared[0]);
  assert.deepEqual(await calls(page, "integration_cancel"), [{ ticket: TICKET }], "the old ticket is dropped");

  // The half second starts again, and a production card needs its two presses again.
  await cardNode(page).locator(".ic-yes").click({ force: true });
  assert.equal(await modeOf(page), "ask", "a press within half a second of the new card is ignored");
  assert.equal((await calls(page, "integration_confirm")).length, 1, "only the refused press so far");
  await allow(page);
  assert.equal(await modeOf(page), "armed");
  await allow(page);
  assert.equal((await outcome(page)).kind, "done");
  assert.deepEqual((await calls(page, "integration_confirm")).at(-1), { ticket: OTHER_TICKET, hash: OTHER_HASH });
  assert.equal(await textOf(page, ".ic-line"), "Done: pod gone");
}

async function other_refusals_keep_the_card(page, url) {
  await openConfirm(page, url, {
    cards: { "kube/delete_pod": prodCard() },
    steps: [{ fault: { code: "failed", reason: "", text: "kubectl could not reach the cluster." } }, { answer: { kind: "ran", pane: null, note: "ok", view: null } }],
  });
  await waitBlock(page);
  await show(page);
  await allow(page);
  await allow(page);
  await page.waitForFunction(() => document.querySelector(".ic-fault")?.hidden === false);
  assert.equal(await textOf(page, ".ic-fault"), "kubectl could not reach the cluster.");
  const fault = await cardNode(page).locator(".ic-fault").evaluate((n) => ({ line: getComputedStyle(n).textDecorationLine, mark: getComputedStyle(n, "::before").content }));
  assert.ok(fault.line.includes("underline") && fault.mark.includes("!"), "danger line with an underline and the mark");
  // Above the buttons in the document.
  assert.equal(await cardNode(page).evaluate((n) => n.querySelector(".ic-fault").compareDocumentPosition(n.querySelector(".ic-actions")) & Node.DOCUMENT_POSITION_FOLLOWING), 4);
  // The card is unarmed again: a new decision, two presses.
  assert.equal(await modeOf(page), "ask");
  assert.equal(await textOf(page, ".ic-yes"), "Allow once");
  await allow(page);
  assert.equal(await modeOf(page), "armed");
  assert.equal(await cardNode(page).locator(".ic-fault").isHidden(), true, "arming clears the sentence");
  await allow(page);
  assert.equal((await outcome(page)).kind, "done");
  assert.equal((await calls(page, "integration_confirm")).length, 2);

  // A prepare the core refuses shows no card and answers with the core's sentence.
  await openConfirm(page, url, { prepareFault: { "kube/delete_pod": { code: "refused", reason: "tool", text: "The tool is not known." } } });
  await waitBlock(page);
  await request(page);
  const refused = await outcome(page);
  assert.deepEqual(refused, { kind: "refused", fault: { code: "refused", reason: "tool", text: "The tool is not known." } });
  assert.equal(await cardNode(page).count(), 0);
}

