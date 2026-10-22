// Who may press the card (docs/design.md section 6.26): only a real press counts. A press within half a second of the card
// appearing, a click a script made and a key a script made are ignored, Deny works at any time for a real press, and Escape with the
// focus inside the card denies and hands the keyboard back to the terminal. The card never takes the focus when it appears.
import assert from "node:assert/strict";
import { HASH, TICKET, allow, calls, cardNode, gone, modeOf, openConfirm, outcome, show, wait } from "./integrate-confirm-lib.mjs";

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(5000);
  await an_early_scripted_or_keyboard_synthetic_press_is_ignored(page, url);
  await escape_denies_and_returns_the_focus(page, url, engine);
  assert.deepEqual(errors, [], "the page reported errors");
}

async function an_early_scripted_or_keyboard_synthetic_press_is_ignored(page, url) {
  await openConfirm(page, url);
  await page.waitForSelector(".block-integration .ig-table tbody tr");
  await show(page);
  const card = cardNode(page);

  // A real press at once (inside the half second) is ignored.
  await card.locator(".ic-yes").click({ force: true });
  assert.equal(await modeOf(page), "ask");
  assert.equal((await calls(page, "integration_confirm")).length, 0, "an early press sends nothing");

  // Scripted input after the half second is ignored: a click, a mouse event, a key on the button and Escape on the card.
  await wait(page, 600);
  await page.evaluate(() => {
    const yes = document.querySelector(".ic-yes");
    yes.click();
    yes.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    yes.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    yes.dispatchEvent(new KeyboardEvent("keyup", { key: " ", bubbles: true }));
    document.querySelector(".ic-no").click();
    document.querySelector("section.ic").dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
  });
  await wait(page, 100);
  assert.equal(await cardNode(page).count(), 1, "a script cannot deny either");
  assert.equal(await modeOf(page), "ask");
  assert.equal((await calls(page, "integration_confirm")).length, 0, "a script sent nothing");
  assert.equal((await calls(page, "integration_cancel")).length, 0);

  // The real press then works, and sends the ticket and the hash once.
  await card.locator(".ic-yes").click();
  assert.equal((await outcome(page)).kind, "done");
  assert.deepEqual(await calls(page, "integration_confirm"), [{ ticket: TICKET, hash: HASH }]);
}

async function escape_denies_and_returns_the_focus(page, url, engine) {
  await openConfirm(page, url);
  await page.waitForSelector(".block-integration .ig-table tbody tr");
  await page.evaluate(() => document.getElementById("term").focus());
  await page.waitForFunction(() => document.activeElement?.id === "ime");
  await show(page);
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime", "the card did not take the focus");

  // Escape with the focus in the terminal does nothing to the card.
  await page.keyboard.press("Escape");
  await wait(page, 100);
  assert.equal(await cardNode(page).count(), 1);
  assert.equal((await calls(page, "integration_cancel")).length, 0);

  // Deny works within the half second for a real press.
  await cardNode(page).locator(".ic-no").click({ force: true });
  await gone(page);
  assert.deepEqual(await calls(page, "integration_cancel"), [{ ticket: TICKET }]);
  assert.equal((await outcome(page)).kind, "denied");

  // Tab reaches the argument box, Allow once and Deny in this order; Escape there denies and the terminal has the keyboard again.
  await show(page);
  // The tab order is the document order: the argument box, Allow once, Deny. WebKit on a Mac does not tab to buttons by default, so the
  // order is read from the document there and walked with the key only in Chromium.
  const stops = await page.evaluate(() => Array.from(document.querySelectorAll("section.ic button, section.ic [tabindex]")).map((n) => n.className.split(" ")[0]));
  assert.deepEqual(stops, ["ic-args", "ic-yes", "ic-no"]);
  if (engine === "chromium") {
    await page.evaluate(() => document.querySelector("section.ic .ic-args").focus());
    const order = [];
    for (let i = 0; i < 3; i++) {
      order.push(await page.evaluate(() => document.activeElement.className.split(" ")[0]));
      await page.keyboard.press("Tab");
    }
    assert.deepEqual(order, ["ic-args", "ic-yes", "ic-no"]);
  }
  await page.evaluate(() => document.querySelector("section.ic .ic-yes").focus());
  await page.keyboard.press("Escape");
  await gone(page);
  assert.equal((await calls(page, "integration_cancel")).length, 2);
  assert.equal((await calls(page, "integration_confirm")).length, 0);
  await page.waitForFunction(() => document.activeElement?.id === "ime");

  // The status line said that a decision waits, and the palette had the entry while the card did.
  await show(page);
  const listed = () => page.evaluate(async () => (await import("./actions.js")).listActions().some((a) => a.id === "integration.card_focus"));
  assert.equal(await listed(), true);
  assert.equal(await page.locator("#status .notice").textContent(), "waiting for your decision");
  await page.evaluate(async () => (await import("./actions.js")).runAction("integration.card_focus"));
  await page.waitForFunction(() => document.activeElement?.classList.contains("ic-no"));
  await allow(page);
  await outcome(page);
  await page.waitForFunction(() => document.querySelector("section.ic") === null);
  assert.equal(await listed(), false, "the entry is gone with the card");
}
