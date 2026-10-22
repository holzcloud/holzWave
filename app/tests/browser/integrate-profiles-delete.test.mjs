// Deleting a profile and the note about syncing (design.md 6.27): the button arms on the first press and removes on the second.
import assert from "node:assert/strict";
import { PROD, STAGING, callsOf, countOf, openDialog, openPage } from "./integrate-profiles-lib.mjs";

const NOTE =
  "The profile file is synced between your computers. Secret values are kept in the keychain and synced inside the encrypted secrets. Paths of this computer are never part of a profile.";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await openPage(page, url, { profiles: [PROD, STAGING] });
  await openDialog(page);
  await page.waitForSelector("label.ip-row");

  // The note about syncing and the keychain is text under the groups.
  assert.equal(await page.locator("p.ip-sync").textContent(), NOTE);

  // There is nothing to delete in a new profile.
  const del = page.locator("button.ip-delete");
  assert.equal(await del.textContent(), "Delete");
  assert.equal(await del.isVisible(), true);
  await page.locator(".ip-add").click();
  assert.equal(await del.isHidden(), true);
  await page.locator("label.ip-row", { hasText: "Production EU" }).locator("input").check();
  assert.equal(await del.isVisible(), true);

  // The first press only asks: the words change, the border is danger, and nothing is called.
  await del.click();
  assert.equal(await del.textContent(), "Delete?");
  assert.equal(await del.getAttribute("data-armed"), "true");
  assert.equal(await countOf(page, "integration_profile_remove"), 0);
  const border = await del.evaluate((n) => getComputedStyle(n).borderTopColor);
  const danger = await page.evaluate(() => {
    const probe = document.createElement("span");
    probe.style.color = "var(--hw-danger)";
    document.body.append(probe);
    const c = getComputedStyle(probe).color;
    probe.remove();
    return c;
  });
  assert.equal(border, danger);
  const line = await del.evaluate((n) => getComputedStyle(n).textDecorationLine);
  assert.match(line, /underline/, "the question is underlined as well");

  // Any other press takes it back.
  await page.locator("#ip-name").click();
  assert.equal(await del.textContent(), "Delete");
  await del.click();
  assert.equal(await del.textContent(), "Delete?");
  // A key typed in a field takes it back too.
  await page.locator("#ip-name").press("End");
  assert.equal(await del.textContent(), "Delete");

  // Escape takes it back and does not close the dialog; the next Escape closes it.
  await del.click();
  assert.equal(await del.textContent(), "Delete?");
  await page.keyboard.press("Escape");
  assert.equal(await del.textContent(), "Delete");
  assert.equal(await page.locator(".dlg.ip-box").count(), 1);
  assert.equal(await countOf(page, "integration_profile_remove"), 0);

  // A refusal of the core is its sentence; nothing is removed and the question is over.
  await page.evaluate(() => {
    window.__PF__.removeFault = { code: "profile", reason: "keychain", text: "profile prod-eu: the keychain could not be used" };
  });
  await del.click();
  await del.click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_profile_remove").length === 1);
  await page.waitForFunction(() => !document.querySelector("#ip-error")?.hidden);
  assert.equal(await page.locator("#ip-error").textContent(), "profile prod-eu: the keychain could not be used");
  assert.equal(await del.textContent(), "Delete");
  assert.equal(await page.locator("label.ip-row").count(), 2);
  await page.evaluate(() => {
    window.__PF__.removeFault = null;
  });

  // The second press removes it: one call with the id, the list from the answer, an empty form.
  await del.click();
  assert.equal(await del.textContent(), "Delete?");
  await del.click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_profile_remove").length === 2);
  assert.deepEqual((await callsOf(page, "integration_profile_remove"))[1], { id: "prod-eu" });
  await page.waitForFunction(() => document.querySelectorAll("label.ip-row").length === 1);
  assert.deepEqual(await page.locator("label.ip-row .ip-name").allTextContents(), ["Staging"]);
  assert.equal(await page.locator("fieldset.ip-form > legend").textContent(), "New profile");
  assert.equal(await page.locator("#ip-name").inputValue(), "");
  assert.equal(await page.locator("label.ip-row[data-chosen=true]").count(), 0);
  assert.equal(await page.locator(".ip-status").textContent(), "Deleted.");
  assert.equal(await del.isHidden(), true);
  assert.equal(await countOf(page, "integration_profiles"), 1, "the answer of the removal is the list");
  assert.equal(await page.evaluate(() => document.querySelector(".dlg.ip-box").contains(document.activeElement)), true, "the keyboard stays in the dialog");

  // No secret value was asked for or sent by a removal; the core removes its own.
  assert.equal(await countOf(page, "integration_secret_set"), 0);
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.ip-box", { state: "detached" });
  assert.deepEqual(errors, [], "the page reported errors");
}
