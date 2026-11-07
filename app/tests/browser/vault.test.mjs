// The vault on the page (docs/design.md section 6.6, rules Lock chip to Vault lost). An Android WebView and the system's own prompt
// are not used: the platform is mocked (`app_info` answers `android`) and the six vault commands are played by the Tauri stub in the
// desktop engines. What a real device shows (the system prompt, a real period in the background, a real removal of the screen lock)
// is not covered here.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { APP_DIR, DIST_DIR, callsOf } from "./lib.mjs";
import { PROFILE } from "./ai-lib.mjs";

// The vault belongs to the Android shell; the web backend never reports `android`.
export const backends = ["tauri"];
export const backendNote = "the vault page belongs to the Android shell, which the web backend never is";

const { STRINGS } = await import(pathToFileURL(join(DIST_DIR, "vaultstate.js")).href);
const golden = JSON.parse(readFileSync(join(APP_DIR, "tests/fixtures/protocol.json"), "utf8"));

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(5000);
  await chipCase(page, url);
  await tabletCase(page, url);
  await flowCase(page, url);
  await cancelCase(page, url);
  await sendCase(page, url);
  await lostCase(page, url);
  await noticeCase(page, url);
  await preferencesCase(page, url);
  await syncCase(page, url);
  await turnOffWhenOpen(page, url);
  await visibilityCase(page, url);
  await otherPlatformsCase(page, url);
  await hostileCase(page, url);
  assert.deepEqual(errors, [], `the page reported errors (${engine})`);
}

const VIEW = {
  rev: 1,
  workspaces: [{ name: "main", active: true, panes: 2, pane_ids: [1, 2] }],
  tabs: [{ title: "shell", active: true, focus: 1, panes: 2, pane_ids: [1, 2] }],
  focus: 1,
  panes: [
    [1, { x: 0, y: 0, w: 0.5, h: 1 }],
    [2, { x: 0.5, y: 0, w: 0.5, h: 1 }],
  ],
  dividers: [{ split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0.5 }],
  pane_names: [],
  windows: 1,
};

/** The status words the host sends, with a change on top. */
const status = (over = {}) => ({
  state: "unlocked", protection: "auth", has_screen_lock: true, strongbox: false, can_protect: false, notice: "none", reason: null, ...over,
});
const LOCKED = status({ state: "locked" });
const OPEN = status();
const NO_LOCK = status({ protection: "plain", has_screen_lock: false, notice: "no_lock" });

const SYNC_PAUSED = {
  configured: true, endpoint: "https://cloud.example.org/remote.php/dav/files/holz/holzwave", user: "holz", state: "paused", last: 1760000000,
  files: 3, conflicts: 0, error: null, restart: false, removed: 0,
};

/**
 * Open the page as a device would and play the six vault commands. `vault` is the status the host starts with; `connect` fails with
 * the locked text until the vault is unlocked, like the host's. `sync` is the answer of `sync_status` (none: the stub's own).
 */
async function open(page, url, { os = "android", width = 390, height = 844, screen = [390, 844], vault = LOCKED, sync = null, view = VIEW, connections = [], raw = null } = {}) {
  await page.setViewportSize({ width, height });
  await page.addInitScript(([w, h]) => {
    for (const [key, value] of [["width", w], ["height", h], ["availWidth", w], ["availHeight", h]]) {
      Object.defineProperty(window.screen, key, { get: () => value, configurable: true });
    }
  }, screen);
  await page.addInitScript(
    ([info, start, shown, syncing, saved, settings, rawAnswer]) => {
      const mock = window.__MOCK__;
      mock.on("app_info", () => info);
      mock.setView(shown);
      mock.on("get_settings", () => structuredClone(settings));
      mock.on("connections_list", () => structuredClone(saved));
      const vault = { status: structuredClone(start), unlockFails: null, protectFails: null };
      window.__VAULT = vault;
      const now = () => structuredClone(vault.status);
      mock.on("vault_status", () => (rawAnswer !== null ? structuredClone(rawAnswer) : now()));
      mock.on("vault_unlock", () => {
        if (vault.unlockFails) throw vault.unlockFails;
        vault.status.state = "unlocked";
        return now();
      });
      mock.on("vault_lock", () => {
        vault.status.state = "locked";
        return now();
      });
      mock.on("vault_protect", ({ on }) => {
        if (vault.protectFails) throw vault.protectFails;
        vault.status.protection = on ? "auth" : "plain";
        vault.status.notice = on ? "none" : "no_lock";
        return now();
      });
      mock.on("vault_reset", () => {
        vault.status.state = "new";
        vault.status.reason = null;
        return now();
      });
      mock.on("vault_hidden", () => null);
      // The host's own refusals: a connection and an AI send need the vault open.
      mock.on("connect", () => {
        if (vault.status.state !== "unlocked") throw "secrets are locked";
        return mock.view();
      });
      if (syncing !== null) {
        mock.on("sync_status", () => structuredClone(syncing));
        mock.on("sync_now", () => structuredClone(syncing)); // the store is still locked: the run does not start and the state stays paused
      }
    },
    [{ version: "0.0.1-alpha1", os, hostname: "pixel" }, vault, view, sync, connections, golden.settings_view, raw],
  );
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 1);
  await page.waitForFunction(() => window.__MOCK__.callsOf("window_ready").length >= 1);
  await page.evaluate(async () => {
    window.__registry = await import(new URL("actions.js", location.href).href);
  });
  if (os === "android") await page.waitForFunction(() => window.__MOCK__.callsOf("vault_status").length >= 1);
}

const calls = (page, cmd) => callsOf(page, cmd);
const text = (page, selector) => page.locator(selector).first().textContent();
const present = (page, selector) => page.evaluate((s) => document.querySelector(s) !== null, selector);

/** Start the connection the way the palette does and keep the promise: `{ r }` is the answer, `{ e }` a failure. */
async function startConnect(page) {
  await page.evaluate(() => {
    window.__R = import(new URL("connections.js", location.href).href).then((m) => m.connect("c1")).then(
      (r) => ({ r }),
      (e) => ({ e: String(e) }),
    );
  });
}
const settled = (page) => page.evaluate(() => window.__R);

/** The chip of the page now: its word, state, glyph, border style and size. */
function chipFacts(page) {
  return page.evaluate(() => {
    const chip = document.getElementById("vault-chip");
    if (!chip) return null;
    const box = chip.getBoundingClientRect();
    return {
      word: chip.textContent,
      state: chip.dataset.state,
      glyph: chip.querySelector("svg")?.getAttribute("data-glyph") ?? null,
      border: getComputedStyle(chip).borderTopStyle,
      width: box.width,
      height: box.height,
      name: chip.getAttribute("aria-label"),
      parent: chip.parentElement?.id || chip.parentElement?.className,
    };
  });
}

async function chipCase(page, url) {
  const seen = [];
  for (const [vault, word, glyph, state, border] of [
    [LOCKED, "LOCKED", "lock-closed", "locked", "solid"],
    [OPEN, "OPEN", "lock-open", "open", "solid"],
    [NO_LOCK, "NO LOCK", "lock-bar", "none", "dashed"],
  ]) {
    await open(page, url, { vault });
    await page.waitForSelector("#vault-chip");
    const chip = await chipFacts(page);
    assert.equal(chip.word, word);
    assert.equal(chip.glyph, glyph);
    assert.equal(chip.state, state);
    assert.equal(chip.border, border, `${word}: the border of the chip`);
    assert.ok(chip.width >= 44 && chip.height >= 44, `${word}: the chip is ${chip.width} by ${chip.height}`);
    assert.equal(chip.parent, "phone-chip", "the chip sits in the place of the top strip");
    seen.push(chip);
    // Nothing opens at start-up by itself: no sheet, no dialog, no prompt.
    assert.equal(await page.locator(".vault-sheet, .vault-lost, .vault-off").count(), 0, `${word}: no sheet at start`);
    assert.equal((await calls(page, "vault_unlock")).length, 0, `${word}: no unlock at start`);
    assert.equal(await present(page, "#vault-answer"), false);
    assert.equal(await page.evaluate(() => [...document.querySelectorAll('link[rel="stylesheet"]')].some((l) => l.href.endsWith("vault.css"))), true);
  }
  assert.equal(new Set(seen.map((c) => c.glyph)).size, 3, "three glyphs");
  assert.equal(new Set(seen.map((c) => c.word)).size, 3, "three words");
  assert.deepEqual(seen.map((c) => c.name), [STRINGS.locked, STRINGS.unlocked, STRINGS.notProtected]);

  // A new vault that protects with the screen lock holds nothing yet: no chip, and the empty place stays.
  await open(page, url, { vault: status({ state: "new" }) });
  assert.equal(await present(page, "#vault-chip"), false);
  assert.equal(await present(page, "#phone-chip"), true, "the place is kept");
  // A host with no vault shows nothing either.
  await open(page, url, { vault: status({ state: "none", protection: "plain" }) });
  assert.equal(await present(page, "#vault-chip"), false);
}

async function tabletCase(page, url) {
  // On a tablet the same chip is a segment of the status line.
  await open(page, url, { vault: LOCKED, width: 1000, height: 700, screen: [1000, 700] });
  await page.waitForSelector("#status #vault-chip");
  const chip = await chipFacts(page);
  assert.equal(chip.word, "LOCKED");
  assert.ok(chip.width >= 44 && chip.height >= 44, `${chip.width} by ${chip.height}`);
  assert.equal(await present(page, "#phone-chip"), false, "no phone strip on a tablet");
}

async function flowCase(page, url) {
  await open(page, url, { vault: LOCKED });
  await startConnect(page);
  await page.waitForSelector(".vault-sheet");
  // The locked answer is the quiet line where the pane was to open, and it is not an error.
  assert.equal(await text(page, "#vault-answer"), "Locked: unlock to connect");
  assert.equal(await page.evaluate(() => getComputedStyle(document.getElementById("vault-answer")).color === getComputedStyle(document.body).color), false, "dim, not the ink");
  assert.equal((await calls(page, "connect")).length, 1);
  // The sheet only starts the system prompt: no field of any kind.
  assert.equal(await page.locator(".vault-sheet input, .vault-sheet textarea, .vault-sheet select, .vault-sheet [contenteditable]").count(), 0, "no input element");
  assert.equal(await text(page, ".vault-sheet .dlg-title"), STRINGS.sheetTitle);
  assert.equal(await text(page, ".vault-sheet .vault-line"), STRINGS.sheetLine);
  assert.deepEqual(await page.locator(".vault-sheet button").allTextContents(), [STRINGS.unlock, STRINGS.notNow]);
  for (const box of await page.locator(".vault-sheet button").evaluateAll((all) => all.map((b) => b.getBoundingClientRect().toJSON()))) {
    assert.ok(box.width >= 44 && box.height >= 44, `a button of the sheet is ${box.width} by ${box.height}`);
  }
  const place = await page.evaluate(() => {
    const box = document.querySelector(".vault-sheet").getBoundingClientRect();
    return { bottom: box.bottom, height: innerHeight, width: box.width, viewport: innerWidth };
  });
  assert.ok(place.bottom >= place.height - 1 && place.width >= place.viewport - 1, "a sheet from the bottom edge, the full width");
  assert.equal(await page.evaluate(() => document.activeElement?.textContent), STRINGS.unlock, "the keyboard starts on Unlock");

  await page.locator(".vault-sheet button", { hasText: STRINGS.unlock }).click();
  assert.deepEqual(await settled(page), { r: null }, "the connection opened after the unlock");
  assert.equal((await calls(page, "vault_unlock")).length, 1);
  assert.deepEqual(await calls(page, "connect"), [{ id: "c1" }, { id: "c1" }], "the same command, once more, with the same arguments");
  assert.equal(await page.locator(".vault-sheet").count(), 0);
  assert.equal(await present(page, "#vault-answer"), false, "the line goes after the unlock");
  assert.equal((await chipFacts(page)).word, "OPEN");

  // A tap on the open chip locks at once; a tap on the locked chip opens the sheet by itself, with nothing to repeat.
  await page.locator("#vault-chip").click();
  await page.waitForFunction(() => document.getElementById("vault-chip")?.textContent === "LOCKED");
  assert.equal((await calls(page, "vault_lock")).length, 1);
  await page.locator("#vault-chip").click();
  await page.waitForSelector(".vault-sheet");
  await page.locator(".vault-sheet button", { hasText: STRINGS.unlock }).click();
  await page.waitForFunction(() => document.getElementById("vault-chip")?.textContent === "OPEN");
  assert.equal((await calls(page, "connect")).length, 2, "nothing was repeated: nothing waited");

  // Two identical requests while the sheet is open are one action: one repeat.
  await page.locator("#vault-chip").click();
  await page.waitForFunction(() => document.getElementById("vault-chip")?.textContent === "LOCKED");
  await page.evaluate(() => {
    window.__R = Promise.all([import(new URL("connections.js", location.href).href).then((m) => Promise.all([m.connect("c1"), m.connect("c1")]))]).then((r) => ({ r }), (e) => ({ e: String(e) }));
  });
  await page.waitForSelector(".vault-sheet");
  assert.equal(await page.locator(".vault-sheet").count(), 1, "one sheet");
  await page.locator(".vault-sheet button", { hasText: STRINGS.unlock }).click();
  assert.deepEqual(await settled(page), { r: [[null, null]] });
  assert.equal((await calls(page, "connect")).length, 2 + 2 + 1, "two first tries and one repeat");
}

async function cancelCase(page, url) {
  await open(page, url, { vault: LOCKED });
  await page.evaluate(() => {
    window.__VAULT.unlockFails = "cancelled";
  });
  await startConnect(page);
  await page.waitForSelector(".vault-sheet");
  await page.locator(".vault-sheet button", { hasText: STRINGS.unlock }).click();
  await page.waitForFunction((word) => document.querySelector(".vault-sheet .vault-quiet")?.textContent === word, STRINGS.notUnlocked);
  assert.equal(await page.locator(".vault-sheet").count(), 1, "the sheet stays");
  assert.equal((await calls(page, "connect")).length, 1, "a cancelled prompt does not repeat the command");
  assert.equal(await page.locator(".vault-sheet button", { hasText: STRINGS.unlock }).isEnabled(), true, "it can be tried again");
  // Leaving the sheet gives the caller the quiet answer; the command is not repeated.
  await page.locator(".vault-sheet button", { hasText: STRINGS.notNow }).click();
  assert.deepEqual(await settled(page), { r: STRINGS.answerConnect });
  assert.equal((await calls(page, "connect")).length, 1);
  assert.equal(await page.locator(".vault-sheet").count(), 0);
  assert.equal((await chipFacts(page)).word, "LOCKED", "still locked");

  // Escape leaves the sheet the same way; a failure that is not the cancel word is also only `Not unlocked`.
  await startConnect(page);
  await page.waitForSelector(".vault-sheet");
  await page.evaluate(() => {
    window.__VAULT.unlockFails = "failed";
  });
  await page.locator(".vault-sheet button", { hasText: STRINGS.unlock }).click();
  await page.waitForFunction((word) => document.querySelector(".vault-sheet .vault-quiet")?.textContent === word, STRINGS.notUnlocked);
  await page.keyboard.press("Escape");
  assert.deepEqual(await settled(page), { r: STRINGS.answerConnect });
  assert.equal(await page.locator(".vault-sheet").count(), 0);
  assert.equal((await calls(page, "connect")).length, 2);
}

async function sendCase(page, url) {
  // An AI send that fails locked: the chat says so quietly, the sheet opens, and the same send is repeated once after the unlock.
  await page.addInitScript(
    ([profile, shown]) => {
      window.__MOCK__.ai.profiles = [profile];
      window.__BLOCKS = [];
      window.__MOCK__.on("pane_blocks", () => structuredClone(window.__BLOCKS));
      window.__MOCK__.on("block_open", ({ kind, source, path }) => {
        const v = window.__MOCK__.view();
        const at = v.panes.find(([id]) => id === v.focus) ?? v.panes[v.panes.length - 1];
        const made = Math.max(...v.panes.map(([id]) => id)) + 1;
        const half = at[1].w / 2;
        at[1].w = half;
        v.panes.push([made, { x: at[1].x + half, y: at[1].y, w: half, h: at[1].h }]);
        v.focus = made;
        v.tabs[0].focus = made;
        v.tabs[0].panes = v.panes.length;
        v.workspaces[0].panes = v.panes.length;
        window.__MOCK__.setView(v);
        window.__BLOCKS.push({ pane: made, kind, source, path, source_label: "This computer" });
        return window.__MOCK__.view();
      });
      window.__MOCK__.on("ai_send", () => {
        if (window.__VAULT.status.state !== "unlocked") throw { code: "key", reason: "locked", text: "secrets are locked" };
        return null;
      });
    },
    [PROFILE, VIEW],
  );
  await open(page, url, { vault: LOCKED, width: 1000, height: 700, screen: [1000, 700] });
  await page.evaluate(() => window.__registry.runAction("ai.open"));
  await page.waitForSelector("textarea.chat-input");
  // The product starts with `Auto` on, which asks the core about the pane first; this case is about the send alone.
  for (const toggle of await page.locator("button.chat-ctx[aria-pressed=true]").all()) await toggle.click();
  await page.locator(".chat-input").fill("hello there");
  await page.locator(".chat-input").press("Enter");
  await page.waitForSelector(".vault-sheet");
  await page.waitForFunction((word) => document.querySelector(".chat-status")?.textContent === word, STRINGS.answerSend);
  assert.equal(await page.locator(".chat-status").getAttribute("data-kind"), "note", "quiet, not an error");
  assert.equal((await calls(page, "ai_send")).length, 1);
  await page.locator(".vault-sheet button", { hasText: STRINGS.unlock }).click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 2);
  const sent = await calls(page, "ai_send");
  assert.deepEqual(sent[1], sent[0], "the same send, once more");
  await page.waitForFunction(() => document.querySelector(".chat-input")?.value === "");
  assert.equal(await page.locator(".vault-sheet").count(), 0);
  assert.equal((await calls(page, "vault_unlock")).length, 1);
}

async function lostCase(page, url) {
  await open(page, url, { vault: status({ state: "lost", reason: "key_gone" }) });
  await page.waitForSelector("#vault-chip");
  assert.equal((await chipFacts(page)).word, "LOCKED", "a lost vault says Locked until the person chooses");
  assert.equal(await page.locator(".vault-lost").count(), 0, "no dialog at start");
  // The first act that needs the vault opens the sheet; the unlock learns that the key is gone and the dialog replaces the sheet.
  await page.evaluate(() => {
    window.__VAULT.unlockFails = "lost";
  });
  await page.locator("#vault-chip").click();
  await page.waitForSelector(".vault-sheet");
  await page.locator(".vault-sheet button", { hasText: STRINGS.unlock }).click();
  await page.waitForSelector(".vault-lost");
  assert.equal(await page.locator(".vault-sheet").count(), 0);
  assert.equal(await text(page, ".vault-lost .dlg-title"), STRINGS.lostTitle);
  assert.deepEqual(await page.locator(".vault-lost p").allTextContents(), [STRINGS.lostLine, STRINGS.lostNote]);
  assert.deepEqual(await page.locator(".vault-lost button").allTextContents(), [STRINGS.startFresh, STRINGS.notNow]);
  assert.equal(await page.locator(".vault-lost input").count(), 0);
  // `Not now` leaves everything as it was.
  await page.locator(".vault-lost button", { hasText: STRINGS.notNow }).click();
  await page.waitForSelector(".vault-lost", { state: "detached" });
  assert.equal((await calls(page, "vault_reset")).length, 0);
  assert.equal((await chipFacts(page)).word, "LOCKED");
  // `Start fresh` throws the old vault away.
  await page.locator("#vault-chip").click();
  await page.waitForSelector(".vault-sheet");
  await page.locator(".vault-sheet button", { hasText: STRINGS.unlock }).click();
  await page.waitForSelector(".vault-lost");
  await page.locator(".vault-lost button", { hasText: STRINGS.startFresh }).click();
  await page.waitForSelector(".vault-lost", { state: "detached" });
  assert.equal((await calls(page, "vault_reset")).length, 1);
  await page.waitForSelector("#vault-chip", { state: "detached" });
}

async function noticeCase(page, url) {
  // No screen lock and nothing the system could switch to: the notice with `Dismiss` alone, once per start.
  await open(page, url, { vault: NO_LOCK });
  await page.waitForSelector("#vault-notice");
  assert.equal(await text(page, "#vault-notice .vault-line"), STRINGS.notice);
  assert.deepEqual(await page.locator("#vault-notice button").allTextContents(), [STRINGS.dismiss], "no `Use protected mode` without a screen lock");
  for (const box of await page.locator("#vault-notice button").evaluateAll((all) => all.map((b) => b.getBoundingClientRect().toJSON()))) {
    assert.ok(box.width >= 44 && box.height >= 44, `${box.width} by ${box.height}`);
  }
  assert.equal(await page.locator(".vault-sheet").count(), 0, "the notice is not the sheet");
  await page.locator("#vault-notice button").click();
  await page.waitForSelector("#vault-notice", { state: "detached" });
  assert.equal((await chipFacts(page)).word, "NO LOCK", "the chip keeps saying it");
  // A later status does not bring it back during this start; a tap on the chip does.
  await page.evaluate(() => {
    Object.defineProperty(document, "hidden", { get: () => false, configurable: true });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await page.waitForFunction(() => window.__MOCK__.callsOf("vault_status").length >= 2);
  assert.equal(await present(page, "#vault-notice"), false, "once per start");
  await page.locator("#vault-chip").click();
  await page.waitForSelector("#vault-notice");
  assert.equal(await text(page, "#vault-notice .vault-line"), STRINGS.notice);

  // A screen lock was set in the meantime: the same words with `Use protected mode` beside `Dismiss`.
  await open(page, url, { vault: { ...NO_LOCK, can_protect: true, has_screen_lock: true } });
  await page.waitForSelector("#vault-notice");
  assert.deepEqual(await page.locator("#vault-notice button").allTextContents(), [STRINGS.useProtected, STRINGS.dismiss]);
  await page.locator("#vault-notice button", { hasText: STRINGS.useProtected }).click();
  await page.waitForSelector("#vault-notice", { state: "detached" });
  assert.deepEqual(await calls(page, "vault_protect"), [{ on: true }]);
  assert.equal((await chipFacts(page)).word, "OPEN");
}

const settingsOpen = async (page) => {
  await page.evaluate(() => window.__registry.runAction("settings.open"));
  await page.waitForSelector(".dlg.settings");
};

async function preferencesCase(page, url) {
  await open(page, url, { vault: OPEN });
  await settingsOpen(page);
  await page.waitForSelector("fieldset.vault-group");
  const labels = await page.locator(".dlg.settings fieldset.set-group > legend").allTextContents();
  assert.deepEqual(labels.slice(-2), ["Accessibility", STRINGS.group], "the group sits after Accessibility");
  assert.equal(await text(page, "#vault-row-label"), STRINGS.rowLabel);
  assert.equal(await text(page, "#vault-row-help"), STRINGS.rowHelp);
  const sw = page.locator("#vault-switch");
  assert.equal(await sw.getAttribute("aria-checked"), "true", "on by default: the vault reflects the host");
  assert.equal(await sw.textContent(), STRINGS.switchOn, "the state is a word too");
  const box = await sw.boundingBox();
  assert.ok(box.width >= 44 && box.height >= 44, `${box.width} by ${box.height}`);
  assert.equal(await sw.getAttribute("aria-labelledby"), "vault-row-label");

  // Turning it off asks first; `Keep on` changes nothing.
  await sw.click();
  await page.waitForSelector(".vault-off");
  assert.equal(await text(page, ".vault-off .vault-line"), STRINGS.offLine);
  assert.equal(await text(page, ".vault-off .vault-quiet"), STRINGS.offScope);
  assert.deepEqual(await page.locator(".vault-off button").allTextContents(), [STRINGS.turnOff, STRINGS.keepOn]);
  assert.equal(await page.evaluate(() => document.activeElement?.textContent), STRINGS.keepOn, "the safe answer has the keyboard first");
  await page.locator(".vault-off button", { hasText: STRINGS.keepOn }).click();
  await page.waitForSelector(".vault-off", { state: "detached" });
  assert.equal((await calls(page, "vault_protect")).length, 0);
  assert.equal(await sw.getAttribute("aria-checked"), "true");
  // Escape is `Keep on` too, and does not close the Settings dialog behind it.
  await sw.click();
  await page.waitForSelector(".vault-off");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".vault-off", { state: "detached" });
  assert.equal((await calls(page, "vault_protect")).length, 0);
  assert.equal(await page.locator(".dlg.settings").count(), 1);
  // Only `Turn off` calls the command.
  await sw.click();
  await page.waitForSelector(".vault-off");
  await page.locator(".vault-off button", { hasText: STRINGS.turnOff }).click();
  await page.waitForFunction(() => document.getElementById("vault-switch")?.getAttribute("aria-checked") === "false");
  assert.deepEqual(await calls(page, "vault_protect"), [{ on: false }]);
  assert.equal(await sw.textContent(), STRINGS.switchOff);
  // Turning it on needs no question.
  await sw.click();
  await page.waitForFunction(() => document.getElementById("vault-switch")?.getAttribute("aria-checked") === "true");
  assert.deepEqual(await calls(page, "vault_protect"), [{ on: false }, { on: true }]);
  // A device fact: no setting of the settings file carries it.
  const settingCalls = JSON.stringify([...(await calls(page, "set_setting")), ...(await calls(page, "change_setting"))]);
  assert.ok(!/protect|vault|secret/i.test(settingCalls), `no setting call carries the vault: ${settingCalls}`);

  // On every other platform the group does not exist.
  await open(page, url, { os: "linux" });
  await settingsOpen(page);
  assert.equal(await page.locator("fieldset.vault-group").count(), 0);
  assert.equal((await calls(page, "vault_status")).length, 0);
}

async function syncCase(page, url) {
  await open(page, url, { vault: LOCKED, sync: SYNC_PAUSED });
  await page.waitForFunction(() => document.querySelector(".sync-btn")?.textContent === "☁ sync paused");
  assert.equal(await text(page, ".sync-btn"), STRINGS.syncButton);
  await page.evaluate(() => window.__registry.runAction("sync.open"));
  await page.waitForSelector(".dlg.sync .set-status");
  const line = await page.evaluate(() => {
    const note = document.querySelector(".dlg.sync .set-status");
    const probe = (token) => {
      const node = document.createElement("span");
      node.style.color = `var(${token})`;
      document.body.append(node);
      const color = getComputedStyle(node).color;
      node.remove();
      return color;
    };
    const button = document.querySelector(".sync-btn");
    return {
      text: note.textContent,
      mark: note.querySelector(".sy-pause")?.textContent,
      failed: note.dataset.failed,
      quiet: note.dataset.quiet,
      color: getComputedStyle(note).color,
      dim: probe("--hw-dim"),
      danger: probe("--hw-danger"),
      buttonColor: getComputedStyle(button).color,
      buttonDecoration: getComputedStyle(button).textDecorationLine,
      chip: document.querySelector(".dlg.sync .sy-chip").textContent,
      chipColor: getComputedStyle(document.querySelector(".dlg.sync .sy-chip")).color,
    };
  });
  assert.equal(line.text, `${STRINGS.syncMark}${STRINGS.syncLine}`);
  assert.equal(line.mark, STRINGS.syncMark, "the pause mark");
  assert.equal(line.failed, "false", "not an error");
  assert.equal(line.quiet, "true");
  assert.equal(line.color, line.dim, "the dim colour");
  assert.notEqual(line.color, line.danger);
  assert.equal(line.buttonDecoration, "none", "the underline belongs to the error");
  assert.notEqual(line.buttonColor, line.danger);
  assert.equal(line.chip, "paused");
  assert.notEqual(line.chipColor, line.danger);
  // `Sync now` while the store is still locked says nothing wrong: the line stays the paused line.
  await page.locator(".dlg.sync button", { hasText: "Sync now" }).click();
  await page.waitForFunction(() => !document.querySelector(".dlg.sync button.primary[disabled]"));
  assert.equal(await text(page, ".dlg.sync .set-status"), `${STRINGS.syncMark}${STRINGS.syncLine}`);
  await turnOffWhileLocked(page);
}

/** While the secrets are locked `Turn off` waits, so that no sync secret is left behind in the vault; its reason is the paused line. */
async function turnOffWhileLocked(page) {
  const off = await page.evaluate(() => {
    const b = [...document.querySelectorAll(".dlg.sync button")].find((n) => n.textContent === "Turn off");
    const others = [...document.querySelectorAll(".dlg.sync button")].filter((n) => n !== b);
    const note = document.querySelector(".dlg.sync .set-status");
    const box = b?.getBoundingClientRect();
    return b && {
      disabled: b.disabled, border: getComputedStyle(b).borderTopStyle, opacity: Number(getComputedStyle(b).opacity),
      describedBy: b.getAttribute("aria-describedby"), noteId: note.id, reason: note.textContent,
      others: others.map((n) => ({ text: n.textContent, disabled: n.disabled, border: getComputedStyle(n).borderTopStyle })),
      high: box.height,
    };
  });
  assert.ok(off, "the popover has a Turn off button");
  assert.equal(off.disabled, true, "Turn off is disabled while the secrets are locked");
  assert.equal(off.border, "dashed", "a second cue besides the dim colour: a dashed border");
  assert.ok(off.opacity < 1, "and it is dim");
  assert.equal(off.describedBy, off.noteId, "the paused line is the description of the button");
  assert.equal(off.reason, `${STRINGS.syncMark}${STRINGS.syncLine}`, "and that line is the existing one, with no new word");
  for (const other of off.others) {
    assert.equal(other.border === "dashed", false, `${other.text} does not look disabled`);
  }
  assert.equal(off.others.find((n) => n.text === "Close")?.disabled, false, "Close stays available");
  assert.equal(off.others.find((n) => n.text === "Sync now")?.disabled, false, "Sync now stays available");
  await page.locator(".dlg.sync button", { hasText: "Turn off" }).click({ force: true, timeout: 500 }).catch(() => {});
  assert.equal(await page.evaluate(() => window.__MOCK__.callsOf("sync_off").length), 0, "nothing turns the sync off while locked");
}

/** Once the secrets are open the same button works as before: it asks with a second click, and only that one turns the sync off. */
async function turnOffWhenOpen(page, url) {
  await open(page, url, { vault: OPEN, sync: { ...SYNC_PAUSED, state: "idle" } });
  await page.waitForFunction(() => document.querySelector(".sync-btn"));
  await page.evaluate(() => window.__registry.runAction("sync.open"));
  await page.waitForSelector(".dlg.sync .set-status");
  const off = page.locator(".dlg.sync .sy-off");
  assert.equal(await off.isDisabled(), false, "Turn off works when the secrets are open");
  assert.equal(await off.evaluate((n) => getComputedStyle(n).borderTopStyle), "solid", "and has the plain border");
  assert.equal(await off.evaluate((n) => n.hasAttribute("aria-describedby")), false);
  await off.click();
  assert.equal(await off.textContent(), "Click again to turn off", "it still asks with a second click");
  assert.equal(await page.evaluate(() => window.__MOCK__.callsOf("sync_off").length), 0, "the first click turns nothing off");
}

async function visibilityCase(page, url) {
  await open(page, url, { vault: OPEN });
  await page.waitForFunction(() => window.__MOCK__.callsOf("vault_hidden").length >= 1);
  assert.deepEqual(await calls(page, "vault_hidden"), [{ hidden: false }], "the state at start");
  const flip = (hidden) =>
    page.evaluate((value) => {
      Object.defineProperty(document, "hidden", { get: () => value, configurable: true });
      document.dispatchEvent(new Event("visibilitychange"));
    }, hidden);
  await flip(true);
  await flip(false);
  await page.waitForFunction(() => window.__MOCK__.callsOf("vault_hidden").length >= 3);
  assert.deepEqual(await calls(page, "vault_hidden"), [{ hidden: false }, { hidden: true }, { hidden: false }]);
  // A shown window reads the status again: the five-minute rule may have locked the vault meanwhile.
  await page.evaluate(() => {
    window.__VAULT.status.state = "locked";
  });
  await flip(true);
  await flip(false);
  await page.waitForFunction(() => document.getElementById("vault-chip")?.textContent === "LOCKED");
}

async function otherPlatformsCase(page, url) {
  for (const os of ["macos", "web", "windows", "linux"]) {
    await open(page, url, { os, vault: LOCKED });
    await page.waitForTimeout(100);
    assert.equal(await present(page, "#vault-chip"), false, `${os}: no chip`);
    assert.equal(await present(page, "#vault-top"), false, `${os}: no notice place`);
    assert.equal((await calls(page, "vault_status")).length, 0, `${os}: no vault_status`);
    assert.equal((await calls(page, "vault_hidden")).length, 0, `${os}: no vault_hidden`);
    assert.equal(
      await page.evaluate(() => [...document.querySelectorAll('link[rel="stylesheet"]')].some((l) => l.href.endsWith("vault.css"))),
      false,
      `${os}: vault.css is not loaded`,
    );
    assert.equal(
      await page.evaluate(() => performance.getEntriesByType("resource").some((r) => /\/vault\.(js|css)$/.test(r.name))),
      false,
      `${os}: vault.js and vault.css are not fetched`,
    );
    assert.equal(await page.locator("#status .vault-seg").count(), 0, `${os}: no status segment`);
  }
}

async function hostileCase(page, url) {
  const hostile = '<img src=x onerror="window.__pwned=1">';
  // A name with markup does not reach the sheet, the notice or the chip, and the answer of the host is matched, never shown.
  const view = structuredClone(VIEW);
  view.workspaces[0].name = hostile;
  view.tabs[0].title = hostile;
  view.pane_names = [[1, hostile]];
  await open(page, url, { vault: LOCKED, view, connections: [{ id: "c1", name: hostile, kind: "ssh", host: hostile, port: 22, user: hostile, auth: "agent" }] });
  await startConnect(page);
  await page.waitForSelector(".vault-sheet");
  assert.equal(await page.locator("#vault-top img, .vault-sheet img, #vault-chip img, .vault-notice img").count(), 0);
  assert.equal(await page.evaluate(() => window.__pwned), undefined);
  await page.locator(".vault-sheet button", { hasText: STRINGS.notNow }).click();
  await settled(page);
  // A status with markup in a field is no status: no chip, no element, no handler.
  await open(page, url, { raw: { state: hostile, protection: "auth", has_screen_lock: true, strongbox: false, can_protect: false, notice: "none", reason: hostile } });
  assert.equal(await present(page, "#vault-chip"), false);
  await open(page, url, { raw: { ...NO_LOCK, reason: hostile } });
  assert.equal(await present(page, "#vault-chip"), false);
  assert.equal(await page.evaluate(() => window.__pwned), undefined);
  assert.equal(await page.locator("img").count(), 0);
}
