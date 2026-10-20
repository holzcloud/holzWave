// The toggle Auto of the chat (docs/design.md section 6.14): with Auto on, a send names the pane the chat acts for and carries
// a redacted offer of everything the core lists for it, with no card once the pane is shared or the core says sharing is not
// required. The page sends ids and never text, and it stores nothing about Auto.
import assert from "node:assert/strict";
import { VIEW_P1, boot, calls, ctxCalls, openAi, sendText } from "./ai-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await traceCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A page with one local profile, the settings core, two terminals and a chat beside pane 1, Auto on as the product starts. */
async function chatBesidePane1(page, url, extra = {}) {
  await boot(page, url, { view: VIEW_P1, settings: true, auto: true, ...extra });
  await openAi(page);
  await page.waitForFunction(() => document.activeElement?.matches("textarea.chat-input"));
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length >= 1);
}

/** Wait for the reply of the turn that was sent to end. */
async function settled(page, messages) {
  await page.waitForFunction(
    (n) => document.querySelectorAll(".chat-msg").length === n && !document.querySelector(".chat-partial"),
    messages,
  );
}

async function traceCase(page, url) {
  await chatBesidePane1(page, url, { context: { localContext: "auto" } });
  await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  assert.deepEqual(await ctxCalls(page), [], "no context command before a send");
  const toggle = page.locator("button.chat-ctx");
  assert.equal(await toggle.textContent(), "● Auto · pane 1");
  assert.equal(await toggle.getAttribute("aria-pressed"), "true");

  await sendText(page, "why did this fail");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  const order = await page.evaluate(() =>
    window.__MOCK__.calls.map((c) => c[0]).filter((c) => c.startsWith("ai_context_") || c === "ai_send"));
  assert.deepEqual(order, ["ai_context_state", "ai_context_preview", "ai_send"], "state, preview, send; no share");
  const previews = await calls(page, "ai_context_preview");
  assert.deepEqual(previews[0].kinds, ["selection", "screen", "last_command"], "every kind the state lists");
  assert.equal(previews[0].pane, 1);
  const sent = (await calls(page, "ai_send"))[0];
  assert.deepEqual(Object.keys(sent), ["conversation", "text", "pane", "profile", "model", "offer", "picked"]);
  assert.equal(sent.text, "why did this fail");
  assert.equal(sent.pane, 1);
  assert.match(sent.offer, /^[0-9a-f]{32}$/);
  assert.deepEqual(sent.picked, [1, 2]);

  await page.waitForSelector('.chat-msg[data-role="user"] p.chat-ctxnote');
  const note = await page.locator("p.chat-ctxnote").textContent();
  assert.ok(note.startsWith("Context sent"), note);
  assert.ok(note.includes("Screen of pane 1"), note);
  assert.equal(await page.locator(".chat-cards .chat-consent").count(), 0, "no card at any time");
  assert.equal((await ctxCalls(page)).some(([cmd]) => cmd === "ai_context_share"), false, "the core needed no share");
  await settled(page, 2);
}
