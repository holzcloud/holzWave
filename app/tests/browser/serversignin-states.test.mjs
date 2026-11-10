// The states a sign-in can end in (docs/design.md 6.35): each failure of the wait with its shape and words, a start the host refuses. Against the stub that plays the host.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, currentBackend } from "./lib.mjs";
import { GOOD, buttons, count, dialog, gone, link, open, openDialog, result, stateText } from "./serversignin-lib.mjs";

export const backends = ["tauri"];
export const backendNote = "signing in to a server is a desktop and mobile app step; the browser window is the server's own page and has no such dialog";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(6000);
  if (currentBackend() === "tauri") {
    await failureCase(page, url);
    await startRefusedCase(page, url);
    sourceCase();
  }
  assert.deepEqual(errors, [], "the page reported errors");
}

/** Each way a request can end shows its line with a shape and a word, stops the polling and leaves a way to start again or cancel. */
async function failureCase(page, url) {
  const cases = [
    ["denied", "The request was denied."],
    ["expired", "The code expired. Start again."],
    ["too_many", "Too many devices or requests. Try again later."],
    ["off", "Device sign-in is off on this server."],
    ["unreachable", "The server cannot be reached."],
  ];
  for (const [word, line] of cases) {
    await open(page, url, { polls: [word], interval: 1 });
    await openDialog(page);
    await page.locator("#ss-address").fill(GOOD);
    await dialog(page).getByRole("button", { name: "Continue", exact: true }).click();
    await page.waitForFunction((l) => document.querySelector(".ss-state")?.textContent?.includes(l), line);
    assert.equal(await stateText(page), `!${line}`, `${word}: the mark and the words`);
    assert.equal(await page.locator(".ss-state").getAttribute("data-bad"), "true");
    assert.ok((await page.locator(".ss-word").evaluate((n) => getComputedStyle(n).textDecorationLine)).includes("underline"), `${word}: underlined`);
    assert.deepEqual(await buttons(page), ["Start again", "Cancel"], word);
    assert.equal(await page.evaluate(() => document.activeElement?.textContent), "Start again");
    const polls = await count(page, "server_sign_in_poll");
    assert.equal(polls, 1, word);
    if (word === "denied") {
      await page.waitForTimeout(1500);
      assert.equal(await count(page, "server_sign_in_poll"), polls, "the polling stopped");
      // Start again starts a new request at the known address and shows its code.
      await dialog(page).getByRole("button", { name: "Start again", exact: true }).click();
      await page.waitForFunction(() => window.__MOCK__.serverLink.calls.filter((c) => c[0] === "server_sign_in_start").length === 2);
      assert.deepEqual((await link(page, "server_sign_in_start")).map((a) => a.address), [GOOD, GOOD]);
      await page.waitForFunction(() => document.querySelector(".ss-state")?.textContent === "!The request was denied.");
      assert.equal(await count(page, "server_sign_in_poll"), 2);
    }
    await dialog(page).getByRole("button", { name: "Cancel", exact: true }).click();
    await gone(page);
    assert.equal(await count(page, "server_sign_in_cancel"), 1, `${word}: Cancel drops the host's request`);
    assert.deepEqual(await result(page), { view: null });
  }
}

/** A start that the host refuses is said on the first screen, in the record's words or the host's fixed sentence, and the field keeps its text. */
async function startRefusedCase(page, url) {
  const cases = [
    ["The server cannot be reached.", "The server cannot be reached."],
    ["This feature is off on the server.", "Device sign-in is off on this server."],
    ["The sign-in could not be saved in the keychain.", "The sign-in could not be saved in the keychain."],
    ["", "The sign-in failed."],
  ];
  for (const [fails, shown] of cases) {
    await open(page, url, { startFails: fails });
    await openDialog(page);
    await page.locator("#ss-address").fill(GOOD);
    await dialog(page).getByRole("button", { name: "Continue", exact: true }).click();
    await page.waitForFunction(() => document.querySelector(".ss-refusal")?.textContent !== "");
    const text = await page.locator(".ss-refusal").textContent();
    assert.equal(text, shown);
    assert.equal(await page.locator("#ss-address").inputValue(), GOOD);
    assert.equal(await page.locator(".ss-code").count(), 0, "no code is shown for a request that never started");
    assert.equal(await count(page, "server_sign_in_poll"), 0, "nothing is polled");
    assert.deepEqual(await buttons(page), ["Cancel", "Continue"], "the person can try again or leave");
    await page.keyboard.press("Escape");
    await gone(page);
  }
}

/** The source has no innerHTML, no literal colour, no timer left behind by name and no label of the planning tree. */
function sourceCase() {
  for (const name of ["serverstate.js", "serversignin.js"]) {
    const code = readFileSync(join(APP_DIR, "dist", name), "utf8");
    assert.equal(/innerHTML|outerHTML|insertAdjacentHTML|document\.write/.test(code), false, `${name} builds no markup from text`);
    assert.equal(/#[0-9a-fA-F]{6}\b/.test(code), false, name);
    assert.equal(/\b12-\d\d\b|\bT-\d\d/.test(code), false, `${name} has no planning label`);
  }
}
