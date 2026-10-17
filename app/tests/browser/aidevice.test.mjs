// The on-device model in the chat (docs/design.md section 6.21): the picker has the row `Apple on-device` only while the
// shell says `available`, a send for it leaves as `ai_send` with the profile `apple` and no model, a chat that is pinned to it
// while it is gone sends nothing and asks for another provider, and nothing is ever sent to another provider by itself.
import assert from "node:assert/strict";
import { OTHER_PROFILE, PROFILE, VIEW3, boot, calls, chatBlock, openAi, sendText, setDevice } from "./ai-lib.mjs";

const AVAILABLE = { state: "available", advice: "none" };
const OFF = { state: "apple_intelligence_not_enabled", advice: "enable_apple_intelligence" };
const SENTENCE = "Apple on-device is not available. Choose another provider, or see the provider settings for the reason.";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await rowCase(page, url);
  await onlyCase(page, url);
  await lazyCase(page, url);
  await pinnedCase(page, url);
  await staleCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A chat restored with the pin `apple` and `system` in pane 3, next to the two terminals. */
const restored = (device, profiles = [PROFILE]) => ({
  view: VIEW3,
  blocks: [chatBlock("chat-x")],
  states: { "chat-x": { title: "Earlier question", profile: "apple", model: "system" } },
  profiles,
  device,
});

const texts = (page, selector) => page.locator(selector).evaluateAll((nodes) => nodes.map((n) => n.textContent));

async function rowCase(page, url) {
  await boot(page, url, { device: AVAILABLE });
  await openAi(page);
  // The list is read once, at the first focus of the composer.
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length === 1);
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.tagName === "BUTTON");
  assert.equal((await calls(page, "ai_profiles")).length, 1, "exactly one read");

  // The picker lists the saved provider, then the on-device model with its model and no host.
  await page.locator(".chat-chip").click();
  await page.waitForSelector(".chat-picker");
  assert.equal(await page.locator('.chat-picker .chat-choice[role="radio"]').count(), 2);
  assert.deepEqual(await texts(page, ".chat-choice"), ["○Localllama3.2localhost", "○Apple on-devicesystem"]);
  assert.equal(await page.locator(".chat-choice").nth(1).locator(".chat-choice-host").textContent(), "", "no host");
  await page.locator(".chat-choice").nth(1).click();
  assert.equal(await page.locator(".chat-picker").count(), 0, "a click on the row closes the picker");
  assert.equal(await page.locator(".chat-chip").textContent(), "Apple on-device · system");

  await sendText(page, "hello");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  const sent = (await calls(page, "ai_send"))[0];
  assert.deepEqual(Object.keys(sent).sort(), ["conversation", "model", "pane", "profile", "text"], "exactly five arguments");
  assert.deepEqual(sent, { conversation: "chat-mock-1", text: "hello", pane: 2, profile: "apple", model: null });
  await page.waitForFunction(() => document.querySelectorAll(".chat-msg").length === 2 && !document.querySelector(".chat-partial"));

  // After the turn the bar names the route the core told: no arrow, and the place word comes from the route.
  assert.equal(await page.locator(".chat-chip").textContent(), "Apple on-device · system");
  assert.equal(await page.locator(".chat-where").textContent(), "on device");
  assert.notEqual(await page.locator(".chat-where").getAttribute("data-leaves"), "true");
  assert.ok(!(await page.locator(".chat-chip").textContent()).includes("→"));

  // The same page with a model that cannot answer: the picker has no such row and nothing names Apple.
  await boot(page, url, { profiles: [PROFILE, OTHER_PROFILE], device: OFF });
  await openAi(page);
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.tagName === "BUTTON");
  await page.locator(".chat-chip").click();
  await page.waitForSelector(".chat-picker");
  assert.deepEqual(await texts(page, ".chat-choice"), ["○Localllama3.2localhost", "○Cloudbig-modelapi.example.org"]);
  assert.equal(await page.locator("body").evaluate((b) => b.innerText.includes("Apple")), false, "no row names Apple");

  await boot(page, url, { device: OFF });
  await openAi(page);
  await page.waitForFunction(() => /Local/.test(document.querySelector(".chat-chip")?.textContent ?? ""));
  assert.equal(await page.locator(".chat-chip").evaluate((n) => n.tagName), "SPAN");
  assert.equal(await page.locator(".chat-picker").count(), 0);
  assert.equal(await page.locator("body").evaluate((b) => b.innerText.includes("Apple")), false);
}

async function onlyCase(page, url) {
  // No saved provider and a model that can answer: it is the only provider, so the chip is plain text and Enter sends.
  await boot(page, url, { profiles: [], device: AVAILABLE });
  await openAi(page);
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.textContent === "Apple on-device · system");
  assert.equal(await page.locator(".chat-chip").evaluate((n) => n.tagName), "SPAN");
  assert.equal(await page.locator(".chat-picker").count(), 0);
  assert.equal(await page.locator(".chat-setup").textContent(), "Providers…");
  await sendText(page, "only one");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  assert.equal((await calls(page, "ai_send"))[0].profile, "apple");
  assert.equal((await calls(page, "ai_send")).length, 1);

  // No saved provider and no model: nothing to send to, and the answer has exactly the two members it always had.
  await boot(page, url, { profiles: [] });
  await openAi(page);
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.textContent === "no provider set up");
  assert.equal(await page.locator(".chat-setup").textContent(), "Set up…");
  const members = await page.evaluate(async () => Object.keys(await window.__TAURI__.core.invoke("ai_profiles")).sort());
  assert.deepEqual(members, ["notes", "profiles"]);
}

async function lazyCase(page, url) {
  // A chat restored with the pin reads no list at mount and shows the pin from its snapshot.
  await boot(page, url, restored(AVAILABLE));
  await page.waitForSelector('.block[data-pane="3"].block-chat');
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.textContent !== "");
  await page.waitForTimeout(500);
  assert.equal((await calls(page, "ai_profiles")).length, 0, "no list at mount or after it");
  assert.equal(await page.locator(".chat-chip").textContent(), "Apple on-device · system");

  // The first focus reads the list once; the sends that follow read nothing more.
  await page.locator(".chat-input").focus();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length === 1);
  await sendText(page, "first");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  assert.equal((await calls(page, "ai_send"))[0].profile, "apple");
  await page.waitForFunction(() => document.querySelectorAll(".chat-msg").length === 2 && !document.querySelector(".chat-partial"));
  await sendText(page, "second");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 2);
  assert.equal((await calls(page, "ai_send"))[1].profile, "apple");
  assert.equal((await calls(page, "ai_profiles")).length, 1, "no more reads");

  // A send that never focused the composer reads the list once first: a pin of the model needs a loaded list.
  await boot(page, url, restored(AVAILABLE));
  await page.waitForSelector('.block[data-pane="3"].block-chat');
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.textContent !== "");
  assert.equal((await calls(page, "ai_profiles")).length, 0);
  await page.evaluate(() => {
    const input = document.querySelector(".chat-input");
    input.value = "without a focus";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
  });
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  assert.equal((await calls(page, "ai_profiles")).length, 1, "the list was read once, before the send");
  assert.equal((await calls(page, "ai_send"))[0].profile, "apple");
}

async function pinnedCase(page, url) {
  // Pinned to the model while it cannot answer: the send says why, offers the other provider and keeps the draft.
  await boot(page, url, restored(OFF));
  await page.waitForSelector('.block[data-pane="3"].block-chat');
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.textContent !== "");
  assert.equal((await calls(page, "ai_profiles")).length, 0, "none at mount");
  await page.locator(".chat-input").focus();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length === 1);
  await page.locator(".chat-input").fill("Explain this error");
  await page.locator(".chat-input").press("Enter");
  await page.waitForFunction((text) => document.querySelector(".chat-status")?.textContent === text, SENTENCE);
  assert.equal(await page.locator(".chat-status").getAttribute("data-kind"), "error");
  assert.equal((await calls(page, "ai_send")).length, 0, "nothing was sent");
  await page.waitForSelector(".chat-picker");
  assert.deepEqual(await texts(page, ".chat-choice"), ["○Localllama3.2localhost"], "the other provider is offered");
  assert.equal(await page.locator(".chat-input").inputValue(), "Explain this error", "the draft stays");

  // The person chooses it, and only then does a send go out, to that provider.
  await page.locator(".chat-choice").first().click();
  assert.equal(await page.locator(".chat-picker").count(), 0);
  await page.locator(".chat-input").press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  const sent = await calls(page, "ai_send");
  assert.equal(sent.length, 1);
  assert.equal(sent[0].profile, "local-1");
  assert.ok(sent.every((s) => s.profile !== "apple"), "nothing was sent to apple at any time");

  // With no saved provider the same Enter says the same, opens no picker and keeps the draft.
  await boot(page, url, restored(OFF, []));
  await page.waitForSelector('.block[data-pane="3"].block-chat');
  await page.locator(".chat-input").focus();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length === 1);
  await page.locator(".chat-input").fill("Explain this error");
  await page.locator(".chat-input").press("Enter");
  await page.waitForFunction((text) => document.querySelector(".chat-status")?.textContent === text, SENTENCE);
  assert.equal(await page.locator(".chat-status").getAttribute("data-kind"), "error");
  assert.equal((await calls(page, "ai_send")).length, 0);
  assert.equal(await page.locator(".chat-picker").count(), 0, "there is nothing to choose");
  assert.equal(await page.locator(".chat-input").inputValue(), "Explain this error");
}

async function staleCase(page, url) {
  // The page chose the model while it could answer; then the state changed and the page does not know.
  await boot(page, url, { device: AVAILABLE });
  await openAi(page);
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.tagName === "BUTTON");
  await page.locator(".chat-chip").click();
  await page.waitForSelector(".chat-picker");
  await page.locator(".chat-choice").nth(1).click();
  await setDevice(page, "model_not_ready", "wait_for_model");

  await page.locator(".chat-input").fill("too late");
  await page.locator(".chat-input").press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  const sent = await calls(page, "ai_send");
  assert.deepEqual(sent, [{ conversation: "chat-mock-1", text: "too late", pane: 2, profile: "apple", model: null }]);
  // The core refuses it, and the page shows the core's sentence: the draft stays and nothing goes anywhere else.
  await page.waitForFunction(() => document.querySelector(".chat-status")?.textContent === "no provider is chosen");
  assert.equal(await page.locator(".chat-status").getAttribute("data-kind"), "error");
  assert.equal(await page.locator(".chat-input").inputValue(), "too late");
  await page.waitForTimeout(300);
  assert.equal((await calls(page, "ai_send")).length, 1, "no other send follows");
  assert.equal(await page.locator(".chat-msg").count(), 0, "no message was added");

  // Opening the picker reads the list again, and the row of the model is gone.
  const before = (await calls(page, "ai_profiles")).length;
  await page.locator(".chat-chip").click();
  await page.waitForSelector(".chat-picker");
  assert.equal((await calls(page, "ai_profiles")).length, before + 1, "the list was read again");
  assert.deepEqual(await texts(page, ".chat-choice"), ["○Localllama3.2localhost"]);
}
