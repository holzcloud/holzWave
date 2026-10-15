// The sync on the page (design.md section 6.9): the title bar button says the state in words, the popover sets an
// account up (and shows why not), shows the last run and the conflicts, and runs the sync now. The stub plays the core.
import assert from "node:assert/strict";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await setupCase(page, url);
  await runCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const OFF = { configured: false, endpoint: "", user: "", state: "off", last: null, files: 0, conflicts: 0, error: null, restart: false };
const ON = { ...OFF, configured: true, endpoint: "https://cloud.example.org/remote.php/dav/files/holz/holzwave", user: "holz", state: "idle", last: 1760000000, files: 3, conflicts: 1 };

async function open(page, url, first) {
  await page.addInitScript((s) => {
    window.__SYNC = s;
    window.__MOCK__.on("sync_status", () => structuredClone(window.__SYNC));
    window.__MOCK__.on("sync_now", () => {
      if (window.__NOW_FAILS) throw window.__NOW_FAILS;
      window.__SYNC = { ...window.__SYNC, state: "idle", files: 2, conflicts: 0, restart: true };
      return structuredClone(window.__SYNC);
    });
    window.__MOCK__.on("sync_off", () => {
      window.__SYNC = { configured: false, endpoint: "", user: "", state: "off", last: null, files: 0, conflicts: 0, error: null, restart: false };
      return structuredClone(window.__SYNC);
    });
    window.__MOCK__.on("sync_save", (args) => {
      if (args.passphrase.length < 10) throw "The passphrase needs at least 10 characters.";
      window.__SYNC = { configured: true, endpoint: args.endpoint, user: args.user, state: "idle", last: 1760000000, files: 1, conflicts: 0, error: null, restart: false };
      return structuredClone(window.__SYNC);
    });
  }, first);
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForSelector(".sync-btn");
  await page.evaluate(async () => {
    window.__registry = await import(new URL("actions.js", location.href).href);
  });
}

async function setupCase(page, url) {
  await open(page, url, OFF);
  assert.equal(await page.locator(".sync-btn").textContent(), "☁ sync off", "the state is a word");
  const titles = await page.evaluate(() => window.__registry.listActions().map((a) => a.title));
  assert.ok(titles.includes("Sync…") && titles.includes("Sync now"));

  await page.locator(".sync-btn").click();
  await page.waitForSelector(".dlg.sync");
  assert.equal(await page.locator(".sync .sy-chip").textContent(), "not set up");
  await page.locator("#sy-endpoint").fill("https://cloud.example.org/remote.php/dav/files/holz/holzwave");
  await page.locator("#sy-user").fill("holz");
  await page.locator("#sy-password").fill("app-password");
  assert.equal(await page.locator("#sy-password").getAttribute("type"), "password", "secrets are not shown");
  await page.locator("#sy-passphrase").fill("short");
  await page.locator(".sy-connect").click();
  await page.waitForFunction(() => document.querySelector(".sync .set-status")?.textContent.includes("at least 10"));
  assert.equal(await page.locator(".sync .set-status").getAttribute("data-failed"), "true");
  assert.equal(await page.locator("#sy-passphrase").inputValue(), "", "the secrets are not filled in again");

  await page.locator("#sy-endpoint").fill("https://cloud.example.org/dav");
  await page.locator("#sy-user").fill("holz");
  await page.locator("#sy-password").fill("app-password");
  await page.locator("#sy-passphrase").fill("a long passphrase");
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => document.querySelector(".sync .sy-chip")?.textContent === "connected");
  assert.equal(await page.locator(".sync-btn").textContent(), "☁ synced");
  const sent = await page.evaluate(() => window.__MOCK__.callsOf("sync_save").at(-1));
  assert.deepEqual(sent, { endpoint: "https://cloud.example.org/dav", user: "holz", password: "app-password", passphrase: "a long passphrase" });
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.sync", { state: "detached" });
}

async function runCase(page, url) {
  await open(page, url, ON);
  assert.equal(await page.locator(".sync-btn").textContent(), "☁ synced");
  await page.locator(".sync-btn").click();
  await page.waitForSelector(".dlg.sync");
  const rows = await page.locator(".sync .sy-row").evaluateAll((n) => n.map((r) => r.textContent));
  assert.match(rows[0], /^Last sync\d\d:\d\d · 3 files$/);
  assert.match(rows[2], /^Conflicts1 · the other version is kept$/);
  assert.match(await page.locator(".sync .sy-sub").first().textContent(), /cloud\.example\.org\/remote\.php\/dav\/files\/holz\/holzwave · holz/);

  // A run that fails says why, in words, and the button says error.
  await page.evaluate(() => { window.__NOW_FAILS = "the passphrase is wrong for this sync folder"; window.__SYNC = { ...window.__SYNC, state: "error", error: "the passphrase is wrong for this sync folder" }; });
  await page.getByRole("button", { name: "Sync now" }).click();
  await page.waitForFunction(() => document.querySelector(".sync .set-status")?.textContent.includes("passphrase is wrong"));
  assert.equal(await page.locator(".sync-btn").textContent(), "☁ sync error");

  await page.evaluate(() => { window.__NOW_FAILS = null; window.__SYNC = { ...window.__SYNC, state: "idle", error: null }; });
  await page.getByRole("button", { name: "Sync now" }).click();
  await page.waitForFunction(() => document.querySelector(".sync .sy-restart"));
  assert.match(await page.locator(".sync .sy-restart").textContent(), /next start/);

  // Turning off needs a second click.
  const off = page.locator(".sync .sy-off");
  await off.click();
  assert.equal((await page.evaluate(() => window.__MOCK__.callsOf("sync_off"))).length, 0);
  await off.click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("sync_off").length === 1);
  await page.waitForFunction(() => document.querySelector(".sync .sy-chip")?.textContent === "not set up");
  assert.equal(await page.locator(".sync-btn").textContent(), "☁ sync off");
}
