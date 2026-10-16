// The state of the AI chat page against the core's events (docs/design.md section 6.12): a hole in the events is repaired
// by one snapshot, the window asks again when it gains the focus, the provider list is read only when the user acts, a
// reply stops when its pane is closed (and not when the pane is only hidden), the composer refuses what the core would
// refuse, and a failed or cut turn has its end row in words.
import assert from "node:assert/strict";
import { VIEW3, boot, calls, chatBlock, openAi, sendText } from "./ai-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await gapCase(page, url);
  await focusCase(page, url);
  await lazyCase(page, url);
  await closeCase(page, url);
  await limitCase(page, url);
  await endCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** Wait until the number of `ai_turn_snapshot` calls stops changing, and answer it. */
async function settledSnapshots(page) {
  let last = -1;
  for (let i = 0; i < 20; i++) {
    const n = (await calls(page, "ai_turn_snapshot")).length;
    if (n === last) return n;
    last = n;
    await page.waitForTimeout(60);
  }
  throw new Error("the snapshot requests did not settle");
}

async function gapCase(page, url) {
  await boot(page, url);
  await page.evaluate(() => {
    window.__MOCK__.ai.hold(0);
    window.__MOCK__.ai.script({ chunks: ["a", "b", "c", "d", "e"] });
  });
  await openAi(page);
  await sendText(page, "gap");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  const before = await settledSnapshots(page);
  assert.ok(before >= 2, "one at the mount and one after the send");

  // The first event is lost on its way; the next one shows the hole.
  await page.evaluate(() => {
    window.__MOCK__.ai.drop(1);
    window.__MOCK__.ai.release();
  });
  await page.waitForFunction(() => document.querySelectorAll(".chat-msg").length === 2 && !document.querySelector(".chat-partial"));
  assert.equal(await settledSnapshots(page), before + 1, "one snapshot repaired the hole, although several deltas followed");
  assert.equal(await page.locator('.chat-msg[data-role="assistant"] .chat-text').textContent(), "abcde", "no text lost or doubled");
  assert.equal(await page.locator('.chat-msg[data-role="user"] .chat-text').textContent(), "gap", "the lost start came with the snapshot");
}

async function focusCase(page, url) {
  await boot(page, url);
  await openAi(page);
  assert.equal(await settledSnapshots(page), 1, "a chat that was just opened asks once");
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_turn_snapshot").length === 2);
  assert.equal(await settledSnapshots(page), 2, "a window focus asks once per mounted chat");
  await sendText(page, "hello");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_turn_snapshot").length >= 3);
  assert.equal(await settledSnapshots(page), 3, "a send that returned asks once");
  await page.waitForFunction(() => document.querySelectorAll(".chat-msg").length === 2 && !document.querySelector(".chat-partial"));
  assert.equal(await settledSnapshots(page), 3, "the reply needed no more");
}

async function lazyCase(page, url) {
  // A chat restored with a pin reads no keychain: the list is read when the user acts.
  await boot(page, url, {
    view: VIEW3,
    blocks: [chatBlock("chat-x")],
    states: { "chat-x": { title: "Earlier question", profile: "local-1", model: "llama3.2" } },
  });
  await page.waitForSelector('.block[data-pane="3"].block-chat');
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.textContent !== "");
  await page.waitForTimeout(500);
  assert.equal((await calls(page, "ai_profiles")).length, 0, "no profile list at mount or after it");
  assert.equal(await page.locator(".chat-chip").textContent(), "local-1 · llama3.2", "the pin from the snapshot, no place word");
  assert.equal(await page.locator(".chat-where").evaluate((n) => n.hidden || n.textContent === ""), true);
  assert.equal(await page.locator('.panehead[data-pane="3"] .ph-ctx').textContent(), "Earlier question");

  await page.locator(".chat-input").focus();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length === 1);
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.textContent === "Local · llama3.2 → localhost");
  await sendText(page, "with the pin");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  assert.equal((await calls(page, "ai_send"))[0].profile, "local-1");
  await page.waitForFunction(() => document.querySelectorAll(".chat-msg").length === 2 && !document.querySelector(".chat-partial"));
  assert.equal((await calls(page, "ai_profiles")).length, 1, "the send used the pin and the list that was already read");

  // A new chat reads the list once, at the first focus of its composer, and not again at the second send.
  await boot(page, url);
  await openAi(page);
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length === 1);
  await sendText(page, "one");
  await page.waitForFunction(() => document.querySelectorAll(".chat-msg").length === 2 && !document.querySelector(".chat-partial"));
  await sendText(page, "two");
  await page.waitForFunction(() => document.querySelectorAll(".chat-msg").length === 4 && !document.querySelector(".chat-partial"));
  assert.equal((await calls(page, "ai_profiles")).length, 1);
}

async function closeCase(page, url) {
  const open = (extra = {}) =>
    boot(page, url, {
      view: VIEW3,
      blocks: [chatBlock("chat-x")],
      states: { "chat-x": { title: "Earlier question", profile: "local-1", model: "llama3.2" } },
      ...extra,
    });
  const refresh = () => page.evaluate(async () => (await import(new URL("blocks.js", location.href).href)).refreshBlocks());
  const hasTitle = () =>
    page.evaluate(async () => (await import(new URL("blockstate.js", location.href).href)).chatTitles.has("chat-x"));
  const running = async () => {
    await page.waitForSelector('.block[data-pane="3"].block-chat');
    await page.evaluate(() => window.__MOCK__.ai.hold(1));
    await page.locator(".chat-input").focus();
    await sendText(page, "keep going");
    await page.waitForFunction(() => document.querySelector(".chat-send")?.dataset.mode === "stop");
  };

  // An idle chat that closes makes no call, and its title is forgotten.
  await open();
  await page.waitForSelector('.block[data-pane="3"].block-chat');
  await page.waitForFunction(() => document.querySelector('.panehead[data-pane="3"] .ph-ctx')?.textContent === "Earlier question");
  assert.equal(await hasTitle(), true);
  await page.evaluate(() => { window.__BLOCKS = []; });
  await refresh();
  await page.waitForFunction(() => document.querySelectorAll(".block-chat").length === 0);
  assert.equal((await calls(page, "ai_cancel")).length, 0);
  assert.equal(await hasTitle(), false);

  // A running chat whose pane left the list is stopped, once.
  await open();
  await running();
  await page.evaluate(() => { window.__BLOCKS = []; });
  await refresh();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_cancel").length === 1);
  assert.deepEqual((await calls(page, "ai_cancel"))[0], { conversation: "chat-x" });
  assert.equal(await hasTitle(), false);

  // A list that cannot be read stops nothing, and a hidden pane is not a closed one.
  await open();
  await running();
  await page.evaluate(() => window.__MOCK__.on("pane_blocks", () => { throw "the shell is busy"; }));
  await refresh();
  await page.waitForFunction(() => document.querySelectorAll(".block-chat").length === 0);
  await page.evaluate(() => window.__MOCK__.on("pane_blocks", () => structuredClone(window.__BLOCKS)));
  await refresh();
  await page.waitForSelector('.block[data-pane="3"].block-chat');
  assert.equal((await calls(page, "ai_cancel")).length, 0, "an unreadable list stops nothing");
  assert.equal(await page.locator('.chat-msg[data-role="user"] .chat-text').textContent(), "keep going", "the session was kept");
  const hidden = { ...VIEW3, panes: VIEW3.panes.slice(0, 2), tabs: [{ title: "shell", active: true, focus: 2, panes: 2 }] };
  await page.evaluate((v) => window.__MOCK__.emit("layout", v), hidden);
  await page.waitForFunction(() => document.querySelectorAll(".block-chat").length === 0);
  await page.evaluate((v) => window.__MOCK__.emit("layout", v), VIEW3);
  await page.waitForSelector('.block[data-pane="3"].block-chat');
  assert.equal((await calls(page, "ai_cancel")).length, 0, "a tab switch stops nothing");
  assert.equal(await page.locator('.chat-msg[data-role="user"] .chat-text').textContent(), "keep going", "the transcript is back");
}

async function limitCase(page, url) {
  await boot(page, url);
  await openAi(page);
  const input = page.locator(".chat-input");
  const count = page.locator(".chat-count");
  await input.fill("a".repeat(3000));
  assert.equal(await count.textContent(), "≈ 1k tokens");
  assert.equal(await count.getAttribute("data-over"), null);
  assert.equal(await page.locator(".chat-send").isDisabled(), false);

  // 65537 two-byte characters are 131074 bytes: two over the limit.
  await input.fill("é".repeat(65537));
  assert.equal(await count.textContent(), "too long · 131074 of 131072 bytes");
  assert.equal(await count.getAttribute("data-over"), "true");
  assert.match(await count.evaluate((n) => getComputedStyle(n).textDecorationLine), /underline/);
  assert.equal(await page.locator(".chat-send").isDisabled(), true);
  await input.press("Enter");
  assert.equal((await calls(page, "ai_send")).length, 0, "Enter sends nothing over the limit");
  await input.press("Backspace");
  await input.press("Backspace");
  assert.equal(await count.getAttribute("data-over"), null);
  assert.equal(await page.locator(".chat-send").isDisabled(), false);
  assert.match(await count.textContent(), /^≈ [0-9.]+k tokens$/);

  // The composer grows with its text, to eight rows at most.
  await input.fill("x");
  const one = await input.evaluate((n) => n.getBoundingClientRect().height);
  await input.fill(Array.from({ length: 30 }, (_, i) => `line ${i}`).join("\n"));
  const many = await input.evaluate((n) => n.getBoundingClientRect().height);
  const metrics = await input.evaluate((n) => {
    const css = getComputedStyle(n);
    const px = (v) => parseFloat(v);
    return { line: px(css.lineHeight), extra: px(css.paddingTop) + px(css.paddingBottom) + px(css.borderTopWidth) + px(css.borderBottomWidth) };
  });
  assert.ok(many > one + metrics.line, `it grew (${one} to ${many})`);
  assert.ok(many <= 8 * metrics.line + metrics.extra + 1, `eight rows at most (${many})`);
  await input.fill("");
  assert.equal(await count.textContent(), "");
}

async function endCase(page, url) {
  await boot(page, url);
  await page.evaluate(() => window.__MOCK__.ai.script({ fail: { class: "overload", text: "The provider is overloaded." } }));
  await openAi(page);
  await sendText(page, "first question");
  await page.waitForSelector('.chat-end[data-kind="error"]');
  assert.equal(await page.locator(".chat-end-label").textContent(), "failed · overload");
  assert.equal(await page.locator(".chat-end-text").textContent(), "The provider is overloaded.");
  await page.waitForFunction(() => document.querySelector(".chat-status")?.textContent === "reply failed");
  assert.equal(await page.locator('.chat-msg[data-role="assistant"]').count(), 0, "a failed turn has no reply");

  // A turn cut by a limit has a note with the limit in words; the row of the failed turn is gone.
  await page.evaluate(() => window.__MOCK__.ai.script({ limit: "limit_steps" }));
  const canary = "zq-chat-ui-4f27a9";
  await sendText(page, `second ${canary}`);
  await page.waitForSelector('.chat-end[data-kind="note"]');
  assert.equal(await page.locator(".chat-end-label").textContent(), "stopped · step limit");
  assert.equal(await page.locator(".chat-end").count(), 1);
  // What the user typed is a text node and an argument of the call, and in no attribute of the chat.
  assert.equal((await calls(page, "ai_send"))[1].text, `second ${canary}`);
  assert.equal(await page.locator('.chat-msg[data-role="user"] .chat-text').nth(1).textContent(), `second ${canary}`);
  const inAttribute = await page.evaluate((word) => {
    const found = [];
    for (const node of document.querySelectorAll(".block-chat *")) {
      if (node.matches("textarea.chat-input")) continue;
      for (const attr of node.attributes) if (attr.value.includes(word)) found.push(`${node.tagName}.${attr.name}`);
    }
    return found;
  }, canary);
  assert.deepEqual(inAttribute, []);

  // A message the core marked interrupted says so.
  const message = (id, role, status, text) => ({ id, role, ts: 1800000000, model: null, status, usage: null, parts: [{ type: "text", text }] });
  await boot(page, url, {
    view: VIEW3,
    blocks: [chatBlock("chat-y")],
    states: {
      "chat-y": {
        title: "Cut",
        turn: 1800000000,
        seq: 2,
        messages: [message("m1-user", "user", "complete", "question"), message("m1-1", "assistant", "interrupted", "half an answ")],
      },
    },
  });
  await page.waitForSelector('.chat-msg[data-status="interrupted"]');
  assert.equal(await page.locator('.chat-msg[data-status="interrupted"] .chat-stat').textContent(), "interrupted");
  assert.equal(await page.locator('.chat-msg[data-status="interrupted"] .chat-who').textContent(), "assistant · interrupted");
  assert.equal(await page.locator(".chat-end").count(), 0);
}
