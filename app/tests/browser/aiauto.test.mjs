// The toggle Auto of the chat (docs/design.md section 6.14): with Auto on, a send names the pane the chat acts for and carries
// a redacted offer of everything the core lists for it, with no card once the pane is shared or the core says sharing is not
// required. The page sends ids and never text, and it stores nothing about Auto.
import assert from "node:assert/strict";
import { AUTO_ASK_WORDS, NOTES } from "../../dist/consentstate.js";
import { PANE_KEY, PANE_TEXT, PROFILE, VIEW_CHAT_ONLY, VIEW_P1, boot, calls, chatBlock, ctxCalls, openAi, openContext, sendText } from "./ai-lib.mjs";

const FIVE = ["conversation", "text", "pane", "profile", "model"];

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await traceCase(page, url);
  await askCase(page, url);
  await toggleCase(page, url);
  await handCase(page, url);
  await faultCase(page, url);
  await noPaneCase(page, url);
  await privacyCase(page, url);
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

/** The text of the status line. */
const status = (page) => page.locator(".chat-status").textContent();

/** The focused pane changes to `pane`, as the core announces it. */
async function focusPane(page, pane) {
  await page.evaluate((n) => {
    const v = window.__MOCK__.view();
    v.focus = n;
    v.tabs[0].focus = n;
    window.__MOCK__.setView(v);
    window.__MOCK__.emit("layout", v);
  }, pane);
  await page.waitForFunction((n) => document.querySelector("button.chat-ctx")?.textContent.endsWith(`pane ${n}`), pane);
}

/** The commands of the context, by name, oldest first. */
const ctxNames = async (page) => (await ctxCalls(page)).map(([cmd]) => cmd);

async function askCase(page, url) {
  await chatBesidePane1(page, url);
  // The first send for a pane and provider that need consent opens the card and sends nothing.
  await sendText(page, "why did this fail");
  await page.waitForSelector('.chat-cards .chat-consent[data-mode="ask"]');
  assert.deepEqual(await ctxNames(page), ["ai_context_state"], "a state call only: nothing is read before the share");
  assert.ok((await page.locator(".chat-consent").textContent()).includes(AUTO_ASK_WORDS), "the card says what Auto does");
  assert.equal(await status(page), NOTES.shareFirst);
  assert.equal(await page.locator(".chat-input").inputValue(), "why did this fail", "the draft stays");
  assert.equal((await calls(page, "ai_send")).length, 0);

  // Share pane 1: the card closes, the status line says to press Send again, and nothing is sent by the share.
  await page.locator(".consent-share").click();
  await page.waitForFunction(() => document.querySelector(".chat-consent") === null);
  assert.equal(await status(page), NOTES.shared);
  assert.equal((await calls(page, "ai_context_share")).length, 1);
  assert.equal((await calls(page, "ai_send")).length, 0, "the share does not resend");
  assert.equal(await page.evaluate(() => document.activeElement?.className), "chat-ctx", "the focus returns to the toggle");

  // Send again: state, preview and the send with an offer, no card.
  await page.locator(".chat-input").press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  assert.deepEqual(await ctxNames(page), ["ai_context_state", "ai_context_share", "ai_context_state", "ai_context_preview"]);
  const sent = (await calls(page, "ai_send"))[0];
  assert.deepEqual(Object.keys(sent), [...FIVE, "offer", "picked"]);
  assert.deepEqual(sent.picked, [1, 2]);
  assert.equal(await page.locator(".chat-consent").count(), 0);
  await settled(page, 2);

  // The pane shows nothing new: the core refuses the same content twice, the page sends the message without it and says so.
  await sendText(page, "and now");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 3);
  const [, withIt, without] = await calls(page, "ai_send");
  assert.deepEqual(Object.keys(withIt), [...FIVE, "offer", "picked"]);
  assert.deepEqual(Object.keys(without), FIVE, "the retry carries no offer");
  assert.equal(without.text, "and now");
  await page.waitForFunction((t) => document.querySelector(".chat-status")?.textContent === t, NOTES.unchanged);
  assert.equal(await page.locator("p.chat-ctxnote").count(), 1, "only the first message shows a line");
  await settled(page, 4);

  // The share is per pane: another pane asks again.
  await focusPane(page, 2);
  assert.equal(await page.locator("button.chat-ctx").textContent(), "● Auto · pane 2");
  await sendText(page, "pane two");
  await page.waitForSelector('.chat-cards .chat-consent[data-mode="ask"]');
  assert.equal((await calls(page, "ai_send")).length, 3, "nothing was sent");
  assert.ok((await page.locator(".chat-consent").textContent()).includes("Share pane 2"));
  const states = await calls(page, "ai_context_state");
  assert.equal(states[states.length - 1].pane, 2);
  assert.equal(await page.locator(".chat-input").inputValue(), "pane two");

  // No keeps the card closed and the message in the field.
  await page.locator(".consent-no").click();
  assert.equal(await page.locator(".chat-consent").count(), 0);
  assert.equal((await calls(page, "ai_context_share")).length, 1, "No shares nothing");
}

async function toggleCase(page, url) {
  await chatBesidePane1(page, url);
  const toggle = page.locator("button.chat-ctx");
  // A press flips the toggle and asks the core nothing.
  await toggle.click();
  assert.equal(await toggle.textContent(), "○ Auto");
  assert.equal(await toggle.getAttribute("aria-pressed"), "false");
  assert.equal(await toggle.getAttribute("aria-label"), "Auto");
  await toggle.click();
  assert.equal(await toggle.textContent(), "● Auto · pane 1");
  assert.equal(await toggle.getAttribute("aria-pressed"), "true");
  assert.equal(await toggle.getAttribute("aria-label"), "Auto · pane 1");
  assert.equal(await page.locator(".chat-consent").count(), 0, "a press never opens a card");
  assert.deepEqual(await ctxCalls(page), [], "a press asks the core nothing");
  assert.equal(await page.evaluate(() => localStorage.length + sessionStorage.length), 0, "nothing is stored");

  // Auto opened a card: switching Auto off closes it and the message goes out plain.
  await sendText(page, "first");
  await page.waitForSelector('.chat-consent[data-mode="ask"]');
  await toggle.click();
  assert.equal(await page.locator(".chat-consent").count(), 0, "the question ended with Auto");
  const before = (await ctxCalls(page)).length;
  await page.locator(".chat-input").press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  assert.deepEqual(Object.keys((await calls(page, "ai_send"))[0]), FIVE, "off: five plain keys");
  assert.equal((await ctxCalls(page)).length, before, "off: no context command");
  await settled(page, 2);

  // On again: the next send asks, because nothing is shared yet.
  await toggle.click();
  await sendText(page, "second");
  await page.waitForSelector('.chat-consent[data-mode="ask"]');
  assert.equal((await calls(page, "ai_send")).length, 1);
  await page.locator(".consent-share").click();
  await page.waitForFunction(() => document.querySelector(".chat-consent") === null);
  // Changed content, so the offer is not a repeat.
  await page.evaluate(() => (window.__MOCK__.ai.context.panes[1].items.screen.text = "changed"));
  await page.locator(".chat-input").press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 2);
  assert.deepEqual(Object.keys((await calls(page, "ai_send"))[1]), [...FIVE, "offer", "picked"], "on: it attaches again");
}

async function handCase(page, url) {
  await chatBesidePane1(page, url);
  // A card the person opened by hand decides what goes with its message; Auto adds nothing.
  await openContext(page);
  await page.locator(".consent-share").click();
  await page.waitForSelector('.chat-consent[data-mode="pick"]');
  await page.locator('label.consent-kind[data-kind="selection"] input').check().catch(() => {});
  await page.locator('label.consent-kind[data-kind="last_command"] input').check();
  await page.locator(".consent-preview").click();
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  const states = (await calls(page, "ai_context_state")).length;
  const previews = (await calls(page, "ai_context_preview")).length;
  await sendText(page, "explain");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  const sent = (await calls(page, "ai_send"))[0];
  assert.deepEqual(Object.keys(sent), [...FIVE, "offer", "picked"]);
  assert.deepEqual(sent.picked, [1], "the one item the hand-made offer holds, not every kind");
  assert.equal((await calls(page, "ai_context_state")).length, states, "Auto made no state call");
  assert.equal((await calls(page, "ai_context_preview")).length, previews, "Auto made no preview call");
  assert.match(await page.locator("p.chat-ctxnote").textContent(), /Last command output of pane 1/);
  await settled(page, 2);

  // A hand-made offer with every item unticked sends plain, and Auto does not step in.
  await openContext(page);
  await page.locator('label.consent-kind[data-kind="screen"] input').check();
  await page.locator(".consent-preview").click();
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  await page.locator('li.consent-item[data-id="1"] input').uncheck();
  const before = (await ctxCalls(page)).length;
  await page.locator(".chat-input").fill("plain");
  await page.locator(".chat-input").press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 2);
  assert.deepEqual(Object.keys((await calls(page, "ai_send"))[1]), FIVE);
  assert.equal((await ctxCalls(page)).length, before, "no further state or preview call");
}

async function faultCase(page, url) {
  await chatBesidePane1(page, url, { context: { localContext: "auto" } });
  // A refusal other than the repeat stops the send, keeps the draft and says why.
  await page.evaluate(() => window.__MOCK__.ai.context.fail("ai_context_preview", "busy"));
  await sendText(page, "will not go");
  await page.waitForFunction((t) => document.querySelector(".chat-status")?.textContent === t, "The pane did not answer within two seconds. Try again.");
  assert.equal(await page.locator(".chat-status").getAttribute("data-kind"), "error");
  assert.equal((await calls(page, "ai_send")).length, 0, "nothing was sent");
  assert.equal(await page.locator(".chat-input").inputValue(), "will not go");

  // Nothing to share is not a fault: the message goes out plain.
  await page.evaluate(() => window.__MOCK__.ai.context.fail("ai_context_preview", "nothing_to_share"));
  await page.locator(".chat-input").press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  assert.deepEqual(Object.keys((await calls(page, "ai_send"))[0]), FIVE);
  await settled(page, 2);

  // A refused state stops the send too.
  await page.evaluate(() => {
    window.__MOCK__.ai.context.fail("ai_context_preview", null);
    window.__MOCK__.ai.context.fail("ai_context_state", "local_only");
  });
  await sendText(page, "also not");
  await page.waitForFunction((t) => document.querySelector(".chat-status")?.textContent === t, "Local only is on, and this provider is not on this computer.");
  assert.equal((await calls(page, "ai_send")).length, 1);
  assert.equal(await page.locator(".chat-input").inputValue(), "also not");
}

async function noPaneCase(page, url) {
  await boot(page, url, {
    profiles: [PROFILE], view: VIEW_CHAT_ONLY, blocks: [chatBlock("chat-x")], auto: true, starts: 1,
    states: { "chat-x": { title: "Earlier question", profile: "local-1", model: "llama3.2" } },
  });
  await page.waitForSelector(".block-chat");
  await page.locator(".chat-input").focus();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length >= 1);
  assert.equal(await page.locator("button.chat-ctx").textContent(), "● Auto · no pane");
  assert.equal(await page.locator("button.chat-ctx").getAttribute("aria-pressed"), "true");
  await sendText(page, "no terminal here");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  assert.deepEqual(Object.keys((await calls(page, "ai_send"))[0]), FIVE);
  assert.deepEqual(await ctxCalls(page), [], "no context command without a pane");
}

async function privacyCase(page, url) {
  await chatBesidePane1(page, url, { context: { localContext: "auto" } });
  await sendText(page, "explain");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  await settled(page, 2);
  const everything = await page.evaluate(() => {
    const attrs = [];
    for (const n of document.querySelectorAll("*")) for (const a of n.attributes) attrs.push(a.value);
    const texts = [];
    const walker = document.createTreeWalker(document.documentElement, NodeFilter.SHOW_TEXT);
    for (let n = walker.nextNode(); n; n = walker.nextNode()) texts.push(n.data);
    const store = [];
    for (const area of [localStorage, sessionStorage]) for (let i = 0; i < area.length; i++) store.push(area.key(i), area.getItem(area.key(i)));
    return JSON.stringify({ calls: window.__MOCK__.calls, attrs, texts, store, cookie: document.cookie });
  });
  for (const canary of [PANE_TEXT, PANE_KEY, "zqconsent5e90b3a"]) assert.ok(!everything.includes(canary), `${canary} reached the page`);
  assert.equal(await page.evaluate(() => localStorage.length + sessionStorage.length), 0, "nothing is stored");
}
