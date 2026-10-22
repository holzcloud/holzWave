// The card of an action in a production context (docs/design.md section 6.26): the guard line names the context in words, in the
// warn colour, underlined and marked; the first real press arms the card and sends nothing; the second sends the confirm; Deny,
// Escape or the card ending disarms it; and nothing is remembered, so the next card for the same action starts unarmed.
import assert from "node:assert/strict";
import { HASH, PROD, TICKET, allow, calls, cardNode, cardOf, gone, modeOf, openConfirm, outcome, prodCard, show, textOf, tokenColor, wait } from "./integrate-confirm-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await a_production_card_needs_a_second_press(page, url);
  await deny_escape_and_a_new_card_start_unarmed(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

async function a_production_card_needs_a_second_press(page, url) {
  await openConfirm(page, url, { cards: { "kube/delete_pod": prodCard() } });
  await page.waitForSelector(".block-integration .ig-table tbody tr");
  await show(page);
  const card = cardNode(page);

  // The guard line names the context in words; the warn colour, the underline and the mark are the cues.
  assert.equal(await textOf(page, ".ic-guard"), `Production context: ${PROD}.`);
  assert.equal(await card.getAttribute("data-production"), "true");
  const guard = await card.locator(".ic-guard").evaluate((n) => {
    const style = getComputedStyle(n);
    return { color: style.color, line: style.textDecorationLine, mark: getComputedStyle(n, "::before").content };
  });
  assert.equal(guard.color, await tokenColor(page, "--hw-warn"), "the warn colour");
  assert.ok(guard.line.includes("underline"), "underlined");
  assert.ok(guard.mark.includes("!"), "the mark");
  assert.equal(await modeOf(page), "ask");
  assert.equal(await textOf(page, ".ic-yes"), "Allow once");

  // The first press, after the half second, arms the card and sends nothing.
  await allow(page);
  assert.equal(await modeOf(page), "armed");
  assert.equal(await textOf(page, ".ic-yes"), "Press again to allow on production");
  assert.equal(await textOf(page, ".ic-line"), "Armed. The next press allows this on production. Nothing has run yet.");
  assert.equal(await card.locator(".ic-line").getAttribute("role"), "status", "the line is announced");
  assert.equal((await calls(page, "integration_confirm")).length, 0, "nothing was sent");
  const bar = await card.evaluate((n) => getComputedStyle(n).borderLeftColor);
  assert.equal(bar, await tokenColor(page, "--hw-danger"), "the bar is the danger colour");
  assert.equal(await card.evaluate((n) => getComputedStyle(n).borderLeftWidth), "2px");

  // A second press within half a second of the first (a double click) does not allow.
  await card.locator(".ic-yes").click();
  assert.equal(await modeOf(page), "armed", "a double click arms and cannot allow");
  assert.equal((await calls(page, "integration_confirm")).length, 0);

  // The second press, a while after the first, sends the confirm once, with the ticket and the hash.
  await allow(page);
  const done = await outcome(page);
  assert.equal(done.kind, "done");
  assert.deepEqual(await calls(page, "integration_confirm"), [{ ticket: TICKET, hash: HASH }]);
  assert.equal(await modeOf(page), "done");
}

async function deny_escape_and_a_new_card_start_unarmed(page, url) {
  await openConfirm(page, url, { cards: { "kube/delete_pod": prodCard() } });
  await page.waitForSelector(".block-integration .ig-table tbody tr");

  // Deny after arming cancels the ticket and removes the card; nothing was confirmed.
  await show(page);
  await allow(page);
  assert.equal(await modeOf(page), "armed");
  await cardNode(page).locator(".ic-no").click();
  await gone(page);
  assert.deepEqual(await calls(page, "integration_cancel"), [{ ticket: TICKET }]);
  assert.equal((await outcome(page)).kind, "denied");

  // The next card for the same action starts unarmed: nothing is remembered.
  await show(page);
  assert.equal(await modeOf(page), "ask");
  assert.equal(await textOf(page, ".ic-yes"), "Allow once");
  await allow(page);
  assert.equal(await modeOf(page), "armed");

  // Escape with the focus in the card disarms it by denying, and hands the keyboard back to the terminal.
  await page.evaluate(() => document.querySelector(".ic-yes").focus());
  await page.keyboard.press("Escape");
  await gone(page);
  assert.equal((await calls(page, "integration_cancel")).length, 2);
  assert.equal((await calls(page, "integration_confirm")).length, 0, "no production action was confirmed");
  await page.waitForFunction(() => document.activeElement?.id === "ime");

  // A card that ends while it is armed (its ticket runs out) is gone with its mode; the next one is unarmed again.
  await openConfirm(page, url, { cards: { "kube/delete_pod": prodCard({ expires_in_secs: 1 }) } });
  await page.waitForSelector(".block-integration .ig-table tbody tr");
  await show(page);
  await allow(page);
  await page.waitForFunction(() => document.querySelector("section.ic")?.getAttribute("data-mode") === "ended");
  assert.equal(await textOf(page, ".ic-line"), "That action is no longer waiting.");
  assert.equal(await cardNode(page).locator("button").count(), 0);
  assert.equal((await outcome(page)).kind, "ended");
  await wait(page, 10);
}
