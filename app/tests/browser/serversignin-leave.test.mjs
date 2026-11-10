// Leaving the sign-in dialog (docs/design.md 6.35): Cancel at either screen and Escape drop the host's request and stop the polling, a known address skips the first screen, a click outside does nothing, and hostile values are shown as bounded text. Against the stub that plays the host.
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
    await cancelCase(page, url);
    await knownAddressCase(page, url);
    await outsideCase(page, url);
    await hostileCase(page, url);
  }
  assert.deepEqual(errors, [], "the page reported errors");
}

/** Cancel at either screen, and Escape, drop the host's request, stop the polling and close the dialog. */
async function cancelCase(page, url) {
  // The first screen: nothing was started, and the host is told all the same.
  await open(page, url);
  await openDialog(page);
  await dialog(page).getByRole("button", { name: "Cancel", exact: true }).click();
  await gone(page);
  assert.equal(await count(page, "server_sign_in_cancel"), 1);
  assert.deepEqual(await result(page), { view: null });

  // The second screen, by the button.
  await open(page, url, { interval: 1 });
  await openDialog(page);
  await page.locator("#ss-address").fill(GOOD);
  await dialog(page).getByRole("button", { name: "Continue", exact: true }).click();
  await page.waitForFunction(() => document.querySelector(".ss-code")?.textContent === "BCDF-GHJK");
  await dialog(page).getByRole("button", { name: "Cancel", exact: true }).click();
  await gone(page);
  assert.equal(await count(page, "server_sign_in_cancel"), 1);
  await page.waitForTimeout(1300);
  assert.equal(await count(page, "server_sign_in_poll"), 0, "the polling stopped with Cancel");
  assert.deepEqual(await result(page), { view: null });
  const view = await page.evaluate(() => window.__MOCK__.serverLink.view);
  assert.equal(view.state, "signed_out", "nothing half-made is left");

  // The second screen, by Escape; the focus goes back to what opened it.
  await open(page, url, { interval: 1 });
  await page.evaluate(() => {
    const opener = document.createElement("button");
    opener.id = "opener";
    opener.textContent = "opener";
    document.body.append(opener);
    opener.focus();
  });
  await openDialog(page);
  await page.locator("#ss-address").fill(GOOD);
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => document.querySelector(".ss-code")?.textContent === "BCDF-GHJK");
  assert.equal(await count(page, "server_sign_in_start"), 1, "Enter in the field is Continue");
  await page.keyboard.press("Escape");
  await gone(page);
  assert.equal(await count(page, "server_sign_in_cancel"), 1);
  assert.equal(await page.evaluate(() => document.activeElement?.id), "opener", "the focus returns to the opener");
  await page.waitForTimeout(1300);
  assert.equal(await count(page, "server_sign_in_poll"), 0);
  assert.deepEqual(await result(page), { view: null });
}

/** With an address known (a sign-out keeps it) the dialog opens on the second screen at once and shows the address in mono. */
async function knownAddressCase(page, url) {
  await open(page, url, { view: { url: GOOD }, polls: ["pending"], interval: 5 });
  await openDialog(page);
  await page.waitForFunction(() => document.querySelector(".ss-code")?.textContent === "BCDF-GHJK");
  assert.equal(await page.locator("#ss-address").count(), 0, "the address is not asked again");
  assert.equal(await page.locator(".ss-kv").textContent(), "Serverholz.example.net");
  assert.deepEqual(await link(page, "server_sign_in_start"), [{ address: GOOD }]);
  await page.waitForFunction(() => window.__MOCK__.callsOf("open_url").length === 1);
  // A second ask while it is open is the same dialog.
  await page.evaluate(() => {
    void import(new URL("serversignin.js", location.href).href).then((m) => m.openServerSignIn());
  });
  await page.waitForTimeout(200);
  assert.equal(await page.locator(".server-signin").count(), 1);
  assert.equal(await count(page, "server_sign_in_start"), 1);
  await page.keyboard.press("Escape");
  await gone(page);
}

/** A click outside does nothing, as in the other layers: a request in progress is not dropped by a stray click. */
async function outsideCase(page, url) {
  await open(page, url, { view: { url: GOOD }, polls: ["pending"], interval: 5 });
  await openDialog(page);
  await page.waitForFunction(() => document.querySelector(".ss-code")?.textContent === "BCDF-GHJK");
  await page.mouse.click(4, 4);
  await page.waitForTimeout(150);
  assert.equal(await page.locator(".server-signin").count(), 1);
  assert.equal(await count(page, "server_sign_in_cancel"), 0);
  await page.keyboard.press("Escape");
  await gone(page);
}


/** A hostile name, address or sentence is shown as bounded text, and no token-shaped text comes out of an unknown member. */
async function hostileCase(page, url) {
  const canary = `hwd_${"A".repeat(43)}`;
  const markup = `<img src=x onerror="window.__PWNED=1">`;
  await open(page, url, {
    view: { name: `${markup}${"n".repeat(300)}`, token: canary, device_code: canary, error: `${markup}${"e".repeat(900)}` },
    startFails: `${markup} ${"s".repeat(900)}`,
  });
  await openDialog(page);
  const name = await page.locator(".ss-kv .ss-v").textContent();
  assert.ok(name.length <= 64 && name.startsWith("<img"), "the name is bounded text, not markup");
  assert.equal(await page.locator(".server-signin img").count(), 0);
  await page.locator("#ss-address").fill(GOOD);
  await dialog(page).getByRole("button", { name: "Continue", exact: true }).click();
  await page.waitForFunction(() => document.querySelector(".ss-refusal")?.textContent !== "");
  const refusal = await page.locator(".ss-refusal").textContent();
  assert.ok(refusal.length <= 200 + 2, `the host's sentence is bounded (${refusal.length})`);
  assert.equal(await page.locator(".server-signin img").count(), 0);
  assert.equal(await page.evaluate(() => window.__PWNED), undefined);
  const html = await page.evaluate(() => document.querySelector(".server-signin").outerHTML);
  assert.equal(html.includes(canary), false, "a token-shaped member of the view never reaches the page");
  assert.equal(await page.evaluate((c) => document.body.textContent.includes(c) || document.title.includes(c) || location.href.includes(c), canary), false);
  await page.keyboard.press("Escape");
  await gone(page);

  // A garbled answer to the start has no code to show.
  await open(page, url);
  await page.evaluate(() => {
    window.__MOCK__.on("server_sign_in_start", () => ({ code: "<b>x</b>", url: "javascript:alert(1)", token: "hwd_x" }));
  });
  await openDialog(page);
  await page.locator("#ss-address").fill(GOOD);
  await dialog(page).getByRole("button", { name: "Continue", exact: true }).click();
  await page.waitForFunction(() => document.querySelector(".ss-refusal")?.textContent !== "");
  assert.equal(await page.locator(".ss-refusal").textContent(), "The sign-in failed.");
  assert.equal((await callsOf(page, "open_url")).length, 0, "no address of an answer that is not a sign-in is opened");
  await page.keyboard.press("Escape");
  await gone(page);

  // An answer to a poll with a word the page does not know stops the wait with a state of the record.
  await open(page, url, { view: { url: GOOD }, interval: 1 });
  await page.evaluate(() => {
    window.__MOCK__.on("server_sign_in_poll", () => ({ state: "slow_down", interval: 1 }));
  });
  await openDialog(page);
  await page.waitForFunction(() => document.querySelector(".ss-state")?.textContent === "!The server cannot be reached.");
  await page.keyboard.press("Escape");
  await gone(page);
}

