// Outcomes of a card (docs/design.md section 6.26), part two: a ticket the core no longer knows or that ran out ends the card, an Exec
// verb opens a pane and the page applies the returned layout, and a second card waits behind the first.
import assert from "node:assert/strict";
import { OPENED, TICKET, allow, calls, cardNode, cardOf, gone, modeOf, openConfirm, outcome, request, show, textOf, ticketFault } from "./integrate-confirm-lib.mjs";
import { LAUNCHED } from "./integrate-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await an_expired_ticket_ends_the_card(page, url);
  await an_exec_confirm_opens_a_pane_and_two_cards_queue(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const waitBlock = (page) => page.waitForSelector(".block-integration .ig-table tbody tr");

async function an_expired_ticket_ends_the_card(page, url) {
  // The core says the ticket is not known any more.
  await openConfirm(page, url, { steps: [{ fault: ticketFault("expired") }] });
  await waitBlock(page);
  await show(page);
  await allow(page);
  await page.waitForFunction(() => document.querySelector("section.ic")?.getAttribute("data-mode") === "ended");
  assert.equal(await textOf(page, ".ic-line"), "That action is no longer waiting.");
  assert.equal(await cardNode(page).locator("button").count(), 0, "no buttons");
  assert.equal((await outcome(page)).kind, "ended");
  assert.equal((await calls(page, "integration_prepare")).length, 1, "an ended card is not asked for again");

  await openConfirm(page, url, { steps: [{ fault: ticketFault("unknown") }] });
  await waitBlock(page);
  await show(page);
  await allow(page);
  await page.waitForFunction(() => document.querySelector("section.ic")?.getAttribute("data-mode") === "ended");

  // The page ends a card by itself when the time the core gave has gone; nothing is sent.
  await openConfirm(page, url, { cards: { "kube/delete_pod": cardOf({ expires_in_secs: 1 }) } });
  await waitBlock(page);
  await show(page);
  await page.waitForFunction(() => document.querySelector("section.ic")?.getAttribute("data-mode") === "ended");
  assert.equal(await textOf(page, ".ic-line"), "That action is no longer waiting.");
  assert.equal((await outcome(page)).kind, "ended");
  assert.equal((await calls(page, "integration_confirm")).length, 0);
  // The card leaves by itself a few seconds later.
  await gone(page);
}

async function an_exec_confirm_opens_a_pane_and_two_cards_queue(page, url) {
  const exec = cardOf({ verb: "exec", effect: "exec", argv: ["exec", "-i", "-t", "--", "api-0"], target: [["context", "dev"], ["pod", "api-0"]] });
  await openConfirm(page, url, {
    cards: { "kube/delete_pod": cardOf(), "kube/exec": exec },
    steps: [{ answer: OPENED(LAUNCHED, 3) }],
  });
  await waitBlock(page);
  await show(page, "kube", "exec", null, { pod: "api-0" });
  assert.equal(await textOf(page, ".ic-title"), "Kubernetes · Exec");
  assert.equal(await textOf(page, ".ic-effect"), "Opens a session with your credentials. Check the command below.");

  // A second card waits behind the first and is counted in the head.
  await request(page, "kube", "delete_pod", null, { pod: "api-0" });
  await page.waitForFunction(() => document.querySelector(".ic-count") !== null);
  assert.equal(await textOf(page, ".ic-count"), "1 of 2");
  assert.equal(await page.locator("section.ic").count(), 1, "one layer at a time");

  // Allow the first: an Exec verb opens a pane, the layout of the core is applied, and the second card is shown after the first leaves.
  await allow(page);
  await page.waitForFunction(() => document.querySelector(".ic-line")?.textContent === "Opened pane 3");
  assert.equal(await page.locator('.panehead[data-pane="3"]').count(), 1, "the new pane is drawn");
  await page.waitForFunction(() => document.querySelector(".ic-title")?.textContent === "Kubernetes · Delete pod", null, { timeout: 8000 });
  assert.equal(await page.locator(".ic-count").count(), 0, "alone again");
  assert.equal(await modeOf(page), "ask");
  // The second card has its own half second.
  await cardNode(page).locator(".ic-yes").click({ force: true });
  assert.equal((await calls(page, "integration_confirm")).length, 1);
  await cardNode(page).locator(".ic-no").click();
  await gone(page);
  assert.deepEqual((await calls(page, "integration_cancel")).at(-1), { ticket: TICKET });
}
