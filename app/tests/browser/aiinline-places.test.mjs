// Where the palette actions of the chat open and what they leave alone (docs/design.md section 6.19): the chat that
// `✦ AI` would focus gets the text and no other, a draft that was typed is kept, a second run leaves one copy and no second
// chat, and a pane that is not a terminal pane is refused in a sentence.
import assert from "node:assert/strict";
import {
  SAVED_A, SAVED_B, VIEW3, VIEW_P1, boot, calls, chatBlock, commandNames, draftOf, openSide, pickChat, runAction, saveChats, sendText, setPrivacy,
  settle,
} from "./ai-lib.mjs";

const EXPLAIN =
  "Explain the error in the attached terminal output: what went wrong, the most likely cause, and the command or change that fixes it.";
const READY = "Nothing is sent until you press Send.";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await placeCase(page, url);
  await draftCase(page, url);
  await refuseCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A page with the saved chats, the local provider read without a share and the layout seen twice. */
async function open(page, url, options = {}) {
  // The column is remembered across loads: every case starts with it hidden.
  await page.addInitScript(() => {
    try {
      localStorage.removeItem("holzwave.sidebar");
    } catch {
      // no storage, nothing remembered
    }
  });
  await boot(page, url, { view: VIEW_P1, ...options });
  await saveChats(page, [SAVED_A, SAVED_B]);
  await setPrivacy(page, { localContext: "auto" });
  await settle(page);
}

const aiCalls = async (page) => (await commandNames(page)).filter((name) => name.startsWith("ai_"));
const newChats = async (page) => (await calls(page, "ai_conversation_new")).length;

/** The composer of the scope holds `text` and has the keyboard; the card is in the strip of the same scope. */
async function landed(page, scope, text) {
  await page.waitForSelector(`${scope} .chat-cards section.chat-consent[data-mode="offer"]`);
  assert.equal(await draftOf(page, scope), text, `the text is in the composer of ${scope}`);
  assert.equal(
    await page.evaluate((s) => document.activeElement === document.querySelector(`${s} textarea.chat-input`), scope),
    true,
    "the composer that holds the text has the keyboard",
  );
}

async function placeCase(page, url) {
  // (a) a restored chat pane in the shown tab
  await open(page, url, { view: VIEW3, blocks: [chatBlock("chat-saved-a")] });
  await page.waitForSelector(".block-chat textarea.chat-input");
  await runAction(page, "ai.explain_error");
  await landed(page, ".block-chat", EXPLAIN);
  assert.equal(await newChats(page), 0, "no new chat");
  assert.equal((await calls(page, "block_open")).length, 0, "no new pane");
  assert.equal(await page.locator("textarea.chat-input").count(), 1, "there is one composer and it holds the text");
  assert.equal(await page.locator(".chat-status").textContent(), READY);

  // (b) a shown column with no chat pane becomes the chat
  await open(page, url);
  await page.keyboard.press("Meta+b");
  await page.waitForSelector("#sidebar:not([hidden])");
  await runAction(page, "ai.explain_error");
  await page.waitForSelector('#sidebar[data-form="chat"] textarea.chat-input');
  await landed(page, "#sidebar", EXPLAIN);
  assert.equal(await newChats(page), 1);
  assert.equal((await calls(page, "block_open")).length, 0, "the column took the chat, no pane opened");

  // (c) the column already shows the AI form on a stored chat: it keeps that chat
  await open(page, url, { view: VIEW_P1 });
  await openSide(page);
  await pickChat(page, "chat-saved-a");
  const made = await newChats(page);
  await runAction(page, "ai.explain_error");
  await landed(page, "#sidebar", EXPLAIN);
  assert.equal(await newChats(page), made, "no further new chat");
  assert.equal((await calls(page, "block_open")).length, 0);
  assert.equal((await calls(page, "ai_context_state")).at(-1).conversation, "chat-saved-a");

  // (d) a chat pane in the tab and the column on another chat: the pane gets the text, not the column
  await open(page, url, { view: VIEW3, blocks: [chatBlock("chat-saved-b")] });
  await openSide(page);
  await pickChat(page, "chat-saved-a");
  await runAction(page, "ai.explain_error");
  await landed(page, ".block-chat:not(.side-chat)", EXPLAIN);
  assert.equal(await draftOf(page, "#sidebar"), "", "the column's composer holds nothing");
  assert.equal((await calls(page, "ai_context_state")).at(-1).conversation, "chat-saved-b");
}

async function draftCase(page, url) {
  // (a) a typed draft is kept; the card still opens and the status line still says what is ready
  await open(page, url, { view: VIEW3, blocks: [chatBlock("chat-saved-a")] });
  await page.waitForSelector(".block-chat textarea.chat-input");
  await page.locator("textarea.chat-input").fill("why is this slow");
  await runAction(page, "ai.explain_error");
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  assert.equal(await draftOf(page), "why is this slow", "the draft is kept exactly");
  assert.equal(await page.locator(".chat-status").textContent(), READY);
  assert.equal((await calls(page, "ai_send")).length, 0);

  // (b) twice leaves one copy, one chat, at most two state calls
  await open(page, url);
  await runAction(page, "ai.explain_error");
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  await runAction(page, "ai.explain_error");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_context_state").length === 2);
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  assert.equal(await draftOf(page), EXPLAIN, "one copy of the text");
  assert.equal(await newChats(page), 1);
  assert.equal((await calls(page, "block_open")).length, 1);
  assert.ok((await calls(page, "ai_context_state")).length <= 2);

  // (c) after the message left, a further run writes the text again; no action ever sent one
  await page.locator("textarea.chat-input").press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  await page.waitForFunction(() => document.querySelector("textarea.chat-input").value === "");
  await page.waitForFunction(() => document.querySelectorAll('.chat-msg[data-role="assistant"][data-status="complete"]').length === 1);
  await runAction(page, "ai.explain_error");
  await page.waitForFunction((text) => document.querySelector("textarea.chat-input").value === text, EXPLAIN);
  assert.equal((await calls(page, "ai_send")).length, 1, "the second run sent nothing");
}

async function refuseCase(page, url) {
  const PREVIEW = { pane: 3, kind: "preview", source: "local", path: "/work/notes.md", source_label: "This computer" };
  const notice = "Focus a terminal pane to explain its error.";
  // The acting pane is a preview
  await boot(page, url, { view: VIEW3, blocks: [PREVIEW] });
  await page.evaluate(() => {
    const v = window.__MOCK__.view();
    v.focus = 3;
    v.tabs[0].focus = 3;
    window.__MOCK__.setView(v);
  });
  await settle(page);
  await runAction(page, "ai.explain_error");
  await page.waitForFunction((text) => document.querySelector("#status .seg.notice")?.textContent === text, notice);
  assert.deepEqual(await aiCalls(page), []);
  assert.equal((await calls(page, "block_open")).length, 0);
  assert.equal(await page.locator(".block-chat").count(), 0);
  assert.equal(await page.locator(".chat-consent").count(), 0);

  // No pane at all
  await page.evaluate(() => {
    window.__MOCK__.setView({ ...window.__MOCK__.view(), panes: [], focus: 0 });
    window.__MOCK__.emit("layout", window.__MOCK__.view());
  });
  await settle(page);
  await runAction(page, "ai.explain_error");
  await page.waitForFunction((text) => document.querySelector("#status .seg.notice")?.textContent === text, notice);
  assert.deepEqual(await aiCalls(page), []);
}
