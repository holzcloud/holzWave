// The cards and the keyboard in the sidebar column (design.md section 6.18): the consent, approval, diff and command cards of the
// chat pane fit the column without a horizontal scroll, a decision is brought to the person whatever the column shows, the
// title-bar button opens the chat where the person already is, a hidden column gives the keyboard back, and every send names the
// pane the chat acts for.
import assert from "node:assert/strict";
import {
  FILE_OLD, PROFILE, SAVED_A, SAVED_B, TOOL_FILE, TOOL_PATH, TOOL_TEXT, VIEW, VIEW3, boot, calls, chatBlock, decisionCalls, openCard, openContext, openSide,
  overflow, pickChat, saveChats, scriptCommand, scriptTool, scriptWrite, seedFile, sendSide, tickClock,
} from "./ai-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await cardsCase(page, url);
  await wideCase(page, url);
  await revealCase(page, url);
  await buttonCase(page, url);
  await hideCase(page, url);
  await actingCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A page with two stored chats and the column showing the stored chat A (two messages). */
async function columnOnA(page, url, extra = {}) {
  await boot(page, url, { clock: true, ...extra });
  await saveChats(page, [SAVED_A, SAVED_B]);
  await openSide(page);
  await pickChat(page, "chat-saved-a");
  await page.waitForFunction(() => document.querySelectorAll("#sidebar .chat-msg").length === 2);
  await page.waitForFunction(() => document.activeElement?.matches("textarea.chat-input"));
}

/** Script one `read_file` call, send a message and wait for its card. */
async function askForFile(page, extra = {}) {
  await scriptTool(page, { id: "call_1", tool: "read_file", path: TOOL_PATH, result: TOOL_TEXT, answer: "Done.", ...extra });
  await sendSide(page, "read my notes");
  return openCard(page, "call_1");
}

const storedHash = (page, id = "chat-saved-a", call = "call_1") =>
  page.evaluate(([chat, callId]) => window.__MOCK__.ai.chat(chat).pending.find((p) => p.call_id === callId)?.hash ?? null, [id, call]);

/** The column and its strip do not scroll sideways, nothing reaches past the strip, and the strip keeps to half of the chat. */
async function assertFits(page, what) {
  const m = await overflow(page);
  assert.ok(m.sideScroll <= m.sideClient, `${what}: the column scrolls sideways (${m.sideScroll} of ${m.sideClient})`);
  assert.ok(m.stripScroll <= m.stripClient, `${what}: the strip scrolls sideways (${m.stripScroll} of ${m.stripClient})`);
  assert.ok(m.reach <= m.edge + 1, `${what}: something reaches past the strip (${m.reach} against ${m.edge})`);
  assert.ok(m.stripHeight <= m.hostHeight / 2 + 1, `${what}: the strip is ${m.stripHeight} of ${m.hostHeight}`);
  if (m.stripScrollHeight > m.stripClientHeight) assert.ok(["auto", "scroll"].includes(m.overflowY), `${what}: a taller strip scrolls`);
}

/** The smallest height of the controls inside `selector`. */
const heights = (page, selector) =>
  page.locator(selector).evaluateAll((nodes) => nodes.map((n) => n.getBoundingClientRect().height));

async function cardsCase(page, url) {
  await columnOnA(page, url);
  const card = await askForFile(page);
  assert.equal(await page.locator("#sidebar div.chat-cards > section.chat-approval").count(), 1, "the card is in the strip of the column");
  const order = await page.evaluate(() => [...document.querySelector("#sidebar .side-chat").children].map((c) => c.className.split(" ")[0]));
  assert.equal(order.indexOf("chat-cards"), order.indexOf("chat-log") + 1, `the strip follows the log: ${order}`);
  assert.equal(order.indexOf("chat-status"), order.indexOf("chat-cards") + 1, `the status line follows the strip: ${order}`);
  assert.equal(await card.getAttribute("data-mode"), "ask");
  const names = await card.locator("li.approval-row").evaluateAll((rows) => rows.map((r) => r.dataset.name));
  assert.deepEqual(names, ["tool", "pane", "target", "remote", "path", "offset", "limit", "resolved_path"]);
  assert.equal(await page.evaluate(() => document.activeElement?.className), "chat-input", "the card takes no keyboard");
  assert.deepEqual(await decisionCalls(page), []);
  assert.equal(await page.locator("#sidebar .chat-status").textContent(), "waiting for your decision");

  await assertFits(page, "the approval card");
  const wide = await card.locator("li.approval-row").evaluateAll((rows) =>
    rows.map((row) => row.querySelector(".approval-value").getBoundingClientRect().width / row.getBoundingClientRect().width));
  assert.ok(wide.every((w) => w >= 0.8), `the value sits under its name: ${wide.map((w) => w.toFixed(2))}`);
  const box = await card.locator(".approval-payload").evaluate((n) => n.getBoundingClientRect().width <= n.closest(".chat-cards").clientWidth);
  assert.equal(box, true, "the card is not wider than the strip");
  for (const h of await heights(page, "#sidebar .chat-approval .approval-allow, #sidebar .chat-approval .approval-deny, #sidebar .approval-payload")) {
    assert.ok(h >= 27.5, `a control of ${h} px`);
  }

  // One real press after the guard time decides with the card's own hash.
  const hash = await storedHash(page);
  assert.match(hash, /^[0-9a-f]{64}$/);
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await page.waitForFunction(() => document.querySelector("section.chat-approval") === null);
  assert.deepEqual(await decisionCalls(page), [{ conversation: "chat-saved-a", call_id: "call_1", decision: "allow", hash }]);
  await page.waitForFunction(() => [...document.querySelectorAll("#sidebar .chat-msg .chat-text")].some((n) => n.textContent === "Done."));
  assert.equal(await page.locator("#sidebar div.chat-cards").evaluate((n) => n.childElementCount), 0, "the strip is empty");
  assert.equal((await calls(page, "ai_send")).length, 1);
  assert.equal((await calls(page, "ai_send"))[0].pane, 2, "the send named the pane the chat acts for");
}

async function wideCase(page, url) {
  const changed = Array.from({ length: 40 }, (_, i) => `new line ${i + 1} zq-wide-${i}`).join("\n") + "\n";
  const old = Array.from({ length: 40 }, (_, i) => `old line ${i + 1}`).join("\n") + "\n";

  // A write card with a diff of about forty lines.
  await columnOnA(page, url);
  await seedFile(page, TOOL_FILE, old);
  await scriptWrite(page, { id: "call_1", tool: "write_file", path: TOOL_FILE, content: changed, answer: "Done." });
  await sendSide(page, "change the file");
  const write = await openCard(page, "call_1");
  await assertFits(page, "the write card");
  const diff = await write.locator(".approval-diff").evaluate((n) => ({
    wide: n.scrollWidth, shown: n.clientWidth, tall: n.scrollHeight, high: n.clientHeight, overflowY: getComputedStyle(n).overflowY,
  }));
  assert.ok(diff.wide <= diff.shown, "the diff wraps");
  assert.ok(diff.tall > diff.high && ["auto", "scroll"].includes(diff.overflowY), "the diff scrolls inside its box");
  for (const h of await heights(page, "#sidebar .chat-approval button")) assert.ok(h >= 27.5, `a button of ${h} px`);

  // A command card with one long word and a second line.
  await columnOnA(page, url);
  await scriptCommand(page, { id: "call_1", command: `${"q".repeat(300)}\nsecond line`, answer: "Done." });
  await sendSide(page, "run it");
  const command = await openCard(page, "call_1");
  await assertFits(page, "the command card");
  const value = await command.locator('li.approval-row[data-name="command"] .approval-value').evaluate((n) => ({ wide: n.scrollWidth, shown: n.clientWidth }));
  assert.ok(value.wide <= value.shown, "the command wraps");
  for (const h of await heights(page, "#sidebar .chat-approval button")) assert.ok(h >= 27.5, `a button of ${h} px`);

  // The consent card in its four modes, then both cards together with the approval card first.
  const findings = [{ rule: "hidden_text", count: 15 }, { rule: "anthropic_key", count: 1 }];
  const items = { screen: { label: "Screen of pane 2", bytes: 2457, findings, text: "t" } };
  await columnOnA(page, url, { context: { panes: { 1: { kinds: ["selection", "screen"], items }, 2: { kinds: ["selection", "screen", "last_command"], items } } } });
  await openContext(page, "chat-saved-a");
  await page.waitForSelector('#sidebar .chat-consent[data-mode="ask"]');
  await assertFits(page, "the consent card, ask");
  await page.locator("#sidebar .consent-share").click();
  await page.waitForSelector('#sidebar .chat-consent[data-mode="pick"]');
  await assertFits(page, "the consent card, pick");
  await page.locator('#sidebar label.consent-kind[data-kind="screen"] input').check();
  await page.locator("#sidebar .consent-preview").click();
  await page.waitForSelector('#sidebar .chat-consent[data-mode="offer"]');
  await assertFits(page, "the consent card, offer");
  for (const h of await heights(page, "#sidebar .chat-consent button, #sidebar .consent-kind, #sidebar .consent-row")) assert.ok(h >= 27.5, `a control of ${h} px`);

  await scriptTool(page, { id: "call_1", tool: "read_file", path: TOOL_PATH, result: TOOL_TEXT, answer: "Done." });
  await sendSide(page, "read my notes");
  await openCard(page, "call_1");
  await openContext(page, "chat-saved-a");
  await page.waitForSelector("#sidebar .chat-consent");
  const strip = await page.evaluate(() => [...document.querySelector("#sidebar .chat-cards").children].map((c) => c.className.split(" ")[0]));
  assert.deepEqual(strip, ["chat-approval", "chat-consent"], "the approval card comes first");
  await assertFits(page, "both cards");

  await columnOnA(page, url);
  await page.evaluate(() => window.__MOCK__.ai.context.fail("ai_context_state", "local_only"));
  await openContext(page, "chat-saved-a");
  await page.waitForSelector("#sidebar .chat-consent");
  await assertFits(page, "the consent card, blocked");
}

async function revealCase(page, url) {
  await columnOnA(page, url);
  await askForFile(page);
  assert.equal(await page.locator(".ai-btn").textContent(), "✦ AI · 1");
  assert.equal(await page.locator(".ai-btn").getAttribute("data-waiting"), "1");
  const hash = await storedHash(page);
  const quiet = async () => {
    assert.equal((await calls(page, "block_open")).length, 0, "no block was opened");
    assert.equal((await calls(page, "ai_conversation_new")).length, 1, "no further chat was made");
    assert.deepEqual(await decisionCalls(page), [], "nothing was decided");
  };

  // The sidebar form: the card is gone from the page, and the button brings it back.
  await page.locator(".ai-side-back").click();
  await page.waitForSelector('#sidebar[data-form="sidebar"]');
  assert.equal(await page.locator("section.chat-approval").count(), 0);
  await page.locator(".ai-btn").click();
  await page.waitForSelector('#sidebar[data-form="chat"] section.chat-approval[data-call="call_1"]');
  assert.ok((await page.locator(".approval-hash").textContent()).includes(hash.slice(0, 12)), "the same card, the same hash");
  assert.equal(await page.evaluate(() => document.activeElement?.matches("textarea.chat-input")), true);
  await quiet();

  // The hidden column.
  await page.keyboard.press("Meta+b");
  await page.waitForSelector("#sidebar", { state: "hidden" });
  await page.locator(".ai-btn").click();
  await page.waitForSelector('#sidebar:not([hidden])[data-form="chat"] section.chat-approval[data-call="call_1"]');
  assert.equal(await storedHash(page), hash);
  await quiet();

  // A chat pane in the tab does not take the press while the column's chat waits; after the decision it does.
  await boot(page, url, { clock: true, view: VIEW3, blocks: [chatBlock("chat-saved-b")] });
  await saveChats(page, [SAVED_A, SAVED_B]);
  await page.waitForSelector('.block[data-pane="3"].block-chat');
  await openSide(page);
  await pickChat(page, "chat-saved-a");
  await page.waitForFunction(() => document.querySelectorAll("#sidebar .chat-msg").length === 2);
  await scriptTool(page, { id: "call_1", tool: "read_file", path: TOOL_PATH, result: TOOL_TEXT, answer: "Done." });
  await sendSide(page, "read my notes");
  await openCard(page, "call_1");
  await page.locator(".ai-side-back").click();
  await page.waitForSelector('#sidebar[data-form="sidebar"]');
  await page.locator(".ai-btn").click();
  await page.waitForSelector('#sidebar[data-form="chat"] section.chat-approval');
  assert.equal((await calls(page, "focus_pane")).length, 0, "the pane was not taken");
  await tickClock(page, 1000);
  await page.locator("#sidebar button.approval-deny").click();
  await page.waitForFunction(() => document.querySelector(".ai-btn").dataset.waiting === undefined);
  await page.locator(".ai-btn").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("focus_pane").length === 1);
  assert.deepEqual((await calls(page, "focus_pane"))[0], { pane: 3 }, "with nothing waiting the chat pane is focused as before");
}

async function buttonCase(page, url) {
  // The column remembers whether it was shown, so each step starts with it hidden.
  const bootHidden = async (...args) => {
    await page.evaluate(() => localStorage.setItem("holzwave.sidebar", "0"));
    await boot(...args);
  };
  const aiPresses = (name) => page.evaluate((n) => window.__MOCK__.callsOf(n).length, name);

  // (a) the column hidden, no chat pane: a chat pane opens and the column stays hidden.
  await bootHidden(page, url);
  await page.locator(".ai-btn").click();
  await page.waitForSelector(".block-chat");
  assert.deepEqual((await calls(page, "block_open")).map((a) => a.kind), ["chat"]);
  assert.equal(await page.locator(".ai-btn").getAttribute("aria-pressed"), "true");
  assert.equal(await page.locator("#sidebar").isHidden(), true);

  // (b) the column shown as the sidebar, no chat pane: the column becomes the chat.
  await bootHidden(page, url);
  await page.keyboard.press("Meta+b");
  await page.waitForSelector("#sidebar:not([hidden])");
  assert.equal(await page.locator(".ai-btn").getAttribute("aria-pressed"), "false");
  await page.locator(".ai-btn").click();
  await page.waitForSelector('#sidebar[data-form="chat"] textarea.chat-input');
  assert.equal(await aiPresses("ai_conversation_new"), 1);
  assert.equal(await aiPresses("block_open"), 0);
  assert.equal(await page.locator(".ai-btn").getAttribute("aria-pressed"), "true");
  await page.evaluate(() => document.getElementById("ime").focus());
  const total = await page.evaluate(() => window.__MOCK__.calls.length);
  await page.locator(".ai-btn").click();
  await page.waitForFunction(() => document.activeElement?.matches("textarea.chat-input"));
  assert.equal(await page.evaluate(() => window.__MOCK__.calls.length), total, "a second press asks for nothing");
  await page.locator(".ai-side-back").click();
  await page.waitForSelector('#sidebar[data-form="sidebar"]');
  assert.equal(await page.locator(".ai-btn").getAttribute("aria-pressed"), "false");

  // (c) the column shown as the sidebar and a chat pane in the tab: the pane is focused, the form does not change.
  await bootHidden(page, url, { view: VIEW3, blocks: [chatBlock("chat-x")] });
  await page.waitForSelector('.block[data-pane="3"].block-chat');
  await page.keyboard.press("Meta+b");
  await page.waitForSelector("#sidebar:not([hidden])");
  await page.locator(".ai-btn").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("focus_pane").length === 1);
  assert.equal(await page.locator("#sidebar").getAttribute("data-form"), "sidebar");
  assert.equal(await aiPresses("ai_conversation_new"), 0);

  // (d) the chord of `ai.open` acts as the button.
  await bootHidden(page, url);
  await page.keyboard.press("Meta+Shift+a");
  await page.waitForSelector(".block-chat");
  assert.equal(await aiPresses("block_open"), 1);
  await bootHidden(page, url);
  await page.keyboard.press("Meta+b");
  await page.waitForSelector("#sidebar:not([hidden])");
  await page.keyboard.press("Meta+Shift+a");
  await page.waitForSelector('#sidebar[data-form="chat"] textarea.chat-input');
  assert.equal(await aiPresses("block_open"), 0);

  // (e) `ai.new_chat` opens a pane even while the column shows the chat, and the column's chat is unchanged.
  await bootHidden(page, url);
  await openSide(page);
  const title = await page.locator(".ai-side-title .ai-side-text").textContent();
  await page.evaluate(() => window.__registry.runAction("ai.new_chat"));
  await page.waitForSelector('.block[data-pane="3"].block-chat');
  assert.deepEqual((await calls(page, "block_open")).map((a) => [a.kind, a.path]), [["chat", "chat-mock-2"]]);
  assert.equal(await page.locator("#sidebar").getAttribute("data-form"), "chat");
  assert.equal(await page.locator(".ai-side-title .ai-side-text").textContent(), title);
  assert.deepEqual((await calls(page, "ai_turn_snapshot")).map((a) => a.conversation).filter((c, i, all) => all.indexOf(c) === i), ["chat-mock-1", "chat-mock-2"]);

  // (f) the palette lists the action.
  await bootHidden(page, url);
  await page.keyboard.press("Meta+k");
  await page.waitForSelector(".palette-input");
  await page.locator(".palette-input").fill("AI chat in the sidebar");
  await page.waitForFunction(() => [...document.querySelectorAll(".palette-title")].some((n) => n.textContent === "AI chat in the sidebar"));
  assert.equal(await page.locator(".palette-row .palette-chord").first().textContent(), "", "no chord");
}

async function hideCase(page, url) {
  await columnOnA(page, url);
  await page.locator("#sidebar .chat-input").fill("a draft");
  await page.keyboard.press("Meta+b");
  await page.waitForSelector("#sidebar", { state: "hidden" });
  assert.equal(await page.locator(".sidebar-btn").getAttribute("aria-pressed"), "false");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime", "the terminal has the keyboard");
  await page.keyboard.press("Meta+b");
  await page.waitForSelector('#sidebar:not([hidden])[data-form="chat"]');
  assert.equal(await page.locator("#sidebar .chat-input").inputValue(), "a draft");
  assert.equal(await page.locator("#sidebar .chat-msg").count(), 2, "the same transcript");
  assert.equal((await calls(page, "ai_conversation_new")).length, 1, "no further chat was made");

  // In the sidebar form the same press also gives the keyboard to the terminal.
  await page.locator(".ai-side-back").click();
  await page.waitForSelector('#sidebar[data-form="sidebar"]');
  await page.locator(".sb-input").focus();
  await page.keyboard.press("Meta+b");
  await page.waitForSelector("#sidebar", { state: "hidden" });
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime");

  // The action shows the hidden column in the AI form.
  await page.evaluate(() => window.__registry.runAction("ai.sidebar"));
  await page.waitForSelector('#sidebar:not([hidden])[data-form="chat"] textarea.chat-input');
  assert.equal(await page.locator("#sidebar .chat-input").inputValue(), "a draft");

  // A reply that runs while the column is hidden keeps streaming and is whole when the column is shown.
  await page.locator("#sidebar .chat-input").fill("");
  await page.evaluate(() => window.__MOCK__.ai.hold(2));
  await sendSide(page, "keep going");
  await page.waitForFunction(() => document.querySelector("#sidebar .chat-partial .chat-text")?.textContent === "Hello ");
  await page.keyboard.press("Meta+b");
  await page.waitForSelector("#sidebar", { state: "hidden" });
  await page.evaluate(() => window.__MOCK__.ai.release());
  await page.waitForFunction(() => window.__MOCK__.ai.chat("chat-saved-a").running === false);
  await page.keyboard.press("Meta+b");
  await page.waitForSelector("#sidebar:not([hidden])");
  await page.waitForFunction(() => document.querySelectorAll("#sidebar .chat-msg").length === 4 && !document.querySelector("#sidebar .chat-partial"));
  assert.equal(await page.locator("#sidebar .chat-log .chat-msg").last().locator(".chat-text").textContent(), "Hello there.");
}

async function actingCase(page, url) {
  const FIVE = ["conversation", "model", "pane", "profile", "text"];
  const sendAndWait = async (text, n) => {
    await sendSide(page, text);
    await page.waitForFunction((count) => window.__MOCK__.callsOf("ai_send").length === count, n);
    await page.waitForFunction(() => !document.querySelector("#sidebar .chat-partial") && window.__MOCK__.ai.chat("chat-saved-a").running === false);
    return (await calls(page, "ai_send"))[n - 1];
  };
  const focusOn = (focus, panes) =>
    page.evaluate(([f, list]) => {
      const v = window.__MOCK__.view();
      v.focus = f;
      v.tabs[0].focus = f;
      v.panes = v.panes.filter(([id]) => list.includes(id));
      v.tabs[0].panes = v.panes.length;
      window.__MOCK__.setView(v);
      window.__MOCK__.emit("layout", v);
    }, [focus, panes]);

  await columnOnA(page, url);
  const first = await sendAndWait("one", 1);
  assert.deepEqual(Object.keys(first).sort(), FIVE);
  assert.equal(first.pane, 2);

  await focusOn(1, [1, 2]);
  await page.waitForFunction(() => window.__MOCK__.view().focus === 1);
  await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  const second = await sendAndWait("two", 2);
  assert.equal(second.pane, 1, "the pane follows the focus");
  assert.deepEqual(Object.keys(second).sort(), FIVE);

  // The context card names the pane of the send.
  await openContext(page, "chat-saved-a");
  assert.ok((await page.locator("#sidebar .chat-consent").textContent()).includes("pane 1"), "the card names the acting pane");
  await page.locator("#sidebar .consent-no").click();

  // Pane 2 becomes a Files block and takes the focus: the chat still acts for the last terminal that had it.
  await page.evaluate(() => { window.__BLOCKS = [{ pane: 2, kind: "files", source: "local", path: "/", source_label: "This computer" }]; });
  await page.evaluate(async () => (await import(new URL("blocks.js", location.href).href)).refreshBlocks());
  await focusOn(2, [1, 2]);
  await page.waitForFunction(() => window.__MOCK__.view().focus === 2);
  await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  const third = await sendAndWait("three", 3);
  assert.equal(third.pane, 1, "a Files pane never counts");

  // The acting pane closes: the next send names the remaining focused pane.
  await focusOn(2, [2]);
  await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  const fourth = await sendAndWait("four", 4);
  assert.equal(fourth.pane, 2, "the pane that is left");
  assert.deepEqual(Object.keys(fourth).sort(), FIVE);

  // No terminal was ever focused: the focused pane is the fallback and nothing is reported.
  await boot(page, url, {
    starts: 0,
    blocks: [
      { pane: 1, kind: "files", source: "local", path: "/", source_label: "This computer" },
      { pane: 2, kind: "files", source: "local", path: "/", source_label: "This computer" },
    ],
  });
  await saveChats(page, [SAVED_A]);
  await openSide(page);
  await pickChat(page, "chat-saved-a");
  await page.waitForFunction(() => document.querySelectorAll("#sidebar .chat-msg").length === 2);
  const only = await sendAndWait("five", 1);
  assert.equal(only.pane, 2, "the focused pane is the only fallback");
  assert.deepEqual(Object.keys(only).sort(), FIVE);
}
