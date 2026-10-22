// The secret variables of the profiles dialog (design.md 6.27): write-only rows. A value is typed once, goes out in one call, and is
// in no field, text, attribute, storage or later call of the page.
import assert from "node:assert/strict";
import { CANARY, HOSTILE, PROD, STAGING, callsOf, countOf, openDialog, openPage, pageText, profile } from "./integrate-profiles-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await writeOnlyCase(page, url);
  await savedFirstCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const TWO = profile({
  env: { AWS_PROFILE: "prod" },
  secret_env: [{ name: "AWS_SECRET_ACCESS_KEY", has_value: true }, { name: "AWS_SESSION_TOKEN", has_value: false }],
});

const rowOf = (page, name) => page.locator(`.ip-secret[data-name="${name}"]`);

async function writeOnlyCase(page, url) {
  const consoleText = [];
  page.on("console", (m) => consoleText.push(m.text()));
  await openPage(page, url, { profiles: [TWO, STAGING], setDelay: 400 });
  await openDialog(page);
  await page.waitForSelector("label.ip-row");

  // The group, its rows, the state words from the core and the fields that are empty on every open.
  assert.equal(await page.locator("fieldset.ip-secrets > legend").textContent(), "Secret variables");
  assert.deepEqual(await page.locator(".ip-secret .ip-vname").allTextContents(), ["AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN"]);
  assert.deepEqual(await page.locator(".ip-secret .ip-state").allTextContents(), ["saved", "not set"]);
  const field = rowOf(page, "AWS_SESSION_TOKEN").locator("input");
  assert.equal(await field.getAttribute("type"), "password");
  assert.equal(await field.getAttribute("autocomplete"), "off");
  assert.equal(await field.getAttribute("maxlength"), "1024");
  assert.equal(await field.getAttribute("aria-label"), "Value of AWS_SESSION_TOKEN");
  assert.equal(await field.inputValue(), "");
  assert.equal(await rowOf(page, "AWS_SECRET_ACCESS_KEY").locator("input").inputValue(), "", "a saved secret is not shown");
  assert.deepEqual(await rowOf(page, "AWS_SESSION_TOKEN").locator("button").allTextContents(), ["Set", "Clear", "Remove"]);
  assert.equal(await page.locator(".ip-secrets .aiset-hint").last().textContent(), "Saved values are never shown. Type a new one to replace it.");

  // `Set` waits for text; `Clear` waits for a value.
  const set = rowOf(page, "AWS_SESSION_TOKEN").locator("button.ip-set");
  const clear = rowOf(page, "AWS_SESSION_TOKEN").locator("button.ip-clear");
  assert.equal(await set.isDisabled(), true);
  assert.equal(await clear.isDisabled(), true, "there is nothing to clear");
  assert.equal(await rowOf(page, "AWS_SECRET_ACCESS_KEY").locator("button.ip-clear").isDisabled(), false);
  await field.fill(CANARY);
  assert.equal(await set.isDisabled(), false);
  assert.equal(await page.locator(".ip-status").textContent(), "", "a value typed is no unsaved change");

  // Set: the field is empty before the core has answered, and the call carries the value once.
  await set.click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_secret_set").length === 1);
  assert.equal(await field.inputValue(), "", "the field was emptied before the answer");
  assert.equal(await page.locator(".ip-secret[data-name=AWS_SESSION_TOKEN] .ip-state").textContent(), "not set", "still what the core said");
  assert.equal((await pageText(page)).includes(CANARY), false, "the value is in no text, attribute or field while the call is out");
  await page.waitForFunction(() => document.querySelector('.ip-secret[data-name=AWS_SESSION_TOKEN] .ip-state')?.textContent === "saved");
  assert.deepEqual((await callsOf(page, "integration_secret_set"))[0], { profile: "prod-eu", name: "AWS_SESSION_TOKEN", value: CANARY });
  assert.equal(await field.inputValue(), "");
  assert.equal(await set.isDisabled(), true);
  assert.equal(await clear.isDisabled(), false);

  // The value is anywhere else: nowhere.
  assert.equal((await pageText(page)).includes(CANARY), false, "not in the page");
  const everyCall = await page.evaluate(() => {
    const out = [];
    for (const cmd of ["integration_profiles", "integration_profile_save", "integration_profile_remove", "integration_secret_clear", "integration_tools", "integration_context"]) {
      out.push(JSON.stringify(window.__MOCK__.callsOf(cmd)));
    }
    return out.join("\n");
  });
  assert.equal(everyCall.includes(CANARY), false, "not in any other call");
  const stored = await page.evaluate(() => JSON.stringify([{ ...localStorage }, { ...sessionStorage }]));
  assert.equal(stored.includes(CANARY), false, "not in storage");
  assert.equal(consoleText.some((t) => t.includes(CANARY)), false, "not in the console");
  assert.equal(await countOf(page, "integration_secret_set"), 1);

  // Enter in the field of a row is `Set` for that row, and Enter anywhere else in the row does not save the profile.
  await field.fill(`${CANARY}-2`);
  await field.press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_secret_set").length === 2);
  assert.equal((await callsOf(page, "integration_secret_set"))[1].value, `${CANARY}-2`);
  assert.equal(await countOf(page, "integration_profile_save"), 0);
  await page.waitForFunction(() => document.querySelector(".ip-secret[data-name=AWS_SESSION_TOKEN] input").value === "");

  // Clear: the call has the profile and the name and nothing else, and the word follows.
  await clear.click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_secret_clear").length === 1);
  assert.deepEqual((await callsOf(page, "integration_secret_clear"))[0], { profile: "prod-eu", name: "AWS_SESSION_TOKEN" });
  await page.waitForFunction(() => document.querySelector('.ip-secret[data-name=AWS_SESSION_TOKEN] .ip-state')?.textContent === "not set");
  assert.equal(await clear.isDisabled(), true);

  // A refusal of the core is its sentence on the row; the word stays what it was and the field is empty.
  await page.evaluate(() => {
    window.__PF__.setFault = { code: "profile", reason: "secret", text: "The keychain could not be used." };
  });
  await field.fill(CANARY);
  await set.click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_secret_set").length === 3);
  const rowError = rowOf(page, "AWS_SESSION_TOKEN").locator(".ip-row-error");
  await page.waitForFunction(() => !document.querySelector(".ip-secret[data-name=AWS_SESSION_TOKEN] .ip-row-error")?.hidden);
  assert.equal(await rowError.textContent(), "The keychain could not be used.");
  assert.equal(await rowError.getAttribute("role"), "alert");
  assert.equal(await field.inputValue(), "");
  assert.equal(await page.locator(".ip-secret[data-name=AWS_SESSION_TOKEN] .ip-state").textContent(), "not set");
  assert.equal((await pageText(page)).includes(CANARY), false, "a refused value is not kept either");

  // Closing and opening again: the fields are empty, the words are the core's.
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.ip-box", { state: "detached" });
  await openDialog(page);
  await page.waitForSelector(".ip-secret");
  assert.deepEqual(await page.locator(".ip-secret input").evaluateAll((l) => l.map((n) => n.value)), ["", ""]);
  assert.deepEqual(await page.locator(".ip-secret .ip-state").allTextContents(), ["saved", "not set"]);
  assert.equal((await pageText(page)).includes(CANARY), false);
  await page.keyboard.press("Escape");
}

async function savedFirstCase(page, url) {
  await page.reload();
  await page.evaluate(([p]) => {
    window.__PF__.profiles = [p];
    window.__PF__.setDelay = 0;
  }, [STAGING]);
  await page.waitForSelector(".cx-row");
  await openDialog(page);
  await page.waitForSelector("label.ip-row");

  // A name is added in a field of its own; the row says it is not saved, and `Set` waits for the save.
  const add = page.locator(".ip-secret-add");
  const nameField = page.locator("#ip-secret-name");
  assert.equal(await nameField.getAttribute("maxlength"), "64");
  await nameField.fill("MY_TOKEN");
  await add.click();
  const row = rowOf(page, "MY_TOKEN");
  assert.equal(await row.locator(".ip-state").textContent(), "not set");
  assert.equal(await nameField.inputValue(), "", "the name field is empty again");
  assert.equal(await page.locator(".ip-status").textContent(), "unsaved", "a name added is a change of the profile");
  assert.equal(await row.locator(".ip-secret-hint").textContent(), "Save the profile first.");
  await row.locator("input").fill(CANARY);
  assert.equal(await row.locator("button.ip-set").isDisabled(), true, "the core refuses a value for a name the profile does not declare");
  assert.equal(await countOf(page, "integration_secret_set"), 0);

  // A name added with Enter in the name field (and not a save).
  await nameField.fill("OTHER");
  await nameField.press("Enter");
  assert.equal(await rowOf(page, "OTHER").count(), 1);
  assert.equal(await countOf(page, "integration_profile_save"), 0);

  // After the save the rows are the core's, and the value typed before is gone; the new row can be set.
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => document.querySelector(".ip-status")?.textContent === "Saved.");
  assert.deepEqual((await callsOf(page, "integration_profile_save"))[0].profile.secret_env, ["MY_TOKEN", "OTHER"]);
  assert.equal(await row.locator("input").inputValue(), "", "a value typed before the save was not kept");
  assert.equal(await row.locator(".ip-secret-hint").count(), 0, "a saved name has no such hint");
  await row.locator("input").fill(CANARY);
  assert.equal(await row.locator("button.ip-set").isDisabled(), false);
  await row.locator("button.ip-set").click();
  await page.waitForFunction(() => document.querySelector('.ip-secret[data-name=MY_TOKEN] .ip-state')?.textContent === "saved");
  assert.deepEqual((await callsOf(page, "integration_secret_set"))[0], { profile: "staging", name: "MY_TOKEN", value: CANARY });

  // Remove takes the name out of the profile at the next save, and wipes the value first: the clear comes before the save.
  await rowOf(page, "MY_TOKEN").locator("button.ip-remove").click();
  assert.equal(await rowOf(page, "MY_TOKEN").count(), 0);
  assert.equal(await page.locator(".ip-status").textContent(), "unsaved");
  assert.equal(await countOf(page, "integration_secret_clear"), 0, "nothing is deleted before the save");
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => document.querySelector(".ip-status")?.textContent === "Saved.");
  const order = await page.evaluate(() => {
    const clearAt = window.__MOCK__.callsOf("integration_secret_clear").length;
    return { clears: window.__MOCK__.callsOf("integration_secret_clear"), saves: window.__MOCK__.callsOf("integration_profile_save").length, clearAt };
  });
  assert.deepEqual(order.clears, [{ profile: "staging", name: "MY_TOKEN" }], "the value of the name that left the profile is cleared");
  assert.deepEqual((await callsOf(page, "integration_profile_save"))[1].profile.secret_env, ["OTHER"]);
  assert.equal(await page.evaluate(() => window.__PF__.values["staging:MY_TOKEN"] === undefined), true, "the core has no value for it any more");
  assert.deepEqual(await page.locator(".ip-secret .ip-vname").allTextContents(), ["OTHER"]);

  // A name that was only added and removed again never reaches the core.
  await nameField.fill("GONE");
  await add.click();
  await rowOf(page, "GONE").locator("button.ip-remove").click();
  assert.equal(await page.locator(".ip-status").textContent(), "", "back to what was saved");
  assert.equal(await countOf(page, "integration_secret_clear"), 1);

  // At most eight names; the page shows the bound and the core says the rest.
  for (let i = 0; i < 7; i += 1) {
    await nameField.fill(`N${i}`);
    await add.click();
  }
  assert.equal(await page.locator(".ip-secret").count(), 8);
  assert.equal(await add.isDisabled(), true);
  // The names are text.
  await page.keyboard.press("Escape");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.ip-box", { state: "detached" });
  await page.evaluate(([h]) => {
    window.__PF__.profiles = [{ ...window.__PF__.profiles[0], secret_env: [{ name: h, has_value: true }] }];
  }, [HOSTILE]);
  await openDialog(page);
  assert.equal(await page.locator(".ip-secret .ip-vname").textContent(), HOSTILE);
  assert.equal(await page.locator(".dlg.ip-box img").count(), 0);
  assert.equal(await page.locator("[onerror]").count(), 0);
  await page.keyboard.press("Escape");
}
