// The AI chat in the sidebar column (design.md section 6.18): the page action `ai.sidebar` turns the shown column into the
// chat of the chat pane's own module, the title button reads the stored chats once, a press on a row shows that chat, and a
// typed message leaves as the one send call. Nothing is read, made or deleted unasked.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { SAVED_A, SAVED_B, boot, calls, openSide, saveChats, sendText, sideRows } from "./ai-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await traceCase(page, url);
  await contractCase(page, url);
  await quietCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** The commands whose name starts with `ai_`, in order. */
const aiCalls = (page) => page.evaluate(() => window.__MOCK__.calls.map((c) => c[0]).filter((c) => c.startsWith("ai_")));

const remPx = (page) => page.evaluate(() => parseFloat(getComputedStyle(document.documentElement).fontSize));
const widthOf = (page, sel) => page.locator(sel).evaluate((n) => n.getBoundingClientRect().width);

async function traceCase(page, url) {
  await boot(page, url);
  await saveChats(page, [SAVED_A, SAVED_B]);
  assert.deepEqual(await aiCalls(page), [], "no ai_ command before a press");
  assert.equal(await page.locator("#sidebar").isHidden(), true);
  assert.equal(await page.locator("#sidebar").getAttribute("data-form"), "sidebar");
  const stage0 = await widthOf(page, "#stage");
  const rem = await remPx(page);

  await page.keyboard.press("Meta+b");
  await page.waitForSelector("#sidebar:not([hidden])");
  assert.equal(await page.locator("#sidebar .sb-main").isVisible(), true);
  assert.equal(await page.locator("#sidebar .sb-ai").isHidden(), true);
  assert.ok(Math.abs((await widthOf(page, "#sidebar")) - 18 * rem) < 1, "the sidebar form is 18 rem wide");
  const stage1 = await widthOf(page, "#stage");
  assert.ok(Math.abs(stage0 - stage1 - 18 * rem) < 1);

  await page.evaluate(() => window.__registry.runAction("ai.sidebar"));
  await page.waitForSelector('#sidebar[data-form="chat"] textarea.chat-input');
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_turn_snapshot").length >= 1);
  assert.equal(await page.locator("#sidebar .sb-main").isHidden(), true);
  assert.equal(await page.locator("#sidebar .sb-ai").isVisible(), true);
  assert.equal(await page.locator("#sidebar div.block.block-chat.side-chat").count(), 1, "the chat pane's module in the column");
  assert.equal((await calls(page, "ai_conversation_new")).length, 1);
  assert.deepEqual(await calls(page, "ai_turn_snapshot"), [{ conversation: "chat-mock-1" }]);
  assert.equal((await calls(page, "ai_conversation_list")).length, 0, "the list is not read yet");
  assert.equal(await page.evaluate(() => document.activeElement?.matches("textarea.chat-input")), true, "the composer has the keyboard");
  assert.ok(Math.abs((await widthOf(page, "#sidebar")) - (70 / 3) * rem) < 1, "the AI form is 23.333 rem wide");
  const stage2 = await widthOf(page, "#stage");
  assert.ok(Math.abs(stage1 - stage2 - (70 / 3 - 18) * rem) < 1, `the stage is narrower by the difference (${stage1} to ${stage2})`);
  assert.equal(await page.locator(".ai-side-title .ai-side-text").textContent(), "new chat");
  assert.equal(await page.locator(".ai-side-title").getAttribute("aria-expanded"), "false");
  assert.equal(await page.locator("#sidebar").getAttribute("aria-label"), "AI chat");
  assert.equal((await calls(page, "block_open")).length, 0, "no block was opened");

  // The title button reads the list once and draws a row for the chat on screen and one per stored chat.
  await page.locator(".ai-side-title").click();
  await page.waitForSelector("#ai-side-chats:not([hidden]) li.ai-side-row");
  assert.equal((await calls(page, "ai_conversation_list")).length, 1);
  assert.equal(await page.locator(".ai-side-title").getAttribute("aria-expanded"), "true");
  const rows = await sideRows(page);
  assert.deepEqual(rows.map((r) => r.id), ["chat-mock-1", "chat-saved-a", "chat-saved-b"]);
  assert.deepEqual([rows[0].mark, rows[0].title, rows[0].meta, rows[0].current], ["●", "new chat", "nothing sent yet", true]);
  assert.deepEqual([rows[1].mark, rows[1].title, rows[1].meta, rows[1].current], ["○", SAVED_A.title, "llama3.2 · 2 h ago", false]);
  assert.match(rows[2].meta, /3 d ago$/);
  assert.equal(await page.locator("#sidebar button.ai-side-pick[aria-current]").count(), 1);

  // A press on a stored chat's row shows its saved transcript, and the next message is the one send call.
  await page.locator('li.ai-side-row[data-id="chat-saved-a"] button.ai-side-pick').click();
  await page.waitForFunction(() => document.querySelectorAll("#sidebar .chat-msg").length === 2);
  assert.equal(await page.locator("#ai-side-chats").isHidden(), true, "the list closed");
  assert.deepEqual((await calls(page, "ai_turn_snapshot")).at(-1), { conversation: "chat-saved-a" });
  assert.equal((await calls(page, "ai_turn_snapshot")).length, 2, "one snapshot for the chat that was chosen");
  assert.equal(await page.locator(".ai-side-title .ai-side-text").textContent(), SAVED_A.title);
  assert.deepEqual(await page.locator("#sidebar .chat-msg .chat-text").allTextContents(), ["Why is talos-3 NotReady?", "Check talosctl health."]);
  assert.equal(await page.evaluate(() => document.activeElement?.matches("textarea.chat-input")), true);

  await sendText(page, "and the second node?");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  const sent = (await calls(page, "ai_send"))[0];
  assert.deepEqual(Object.keys(sent).sort(), ["conversation", "model", "pane", "profile", "text"], "exactly five arguments");
  assert.deepEqual(sent, { conversation: "chat-saved-a", text: "and the second node?", pane: 2, profile: "local-1", model: null });
  await page.waitForFunction(() => document.querySelectorAll("#sidebar .chat-msg").length === 4 && !document.querySelector("#sidebar .chat-partial"));
  assert.equal(await page.locator("#sidebar .chat-log .chat-msg").last().locator(".chat-text").textContent(), "Hello there.");

  // `◂ Sidebar` returns the column to the sidebar form.
  await page.locator(".ai-side-back").click();
  await page.waitForSelector('#sidebar[data-form="sidebar"]');
  assert.equal(await page.locator("#sidebar .sb-main").isVisible(), true);
  assert.equal(await page.locator("#sidebar .sb-ai").isHidden(), true);
  assert.equal(await page.locator("#sidebar .sb-section").count(), 3);
  assert.equal((await calls(page, "block_open")).length, 0);
  assert.equal((await calls(page, "ai_conversation_new")).length, 1, "no second chat was made");
}

/** What the core's list answer and the page's reader agree on. */
async function contractCase(page, url) {
  const protocol = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));
  const golden = JSON.parse(readFileSync(new URL("../fixtures/ai_chats.json", import.meta.url), "utf8"));
  await boot(page, url);
  await saveChats(page, [SAVED_A, SAVED_B]);
  await openSide(page);
  await page.locator(".ai-side-title").click();
  await page.waitForSelector("li.ai-side-row");

  // The answer the mock gives, called as the page calls it: no argument, the members of the golden.
  const answer = await page.evaluate(() => window.__TAURI__.core.invoke("ai_conversation_list"));
  assert.deepEqual(Object.keys(answer).sort(), Object.keys(protocol.answers.ai.conversations).sort());
  assert.equal(answer.chats.length, 2);
  for (const chat of answer.chats) {
    assert.deepEqual(Object.keys(chat).sort(), Object.keys(golden.summary).sort(), "the six members of the core's summary");
    assert.match(String(chat.created), /^\d{10}$/, "Unix seconds");
    assert.match(String(chat.updated), /^\d{10}$/, "Unix seconds");
  }
  const read = await page.evaluate(async (raw) => {
    const { readChatList } = await import(new URL("chatlist.js", location.href).href);
    return readChatList(raw);
  }, answer);
  assert.deepEqual(read.chats.map((c) => [c.id, c.title]), answer.chats.map((c) => [c.id, c.title]));

  // The two commands carry no argument.
  for (const name of ["ai_conversation_list", "ai_conversation_new"]) {
    for (const args of await calls(page, name)) assert.deepEqual(Object.keys(args ?? {}), [], `${name} has no argument`);
  }
  // Only the commands this form needs, besides the ones the page always makes.
  const allowed = new Set(["ai_conversation_list", "ai_conversation_new", "ai_profiles", "ai_send", "ai_turn_snapshot"]);
  const made = new Set(await aiCalls(page));
  for (const name of made) assert.ok(allowed.has(name), `${name} was called`);
}

/** Nothing is asked of the core until a user act. */
async function quietCase(page, url) {
  // The column is remembered as shown: it opens as the sidebar and the page makes no ai_ call.
  await page.addInitScript(() => localStorage.setItem("holzwave.sidebar", "1"));
  await boot(page, url, { search: "" });
  await saveChats(page, [SAVED_A, SAVED_B]);
  await page.waitForSelector("#sidebar:not([hidden])");
  await page.waitForFunction(() => window.__MOCK__.callsOf("widgets_list").length >= 1);
  assert.equal(await page.locator("#sidebar").getAttribute("data-form"), "sidebar");
  await page.waitForTimeout(200);
  assert.deepEqual(await aiCalls(page), [], "no ai_ call at launch with the column shown");
  const widgets = (await calls(page, "widgets_list")).length;

  await page.evaluate(() => window.__registry.runAction("ai.sidebar"));
  await page.waitForSelector('#sidebar[data-form="chat"] textarea.chat-input');
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_turn_snapshot").length >= 1);
  const opened = await aiCalls(page);
  assert.deepEqual(opened.filter((c) => c === "ai_conversation_new"), ["ai_conversation_new"]);
  assert.equal(opened.includes("ai_conversation_list"), false, "the list is not read");
  assert.ok(opened.filter((c) => c === "ai_profiles").length <= 1, "at most the composer's first focus reads the providers");
  assert.equal((await calls(page, "widgets_list")).length, widgets, "opening the form does not draw the sidebar's widgets again");

  // A window focus event asks a snapshot only for the mounted chat.
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await page.waitForTimeout(100);
  for (const args of await calls(page, "ai_turn_snapshot")) assert.equal(args.conversation, "chat-mock-1");

  // Running the action again while the form shows asks nothing and gives the composer the keyboard.
  const before = (await aiCalls(page)).length;
  await page.evaluate(() => document.getElementById("ime").focus());
  await page.evaluate(() => window.__registry.runAction("ai.sidebar"));
  await page.waitForFunction(() => document.activeElement?.matches("textarea.chat-input"));
  assert.equal((await aiCalls(page)).length, before, "no command");

  // `◂ Sidebar` asks the core for nothing of the chat and the sidebar draws again.
  await page.locator(".ai-side-back").click();
  await page.waitForSelector('#sidebar[data-form="sidebar"]');
  await page.waitForFunction((n) => window.__MOCK__.callsOf("widgets_list").length === n + 1, widgets);
  assert.equal((await aiCalls(page)).length, before, "no ai_ command by leaving");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime", "the keyboard went back to the terminal");

  // With the column hidden the action shows it in the AI form without drawing the hosts or reading the widgets.
  await page.keyboard.press("Meta+b");
  await page.waitForSelector("#sidebar", { state: "hidden" });
  const reads = (await calls(page, "widgets_list")).length;
  await page.evaluate(() => window.__registry.runAction("ai.sidebar"));
  await page.waitForSelector('#sidebar:not([hidden])[data-form="chat"] textarea.chat-input');
  assert.equal((await calls(page, "widgets_list")).length, reads, "the sidebar's widgets are not read for the AI form");
  assert.equal((await aiCalls(page)).filter((c) => c === "ai_conversation_new").length, 1, "the chat that was on screen is shown again");
}
