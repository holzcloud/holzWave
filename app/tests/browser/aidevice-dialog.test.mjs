// The group `On device` of the AI providers dialog (docs/design.md section 6.21): it tells why the on-device model is not
// available, in sentences of the page's own chosen by the shell's state and advice words, and `Check again` reads the list
// once more. The sentences below are the design record's; the page's tables are held to them here and to the golden of the
// bridge crate in the unit tests.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { OTHER_PROFILE, PROFILE, VIEW_CHAT_ONLY, boot, calls, chatBlock, openAi, openDialog, setDevice } from "./ai-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await groupCase(page, url);
  await absentCase(page, url);
  await hostileCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** The golden the bridge crate writes from its enums: every state the shell can send, with its advice word. */
const golden = JSON.parse(readFileSync(new URL("../fixtures/ai_device.json", import.meta.url), "utf8"));

/** The reason of each state and the sentence of each advice word, as the design record spells them. */
const REASONS = {
  apple_intelligence_not_enabled: "Apple Intelligence is switched off.",
  device_not_eligible: "This Mac cannot run Apple Intelligence.",
  model_not_ready: "The model is not ready yet.",
  os_too_old: "This needs macOS 26 or later.",
  sdk_lacks_framework: "This build of holzWave was made without Apple's model framework.",
  other: "The on-device model is not available.",
};
const ADVICE = {
  enable_apple_intelligence: "Turn it on in System Settings, then choose Check again.",
  wait_for_model: "It may still be downloading. Try again in a few minutes.",
  not_supported: "Choose another provider.",
  update_macos: "Update macOS, then choose Check again.",
  not_in_this_build: "Use a build made with Xcode 26 or later.",
  unknown: "The reason is not known. Check again later.",
};

const dlg = ".dlg.ai-settings";
const count = async (page, cmd) => (await calls(page, cmd)).length;
const text = (page, selector) => page.locator(selector).first().textContent();

/** Open a chat with the saved provider and the model in `device` (or none), then the dialog. */
async function chatAndDialog(page, url, device, profiles = [PROFILE]) {
  await boot(page, url, { profiles, device, settings: true });
  await openAi(page);
  await page.waitForFunction(() => document.activeElement?.matches("textarea.chat-input"));
  await openDialogNow(page);
}

async function openDialogNow(page) {
  await page.evaluate(() => window.__registry.runAction("ai.settings"));
  await openDialog(page);
}

async function closeDialog(page) {
  await page.keyboard.press("Escape");
  await page.waitForSelector(dlg, { state: "detached" });
}

async function groupCase(page, url) {
  await chatAndDialog(page, url, { state: "available", advice: "none" });
  const group = page.locator("fieldset.aiset-device");
  assert.equal(await group.count(), 1);
  // Between Privacy and Providers, in this order.
  const order = await page.evaluate(() => {
    const privacy = document.querySelector(".aiset-privacy");
    const device = document.querySelector(".aiset-device");
    const list = document.querySelector(".aiset-list");
    const follows = (a, b) => (a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0;
    return { afterPrivacy: follows(privacy, device), beforeList: follows(device, list), role: device.getAttribute("role"), label: device.getAttribute("aria-labelledby") };
  });
  assert.deepEqual(order, { afterPrivacy: true, beforeList: true, role: "group", label: "aiset-device-legend" });
  assert.equal(await text(page, ".aiset-device > legend"), "On device");
  assert.equal(await page.locator("#aiset-device-legend").count(), 1);
  assert.equal(await page.locator(".aiset-dev-row").count(), 1);
  const row = page.locator(".aiset-dev-row");
  assert.equal(await row.getAttribute("data-state"), "available");
  assert.equal(await row.getAttribute("data-tone"), "ok");
  assert.equal(await row.getAttribute("tabindex"), "-1");
  assert.equal(await text(page, ".aiset-dev-name"), "Apple on-device");
  assert.equal(await text(page, ".aiset-dev-meta"), "system model");
  assert.equal(await text(page, ".aiset-dev-state"), "available");
  assert.equal(await text(page, ".aiset-dev-advice"), "The model runs on this Mac.");
  assert.equal(await page.locator("button.aiset-dev-check").count(), 0, "nothing to check while it answers");
  assert.equal(await count(page, "ai_profiles"), 2, "the chat read once and the dialog read once");

  // Every other state the shell can send: its reason, its advice and the button, in the warn tone.
  for (const { state, advice } of golden.states.filter((s) => s.state !== "available")) {
    await closeDialog(page);
    await setDevice(page, state, advice);
    await openDialogNow(page);
    const shown = page.locator(".aiset-dev-row");
    assert.equal(await shown.getAttribute("data-state"), state);
    assert.equal(await shown.getAttribute("data-tone"), "warn", state);
    assert.equal(await text(page, ".aiset-dev-state"), REASONS[state], state);
    assert.equal(await text(page, ".aiset-dev-advice"), ADVICE[advice], state);
    assert.equal(await text(page, "button.aiset-dev-check"), "Check again");
    // The mark and the underline are cues besides the colour.
    const cues = await page.locator(".aiset-dev-state").evaluate((n) => ({
      mark: getComputedStyle(n, "::before").content, line: getComputedStyle(n).textDecorationLine,
    }));
    assert.match(cues.mark, /!/);
    assert.equal(cues.line, "underline");
    // The keyboard reaches the button.
    await page.locator(dlg).focus();
    for (let i = 0; i < 60 && !(await page.evaluate(() => document.activeElement?.matches(".aiset-dev-check"))); i++) {
      await page.keyboard.press("Tab");
    }
    assert.equal(await page.evaluate(() => document.activeElement?.matches(".aiset-dev-check")), true, `Tab reaches the button for ${state}`);
  }

  // `Check again` reads the list once more and says so; nothing in the group changes while the state is the same.
  await closeDialog(page);
  await setDevice(page, "apple_intelligence_not_enabled", "enable_apple_intelligence");
  await openDialogNow(page);
  const before = await count(page, "ai_profiles");
  const words = () => page.locator(".aiset-device").evaluate((n) => n.innerText);
  const same = await words();
  await page.locator("#aiset-name").fill("Typed before the check");
  await page.locator("button.aiset-dev-check").click();
  await page.waitForFunction(() => document.querySelector(".aiset-status")?.textContent === "Checked.");
  assert.equal(await count(page, "ai_profiles"), before + 1, "exactly one more read");
  assert.equal(await words(), same, "the group is unchanged");
  assert.equal(await page.locator("#aiset-name").inputValue(), "Typed before the check", "a form that was being typed stays");
  assert.equal(await page.locator("button.aiset-dev-check").count(), 1);

  // The model comes back: the same press shows it, the button is gone and the keyboard is on the row.
  await setDevice(page, "available", "none");
  await page.locator("button.aiset-dev-check").focus();
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => document.querySelector(".aiset-dev-state")?.textContent === "available");
  assert.equal(await count(page, "ai_profiles"), before + 2);
  assert.equal(await page.locator("button.aiset-dev-check").count(), 0);
  assert.equal(await page.locator(".aiset-dev-row").getAttribute("data-tone"), "ok");
  assert.equal(await page.evaluate(() => document.activeElement === document.querySelector(".aiset-dev-row")), true, "the keyboard moved to the row");
  assert.equal(await text(page, ".aiset-status"), "Checked.");

  // The chat has the answer without a reload: its chip became a button and its picker holds the row of the model.
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.tagName === "BUTTON");
  await closeDialog(page);

  // Opened from the bar of the chat, the dialog takes the keys: a character typed in it reaches no terminal, and Escape gives
  // the keyboard back to the composer.
  await page.locator(".chat-setup").click();
  await openDialog(page);
  assert.equal(await page.locator(".aiset-dev-row").getAttribute("data-state"), "available");
  const keys = await count(page, "key");
  await page.keyboard.type("x");
  assert.equal(await count(page, "key"), keys, "nothing reached a terminal");
  await closeDialog(page);
  await page.waitForFunction(() => document.activeElement?.matches("textarea.chat-input"));
  await page.locator(".chat-chip").click();
  await page.waitForSelector(".chat-picker");
  assert.deepEqual(await page.locator(".chat-choice").evaluateAll((rows) => rows.map((r) => r.textContent)), [
    "○Localllama3.2localhost", "○Apple on-devicesystem",
  ]);
}

async function absentCase(page, url) {
  // Without a model of this Mac the dialog has no trace of Apple, and opening it reads the list once, as before.
  await boot(page, url, { settings: true });
  await openAi(page);
  await page.waitForFunction(() => document.activeElement?.matches("textarea.chat-input"));
  const before = await count(page, "ai_profiles");
  await openDialogNow(page);
  assert.equal(await count(page, "ai_profiles"), before + 1, "one read on opening");
  assert.equal(await page.locator(".aiset-device").count(), 0, "no group");
  assert.equal(await page.locator(dlg).evaluate((n) => n.innerText.includes("Apple")), false, "no word of Apple");
  assert.equal(await page.locator(dlg).evaluate((n) => n.querySelectorAll("button.aiset-dev-check, .aiset-dev-row").length), 0);

  // A model that goes away again: a reread removes the group.
  await closeDialog(page);
  await setDevice(page, "apple_intelligence_not_enabled", "enable_apple_intelligence");
  await openDialogNow(page);
  assert.equal(await page.locator(".aiset-device").count(), 1);
  await setDevice(page, null);
  await page.locator("button.aiset-dev-check").click();
  await page.waitForFunction(() => document.querySelectorAll(".aiset-device").length === 0);
  assert.equal(await page.locator(dlg).evaluate((n) => n.innerText.includes("Apple")), false);
  assert.equal(await page.locator(".aiset-status").textContent(), "Checked.");
}

async function hostileCase(page, url) {
  await boot(page, url, { profiles: [PROFILE, OTHER_PROFILE], view: VIEW_CHAT_ONLY, blocks: [chatBlock("chat-x")], starts: 1, settings: true });
  await page.waitForSelector(".block-chat");
  const answer = (onDevice) =>
    page.evaluate((member) => {
      window.__MOCK__.on("ai_profiles", () => ({ profiles: [], notes: [], on_device: JSON.parse(member) }));
    }, JSON.stringify(onDevice));

  // A markup state and a `javascript:` advice read as `other` and `unknown`; nothing of them reaches the page.
  await answer({ id: "apple", model: "system", state: "<img src=x onerror=window.__XSS=1>", advice: "javascript:alert(1)" });
  await openDialogNow(page);
  const row = page.locator(".aiset-dev-row");
  assert.equal(await row.getAttribute("data-state"), "other");
  assert.equal(await row.getAttribute("data-tone"), "warn");
  assert.equal(await text(page, ".aiset-dev-state"), REASONS.other);
  assert.equal(await text(page, ".aiset-dev-advice"), ADVICE.unknown);
  const hostile = await page.evaluate(() => {
    const box = document.querySelector(".dlg.ai-settings");
    return {
      images: box.querySelectorAll("img").length,
      handlers: [...box.querySelectorAll("*")].filter((n) => [...n.attributes].some((a) => a.name.startsWith("on"))).length,
      xss: typeof window.__XSS,
      leaked: document.body.innerText.includes("onerror") || document.body.innerHTML.includes("onerror"),
    };
  });
  assert.deepEqual(hostile, { images: 0, handlers: 0, xss: "undefined", leaked: false });
  await closeDialog(page);

  // A member with another id, an array and a string draw no group.
  for (const member of [
    { id: "evil", model: "system", state: "available", advice: "none" },
    [{ id: "apple", model: "system", state: "available", advice: "none" }],
    "available",
  ]) {
    await page.evaluate((json) => {
      window.__MOCK__.on("ai_profiles", () => ({ profiles: [], notes: [], on_device: JSON.parse(json) }));
    }, JSON.stringify(member));
    await openDialogNow(page);
    assert.equal(await page.locator(".aiset-device").count(), 0, JSON.stringify(member));
    await closeDialog(page);
  }

  // An own `__proto__` member beside a valid body changes nothing: the group is drawn as usual.
  await page.evaluate(() => {
    const body = '{"id":"apple","model":"system","state":"available","advice":"none","__proto__":{"polluted":true}}';
    window.__MOCK__.on("ai_profiles", () => ({ profiles: [], notes: [], on_device: JSON.parse(body) }));
  });
  await openDialogNow(page);
  assert.equal(await page.locator(".aiset-dev-row").getAttribute("data-state"), "available");
  assert.equal(await text(page, ".aiset-dev-state"), "available");
  assert.equal(await page.evaluate(() => ({}).polluted), undefined, "nothing was added to Object.prototype");
  await closeDialog(page);
}
