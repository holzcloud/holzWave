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
  await narrowCase(page, url);
  await modesCase(page, url);
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

/** A phone-sized or desktop page with a chat and the saved provider; the model of this Mac says `device`. */
async function sized(page, url, size, device) {
  await page.setViewportSize(size);
  await boot(page, url, { profiles: [PROFILE], view: VIEW_CHAT_ONLY, blocks: [chatBlock("chat-x")], starts: 1, settings: true, device });
  await page.waitForSelector(".block-chat");
  await page.locator(".chat-input").focus();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length >= 1);
}

const heightOf = (page, selector) => page.locator(selector).first().evaluate((n) => n.getBoundingClientRect().height);
const topOf = (page, selector) => page.locator(selector).first().evaluate((n) => n.getBoundingClientRect().top);

async function narrowCase(page, url) {
  await sized(page, url, { width: 390, height: 780 }, { state: "available", advice: "none" });

  // The picker row of the model is a 44 px target on a phone.
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.tagName === "BUTTON");
  await page.locator(".chat-chip").click();
  await page.waitForSelector(".chat-picker");
  const row = page.locator(".chat-choice", { hasText: "Apple on-device" });
  assert.equal(await row.count(), 1);
  assert.ok((await row.evaluate((n) => n.getBoundingClientRect().height)) >= 44 - 0.5, "the picker row is 44 px high");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".chat-picker", { state: "detached" });

  // The dialog is the screen, and the row of the model stacks its parts in targets of at least 44 px.
  await setDevice(page, "apple_intelligence_not_enabled", "enable_apple_intelligence");
  await openDialogNow(page);
  const sheet = await page.locator(dlg).evaluate((n) => {
    const r = n.getBoundingClientRect();
    return { w: r.width, h: r.height, vw: innerWidth, vh: innerHeight };
  });
  assert.deepEqual([sheet.w, sheet.h], [sheet.vw, sheet.vh], "the dialog is as wide and as high as the screen");
  assert.ok((await heightOf(page, ".aiset-dev-row")) >= 44 - 0.5, "the row is 44 px high");
  assert.ok((await heightOf(page, "button.aiset-dev-check")) >= 44 - 0.5, "the button is 44 px high");
  const tops = [await topOf(page, ".aiset-dev-name"), await topOf(page, ".aiset-dev-meta"), await topOf(page, ".aiset-dev-state"), await topOf(page, ".aiset-dev-advice"), await topOf(page, "button.aiset-dev-check")];
  for (let i = 1; i < tops.length; i++) assert.ok(tops[i] > tops[i - 1], `the parts stack: ${tops}`);
  // Nothing is wider than the sheet.
  assert.ok(await page.locator(".aiset-device").evaluate((n) => n.scrollWidth <= n.clientWidth + 1), "the group fits the sheet");

  // A desktop window: the button is a 28 px target and the box leaves room around it.
  await page.setViewportSize({ width: 1000, height: 640 });
  assert.ok((await heightOf(page, "button.aiset-dev-check")) >= 28 - 0.5, "the button is 28 px high on the desktop");
  const box = await page.locator(dlg).evaluate((n) => ({ w: n.getBoundingClientRect().width, vw: innerWidth }));
  assert.ok(box.vw - box.w >= 40, `the box leaves ${box.vw - box.w} px of the window`);
  const parts = [await topOf(page, ".aiset-dev-advice"), await topOf(page, "button.aiset-dev-check")];
  assert.ok(parts[1] > parts[0], "the button is below the advice on the desktop too");
  await page.setViewportSize({ width: 1000, height: 640 });
}

async function modesCase(page, url) {
  await sized(page, url, { width: 1000, height: 640 }, { state: "model_not_ready", advice: "wait_for_model" });
  await openDialogNow(page);
  const edge = () => page.locator("fieldset.aiset-device").evaluate((n) => getComputedStyle(n).borderTopColor);
  const root = (add, name) => page.evaluate(([a, n]) => document.documentElement.classList[a ? "add" : "remove"](n), [add, name]);
  const duration = () => page.locator("button.aiset-dev-check").evaluate((n) => getComputedStyle(n).transitionDuration);

  // Higher contrast: the hairline of the group follows `--hw-line`, which that mode swaps.
  const normal = await edge();
  await root(true, "contrast-high");
  assert.notEqual(await edge(), normal, "the hairline of the group follows the token");
  await root(false, "contrast-high");
  assert.equal(await edge(), normal);

  // No motion at all, with or without the setting.
  assert.equal(await duration(), "0s");
  await root(true, "motion-reduce");
  assert.equal(await duration(), "0s");
  await root(false, "motion-reduce");

  // Forced colours: the style sheet outlines the row with a system colour.
  const css = await (await fetch(`${url}aisettings.css`)).text();
  const forced = css.slice(css.indexOf("@media (forced-colors: active)"));
  assert.ok(forced.startsWith("@media"), "aisettings.css has a forced-colors block");
  assert.match(forced, /\.aiset-dev-row[^{]*\{[^}]*outline[^}]*(Highlight|CanvasText)/);

  // The button focused with the keyboard has a 2 px ring; the group draws no gradient and no filter.
  await page.locator("button.aiset-dev-check").focus();
  await page.keyboard.press("Shift+Tab");
  await page.keyboard.press("Tab");
  assert.equal(await page.evaluate(() => document.activeElement?.matches(".aiset-dev-check")), true);
  assert.equal(await page.locator("button.aiset-dev-check").evaluate((n) => getComputedStyle(n).outlineWidth), "2px");
  const flat = await page.locator(".aiset-device, .aiset-device *").evaluateAll((nodes) =>
    nodes.filter((n) => {
      const css = getComputedStyle(n);
      return css.backgroundImage.includes("gradient") || css.filter !== "none" || css.boxShadow !== "none";
    }).length,
  );
  assert.equal(flat, 0, "no gradient, filter or shadow in the group");
  // The mark of a state that needs attention is part of the state word.
  assert.match(await page.locator(".aiset-dev-state").evaluate((n) => getComputedStyle(n, "::before").content), /!/);
}
