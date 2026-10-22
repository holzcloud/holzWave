// Saving and refusing (design.md 6.27): the object a save sends, the core's sentence shown as it is, the status line and the
// question before a dialog with unsaved changes is closed.
import assert from "node:assert/strict";
import { CANARY, HOSTILE, PROD, STAGING, callsOf, countOf, openDialog, openPage } from "./integrate-profiles-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await saveCase(page, url);
  await refusalCase(page, url);
  await closeCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

async function saveCase(page, url) {
  await openPage(page, url, { profiles: [STAGING] });
  await openDialog(page);
  await page.waitForSelector("label.ip-row");
  await page.locator(".ip-add").click();

  await page.locator("#ip-name").fill("Production EU");
  await page.locator("#ip-field-context").fill("prod-eu");
  await page.locator("#ip-field-namespace").fill("payments");
  await page.locator("#ip-production").check();
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_profile_save").length === 1);

  // Exactly one argument, `profile`, with exactly the twelve members of the file's shape.
  const [call] = await callsOf(page, "integration_profile_save");
  assert.deepEqual(Object.keys(call), ["profile"]);
  assert.deepEqual(call.profile, {
    id: "production-eu", tool: "kube", name: "Production EU", context: "prod-eu", namespace: "payments", subscription: null, section: null,
    node: null, production: true, isolated: false, env: {}, secret_env: [],
  });

  // The list is the answer of the save, the saved profile is the chosen one, and the line says so.
  await page.waitForFunction(() => document.querySelectorAll("label.ip-row").length === 2);
  assert.equal(await countOf(page, "integration_profiles"), 1, "the answer of the save is the list: it is not asked for again");
  assert.equal(await page.locator("label.ip-row[data-chosen=true] .ip-name").textContent(), "Production EU");
  assert.equal(await page.locator("fieldset.ip-form > legend").textContent(), "Profile");
  await page.waitForFunction(() => document.querySelector(".ip-status")?.textContent === "Saved.");
  assert.equal(await page.locator(".ip-status").getAttribute("data-kind"), null);
  assert.equal(await page.locator("#ip-error").isHidden(), true);
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.ip-box", { state: "detached" });
}

async function refusalCase(page, url) {
  await page.reload();
  await page.evaluate(([p, fault]) => {
    window.__PF__.profiles = [p];
    window.__PF__.saveFaults = [fault];
  }, [PROD, { code: "profile", reason: "invalid", text: `Profile prod-eu: context ${HOSTILE} is not valid.` }]);
  await page.waitForSelector(".cx-row");
  await openDialog(page);
  await page.waitForSelector("label.ip-row");

  await page.locator("#ip-field-context").fill("bad context");
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("integration_profile_save").length === 1);

  // The core's sentence, as it is, under the field group: danger, underlined, the mark `!`, and as text.
  const line = page.locator("#ip-error");
  await page.waitForFunction(() => !document.querySelector("#ip-error")?.hidden);
  assert.equal(await line.textContent(), `Profile prod-eu: context ${HOSTILE} is not valid.`);
  const style = await line.evaluate((n) => ({ line: getComputedStyle(n).textDecorationLine, mark: getComputedStyle(n, "::before").content }));
  assert.match(style.line, /underline/);
  assert.match(style.mark, /!/);
  assert.equal(await line.getAttribute("role"), "alert");
  assert.equal(await page.locator(".dlg.ip-box img").count(), 0);
  assert.equal(await page.locator("[onerror]").count(), 0);

  // A refusal changes nothing: the form keeps what was typed, the list is not read again, and the line is not "Saved.".
  assert.equal(await page.locator("#ip-field-context").inputValue(), "bad context");
  assert.equal(await countOf(page, "integration_profiles"), 1);
  assert.notEqual(await page.locator(".ip-status").textContent(), "Saved.");
  assert.equal(await page.locator("#ip-field-context").getAttribute("aria-invalid"), null, "the page does not guess which field the sentence means");

  // An edit takes the sentence away; the next save works (the mock refuses once).
  await page.locator("#ip-field-context").fill("prod-eu");
  await page.waitForFunction(() => document.querySelector("#ip-error")?.hidden === true);
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => document.querySelector(".ip-status")?.textContent === "Saved.");
  assert.equal(await countOf(page, "integration_profile_save"), 2);

  // A refusal that is not in the core's shape (Tauri's own text) is shown as a sentence too, and holds no value.
  await page.evaluate(() => {
    window.__PF__.saveFaults = ["invalid args `profile` for command `integration_profile_save`"];
  });
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => !document.querySelector("#ip-error")?.hidden);
  assert.equal(await page.locator("#ip-error").textContent(), "invalid args `profile` for command `integration_profile_save`");
  assert.equal((await page.evaluate(() => document.body.innerText)).includes(CANARY), false);
  await page.keyboard.press("Escape");
}

async function closeCase(page, url) {
  await page.reload();
  await page.evaluate(([p]) => {
    window.__PF__.profiles = [p];
  }, [STAGING]);
  await page.waitForSelector(".cx-row");
  await openDialog(page);
  await page.waitForSelector("label.ip-row");

  // Nothing was edited: no word, and Escape closes at once.
  assert.equal(await page.locator(".ip-status").textContent(), "");

  // An edit says `unsaved` in the status line, in words and in the warn tone.
  await page.locator("#ip-name").fill("Staging EU");
  await page.waitForFunction(() => document.querySelector(".ip-status")?.textContent === "unsaved");
  assert.equal(await page.locator(".ip-status").getAttribute("data-kind"), "warn");
  const tone = await page.locator(".ip-status").evaluate((n) => getComputedStyle(n).color);
  const warn = await page.evaluate(() => {
    const probe = document.createElement("span");
    probe.style.color = "var(--hw-warn)";
    document.body.append(probe);
    const c = getComputedStyle(probe).color;
    probe.remove();
    return c;
  });
  assert.equal(tone, warn, "the word is in the warn colour");

  // Editing it back to what was saved takes the word away.
  await page.locator("#ip-name").fill("Staging");
  await page.waitForFunction(() => document.querySelector(".ip-status")?.textContent === "");
  await page.locator("#ip-name").fill("Staging EU");
  await page.waitForFunction(() => document.querySelector(".ip-status")?.textContent === "unsaved");

  // Escape asks first; nothing closes. The button says what the next press does.
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => document.querySelector(".aiset-close")?.textContent === "Discard changes?");
  assert.equal(await page.locator(".dlg.ip-box").count(), 1);
  assert.equal(await page.locator(".ip-status").textContent(), "Unsaved changes. Close again to discard them.");
  assert.equal(await page.locator(".aiset-close").getAttribute("data-armed"), "true");

  // Any other action takes the question back.
  await page.locator("#ip-name").press("End");
  await page.keyboard.type("x");
  await page.waitForFunction(() => document.querySelector(".aiset-close")?.textContent === "Close");
  assert.equal(await page.locator(".ip-status").textContent(), "unsaved");

  // The button asks the same way, and the second press closes without saving anything.
  await page.locator(".aiset-close").click();
  assert.equal(await page.locator(".aiset-close").textContent(), "Discard changes?");
  assert.equal(await page.locator(".dlg.ip-box").count(), 1);
  await page.locator(".aiset-close").click();
  await page.waitForSelector(".dlg.ip-box", { state: "detached" });
  await page.waitForFunction(() => document.activeElement?.id === "ime");
  assert.equal(await countOf(page, "integration_profile_save"), 0, "closing saved nothing");

  // A saved dialog closes without a question.
  await openDialog(page);
  await page.locator("#ip-name").fill("Staging EU");
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => document.querySelector(".ip-status")?.textContent === "Saved.");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.ip-box", { state: "detached" });
}
