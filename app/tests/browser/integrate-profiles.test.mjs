// The profiles dialog (design.md 6.27), first slice: it opens from the `Profiles…` button in the head of the Context section,
// reads the core's list once, shows every profile as text with the word `production` where the core says so, shows the core's
// notes as dim lines, takes the keyboard while it is open and gives it back to the terminal on Escape.
import assert from "node:assert/strict";
import { AZURE_PROFILE, HOSTILE, PROD, STAGING, callsOf, countOf, openDialog, openPage, profile } from "./integrate-profiles-lib.mjs";

const NOTE = "integrations.json was not used: it has a member or a value that is not allowed (line 3, column 9)";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await listCase(page, url);
  await hostileCase(page, url);
  await keysCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

async function listCase(page, url) {
  await openPage(page, url, { profiles: [PROD, STAGING, AZURE_PROFILE], notes: [NOTE] });

  // The button sits in the head of the Context section, before `↻`, and nothing is read before it is pressed.
  const button = page.locator(".cx-section .sb-head .cx-profiles");
  assert.equal(await button.textContent(), "Profiles…");
  assert.equal(await page.locator(".cx-section .sb-head .cx-profiles + .cx-refresh, .cx-section .sb-head .cx-head-acts > .cx-profiles + .cx-refresh").count(), 1);
  assert.equal(await countOf(page, "integration_profiles"), 0, "the list is read when the dialog opens, not before");

  await button.click();
  await page.waitForSelector(".dlg.ip-box");
  assert.equal(await page.locator(".dlg.ip-box").getAttribute("role"), "dialog");
  assert.equal(await page.locator(".dlg.ip-box").getAttribute("aria-modal"), "true");
  const labelled = await page.locator(".dlg.ip-box").getAttribute("aria-labelledby");
  assert.equal(await page.locator(`#${labelled}`).textContent(), "Integration profiles");
  await page.waitForSelector("label.ip-row");
  assert.equal(await countOf(page, "integration_profiles"), 1);
  await page.waitForFunction(() => document.querySelector(".dlg.ip-box")?.contains(document.activeElement));

  // The group `Profiles`: a radio group, one row for each profile, the core's order.
  assert.equal(await page.locator("fieldset.ip-list > legend").textContent(), "Profiles");
  assert.equal(await page.locator(".ip-rows").getAttribute("role"), "radiogroup");
  const rows = page.locator("label.ip-row");
  assert.equal(await rows.count(), 3);
  assert.deepEqual(await rows.locator(".ip-name").allTextContents(), ["Production EU", "Staging", "Holz Azure"]);
  assert.deepEqual(await rows.locator(".ip-tool").allTextContents(), ["Kubernetes", "Kubernetes", "Azure"]);
  assert.deepEqual(await rows.locator(".ip-sel").allTextContents(), ["prod-eu · payments", "stg · default", "00000000-0000-0000-0000-000000000000"]);

  // `production` is a word with an underline and the mark `!`, only where the core's flag is set.
  assert.deepEqual(await rows.locator(".ip-prod").allTextContents(), ["production"]);
  const style = await rows.first().locator(".ip-prod").evaluate((n) => ({
    line: getComputedStyle(n).textDecorationLine,
    mark: getComputedStyle(n, "::before").content,
  }));
  assert.match(style.line, /underline/);
  assert.match(style.mark, /!/);
  assert.equal(await rows.nth(1).locator(".ip-prod").count(), 0);

  // The notes of the core are lines above the groups.
  assert.deepEqual(await page.locator("ul.ip-notes li").allTextContents(), [NOTE]);
  assert.equal(await page.locator("ul.ip-notes").isVisible(), true);

  // The first profile is the chosen row; the keyboard is on it.
  assert.equal(await rows.first().locator('input[type="radio"]').isChecked(), true);
  assert.equal(await rows.first().getAttribute("data-chosen"), "true");
  assert.equal(await rows.nth(1).getAttribute("data-chosen"), "false");

  // One dialog at a time; opening it did not call anything that changes something.
  await page.evaluate(() => document.querySelector(".cx-profiles").click());
  assert.equal(await page.locator(".dlg.ip-box").count(), 1);
  assert.equal(await countOf(page, "integration_profiles"), 1);
  for (const cmd of ["integration_profile_save", "integration_profile_remove", "integration_secret_set", "integration_secret_clear"]) {
    assert.equal(await countOf(page, cmd), 0, cmd);
  }

  // Escape closes it and the terminal has the keyboard back.
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.ip-box", { state: "detached" });
  await page.waitForFunction(() => document.activeElement?.id === "ime");

  // Opened again it reads the list again (a profile may have changed in another window).
  await openDialog(page);
  assert.equal(await countOf(page, "integration_profiles"), 2);
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.ip-box", { state: "detached" });
}

async function hostileCase(page, url) {
  await page.reload();
  const odd = profile({ id: "odd", tool: "<b>nope</b>", name: HOSTILE, context: HOSTILE, namespace: HOSTILE });
  await page.evaluate(([p]) => {
    window.__PF__.profiles = [p];
  }, [odd]);
  await page.waitForSelector(".cx-row");
  await openDialog(page);
  await page.waitForSelector("label.ip-row");

  // A name that looks like markup is those characters, and no element is made from it.
  assert.equal(await page.locator("label.ip-row .ip-name").textContent(), HOSTILE);
  assert.equal(await page.locator("label.ip-row .ip-sel").textContent(), `${HOSTILE} · ${HOSTILE}`);
  assert.equal(await page.locator("label.ip-row .ip-tool").textContent(), "<b>nope</b>", "a tool word the page does not know is text");
  assert.equal(await page.locator(".dlg.ip-box img").count(), 0);
  assert.equal(await page.locator(".dlg.ip-box b").count(), 0);
  assert.equal(await page.locator("[onerror]").count(), 0);
  await page.keyboard.press("Escape");
}

async function keysCase(page, url) {
  await page.reload();
  await page.evaluate(([a, b]) => {
    window.__PF__.profiles = [a, b];
  }, [PROD, STAGING]);
  await page.waitForSelector(".cx-row");
  await openDialog(page);
  await page.waitForSelector("label.ip-row");

  // Tab stays inside the box, forwards and backwards, and the terminal behind it gets nothing.
  const inside = () => page.evaluate(() => document.querySelector(".dlg.ip-box").contains(document.activeElement));
  const seen = new Set();
  for (let i = 0; i < 40; i += 1) {
    await page.keyboard.press("Tab");
    assert.equal(await inside(), true, `Tab ${i} left the dialog`);
    seen.add(await page.evaluate(() => document.activeElement.id || document.activeElement.className));
  }
  for (let i = 0; i < 40; i += 1) {
    await page.keyboard.press("Shift+Tab");
    assert.equal(await inside(), true, `Shift+Tab ${i} left the dialog`);
  }
  assert.ok(seen.size > 8, "the whole dialog can be reached with Tab");
  assert.equal(await countOf(page, "key"), 0, "no key reached the terminal");

  // The arrows move inside the list of profiles and choose the profile (a radio group).
  await page.locator("label.ip-row").first().locator("input").focus();
  await page.keyboard.press("ArrowDown");
  assert.equal(await page.locator("label.ip-row[data-chosen=true] .ip-name").textContent(), "Staging");
  assert.equal(await page.locator("#ip-name").inputValue(), "Staging");

  // Enter in a text field saves the profile in the form.
  await page.locator("#ip-name").fill("Staging two");
  await page.locator("#ip-name").press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_profile_save").length === 1);
  assert.equal((await callsOf(page, "integration_profile_save"))[0].profile.name, "Staging two");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.ip-box", { state: "detached" });
}
