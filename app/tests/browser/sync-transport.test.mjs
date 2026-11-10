// The choice of the sync target in the popover (docs/design.md 6.9, "The choice of the target"): WebDAV or the holzWave server, exactly one,
// with the passphrase shared; the state line of the server variant and why Connect cannot be pressed; the connected popover; and what
// a run that found the sign-in ended shows. Against the stub whose hooks play the core (`sync_*`) and the host (`__MOCK__.serverLink`).
// Nothing here runs a real server, a real browser sign-in or a real keychain.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, currentBackend } from "./lib.mjs";

export const backends = ["tauri"];
export const backendNote = "the choice of the sync target is drawn by the desktop and mobile app; the browser window is the server's own page and does not sync to a server";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(6000);
  if (currentBackend() === "tauri") {
    await choiceCase(page, url);
    await signInCase(page, url);
    await offCase(page, url);
    await refusedCase(page, url);
    await connectedCase(page, url);
    await accessibilityCase(page, url);
  }
  assert.deepEqual(errors, [], "the page reported errors");
}

const HOST = "holz.example.net";
const GOOD = `https://${HOST}`;
const OFF = { configured: false, transport: "webdav", endpoint: "", user: "", state: "off", last: null, files: 0, conflicts: 0, error: null, restart: false, removed: 0 };
const SERVER_ON = { ...OFF, configured: true, transport: "server", endpoint: GOOD, state: "idle", last: 1760000000, files: 3, conflicts: 0 };
const VIEW = { state: "signed_out", url: "", name: "Holz MacBook", locked: false, sync_on: true, remote_on: false, error: null };

/** Load the page against the stub with the sync status `sync` and the link `view`; every case sets every knob. */
async function open(page, url, { sync = OFF, view = {}, polls = [] } = {}) {
  await page.addInitScript(
    ([s, v, p]) => {
      window.__SYNC = s;
      const link = window.__MOCK__.serverLink;
      link.reset();
      link.set(v);
      link.polls = p;
      link.setInterval(1);
      window.__MOCK__.on("sync_status", () => structuredClone(window.__SYNC));
      window.__MOCK__.on("sync_now", () => {
        if (window.__NOW_FAILS) throw window.__NOW_FAILS;
        window.__SYNC = { ...window.__SYNC, state: "idle", files: 2 };
        return structuredClone(window.__SYNC);
      });
    },
    [sync, { ...VIEW, ...view }, polls],
  );
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForSelector(".sync-btn");
}

const popover = (page) => page.locator(".dlg.sync");
const link = (page, cmd) => page.evaluate((c) => window.__MOCK__.serverLink.calls.filter((x) => x[0] === c).map((x) => x[1]), cmd);
const count = async (page, cmd) => (await link(page, cmd)).length;

async function openPopover(page) {
  await page.locator(".sync-btn").click();
  await popover(page).waitFor();
}

async function closePopover(page) {
  await page.keyboard.press("Escape");
  await popover(page).waitFor({ state: "detached" });
}

const target = (page) => page.locator("#sy-target").textContent();
const radios = (page) => page.locator('.sync .sy-choice input[type="radio"]');
const checked = (page) => page.evaluate(() => document.querySelector('.sync .sy-choice input:checked')?.value);

/** The two-option choice, the server variant of the rows, what is kept when the choice goes back and forth, and the order of the keys. */
async function choiceCase(page, url) {
  await open(page, url);
  await openPopover(page);
  const group = page.getByRole("radiogroup", { name: "Transport" });
  assert.equal(await group.count(), 1);
  assert.deepEqual(await page.locator(".sync .sy-option").allTextContents(), ["WebDAV", "holzWave server"]);
  assert.equal(await checked(page), "webdav", "WebDAV is the one active by default");
  assert.equal(await page.locator(".sync .set-title").textContent(), "Nextcloud");
  assert.equal(await radios(page).count(), 2);
  assert.equal(await page.locator("#sy-endpoint").count(), 1);
  assert.equal(await page.locator(".sy-change").textContent(), "Changing the target keeps your passphrase. Files are not copied from one target to the other; the next run reads the new one.");
  assert.equal(await count(page, "server_status") + (await count(page, "server_refresh")), 0, "nothing of the server is asked while WebDAV is the choice");
  assert.equal(await page.evaluate(() => document.activeElement?.value), "webdav", "the focus starts on the chosen radio");

  await page.locator("#sy-endpoint").fill("https://cloud.example.org/dav");
  await page.locator("#sy-user").fill("holz");
  await page.locator("#sy-passphrase").fill("a long passphrase");

  // The arrows move to the other option and choose it; Tab reaches the chosen radio only.
  await page.locator('.sy-choice input[value="webdav"]').focus();
  await page.keyboard.press("ArrowRight");
  await page.waitForFunction(() => document.querySelector(".sync .set-title")?.textContent === "holzWave server");
  assert.equal(await checked(page), "server");
  assert.equal(await page.evaluate(() => document.activeElement?.value), "server", "the focus stays on the radio that was chosen");
  assert.equal(await page.locator("#sy-endpoint").count() + (await page.locator("#sy-user").count()) + (await page.locator("#sy-password").count()), 0, "the WebDAV rows are replaced");
  assert.equal(await page.locator("#sy-address-label").textContent(), "Server address");
  assert.equal(await page.locator(".sy-address").textContent(), "–", "a dim dash while there is no address");
  assert.equal(await page.locator("#sy-passphrase").inputValue(), "a long passphrase", "the passphrase row is shared and kept");
  await page.waitForFunction(() => document.querySelector("#sy-target")?.textContent?.includes("not signed in"));
  assert.equal(await target(page), "not signed inSign in…");
  assert.equal(await page.locator("#sy-target").getAttribute("role"), "status");
  assert.equal(await page.locator(".sy-dot").getAttribute("class"), "sy-dot sy-hollow");
  const connect = page.locator(".sy-connect");
  assert.equal(await connect.isDisabled(), true, "Connect waits for the sign-in");
  assert.equal(await connect.getAttribute("aria-describedby"), "sy-target", "the state line is its description");
  assert.equal(await connect.evaluate((n) => getComputedStyle(n).borderTopStyle), "dashed", "a dashed border besides the dim colour");
  assert.equal(await page.locator(".sy-change").count(), 1);
  assert.equal(await count(page, "server_status") >= 1, true);
  assert.equal((await count(page, "server_refresh")), 1, "the server is asked once when the server variant is drawn");
  assert.equal((await callsOf(page, "sync_use_server")).length, 0);
  // Tab: the chosen radio, then the link, then the passphrase; the other radio is not a stop.
  const order = [];
  for (let i = 0; i < 4; i++) {
    await page.keyboard.press("Tab");
    order.push(await page.evaluate(() => document.activeElement?.value ?? document.activeElement?.textContent));
  }
  assert.ok(!order.includes("webdav"), `Tab skips the radio that is not chosen (${order})`);

  // Back to WebDAV: what was typed is still there, in memory only.
  await page.locator('.sy-choice input[value="server"]').focus();
  await page.keyboard.press("ArrowLeft");
  await page.waitForFunction(() => document.querySelector(".sync .set-title")?.textContent === "Nextcloud");
  assert.equal(await page.locator("#sy-endpoint").inputValue(), "https://cloud.example.org/dav");
  assert.equal(await page.locator("#sy-user").inputValue(), "holz");
  assert.equal(await page.locator("#sy-passphrase").inputValue(), "a long passphrase");
  assert.equal(await count(page, "server_refresh"), 1, "going back and forth asks the server once");
  await closePopover(page);
  await openPopover(page);
  assert.equal(await page.locator("#sy-passphrase").inputValue(), "", "nothing typed survives the popover");
  assert.equal(await checked(page), "webdav");
  await closePopover(page);
}

/** Sign in… opens the dialog; a sign-in that ends signed in refreshes the popover and Connect can be pressed; then Connect. */
async function signInCase(page, url) {
  await open(page, url, { view: { url: GOOD }, polls: ["approved"] });
  await openPopover(page);
  await page.locator('.sy-choice input[value="server"]').check();
  await page.waitForFunction(() => document.querySelector("#sy-target")?.textContent?.includes("not signed in"));
  assert.equal(await page.locator(".sy-address").textContent(), HOST, "the address from the earlier sign-in");
  await page.locator("#sy-passphrase").fill("a long passphrase");
  await page.getByRole("button", { name: "Sign in…" }).click();
  const dialog = page.getByRole("dialog", { name: "Sign in to the server" });
  await dialog.waitFor();
  await page.waitForFunction(() => document.querySelector(".ss-state")?.textContent === "Signed in");
  assert.equal(await page.locator(".ss-kv").textContent(), `Server${HOST}`, "the address is known, so the first screen is skipped");
  await dialog.getByRole("button", { name: "Close", exact: true }).click();
  await dialog.waitFor({ state: "detached" });
  // The popover is refreshed: signed in, Connect can be pressed, and what was typed is still there.
  await page.waitForFunction(() => document.querySelector("#sy-target")?.textContent === "signed in");
  assert.equal(await page.locator(".sy-dot").getAttribute("class"), "sy-dot sy-filled");
  assert.equal(await page.locator(".sy-connect").isDisabled(), false);
  assert.equal(await page.locator("#sy-passphrase").inputValue(), "a long passphrase");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "sy-passphrase", "the focus goes to the next thing to do");
  assert.equal(await page.locator(".sy-connect").getAttribute("aria-describedby"), null);

  await page.locator(".sy-connect").click();
  await page.waitForFunction(() => document.querySelector(".sync .sy-chip")?.textContent === "connected");
  assert.deepEqual(await link(page, "sync_use_server"), [{ passphrase: "a long passphrase" }]);
  assert.equal(await page.locator(".sync .set-title").textContent(), "holzWave server");
  assert.equal(await page.locator(".sync .sy-sub").first().textContent(), `${HOST} · signed in`);
  const rows = await page.locator(".sync .sy-row").evaluateAll((n) => n.map((r) => r.textContent));
  assert.deepEqual(rows.map((r) => r.replace(/\d\d:\d\d/, "HH:MM")), ["Last syncnot yet", "Encryptionend-to-end · passphrase", "Conflictsnone"]);
  assert.equal(await page.locator(".sync .sy-choice").count(), 0, "the choice is drawn only while setting up");
  assert.equal(await page.locator(".sync .sy-change").count(), 0, "so is the sentence under it");
  assert.equal(await page.locator("#sy-passphrase").count(), 0);
  assert.equal(await page.locator(".sync-btn").textContent(), "☁ synced");
  assert.deepEqual(await page.locator(".sync .dlg-actions button").allTextContents(), ["Close", "Turn off", "Sync now"]);
  await closePopover(page);
}

/** The server has the sync target off, or the secrets are locked: Connect waits, the state line says why, and nothing is called. */
async function offCase(page, url) {
  await open(page, url, { view: { state: "signed_in", url: GOOD, sync_on: false } });
  await openPopover(page);
  await page.locator('.sy-choice input[value="server"]').check();
  await page.waitForFunction(() => document.querySelector("#sy-target")?.textContent?.includes("Sync is off on this server."));
  assert.equal(await target(page), "!Sync is off on this server.");
  assert.equal(await page.locator("#sy-target").getAttribute("data-bad"), "true");
  assert.ok((await page.locator(".sy-word").evaluate((n) => getComputedStyle(n).textDecorationLine)).includes("underline"));
  assert.equal(await page.locator(".sy-connect").isDisabled(), true);
  assert.equal(await page.locator(".sy-connect").getAttribute("aria-describedby"), "sy-target");
  assert.equal(await page.locator("#sy-target .sy-link").count(), 0, "signed in: no link");
  await page.locator("#sy-passphrase").fill("a long passphrase");
  await page.keyboard.press("Enter");
  await page.waitForTimeout(150);
  assert.equal(await count(page, "sync_use_server"), 0, "Enter does not press a Connect that is disabled");
  await closePopover(page);

  // The secrets are locked: the record's words, no request to the server, and the state line returns by itself after the unlock.
  await open(page, url, { view: { state: "signed_in", url: GOOD, locked: true } });
  await openPopover(page);
  await page.locator('.sy-choice input[value="server"]').check();
  await page.waitForFunction(() => document.querySelector("#sy-target")?.textContent?.includes("Locked: unlock to connect"));
  assert.equal(await target(page), "Locked: unlock to connect");
  assert.equal(await page.locator(".sy-connect").isDisabled(), true);
  await page.waitForTimeout(250);
  assert.equal(await count(page, "server_refresh"), 0, "the server is not asked while the secrets are locked");
  assert.equal(await count(page, "sync_use_server"), 0);
  await page.evaluate(() => window.__MOCK__.serverLink.set({ locked: false }));
  await page.waitForFunction(() => document.querySelector("#sy-target")?.textContent === "signed in", undefined, { timeout: 5000 });
  assert.equal(await page.locator(".sy-connect").isDisabled(), false);
  assert.equal(await count(page, "server_refresh"), 1, "the server is asked once after the unlock");
  await closePopover(page);
}

/** A refused Connect says the host's fixed sentence and leaves the choice and the fields; a sign-in that ended is said by the state line. */
async function refusedCase(page, url) {
  await open(page, url, { view: { state: "signed_in", url: GOOD } });
  await page.evaluate(() => {
    window.__MOCK__.on("sync_use_server", () => {
      throw "The passphrase is wrong for this sync target.";
    });
  });
  await openPopover(page);
  await page.locator('.sy-choice input[value="server"]').check();
  await page.waitForFunction(() => document.querySelector("#sy-target")?.textContent === "signed in");
  await page.locator("#sy-passphrase").fill("not the passphrase");
  await page.locator(".sy-connect").click();
  await page.waitForFunction(() => document.querySelector(".sync .set-status")?.textContent?.includes("passphrase is wrong"));
  assert.equal(await page.locator(".sync .set-status").textContent(), "The passphrase is wrong for this sync target.");
  assert.equal(await page.locator(".sync .set-status").getAttribute("data-failed"), "true");
  assert.equal(await checked(page), "server", "the choice stays");
  assert.equal(await page.locator("#sy-passphrase").inputValue(), "", "a secret is not put back");
  assert.equal(await page.locator(".sync .sy-chip").textContent(), "not set up");
  await page.waitForFunction(() => document.querySelector("#sy-target")?.textContent === "signed in");

  // The sign-in ended: the sentence of the host, and the state line says not signed in with the link.
  await page.evaluate(() => {
    window.__MOCK__.on("sync_use_server", () => {
      window.__MOCK__.serverLink.set({ state: "signed_out" });
      throw "The server ended this sign-in. Sign in again.";
    });
  });
  await page.locator("#sy-passphrase").fill("a long passphrase");
  await page.locator(".sy-connect").click();
  await page.waitForFunction(() => document.querySelector(".sync .set-status")?.textContent?.includes("ended this sign-in"));
  await page.waitForFunction(() => document.querySelector("#sy-target .sy-link"));
  assert.equal(await target(page), "not signed inSign in…");
  assert.equal(await page.locator(".sy-connect").isDisabled(), true);
  assert.equal(await checked(page), "server");
  await closePopover(page);

  // A WebDAV Connect that is refused keeps the choice and the address and user, and puts no secret back.
  await open(page, url);
  await page.evaluate(() => {
    window.__MOCK__.on("sync_save", () => {
      throw "The server refused the request.";
    });
  });
  await openPopover(page);
  await page.locator("#sy-endpoint").fill("https://cloud.example.org/dav");
  await page.locator("#sy-user").fill("holz");
  await page.locator("#sy-password").fill("app-password");
  await page.locator("#sy-passphrase").fill("a long passphrase");
  await page.locator(".sy-connect").click();
  await page.waitForFunction(() => document.querySelector(".sync .set-status")?.textContent === "The server refused the request.");
  assert.equal(await page.locator("#sy-endpoint").inputValue(), "https://cloud.example.org/dav");
  assert.equal(await page.locator("#sy-user").inputValue(), "holz");
  assert.equal(await page.locator("#sy-password").inputValue(), "");
  assert.equal(await page.locator("#sy-passphrase").inputValue(), "");
  await closePopover(page);
}

/** A connected popover with the server transport, and a run that found the sign-in ended. */
async function connectedCase(page, url) {
  await open(page, url, { sync: SERVER_ON, view: { state: "signed_in", url: GOOD } });
  assert.equal(await page.locator(".sync-btn").textContent(), "☁ synced", "the word of the button does not change with the target");
  await openPopover(page);
  assert.equal(await page.locator(".sync .set-title").textContent(), "holzWave server");
  assert.equal(await page.locator(".sync .sy-chip").textContent(), "connected");
  assert.equal(await page.locator(".sync .sy-sub").first().textContent(), `${HOST} · signed in`);
  const rows = await page.locator(".sync .sy-row").evaluateAll((n) => n.map((r) => r.textContent));
  assert.match(rows[0], /^Last sync\d\d:\d\d · 3 files$/);
  assert.equal(rows[1], "Encryptionend-to-end · passphrase");
  assert.equal(rows[2], "Conflictsnone");
  assert.equal(await page.locator(".sync .sy-choice").count(), 0);
  assert.equal(await page.locator("#sy-target").count(), 0);
  assert.equal(await count(page, "server_refresh"), 0, "a connected popover asks the server for nothing");
  await closePopover(page);

  // A refused token found by a run: the title-bar word, the chip, the line under the title and the link say so; no error text.
  await open(page, url, { sync: { ...SERVER_ON, state: "signed_out" }, view: { state: "signed_out", url: GOOD } });
  assert.equal(await page.locator(".sync-btn").textContent(), "☁ sync error");
  assert.equal(await page.locator(".sync-btn").getAttribute("data-state"), "signed_out");
  await openPopover(page);
  assert.equal(await page.locator(".sync .sy-chip").textContent(), "not signed in");
  assert.equal(await page.locator(".sync .sy-sub").first().textContent(), `${HOST} · not signed in`);
  assert.equal(await page.locator(".sync .set-status").textContent(), "", "not a generic error");
  assert.equal(await target(page), "not signed inSign in…");
  await page.getByRole("button", { name: "Sign in…" }).click();
  const dialog = page.getByRole("dialog", { name: "Sign in to the server" });
  await dialog.waitFor();
  assert.equal(await page.locator(".ss-kv").textContent(), `Server${HOST}`);
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await dialog.waitFor({ state: "detached" });
  assert.equal(await popover(page).count(), 1, "the popover is still there");
  await closePopover(page);

  // A status answer without the member reads as WebDAV, as before.
  await open(page, url, { sync: { ...SERVER_ON, transport: undefined, endpoint: "https://cloud.example.org/remote.php/dav/files/holz/holzwave", user: "holz" } });
  await openPopover(page);
  assert.equal(await page.locator(".sync .set-title").textContent(), "Nextcloud");
  assert.match(await page.locator(".sync .sy-sub").first().textContent(), /cloud\.example\.org\/remote\.php\/dav\/files\/holz\/holzwave · holz/);
  assert.equal(await page.locator("#sy-target").count(), 0);
  await closePopover(page);

  // A hostile address in the link is shown as a dash, never as text or markup.
  await open(page, url, { view: { url: `javascript:alert(1)<img src=x onerror="window.__PWNED=1">`, token: `hwd_${"A".repeat(43)}` } });
  await openPopover(page);
  await page.locator('.sy-choice input[value="server"]').check();
  await page.waitForFunction(() => document.querySelector("#sy-target")?.textContent?.includes("not signed in"));
  assert.equal(await page.locator(".sy-address").textContent(), "–");
  assert.equal(await page.evaluate(() => window.__PWNED), undefined);
  assert.equal(await page.evaluate(() => document.querySelector(".dlg.sync").outerHTML.includes("hwd_")), false);
  await closePopover(page);
}

/** The words, shapes and sizes of the choice in every contrast, motion and pointer mode. */
async function accessibilityCase(page, url) {
  await open(page, url, { view: { state: "signed_in", url: GOOD, sync_on: false } });
  await openPopover(page);
  await page.locator('.sy-choice input[value="server"]').check();
  await page.waitForFunction(() => document.querySelector("#sy-target")?.textContent?.includes("Sync is off"));
  const probe = () =>
    page.evaluate(() => {
      const pick = (selector) => {
        const node = document.querySelector(selector);
        const c = getComputedStyle(node);
        const r = node.getBoundingClientRect();
        return { text: node.textContent.trim(), border: parseFloat(c.borderTopWidth), style: c.borderTopStyle, colour: c.borderTopColor, height: r.height, animation: c.animationName, transition: c.transitionDuration };
      };
      return { option: pick(".sync .sy-option"), address: pick(".sy-address"), connect: pick(".sy-connect"), line: pick("#sy-target"), box: pick(".dlg.sync") };
    });
  const plain = await probe();
  assert.ok(plain.option.height >= 27.99, `an option is at least 28 px high (${plain.option.height})`);
  assert.equal(plain.connect.style, "dashed");

  await page.emulateMedia({ forcedColors: "active" });
  const forced = await probe();
  assert.equal(forced.line.text, "!Sync is off on this server.", "the words and the mark stay");
  for (const name of ["address", "connect", "box"]) assert.ok(forced[name].border >= 1 && forced[name].style !== "none", `${name} keeps its border`);
  assert.equal(forced.connect.style, "dashed", "the dashes stay under forced colours");
  await page.emulateMedia({ forcedColors: "none" });

  await page.emulateMedia({ contrast: "more" });
  await page.waitForFunction(() => document.documentElement.classList.contains("contrast-high"));
  const high = await probe();
  assert.notEqual(high.address.colour, plain.address.colour, "the hairline follows the stronger token");
  await page.emulateMedia({ contrast: "no-preference" });
  await page.waitForFunction(() => !document.documentElement.classList.contains("contrast-high"));

  await page.emulateMedia({ reducedMotion: "reduce" });
  const still = await probe();
  for (const name of ["option", "address", "line"]) assert.equal(still[name].animation, "none", name);
  await page.emulateMedia({ reducedMotion: "no-preference" });

  // The 2 px ring of the keyboard on the chosen radio.
  await page.locator('.sy-choice input[value="server"]').focus();
  await page.keyboard.press("Shift+Tab");
  await page.keyboard.press("Tab");
  assert.equal(await page.evaluate(() => getComputedStyle(document.activeElement).outlineWidth), "2px");
  await closePopover(page);

  const css = readFileSync(join(APP_DIR, "dist", "sync.css"), "utf8");
  const coarse = css.slice(css.indexOf("@media (pointer: coarse)"), css.indexOf("@media (forced-colors"));
  assert.ok(coarse.includes(".sy-option") && coarse.includes(".sy-link"));
  assert.match(coarse, /min-height: max\(44px, calc\(44rem \/ 12\)\)/);
  assert.equal(/#[0-9a-fA-F]{3,8}\b/.test(css), false, "no literal colour");
  const code = readFileSync(join(APP_DIR, "dist", "sync.js"), "utf8");
  assert.equal(/innerHTML|outerHTML|insertAdjacentHTML/.test(code), false);
}
