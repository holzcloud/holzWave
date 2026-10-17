// The touch-target sweep of the sidebar form, the card of a `wsh ai` request, the providers dialog and the row of the model on this
// computer: at 390 by 780 every control is at least 44 CSS pixels in both dimensions, at 1000 by 640 at least 28. Each state names
// the controls it must show, and a scan of every visible control of the panel finds one that the table forgot.
import assert from "node:assert/strict";
import {
  ASK, PROFILE, SAVED_A, SAVED_B, VIEW_CHAT_ONLY, askCard, boot, chatBlock, emitAsk, openDialog, openSide, saveChats,
} from "./ai-lib.mjs";
import { WINDOWS, measure } from "./aigate-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(8000);
  for (const window of WINDOWS) await panelsCase(page, url, window);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A gateway that waits for its review, with a key saved, and a server of the user's own on a public host (it has an address field). */
const ROUTER = {
  id: "router", name: "Router", preset: "openrouter", wire: "openai_chat", base_url: "https://openrouter.ai/api/v1", model: "m1",
  allow_lan_http: false, host: "openrouter.ai", key: "present", gateway_key: null, disclosure: null, locality: "cloud",
};
const OWN = {
  id: "own", name: "Own", preset: "custom", wire: "openai_chat", base_url: "https://llm.example.org/v1", model: "o1",
  allow_lan_http: false, host: "llm.example.org", key: "not_set", gateway_key: null, disclosure: null, locality: "cloud",
};

/** Add what is wrong with the controls of `scope` to `found` under the name of the state. */
async function sweep(page, window, found, state, selectors, scope) {
  const lines = await measure(page, { selectors, scope, min: window.min, narrow: window.narrow });
  for (const line of lines) found.push(`${window.name} / ${state}: ${line}`);
}

/** The form of the sidebar column with two stored chats and its list open. */
async function sidebarState(page, url, window, found) {
  await page.setViewportSize(window.size);
  await boot(page, url, { clock: true, profiles: [PROFILE] });
  await saveChats(page, [SAVED_A, SAVED_B]);
  await openSide(page);
  await page.locator(".ai-side-title").click();
  await page.waitForSelector("#ai-side-chats:not([hidden]) li.ai-side-row");
  await sweep(page, window, found, "sidebar", [".ai-side-back", ".ai-side-title", ".ai-side-new", ".ai-side-pick", ".ai-side-del"], "#sidebar");
  // The column remembers that it is open; the next state starts without it.
  await page.evaluate(() => localStorage.clear());
}

/** The card of a request of `wsh ai`. */
async function askState(page, url, window, found) {
  await page.setViewportSize(window.size);
  await boot(page, url, { clock: true });
  await emitAsk(page, ASK);
  await askCard(page).waitFor();
  await sweep(page, window, found, "ask card", [".ask-no", ".ask-yes"], "section.ask");
}

/** The chat that fills the window, with the providers the dialog needs. */
async function openChat(page, url, window, device = null) {
  await page.setViewportSize(window.size);
  await boot(page, url, {
    profiles: [ROUTER, OWN, PROFILE], view: VIEW_CHAT_ONLY, blocks: [chatBlock("chat-x")], starts: 1, settings: true, device,
  });
  await page.evaluate(() => {
    window.__MOCK__.ai.keys = { router: { provider: { scope: "https://openrouter.ai", length: 20 } } };
  });
  await page.waitForSelector(".block-chat");
  await page.locator(".chat-input").focus();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length >= 1);
  await page.waitForFunction(() => document.querySelector(".chat-setup")?.textContent.startsWith("Providers…"));
  await page.evaluate(() => window.__registry.runAction("ai.settings"));
  await openDialog(page);
}

/** The providers dialog with a gateway that waits for its review and has a saved key, then a server of the user's own. */
async function dialogState(page, url, window, found) {
  await openChat(page, url, window);
  await page.waitForFunction(() => document.querySelector(".aiset-allow") && !document.querySelector(".aiset-disclosure").hidden);
  const common = [
    "label.aiset-row", "label.aiset-option", ".aiset-add", ".aiset-save", ".aiset-remove", ".aiset-allow", ".aiset-close", ".aiset-key-clear",
    "#aiset-name", "#aiset-model", "#aiset-key",
  ];
  await sweep(page, window, found, "providers dialog: gateway", common, ".dlg.ai-settings");
  await page.locator('label.aiset-row[data-id="own"] input').check();
  await page.waitForSelector("#aiset-address");
  await sweep(page, window, found, "providers dialog: own server", ["#aiset-address", "#aiset-name", "#aiset-model", ".aiset-save"], ".dlg.ai-settings");
}

/** The row of the model on this computer in a state in which it offers `Check again`. */
async function deviceState(page, url, window, found) {
  await openChat(page, url, window, { state: "apple_intelligence_not_enabled", advice: "enable_apple_intelligence" });
  await page.waitForSelector(".aiset-dev-row");
  await sweep(page, window, found, "device row", [".aiset-dev-row", "button.aiset-dev-check"], ".dlg.ai-settings .aiset-device");
}

/** One window of the sweep: every state, then everything that was too small, together. */
async function panelsCase(page, url, window) {
  const found = [];
  for (const state of [sidebarState, askState, dialogState, deviceState]) {
    console.log(`aigate targets: ${window.name}: ${state.name}`);
    await state(page, url, window, found);
  }
  console.log(`aigate targets: ${window.name}: panels: ${found.length} problems`);
  assert.deepEqual(found, [], `targets of the panels in the ${window.name} window`);
}
