// The AI chat as a pane (docs/design.md section 6.12): a click on `✦ AI` opens a chat beside the focused pane, a typed
// message leaves as `ai_send` with exactly five arguments, the reply streams into the transcript from the core's events,
// and the mock that plays the core is held to the run the core recorded.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { OTHER_PROFILE, PROFILE, boot, calls, openAi, sendText, watchStatus } from "./ai-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await openCase(page, url);
  await sendCase(page, url);
  await contractCase(page, url);
  await faultCase(page, url);
  await pickerCase(page, url);
  await stopCase(page, url);
  await actionsCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

async function openCase(page, url) {
  await boot(page, url);
  // The button sits in the title bar after the clock and before the sidebar button.
  const order = await page.evaluate(() => [...document.querySelector(".bar-info").children].map((c) => c.className.split(" ")[0]));
  assert.ok(order.indexOf("ai-btn") > order.indexOf("clock"), `after the clock: ${order}`);
  assert.ok(order.indexOf("ai-btn") < order.indexOf("sidebar-btn"), `before the sidebar button: ${order}`);
  assert.equal(await page.locator(".ai-btn").textContent(), "✦ AI");
  assert.equal(await page.locator(".ai-btn").getAttribute("aria-pressed"), "false");

  await page.locator(".ai-btn").click();
  await page.waitForSelector('.block[data-pane="3"].block-chat');
  assert.equal((await calls(page, "ai_conversation_new")).length, 1);
  assert.deepEqual(await calls(page, "block_open"), [{ kind: "chat", source: "local", path: "chat-mock-1", placement: "right" }]);
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_turn_snapshot").length >= 1);
  assert.deepEqual((await calls(page, "ai_turn_snapshot"))[0], { conversation: "chat-mock-1" });
  assert.deepEqual(await page.evaluate(() => window.__BLOCKS), [
    { pane: 3, kind: "chat", source: "local", path: "chat-mock-1", source_label: "This computer" },
  ]);
  // The composer has the keyboard, and the header says what the pane is.
  await page.waitForFunction(() => document.activeElement?.matches("textarea.chat-input"));
  assert.equal(await page.locator('.panehead[data-pane="3"] .ph-title').textContent(), "chat");
  assert.equal(await page.locator('.panehead[data-pane="3"] .ph-ctx').textContent(), "new chat");
  assert.equal(await page.locator(".ai-btn").getAttribute("aria-pressed"), "true", "pressed while the tab has a chat");

  // `ai.open` with a chat in the tab only focuses it: no new chat, no new pane.
  await page.evaluate(() => document.getElementById("ime").focus());
  await page.evaluate(() => window.__registry.runAction("ai.open"));
  await page.waitForFunction(() => document.activeElement?.matches("textarea.chat-input"));
  assert.equal((await calls(page, "ai_conversation_new")).length, 1);
  assert.equal((await calls(page, "block_open")).length, 1);
}

async function sendCase(page, url) {
  await boot(page, url);
  await page.evaluate(() => window.__MOCK__.ai.hold(2));
  await openAi(page);
  await sendText(page, "hello there");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  const sent = (await calls(page, "ai_send"))[0];
  assert.deepEqual(Object.keys(sent).sort(), ["conversation", "model", "pane", "profile", "text"], "exactly five arguments");
  assert.deepEqual(sent, { conversation: "chat-mock-1", text: "hello there", pane: 2, profile: "local-1", model: null }, "the pane the chat acts for");

  // While the reply streams it is text: no element inside, no finished assistant message yet.
  await page.waitForFunction(() => document.querySelector(".chat-partial .chat-text")?.textContent === "Hello ");
  assert.equal(await page.locator('.chat-msg[data-role="user"] .chat-text').textContent(), "hello there");
  assert.equal(await page.locator(".chat-partial .chat-text").evaluate((n) => n.children.length), 0);
  assert.equal(await page.locator('.chat-msg[data-role="assistant"]:not(.chat-partial)').count(), 0);

  await page.evaluate(() => window.__MOCK__.ai.release());
  await page.waitForFunction(() => document.querySelectorAll(".chat-partial").length === 0 && document.querySelectorAll(".chat-msg").length === 2);
  assert.equal(await page.locator('.chat-msg[data-role="assistant"] .chat-text').textContent(), "Hello there.");
  assert.equal(await page.locator('.chat-msg[data-role="user"]').count(), 1);
  assert.equal(await page.locator(".chat-input").inputValue(), "", "the draft left with the message");
  assert.equal(await page.locator('.panehead[data-pane="3"] .ph-ctx').textContent(), "hello there", "the title the core made");
  assert.equal((await calls(page, "ai_send")).length, 1);
}

/** The recorded run, read from the golden file of the core. */
const RECORDED = JSON.parse(readFileSync(new URL("../fixtures/ai_stream.json", import.meta.url), "utf8"));

async function contractCase(page, url) {
  await boot(page, url);
  const run = await page.evaluate(async () => {
    const ai = window.__MOCK__.ai;
    const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);
    const settle = (n) => new Promise((resolve, reject) => {
      const t0 = Date.now();
      const tick = () => {
        if (ai.sent.filter((e) => e.kind === "turn_ended").length >= n) resolve();
        else if (Date.now() - t0 > 3000) reject(new Error("the mock did not end the turn"));
        else setTimeout(tick, 5);
      };
      tick();
    });
    const chat = await invoke("ai_conversation_new");
    const snapshots = [];
    const snap = async () => snapshots.push(await invoke("ai_turn_snapshot", { conversation: chat }));
    // The three turns of the recorded run: finished with reasoning and usage, failed with an overload, cancelled mid-stream.
    ai.script({ reasoning: "Checking the question.", chunks: ["Hello there."], usage: { input_tokens: 11, output_tokens: 5 } });
    await invoke("ai_send", { conversation: chat, text: "What is a pty?", pane: 1, profile: "local-1", model: null });
    await settle(1);
    await snap();
    ai.script({ fail: { class: "overload", text: "the provider is overloaded" } });
    await invoke("ai_send", { conversation: chat, text: "And a tty?", pane: 1, profile: null, model: null });
    await settle(2);
    ai.script({ chunks: ["partial"], stall: true });
    await invoke("ai_send", { conversation: chat, text: "Explain a pipe.", pane: 1, profile: null, model: null });
    await new Promise((resolve) => {
      const tick = () => (ai.sent.some((e) => e.kind === "delta" && e.text === "partial") ? resolve() : setTimeout(tick, 5));
      tick();
    });
    await snap();
    await invoke("ai_cancel", { conversation: chat });
    await settle(3);
    await snap();
    // Every event and state the mock sent must be accepted unchanged by the page's readers.
    const { readChatEvent, readChatState } = await import(new URL("chatstate.js", location.href).href);
    const canon = (v) => JSON.stringify(v, (_k, x) => (x && typeof x === "object" && !Array.isArray(x) ? Object.fromEntries(Object.entries(x).sort(([a], [b]) => (a < b ? -1 : 1))) : x));
    const unchanged = {
      events: ai.sent.map((e) => canon(readChatEvent(e)) === canon(e)),
      states: snapshots.map((s) => canon(readChatState(s)) === canon(s)),
    };
    return { events: ai.sent, snapshots, unchanged };
  });

  const names = (o) => Object.keys(o).sort();
  /** The first event of each kind, in a given turn index of the recording. */
  const firstOf = (events, kind) => events.find((e) => e.kind === kind);
  for (const kind of ["turn_started", "reasoning", "delta", "usage", "message", "turn_ended"]) {
    assert.deepEqual(names(firstOf(run.events, kind)), names(firstOf(RECORDED.events, kind)), `the members of a ${kind} event`);
  }
  const recorded = RECORDED.events.find((e) => e.kind === "turn_started");
  const mocked = run.events.find((e) => e.kind === "turn_started");
  assert.deepEqual(names(mocked.message), names(recorded.message), "turn_started.message");
  assert.deepEqual(names(mocked.route), names(recorded.route), "route");
  const failedRecorded = RECORDED.events.find((e) => e.kind === "turn_ended" && e.error);
  const failedMocked = run.events.find((e) => e.kind === "turn_ended" && e.error);
  assert.deepEqual(names(failedMocked.error), names(failedRecorded.error), "turn_ended.error");
  assert.deepEqual(names(failedMocked), names(failedRecorded), "a failed turn_ended");
  const cancelledRecorded = RECORDED.events.find((e) => e.kind === "message" && e.message.status === "cancelled");
  const cancelledMocked = run.events.find((e) => e.kind === "message" && e.message.status === "cancelled");
  assert.deepEqual(names(cancelledMocked.message), names(cancelledRecorded.message), "a cancelled message");

  // The order of kinds in each turn, with repeated deltas collapsed.
  const kinds = (events) => {
    const out = new Map();
    for (const e of events) {
      const list = out.get(e.turn - Math.min(...events.map((x) => x.turn))) ?? [];
      if (list[list.length - 1] !== e.kind) list.push(e.kind);
      out.set(e.turn - Math.min(...events.map((x) => x.turn)), list);
    }
    return [...out.values()];
  };
  assert.deepEqual(kinds(run.events), kinds(RECORDED.events), "the kinds of each turn in order");
  // Seq runs 1..n inside each turn.
  for (const turn of new Set(run.events.map((e) => e.turn))) {
    const seqs = run.events.filter((e) => e.turn === turn).map((e) => e.seq);
    assert.deepEqual(seqs, seqs.map((_, i) => i + 1), "seq counts from 1 in a turn");
  }

  // The state: the members of the state, of its first message, of `partial` and of `ended`.
  const last = RECORDED.states[RECORDED.states.length - 1];
  const mockedLast = run.snapshots[run.snapshots.length - 1];
  assert.deepEqual(names(mockedLast), names(last), "the members of a state");
  assert.deepEqual(names(mockedLast.messages[0]), names(last.messages[0]), "the members of a message");
  assert.deepEqual(names(mockedLast.partial), names(last.partial), "partial");
  assert.deepEqual(names(mockedLast.ended), names(last.ended), "ended");
  assert.deepEqual(names(mockedLast.usage), names(last.usage), "usage");
  assert.deepEqual(names(mockedLast.trimmed), names(last.trimmed), "trimmed");
  // The mock's last state is the recorded one but for the chat id and the ids of the turns.
  assert.deepEqual(mockedLast.messages.map((m) => [m.role, m.status]), last.messages.map((m) => [m.role, m.status]));
  assert.equal(mockedLast.title, last.title);

  assert.ok(run.unchanged.events.length >= 12 && run.unchanged.events.every(Boolean), "every event passes the reader unchanged");
  assert.ok(run.unchanged.states.every(Boolean), "every state passes the reader unchanged");
}

async function faultCase(page, url) {
  await boot(page, url, { profiles: [] });
  await openAi(page);
  // A blank draft sends nothing.
  await page.locator(".chat-input").press("Enter");
  assert.equal((await calls(page, "ai_send")).length, 0);
  // With no profile the page names none and the core decides; its refusal is shown in its words.
  await sendText(page, "hello");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  assert.equal((await calls(page, "ai_send"))[0].profile, null);
  await page.waitForFunction(() => document.querySelector(".chat-status")?.textContent === "no provider is chosen");
  assert.equal(await page.locator(".chat-status").getAttribute("data-kind"), "error");
  assert.equal(await page.locator(".chat-input").inputValue(), "hello", "the draft stays");
  assert.equal(await page.locator(".chat-msg").count(), 0, "no message was added");
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.textContent === "no provider set up");
  assert.equal(await page.locator("button.chat-chip").count(), 0, "no button: there is nothing to choose");
  // Typing clears the status.
  await page.locator(".chat-input").press("x");
  await page.waitForFunction(() => document.querySelector(".chat-status")?.textContent === "");
}

async function pickerCase(page, url) {
  await boot(page, url, { profiles: [PROFILE, OTHER_PROFILE] });
  await openAi(page);
  const chip = page.locator('.block[data-pane="3"] .chat-chip');
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length >= 1);
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.tagName === "BUTTON");
  assert.notEqual(await chip.getAttribute("aria-haspopup"), null, "the chip announces a popup");
  assert.equal(await chip.getAttribute("aria-expanded"), "false");

  // Two profiles and no choice: the first Enter sends nothing, opens the picker and says why.
  await sendText(page, "which one?");
  await page.waitForSelector(".chat-picker");
  assert.equal((await calls(page, "ai_send")).length, 0);
  assert.equal(await page.locator(".chat-picker").getAttribute("role"), "radiogroup");
  assert.equal(await page.locator('.chat-picker .chat-choice[role="radio"]').count(), 2);
  assert.equal(await page.locator(".chat-status").textContent(), "Choose a provider first.");
  assert.equal(await page.locator(".chat-input").inputValue(), "which one?", "the draft stays");
  assert.equal(await chip.getAttribute("aria-expanded"), "true");
  const radio = (n) => page.locator(".chat-choice").nth(n);
  assert.equal(await page.evaluate(() => document.activeElement === document.querySelector(".chat-choice")), true, "focus is on the first row");
  assert.deepEqual(await page.locator(".chat-choice").evaluateAll((rs) => rs.map((r) => r.getAttribute("aria-checked"))), ["false", "false"]);
  // Arrows check and move; Enter closes and gives the focus back to the chip.
  await page.keyboard.press("ArrowDown");
  assert.equal(await radio(1).getAttribute("aria-checked"), "true");
  assert.equal(await page.evaluate(() => document.activeElement === document.querySelectorAll(".chat-choice")[1]), true);
  assert.equal(await radio(1).textContent(), "●Cloudbig-modelapi.example.org");
  assert.equal(await radio(0).textContent(), "○Localllama3.2localhost");
  await page.keyboard.press("Enter");
  assert.equal(await page.locator(".chat-picker").count(), 0, "the picker is closed");
  assert.equal(await page.evaluate(() => document.activeElement?.classList.contains("chat-chip")), true, "the chip has the focus");
  assert.match(await chip.textContent(), /Cloud · big-model → api\.example\.org/);

  // The next Enter sends with the chosen profile.
  await page.locator(".chat-input").focus();
  await page.locator(".chat-input").press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  assert.equal((await calls(page, "ai_send"))[0].profile, "other-1");
  await page.waitForFunction(() => document.querySelectorAll(".chat-msg").length === 2 && !document.querySelector(".chat-partial"));
  // The chip names the host the core's route names, and the place word comes from the route.
  assert.equal(await page.locator(".chat-where").textContent(), "cloud");
  assert.equal(await page.locator(".chat-where").getAttribute("data-leaves"), "true");
  assert.ok((await chip.getAttribute("aria-label")).includes("api.example.org"));
  assert.equal(await page.locator(".chat-usage").textContent(), "in 11 · out 5");

  // A click on the chip reads the list again.
  const before = (await calls(page, "ai_profiles")).length;
  await chip.click();
  await page.waitForSelector(".chat-picker");
  assert.equal((await calls(page, "ai_profiles")).length, before + 1);
  assert.equal(await page.locator('.chat-choice[aria-checked="true"]').count(), 1);
  await page.keyboard.press("Escape");
  assert.equal(await page.locator(".chat-picker").count(), 0);
  assert.equal(await page.evaluate(() => document.activeElement?.classList.contains("chat-chip")), true);

  // A second new chat in the same window starts with the last choice, and the first send needs no picker.
  await page.evaluate(() => window.__registry.runAction("ai.new_chat"));
  await page.waitForSelector('.block[data-pane="4"].block-chat');
  await page.waitForFunction(() => /Cloud/.test(document.querySelector('.block[data-pane="4"] .chat-chip')?.textContent ?? ""));
  await page.locator('.block[data-pane="4"] .chat-input').fill("again");
  await page.locator('.block[data-pane="4"] .chat-input').press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 2);
  assert.deepEqual((await calls(page, "ai_send"))[1], { conversation: "chat-mock-2", text: "again", pane: 2, profile: "other-1", model: null });
  assert.equal(await page.locator(".chat-picker").count(), 0);

  // With one saved profile the chip is plain text and the picker never exists.
  await boot(page, url, { profiles: [PROFILE] });
  await openAi(page);
  await page.waitForFunction(() => /Local/.test(document.querySelector(".chat-chip")?.textContent ?? ""));
  assert.equal(await page.locator(".chat-chip").evaluate((n) => n.tagName), "SPAN");
  assert.equal(await page.locator(".chat-picker").count(), 0);
}

async function stopCase(page, url) {
  await boot(page, url);
  await page.evaluate(() => window.__MOCK__.ai.hold(2));
  await openAi(page);
  await watchStatus(page);
  await sendText(page, "stop me");
  await page.waitForFunction(() => document.querySelector(".chat-partial .chat-text")?.textContent === "Hello ");
  const send = page.locator(".chat-send");
  await page.waitForFunction(() => document.querySelector(".chat-send")?.dataset.mode === "stop");
  assert.equal(await send.textContent(), "Stop");
  await page.waitForFunction(() => document.querySelector(".chat-status")?.textContent === "writing");
  assert.equal(await page.locator(".chat-partial .chat-who").textContent(), "assistant · writing");

  // Two presses before the turn has ended are one call: a turn is cancelled once.
  await page.evaluate(() => {
    const stop = document.querySelector(".chat-send");
    stop.click();
    stop.click();
  });
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_cancel").length === 1);
  assert.deepEqual((await calls(page, "ai_cancel"))[0], { conversation: "chat-mock-1" });
  await page.waitForFunction(() => document.querySelector('.chat-msg[data-role="assistant"]')?.dataset.status === "cancelled");
  assert.equal((await calls(page, "ai_cancel")).length, 1, "one call for one turn");
  assert.equal(await page.locator('.chat-msg[data-role="assistant"] .chat-stat').textContent(), "stopped");
  assert.equal(await page.locator('.chat-msg[data-role="assistant"] .chat-text').textContent(), "Hello ");
  await page.waitForFunction(() => document.querySelector(".chat-send")?.dataset.mode === "send");
  assert.equal(await send.textContent(), "Send");
  await page.waitForFunction(() => document.querySelector(".chat-status")?.textContent === "reply stopped");
  const seen = await page.evaluate(() => window.__STATUS);
  const at = (word) => seen.indexOf(word);
  for (const word of ["writing", "stopping", "reply stopped"]) assert.ok(at(word) >= 0, `the status said ${word}: ${seen}`);
  assert.ok(at("writing") < at("stopping") && at("stopping") < at("reply stopped"), `in order: ${seen}`);
  assert.equal(await page.locator(".chat-end").count(), 0, "the stopped message says it already");

  // Escape in the composer stops a running reply, with the same call.
  await page.evaluate(() => window.__MOCK__.ai.hold(2));
  await sendText(page, "again");
  await page.waitForFunction(() => document.querySelector(".chat-send")?.dataset.mode === "stop" && document.querySelector(".chat-partial"));
  await page.locator(".chat-input").press("Escape");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_cancel").length === 2);
  assert.deepEqual((await calls(page, "ai_cancel"))[1], { conversation: "chat-mock-1" });
  await page.waitForFunction(() => document.querySelector(".chat-send")?.dataset.mode === "send");

  // Escape with nothing running gives the keyboard back to the terminal and calls nothing.
  await page.locator(".chat-input").focus();
  await page.locator(".chat-input").press("Escape");
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime");
  assert.equal((await calls(page, "ai_cancel")).length, 2);

  // A turn with reasoning only says thinking.
  await page.evaluate(() => {
    window.__MOCK__.ai.release();
    window.__MOCK__.ai.script({ reasoning: "hm", chunks: [], stall: true });
  });
  await page.locator(".chat-input").focus();
  await sendText(page, "think");
  await page.waitForFunction(() => document.querySelector(".chat-status")?.textContent === "thinking");
  assert.equal(await page.locator(".chat-send").getAttribute("data-mode"), "stop");
  await page.locator(".chat-send").click();
  await page.waitForFunction(() => document.querySelector(".chat-send")?.dataset.mode === "send");
}

async function actionsCase(page, url) {
  await boot(page, url);
  // The palette lists both actions.
  await page.keyboard.press("Meta+k");
  await page.waitForFunction(() => document.activeElement?.classList.contains("palette-input"));
  await page.locator(".palette-input").fill("AI");
  const titles = await page.locator(".palette-row .palette-title").allTextContents();
  assert.ok(titles.includes("AI chat"), `the palette lists AI chat: ${titles}`);
  assert.ok(titles.includes("New AI chat"), `the palette lists New AI chat: ${titles}`);
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => document.activeElement?.id === "ime");

  // The chord opens a chat when the tab has none, and only focuses it when it has one. It reaches no terminal.
  await page.keyboard.press("Meta+Shift+A");
  await page.waitForSelector('.block[data-pane="3"].block-chat');
  assert.equal((await calls(page, "ai_conversation_new")).length, 1);
  await page.evaluate(() => document.getElementById("ime").focus());
  await page.keyboard.press("Meta+Shift+A");
  await page.waitForFunction(() => document.activeElement?.matches("textarea.chat-input"));
  assert.equal((await calls(page, "ai_conversation_new")).length, 1, "the second press made no chat");
  assert.equal((await calls(page, "block_open")).length, 1);
  assert.equal((await calls(page, "key")).length, 0, "the chord reached no terminal");

  // `ai.new_chat` always starts a conversation in a new pane.
  await page.evaluate(() => window.__registry.runAction("ai.new_chat"));
  await page.waitForSelector('.block[data-pane="4"].block-chat');
  assert.equal((await calls(page, "ai_conversation_new")).length, 2);
  assert.equal((await calls(page, "block_open")).length, 2);
  assert.deepEqual((await calls(page, "block_open"))[1], { kind: "chat", source: "local", path: "chat-mock-2", placement: "right" });
}
