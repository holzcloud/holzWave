// Signing the app in to a server (docs/design.md 6.35, "The step in the app"): the first screen, the approval with its pacing, against the stub whose hook `__MOCK__.serverLink` plays the host. The words are the design record's. Nothing here runs a real server, a real browser sign-in or a real keychain.
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
    await firstScreenCase(page, url);
    await approveCase(page, url);
  }
  assert.deepEqual(errors, [], "the page reported errors");
}

/** The first screen: the field, its help line, the refusals, the device row and the two buttons. */
async function firstScreenCase(page, url) {
  await open(page, url);
  await openDialog(page);
  assert.equal(await page.locator(".server-signin .set-title").textContent(), "Sign in to the server");
  assert.equal(await page.locator(".server-signin label").textContent(), "Server address");
  assert.equal(await page.locator(".server-signin .ss-help").textContent(), "The address of your holzWave server, starting with https://.");
  assert.deepEqual(await buttons(page), ["Cancel", "Continue"]);
  const kv = await page.locator(".server-signin .ss-kv").textContent();
  assert.equal(kv, "DeviceHolz MacBook", "the name of the computer is shown, never asked");
  assert.equal(await page.locator(".server-signin input").count(), 1, "one field: the address");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ss-address", "the focus starts in the field");
  assert.equal(await page.locator("#ss-address").getAttribute("aria-describedby"), "ss-help");

  // An address that is not an https origin is refused before any command is called.
  for (const bad of ["http://holz.example.net", "holz.example.net", "https://holz.example.net/path", "https://u@holz.example.net", ""]) {
    await page.locator("#ss-address").fill(bad);
    await dialog(page).getByRole("button", { name: "Continue", exact: true }).click();
    await page.waitForFunction(() => document.querySelector(".ss-refusal")?.textContent !== "");
    assert.equal(await page.locator(".ss-refusal").textContent(), "Use an address that starts with https://.", bad);
    assert.equal(await page.locator(".ss-refusal").getAttribute("role"), "alert");
    assert.equal(await page.locator("#ss-address").getAttribute("aria-invalid"), "true");
    assert.equal(await page.locator("#ss-address").getAttribute("aria-describedby"), "ss-help ss-refusal");
    assert.equal(await page.locator("#ss-address").inputValue(), bad, "what was typed is kept");
  }
  assert.equal(await count(page, "server_sign_in_start"), 0, "no command ran for a refused address");
  // Typing again takes the refusal away.
  await page.locator("#ss-address").fill("https://");
  assert.equal(await page.locator(".ss-refusal").textContent(), "");
  await dialog(page).getByRole("button", { name: "Cancel", exact: true }).click();
  await gone(page);
  assert.deepEqual(await result(page), { view: null });
}

/** Continue with a good address: the code, the browser opened once, the polls no faster than the host says, and Signed in. */
async function approveCase(page, url) {
  await open(page, url, { polls: ["pending", "approved"], interval: 1 });
  await openDialog(page);
  await page.locator("#ss-address").fill(` ${GOOD}/ `);
  const t0 = Date.now();
  await dialog(page).getByRole("button", { name: "Continue", exact: true }).click();
  await page.waitForFunction(() => document.querySelector(".ss-code")?.textContent === "BCDF-GHJK");
  assert.deepEqual(await link(page, "server_sign_in_start"), [{ address: GOOD }], "the origin as the browser writes it");
  assert.equal(await page.locator(".ss-code").getAttribute("aria-label"), "Code");
  assert.equal(await page.locator(".set-title").textContent(), "Sign in to the server");
  assert.equal(await page.locator(".ss-kv").textContent(), "Serverholz.example.net", "the address the request goes to");
  assert.equal(await page.locator(".ss-compare").textContent(), "Enter this code only if you started this here.");
  assert.equal(await stateText(page), "Waiting for approval…");
  assert.equal(await page.locator(".ss-state").getAttribute("role"), "status");
  assert.equal(await page.locator(".ss-dot").getAttribute("class"), "ss-dot ss-ring", "a ring while waiting");
  assert.deepEqual(await buttons(page), ["Open again", "Cancel"]);

  // The system browser is asked once, with the address the host returned and nothing else.
  await page.waitForFunction(() => window.__MOCK__.callsOf("open_url").length === 1);
  assert.deepEqual(await callsOf(page, "open_url"), [{ url: `${GOOD}/#device=hwg_mock`, pane: null }]);
  await page.waitForFunction(() => document.querySelector(".ss-note")?.hidden === true);

  // No poll before the interval the host stated; the first one after it.
  await page.waitForTimeout(500);
  assert.equal(await count(page, "server_sign_in_poll"), 0, `no poll after ${Date.now() - t0} ms of a 1 s interval`);
  await page.waitForFunction(() => window.__MOCK__.serverLink.calls.filter((c) => c[0] === "server_sign_in_poll").length === 1);
  const first = Date.now() - t0;
  assert.ok(first >= 950, `the first poll came ${first} ms after the start`);

  // Open again opens the browser once more, and only then.
  await dialog(page).getByRole("button", { name: "Open again", exact: true }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("open_url").length === 2);

  // The second poll approves: the state is a filled dot and Signed in, and the one button is Close.
  await page.waitForFunction(() => document.querySelector(".ss-state")?.textContent === "Signed in");
  assert.equal(await page.locator(".ss-dot").getAttribute("class"), "ss-dot ss-filled");
  assert.deepEqual(await buttons(page), ["Close"]);
  assert.equal(await page.evaluate(() => document.activeElement?.textContent), "Close");
  const polls = await count(page, "server_sign_in_poll");
  assert.equal(polls, 2);
  await page.waitForTimeout(1300);
  assert.equal(await count(page, "server_sign_in_poll"), polls, "the polling stopped with the approval");
  assert.equal(await page.locator(".server-signin").count(), 1, "the dialog stays until the person presses the one button");
  assert.equal(await count(page, "server_sign_in_cancel"), 0, "an approved sign-in is not cancelled");
  await dialog(page).getByRole("button", { name: "Close", exact: true }).click();
  await gone(page);
  const closed = await result(page);
  assert.equal(closed.view.state, "signed_in");
  assert.equal(closed.view.url, GOOD);
  assert.equal(await count(page, "server_sign_in_cancel"), 0);
}

