// The plain environment variables of the profiles dialog (design.md 6.27): rows of a name and a value, the core's refusal on the
// row it names, and no rule of the core in the page.
import assert from "node:assert/strict";
import { HOSTILE, PROD, STAGING, callsOf, countOf, openDialog, openPage, profile } from "./integrate-profiles-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await refusalCase(page, url);
  await rowsCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const names = (page) => page.locator(".ip-env-row .ip-env-name").evaluateAll((l) => l.map((n) => n.value));
const values = (page) => page.locator(".ip-env-row .ip-env-value").evaluateAll((l) => l.map((n) => n.value));

async function refusalCase(page, url) {
  await openPage(page, url, { profiles: [PROD, STAGING] });
  await openDialog(page);
  await page.waitForSelector("label.ip-row");

  // The group, and the variable of the profile as a row.
  assert.equal(await page.locator("fieldset.ip-env > legend").textContent(), "Environment");
  assert.deepEqual(await names(page), ["AWS_PROFILE"]);
  assert.deepEqual(await values(page), ["prod"]);
  assert.equal(await page.locator(".ip-env-row .ip-env-value").getAttribute("aria-label"), "Value of AWS_PROFILE");
  assert.equal(await page.locator(".ip-env-row .ip-env-name").getAttribute("aria-label"), "Name");
  assert.equal(await page.locator(".ip-env-row .ip-env-name").getAttribute("maxlength"), "64");
  assert.equal(await page.locator(".ip-env-row .ip-env-value").getAttribute("maxlength"), "1024");
  assert.equal(await page.locator(".ip-env-row button").textContent(), "Remove");

  // `Add` makes an empty row and puts the keyboard in its name.
  await page.locator(".ip-env-add").click();
  assert.deepEqual(await names(page), ["AWS_PROFILE", ""]);
  await page.waitForFunction(() => document.activeElement === document.querySelectorAll(".ip-env-name")[1]);
  assert.equal(await page.locator(".ip-status").textContent(), "", "a row that is still empty is no change");
  await page.locator(".ip-env-name").nth(1).fill("LD_PRELOAD");
  await page.locator(".ip-env-value").nth(1).fill("x");
  assert.equal(await page.locator(".ip-status").textContent(), "unsaved");
  assert.equal(await page.locator(".ip-env-value").nth(1).getAttribute("aria-label"), "Value of LD_PRELOAD", "the label follows the name");

  // The page applies no rule: the row goes to the core as typed, and the core's sentence comes back onto that row.
  await page.evaluate(() => {
    window.__PF__.saveFaults = [{ code: "profile", reason: "invalid", text: "profile prod-eu: env LD_PRELOAD is not allowed" }];
  });
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_profile_save").length === 1);
  assert.deepEqual((await callsOf(page, "integration_profile_save"))[0].profile.env, { AWS_PROFILE: "prod", LD_PRELOAD: "x" });
  const rowLine = page.locator(".ip-env-row").nth(1).locator(".ip-row-error");
  await page.waitForFunction(() => !document.querySelectorAll(".ip-env-row .ip-row-error")[1]?.hidden);
  assert.equal(await rowLine.textContent(), "profile prod-eu: env LD_PRELOAD is not allowed");
  assert.equal(await page.locator(".ip-env-row").first().locator(".ip-row-error").isHidden(), true, "the other row has no sentence");
  assert.equal(await page.locator("#ip-error").isHidden(), true, "the sentence is on the row, not under the profile");
  assert.equal(await page.locator(".ip-env-name").nth(1).getAttribute("aria-invalid"), "true");
  const style = await rowLine.evaluate((n) => ({ line: getComputedStyle(n).textDecorationLine, mark: getComputedStyle(n, "::before").content }));
  assert.match(style.line, /underline/);
  assert.match(style.mark, /!/);
  assert.equal(await rowLine.getAttribute("role"), "alert");
  assert.deepEqual(await values(page), ["prod", "x"], "nothing was changed by the refusal");
  assert.equal(await countOf(page, "integration_profiles"), 1);

  // Editing the row takes its sentence away.
  await page.locator(".ip-env-value").nth(1).fill("y");
  await page.waitForFunction(() => document.querySelectorAll(".ip-env-row .ip-row-error")[1]?.hidden === true);
  assert.equal(await page.locator(".ip-env-name").nth(1).getAttribute("aria-invalid"), null);

  // A sentence that names no row is the line under the profile.
  await page.evaluate(() => {
    window.__PF__.saveFaults = [{ code: "profile", reason: "invalid", text: "profile prod-eu: too many variables" }];
  });
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => !document.querySelector("#ip-error")?.hidden);
  assert.equal(await page.locator("#ip-error").textContent(), "profile prod-eu: too many variables");
  assert.equal(await page.locator(".ip-env-row .ip-row-error:not([hidden])").count(), 0);

  // A name that two rows use is found in the page, since an object cannot hold both; nothing is sent.
  await page.locator(".ip-env-add").click();
  await page.locator(".ip-env-name").nth(2).fill("AWS_PROFILE");
  const before = await countOf(page, "integration_profile_save");
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => document.querySelector("#ip-error")?.textContent === "Two rows use the name AWS_PROFILE.");
  assert.equal(await countOf(page, "integration_profile_save"), before);
  await page.keyboard.press("Escape");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.ip-box", { state: "detached" });
}

async function rowsCase(page, url) {
  await page.reload();
  await page.evaluate(([p]) => {
    window.__PF__.profiles = [p];
  }, [profile({ id: "odd", name: "Odd", env: { [HOSTILE]: HOSTILE, A: "1" }, production: false })]);
  await page.waitForSelector(".cx-row");
  await openDialog(page);
  await page.waitForSelector("label.ip-row");

  // Names and values are the text of fields: markup stays those characters.
  assert.deepEqual(await names(page), [HOSTILE, "A"]);
  assert.deepEqual(await values(page), [HOSTILE, "1"]);
  assert.equal(await page.locator(".dlg.ip-box img").count(), 0);
  assert.equal(await page.locator("[onerror]").count(), 0);

  // Remove takes a row out of the next save; a blank row added and left alone is not sent.
  await page.locator(".ip-env-row").first().locator("button").click();
  assert.deepEqual(await names(page), ["A"]);
  await page.locator(".ip-env-add").click();
  assert.equal(await page.locator(".ip-env-row").count(), 2);
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_profile_save").length === 1);
  const sent = (await callsOf(page, "integration_profile_save"))[0].profile;
  assert.deepEqual(sent.env, { A: "1" });
  await page.waitForFunction(() => document.querySelector(".ip-status")?.textContent === "Saved.");
  assert.deepEqual(await names(page), ["A"], "the rows are the saved profile's");

  // At most 32 rows.
  for (let i = 0; i < 31; i += 1) await page.locator(".ip-env-add").click();
  assert.equal(await page.locator(".ip-env-row").count(), 32);
  assert.equal(await page.locator(".ip-env-add").isDisabled(), true);
  await page.keyboard.press("Escape");
  await page.keyboard.press("Escape");
}
