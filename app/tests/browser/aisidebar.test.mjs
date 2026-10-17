// The AI chat in the sidebar column (design.md section 6.18): the page action `ai.sidebar` turns the shown column into the
// chat of the chat pane's own module, the title button reads the stored chats once, a press on a row shows that chat, and a
// typed message leaves as the one send call. Nothing is read, made or deleted unasked.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { SAVED_A, SAVED_B, VIEW3, boot, calls, chatBlock, openSide, pickChat, saveChats, sendText, sideRows } from "./ai-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await traceCase(page, url);
  await contractCase(page, url);
  await quietCase(page, url);
  await listCase(page, url);
  await newCase(page, url);
  await deleteCase(page, url);
  await paneCase(page, url);
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

const PROTOCOL = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));
const FAULT = (text) => ({ code: "store", reason: null, text });
const status = (page) => page.locator("p.ai-side-status");
const focusedRow = (page) => page.evaluate(() => document.activeElement?.closest("li.ai-side-row")?.dataset.id ?? null);

/** Open the list from the title button and wait for it to be drawn. */
async function openList(page) {
  await page.locator(".ai-side-title").click();
  await page.waitForSelector("#ai-side-chats:not([hidden])");
}

async function listCase(page, url) {
  await boot(page, url);
  await saveChats(page, [SAVED_A, SAVED_B]);
  await openSide(page);

  // The list is read each time it opens and nowhere else.
  await openList(page);
  assert.equal((await calls(page, "ai_conversation_list")).length, 1);
  await page.locator(".ai-side-title").click();
  assert.equal(await page.locator("#ai-side-chats").isHidden(), true);
  assert.equal(await page.locator(".ai-side-caret").textContent(), "▾");
  assert.equal((await calls(page, "ai_conversation_list")).length, 1, "closing reads nothing");
  await openList(page);
  assert.equal((await calls(page, "ai_conversation_list")).length, 2);
  assert.equal(await page.locator(".ai-side-caret").textContent(), "▴");
  assert.deepEqual((await sideRows(page)).map((r) => r.id), ["chat-mock-1", "chat-saved-a", "chat-saved-b"], "the core's order, the unsaved chat first");

  // Escape with the focus in the list closes it and puts the focus on the title button.
  await page.locator('li[data-id="chat-saved-a"] button.ai-side-pick').focus();
  await page.keyboard.press("Escape");
  assert.equal(await page.locator("#ai-side-chats").isHidden(), true);
  assert.equal(await page.evaluate(() => document.activeElement?.classList.contains("ai-side-title")), true);

  // The arrows move between the rows; Enter on a pick button switches.
  await openList(page);
  const first = page.locator('li[data-id="chat-mock-1"] button.ai-side-pick');
  await first.focus();
  await page.keyboard.press("ArrowDown");
  assert.equal(await focusedRow(page), "chat-saved-a");
  await page.keyboard.press("ArrowUp");
  assert.equal(await focusedRow(page), "chat-mock-1");
  await page.keyboard.press("ArrowUp");
  assert.equal(await focusedRow(page), "chat-mock-1", "the first stays");
  await page.keyboard.press("End");
  assert.equal(await focusedRow(page), "chat-saved-b");
  await page.keyboard.press("Home");
  assert.equal(await focusedRow(page), "chat-mock-1");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => document.querySelectorAll("#sidebar .chat-msg").length === 2);
  assert.equal(await page.locator(".ai-side-title .ai-side-text").textContent(), SAVED_A.title);

  // Notes of the core: shown as text above the rows, at most five.
  await page.evaluate(() => { window.__MOCK__.ai.chatNotes = ["chat-9.json: damaged"]; });
  await openList(page);
  assert.deepEqual(await page.locator("p.ai-side-note").allTextContents(), ["chat-9.json: damaged"]);
  await page.locator(".ai-side-title").click();
  await page.evaluate(() => { window.__MOCK__.ai.chatNotes = Array.from({ length: 6 }, (_, i) => `note ${i}`); });
  await openList(page);
  assert.equal(await page.locator("p.ai-side-note").count(), 5);
  await page.locator(".ai-side-title").click();
  await page.evaluate(() => { window.__MOCK__.ai.chatNotes = []; });

  // A refusal is said in the core's words and leaves the row of the chat on screen; a later read clears it.
  await page.evaluate((f) => window.__MOCK__.on("ai_conversation_list", () => { throw f; }), FAULT("The chats could not be read."));
  await openList(page);
  assert.equal(await status(page).textContent(), "The chats could not be read.");
  assert.equal(await status(page).getAttribute("data-kind"), "error");
  assert.deepEqual((await sideRows(page)).map((r) => r.id), ["chat-saved-a"], "only the chat on screen");
  await page.locator(".ai-side-title").click();
  await page.evaluate(() => window.__MOCK__.on("ai_conversation_list", null));
  await openList(page);
  assert.equal(await status(page).isHidden(), true, "a later read clears the sentence");
  await page.locator(".ai-side-title").click();

  // An answer the reader cannot use.
  await page.evaluate(() => window.__MOCK__.on("ai_conversation_list", () => null));
  await openList(page);
  assert.equal(await status(page).textContent(), "The list was not understood.");
  assert.equal(await status(page).getAttribute("data-kind"), "error");
  await page.locator(".ai-side-title").click();
  await page.evaluate(() => window.__MOCK__.on("ai_conversation_list", null));

  // Sixty stored chats: the list is at most 40 % of the column and scrolls inside; the column never scrolls sideways.
  await saveChats(page, Array.from({ length: 60 }, (_, i) => ({ ...SAVED_B, id: `chat-bulk-${i}`, title: `Bulk chat ${i} with a title that is long enough to be cut by the column` })));
  await openList(page);
  const box = await page.evaluate(() => {
    const list = document.getElementById("ai-side-chats");
    const side = document.getElementById("sidebar");
    return {
      list: list.getBoundingClientRect().height, side: side.getBoundingClientRect().height, scroll: list.scrollHeight, client: list.clientHeight,
      overflow: getComputedStyle(list).overflowY, wide: side.scrollWidth, shown: side.clientWidth,
    };
  });
  assert.ok(box.list <= box.side * 0.4 + 1, `the list is at most 40 % of the column (${box.list} of ${box.side})`);
  assert.ok(box.scroll > box.client && ["auto", "scroll"].includes(box.overflow), "it scrolls inside");
  assert.ok(box.wide <= box.shown, "no horizontal scroll");
}

async function newCase(page, url) {
  await boot(page, url);
  await saveChats(page, [SAVED_A, SAVED_B]);
  await openSide(page);
  assert.equal(await page.locator("button.ai-side-new").textContent(), "＋ New chat");
  await pickChat(page, "chat-saved-a");
  await page.waitForFunction(() => document.querySelectorAll("#sidebar .chat-msg").length === 2);

  // A press makes a second id, no list call, an empty chat and the keyboard on the composer.
  const lists = (await calls(page, "ai_conversation_list")).length;
  await page.locator("button.ai-side-new").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_conversation_new").length === 2);
  await page.waitForFunction(() => document.querySelectorAll("#sidebar .chat-msg").length === 0);
  assert.equal((await calls(page, "ai_conversation_list")).length, lists, "no list call");
  assert.equal(await page.locator(".ai-side-title .ai-side-text").textContent(), "new chat");
  assert.equal(await page.evaluate(() => document.activeElement?.matches("textarea.chat-input")), true);
  assert.deepEqual((await calls(page, "ai_turn_snapshot")).at(-1), { conversation: "chat-mock-2" });

  // The chat that was left is still in the next list, and an open list is closed by the press.
  await openList(page);
  assert.deepEqual((await sideRows(page)).map((r) => r.id), ["chat-mock-2", "chat-saved-a", "chat-saved-b"]);
  await page.locator("button.ai-side-new").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_conversation_new").length === 3);
  assert.equal(await page.locator("#ai-side-chats").isHidden(), true, "the press closed the list");

  // Two quick presses while the answer is held make one call.
  await page.evaluate(() => window.__MOCK__.on("ai_conversation_new", () => new Promise((go) => { window.__RELEASE_NEW = () => go("chat-held-1"); })));
  await page.evaluate(() => { const b = document.querySelector("button.ai-side-new"); b.click(); b.click(); });
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_conversation_new").length === 4);
  await page.waitForTimeout(100);
  assert.equal((await calls(page, "ai_conversation_new")).length, 4, "one call for two presses");
  await page.evaluate(() => window.__RELEASE_NEW());
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_turn_snapshot").some((a) => a.conversation === "chat-held-1"));

  // A refusal leaves the chat as it was and says why.
  await page.evaluate((f) => window.__MOCK__.on("ai_conversation_new", () => { throw f; }), FAULT("The chat could not be made."));
  const snapshots = (await calls(page, "ai_turn_snapshot")).length;
  await page.locator("button.ai-side-new").click();
  await page.waitForFunction(() => document.querySelector("p.ai-side-status")?.textContent === "The chat could not be made.");
  assert.equal(await status(page).getAttribute("data-kind"), "error");
  assert.equal((await calls(page, "ai_turn_snapshot")).length, snapshots, "the chat on screen is unchanged");
}

async function deleteCase(page, url) {
  await boot(page, url);
  await saveChats(page, [SAVED_A, SAVED_B]);
  await openSide(page);
  await openList(page);
  const unsaved = (await sideRows(page))[0];
  assert.equal(unsaved.id, "chat-mock-1");
  assert.equal(unsaved.canDelete, false, "the unsaved chat has no delete");
  await page.locator(".ai-side-title").click();
  await pickChat(page, "chat-saved-a");
  await page.waitForFunction(() => document.querySelectorAll("#sidebar .chat-msg").length === 2);
  await openList(page);
  const del = (id) => page.locator(`li[data-id="${id}"] button.ai-side-del`);
  assert.equal(await del("chat-saved-b").textContent(), "×");
  assert.equal(await del("chat-saved-b").getAttribute("aria-label"), "Delete");
  assert.equal(await page.locator("button.ai-side-del").count(), 2, "a saved chat on screen can be deleted too");

  // The first press arms and sends nothing.
  await del("chat-saved-b").click();
  assert.equal(await del("chat-saved-b").getAttribute("data-armed"), "true");
  assert.equal(await del("chat-saved-b").textContent(), "Delete?");
  assert.equal((await calls(page, "ai_conversation_delete")).length, 0);
  // Escape disarms and the list stays; a second Escape closes it.
  await del("chat-saved-b").focus();
  await page.keyboard.press("Escape");
  assert.equal((await sideRows(page)).some((r) => r.armed), false);
  assert.equal(await page.locator("#ai-side-chats").isHidden(), false);
  await page.keyboard.press("Escape");
  assert.equal(await page.locator("#ai-side-chats").isHidden(), true);

  // Any other press disarms: another button's first press, the title button, reading the list again.
  await openList(page);
  await del("chat-saved-b").click();
  await del("chat-saved-a").click();
  assert.deepEqual((await sideRows(page)).map((r) => r.armed), [true, false], "arming another disarms the first (rows: A, B)");
  await page.locator(".ai-side-title").click();
  assert.equal(await page.locator("button.ai-side-del[data-armed]").count(), 0, "closing disarms");
  await openList(page);
  await del("chat-saved-b").click();
  await page.locator(".ai-side-title").click();
  await openList(page);
  assert.equal((await sideRows(page)).some((r) => r.armed), false, "reading again disarms");
  // A press or a key anywhere else disarms it, the list stays open.
  await del("chat-saved-b").click();
  await page.locator(".ai-side-head span").first().click();
  assert.equal(await page.locator("button.ai-side-del[data-armed]").count(), 0, "a press elsewhere disarms");
  assert.equal(await page.locator("#ai-side-chats").isHidden(), false);
  await del("chat-saved-b").click();
  await page.keyboard.press("a");
  assert.equal(await page.locator("button.ai-side-del[data-armed]").count(), 0, "a key elsewhere disarms");
  await page.locator(".ai-side-title").click();
  assert.equal((await calls(page, "ai_conversation_delete")).length, 0, "nothing was deleted by any of it");

  // The second press on the same button deletes: one call with the row's id.
  await openList(page);
  await del("chat-saved-b").click();
  await del("chat-saved-b").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_conversation_delete").length === 1);
  const asked = (await calls(page, "ai_conversation_delete"))[0];
  assert.deepEqual(asked, { conversation: "chat-saved-b" });
  assert.deepEqual(Object.keys(asked), PROTOCOL.commands.ai_conversation_delete);
  await page.waitForFunction(() => !document.querySelector('li[data-id="chat-saved-b"]'));
  assert.equal(await status(page).textContent(), "Chat deleted.");
  assert.equal(await status(page).getAttribute("data-kind"), "note");
  assert.deepEqual(await page.evaluate(() => window.__MOCK__.ai.deleted), ["chat-saved-b"]);
  assert.equal(await page.locator(".ai-side-title .ai-side-text").textContent(), SAVED_A.title, "the chat on screen is unchanged");

  // Deleting the chat on screen shows a new empty chat.
  await del("chat-saved-a").click();
  await del("chat-saved-a").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_conversation_delete").length === 2);
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_conversation_new").length === 2);
  await page.waitForFunction(() => document.querySelectorAll("#sidebar .chat-msg").length === 0);
  assert.equal(await page.locator(".ai-side-title .ai-side-text").textContent(), "new chat");
  assert.equal(await page.evaluate(() => document.activeElement?.matches("textarea.chat-input")), true);
  await openList(page);
  assert.deepEqual((await sideRows(page)).map((r) => r.id), ["chat-mock-2"], "its row is gone at the next read");
  await page.locator(".ai-side-title").click();

  // A refusal is told in the core's words and keeps the row.
  await saveChats(page, [SAVED_A, SAVED_B]);
  await page.evaluate((f) => window.__MOCK__.on("ai_conversation_delete", () => { throw f; }), FAULT("The chat could not be deleted."));
  await openList(page);
  await del("chat-saved-b").click();
  await del("chat-saved-b").click();
  await page.waitForFunction(() => document.querySelector("p.ai-side-status")?.textContent === "The chat could not be deleted.");
  assert.equal(await status(page).getAttribute("data-kind"), "error");
  assert.equal(await page.locator('li[data-id="chat-saved-b"]').count(), 1, "the row stays");
  assert.equal(await page.locator("button.ai-side-del[data-armed]").count(), 0);
  await page.evaluate(() => window.__MOCK__.on("ai_conversation_delete", null));
  await page.locator(".ai-side-title").click();

  // A reply that runs while its chat is deleted: the late end event changes nothing in the new chat.
  await pickChat(page, "chat-saved-b");
  await page.waitForFunction(() => document.querySelectorAll("#sidebar .chat-msg").length === 2);
  await page.evaluate(() => window.__MOCK__.ai.hold(2));
  await sendText(page, "keep talking");
  await page.waitForFunction(() => document.querySelector(".chat-partial .chat-text")?.textContent === "Hello ");
  await openList(page);
  await del("chat-saved-b").click();
  await del("chat-saved-b").click();
  await page.waitForFunction(() => window.__MOCK__.ai.deleted.includes("chat-saved-b"));
  await page.waitForFunction(() => document.querySelectorAll("#sidebar .chat-msg").length === 0 && !document.querySelector("#sidebar .chat-partial"));
  await page.waitForTimeout(150);
  assert.equal(await page.locator("#sidebar .chat-msg, #sidebar .chat-partial, #sidebar .chat-end").count(), 0, "the new chat is untouched");
}

async function paneCase(page, url) {
  await boot(page, url, { view: VIEW3, blocks: [chatBlock("chat-saved-b")] });
  await saveChats(page, [SAVED_A, SAVED_B]);
  await page.waitForSelector('.block[data-pane="3"].block-chat');
  await openSide(page);
  await pickChat(page, "chat-saved-a");
  await page.waitForFunction(() => document.querySelectorAll("#sidebar .chat-msg").length === 2);

  await openList(page);
  const rows = await sideRows(page);
  const b = rows.find((r) => r.id === "chat-saved-b");
  assert.equal(b.pane, 3);
  assert.equal(b.meta, "llama3.2 · in pane 3");
  assert.equal(b.canDelete, false, "a chat in a pane has no delete");
  const snapshots = (await calls(page, "ai_turn_snapshot")).length;
  await page.locator('li[data-id="chat-saved-b"] button.ai-side-pick').click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("focus_pane").length === 1);
  assert.deepEqual((await calls(page, "focus_pane"))[0], { pane: 3 });
  assert.equal(await page.locator("#ai-side-chats").isHidden(), true);
  assert.equal(await page.locator(".ai-side-title .ai-side-text").textContent(), SAVED_A.title, "the column stays on its chat");
  assert.equal((await calls(page, "ai_turn_snapshot")).length, snapshots, "the column asked for no snapshot");

  // With the pane in another tab the press only says so.
  const without = { ...VIEW3, panes: VIEW3.panes.slice(0, 2), tabs: [{ title: "shell", active: true, focus: 2, panes: 2 }] };
  await page.evaluate((v) => window.__MOCK__.emit("layout", v), without);
  await page.waitForFunction(() => !document.querySelector('.block[data-pane="3"].block-chat'));
  await openList(page);
  await page.locator('li[data-id="chat-saved-b"] button.ai-side-pick').click();
  await page.waitForFunction(() => document.querySelector("p.ai-side-status")?.textContent === "That chat is open in another tab.");
  assert.equal((await calls(page, "focus_pane")).length, 1, "no further focus_pane call");
  assert.equal(await page.locator("#ai-side-chats").isHidden(), false, "the list stays");

  // `ai.new_chat` opens a pane with a fresh id and never the chat the column shows.
  await page.evaluate(() => window.__registry.runAction("ai.new_chat"));
  await page.waitForFunction(() => window.__MOCK__.callsOf("block_open").length === 1);
  const opened = (await calls(page, "block_open"))[0];
  assert.equal(opened.kind, "chat");
  assert.notEqual(opened.path, "chat-saved-a");
  assert.equal(opened.path, "chat-mock-2");
  await page.locator(".ai-side-title").click();

  // After the pane is closed the chat is an ordinary stored chat again.
  await page.evaluate(() => { window.__BLOCKS = window.__BLOCKS.filter((b) => b.path !== "chat-saved-b"); });
  await page.evaluate(async () => (await import(new URL("blocks.js", location.href).href)).refreshBlocks());
  await openList(page);
  const after = (await sideRows(page)).find((r) => r.id === "chat-saved-b");
  assert.equal(after.pane, null);
  assert.equal(after.canDelete, true);
  assert.match(after.meta, /3 d ago$/);
  await page.locator('li[data-id="chat-saved-b"] button.ai-side-pick').click();
  await page.waitForFunction(() => document.querySelector(".ai-side-title .ai-side-text")?.textContent === "Plan the backup job");
}
