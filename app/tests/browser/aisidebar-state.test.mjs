// What the column's chat keeps and what it lets go of when the user switches conversation (design.md section 6.18): an idle
// chat that was left drops its page session, a reply that runs is never stopped or let go, a draft stays, one chat's share, offer
// and draft never reach another, and stored text stays text however hostile it is.
import assert from "node:assert/strict";
import { SAVED_A, SAVED_B, VIEW3, boot, calls, chatBlock, ctxCalls, openContext, openSide, pickChat, saveChats, sendText } from "./ai-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await releaseCase(page, url);
  await isolationCase(page, url);
  await hostileCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const snapshotsOf = async (page, id) => (await calls(page, "ai_turn_snapshot")).filter((a) => a.conversation === id).length;
const messages = (page) => page.locator("#sidebar .chat-msg .chat-text").allTextContents();
const settledAt = (page, n) =>
  page.waitForFunction((count) => document.querySelectorAll("#sidebar .chat-msg").length === count && !document.querySelector("#sidebar .chat-partial"), n);
const release = (page, id) =>
  page.evaluate(async (chat) => (await import(new URL("chat.js", location.href).href)).releaseChat(chat), id);

async function show(page, id, count) {
  await pickChat(page, id);
  await settledAt(page, count);
}

async function releaseCase(page, url) {
  await boot(page, url);
  await saveChats(page, [SAVED_A, SAVED_B]);
  await openSide(page);

  // An idle chat that was left is let go: coming back asks the core again and shows the same messages.
  await show(page, "chat-saved-a", 2);
  const first = await snapshotsOf(page, "chat-saved-a");
  await show(page, "chat-saved-b", 2);
  assert.equal(await release(page, "chat-saved-a"), false, "the switch itself already let go of the idle chat");
  await show(page, "chat-saved-a", 2);
  assert.ok((await snapshotsOf(page, "chat-saved-a")) > first, "its session was let go and the mount asked again");
  assert.deepEqual(await messages(page), ["Why is talos-3 NotReady?", "Check talosctl health."]);

  // A reply that runs is neither stopped nor let go, and is whole when the user returns.
  await page.evaluate(() => window.__MOCK__.ai.hold(2));
  await sendText(page, "and the second node?");
  await page.waitForFunction(() => document.querySelector("#sidebar .chat-partial .chat-text")?.textContent === "Hello ");
  await show(page, "chat-saved-b", 2);
  assert.equal((await calls(page, "ai_cancel")).length, 0, "switching away stops nothing");
  await page.evaluate(() => window.__MOCK__.ai.release());
  await page.waitForFunction(() => window.__MOCK__.ai.chat("chat-saved-a").running === false);
  // Idle now, nothing shows it, no draft, no pane: it can be let go. Nothing else about it can: a call is not made.
  const before = (await page.evaluate(() => window.__MOCK__.calls.length));
  assert.equal(await release(page, "chat-saved-a"), true, "an idle chat that was left");
  assert.equal(await page.evaluate(() => window.__MOCK__.calls.length), before, "no command");
  assert.equal(await release(page, "chat-saved-a"), false, "it is gone");
  assert.equal(await release(page, "no-such-chat"), false, "an unknown id");
  await show(page, "chat-saved-a", 4);
  assert.deepEqual((await messages(page)).slice(2), ["and the second node?", "Hello there."], "one complete reply, once");
  assert.equal(await release(page, "chat-saved-a"), false, "the chat on screen");
  assert.equal(await page.locator("#sidebar .chat-partial").count(), 0);

  // A draft keeps the session.
  await page.locator("#sidebar .chat-input").fill("keep this");
  await show(page, "chat-saved-b", 2);
  assert.equal(await page.locator("#sidebar .chat-input").inputValue(), "", "the draft stays with its chat");
  await show(page, "chat-saved-a", 4);
  assert.equal(await page.locator("#sidebar .chat-input").inputValue(), "keep this");
}

async function releaseBlockCase(page, url) {
  // A chat that has a chat block is never let go, even while no pane shows it.
  await boot(page, url, { view: VIEW3, blocks: [chatBlock("chat-saved-b")] });
  await saveChats(page, [SAVED_A, SAVED_B]);
  await page.waitForSelector('.block[data-pane="3"].block-chat');
  const without = { ...VIEW3, panes: VIEW3.panes.slice(0, 2), tabs: [{ title: "shell", active: true, focus: 2, panes: 2 }] };
  await page.evaluate((v) => window.__MOCK__.emit("layout", v), without);
  await page.waitForFunction(() => !document.querySelector('.block[data-pane="3"].block-chat'));
  assert.equal(await release(page, "chat-saved-b"), false, "a chat block still holds it");
}

/** Open the card by hand, share the pane when asked, tick the screen and preview it; leaves the card in the mode `offer`. */
async function attachScreen(page, conversation = "chat-saved-a") {
  await openContext(page, conversation);
  if ((await page.locator("#sidebar .chat-consent").getAttribute("data-mode")) === "ask") await page.locator("#sidebar .consent-share").click();
  await page.waitForSelector('#sidebar .chat-consent[data-mode="pick"]');
  await page.locator('#sidebar label.consent-kind[data-kind="screen"] input').check();
  await page.locator("#sidebar .consent-preview").click();
  await page.waitForSelector('#sidebar .chat-consent[data-mode="offer"]');
}

const FIVE = ["conversation", "model", "pane", "profile", "text"];

async function isolationCase(page, url) {
  await releaseBlockCase(page, url);
  await boot(page, url);
  await saveChats(page, [SAVED_A, SAVED_B]);
  await openSide(page);
  await show(page, "chat-saved-a", 2);
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length >= 1);
  await attachScreen(page);
  assert.equal(await page.locator("#sidebar li.consent-item").count(), 1);
  const before = (await ctxCalls(page)).length;

  // Another chat shows no card, no offer and no draft of the first, and its message leaves with the five keys only.
  await show(page, "chat-saved-b", 2);
  assert.equal(await page.locator("#sidebar .chat-consent").count(), 0, "no card");
  assert.equal(await page.locator("#sidebar button.chat-ctx").textContent(), "○ Auto");
  assert.equal(await page.locator("#sidebar .chat-input").inputValue(), "");
  assert.equal((await ctxCalls(page)).length, before, "the switch makes no context call");
  await sendText(page, "a question for the second chat");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  const sent = (await calls(page, "ai_send"))[0];
  assert.deepEqual(Object.keys(sent).sort(), FIVE, "no offer, no picked");
  assert.equal(sent.conversation, "chat-saved-b");
  await settledAt(page, 4);

  // The first chat was let go with its offer: no card and the toggle as it was on return, and its message has five keys too.
  await show(page, "chat-saved-a", 2);
  assert.equal(await page.locator("#sidebar .chat-consent").count(), 0);
  assert.equal(await page.locator("#sidebar button.chat-ctx").textContent(), "○ Auto");
  assert.equal((await ctxCalls(page)).length, before, "the return makes no context call");
  await sendText(page, "back in the first chat");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 2);
  const again = (await calls(page, "ai_send"))[1];
  assert.deepEqual(Object.keys(again).sort(), FIVE);
  assert.equal(again.conversation, "chat-saved-a");
  await settledAt(page, 4);

  // With a draft the session stays: the draft and the offer are still there on return and never in the other chat.
  await attachScreen(page);
  await page.locator("#sidebar .chat-input").fill("keep this");
  await show(page, "chat-saved-b", 4);
  assert.equal(await page.locator("#sidebar .chat-input").inputValue(), "");
  assert.equal(await page.locator("#sidebar .chat-consent").count(), 0);
  assert.equal(await page.locator("#sidebar button.chat-ctx").textContent(), "○ Auto");
  await show(page, "chat-saved-a", 4);
  assert.equal(await page.locator("#sidebar .chat-input").inputValue(), "keep this");
  assert.equal(await page.locator("#sidebar li.consent-item").count(), 1);
}

const CANARY = "zq-chat-title-<img src=x onerror=window.__XSS=1>-6e2d";

async function hostileCase(page, url) {
  await boot(page, url);
  await saveChats(page, [SAVED_A]);
  await page.evaluate((canary) => {
    const filler = Array.from({ length: 597 }, (_, i) => ({
      id: `chat-f-${i}`, title: `Filler ${i}`, created: 1700000000, updated: 1700000000 - i, profile: null, model: null,
    }));
    window.__MOCK__.on("ai_conversation_list", () => ({
      chats: [
        { id: "chat-h1", title: canary, created: 1700000000, updated: 1700009999, profile: null, model: null },
        { id: "chat-h2", title: `${"long ".repeat(80)}‮\u0000end`, created: 1700000000, updated: 1700009998, profile: null, model: null },
        { id: "../x", title: "traversal", created: 1700000000, updated: 1700009997, profile: null, model: null },
        ...filler,
      ],
      notes: ["<b>x</b>"],
    }));
  }, CANARY);
  await openSide(page);
  await page.locator(".ai-side-title").click();
  await page.waitForSelector("#ai-side-chats:not([hidden]) li.ai-side-row");

  assert.equal(await page.locator("#sidebar img, #sidebar b").count(), 0, "no element was made from a title or a note");
  assert.equal(await page.evaluate(() => window.__XSS), undefined);
  const rows = await page.locator("li.ai-side-row").count();
  assert.ok(rows <= 501, `${rows} rows`);
  const names = await page.locator(".ai-side-name").allTextContents();
  assert.equal(names[1], CANARY, "text, whole");
  assert.equal(Array.from(names[2]).length, 80, "cut at 80");
  assert.equal(/[‮\u0000]/.test(names[2]), false, "clean");
  assert.equal(names.includes("traversal"), false, "the bad id is not shown");
  assert.deepEqual(await page.locator("p.ai-side-note").allTextContents(), ["<b>x</b>"]);
  const leaked = await page.evaluate(() => {
    for (const node of document.querySelectorAll("#sidebar *")) {
      for (const attr of node.attributes) if (attr.value.includes("zq-chat-title")) return `${node.localName}[${attr.name}]`;
    }
    return null;
  });
  assert.equal(leaked, null, "no title in an attribute");

  // The page stays responsive.
  await page.locator('li[data-id="chat-h1"] button.ai-side-pick').click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_turn_snapshot").some((a) => a.conversation === "chat-h1"));
  assert.equal(await page.evaluate(() => window.__XSS), undefined);
}
