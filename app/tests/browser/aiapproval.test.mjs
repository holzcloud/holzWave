// The approval card of a tool call (docs/design.md section 6.15): the card shows the exact request the core sent, one real
// press decides it with the four ids and the card's own hash, and the core's result removes it. The page decides nothing
// itself and sends ids only, and a mock that plays the core is held to the recorded tool run and to the command table.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  OTHER_PROFILE, PROFILE, TOOL_PATH, TOOL_TEXT, VIEW_P1, boot, calls, decisionCalls, openAi, openCard, reloadKeeping, scriptTool, sendText,
  tickClock,
} from "./ai-lib.mjs";

const GOLDEN = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));
const RUN = JSON.parse(readFileSync(new URL("../fixtures/ai_tool_stream.json", import.meta.url), "utf8"));

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(5000);
  await traceCase(page, url);
  await contractCase(page, url);
  await idsCase(page, url);
  await denyCase(page, url);
  await guardCase(page, url);
  await gapCase(page, url);
  await reloadCase(page, url);
  await cancelCase(page, url);
  await keysCase(page, url, engine);
  await badgeCase(page, url);
  await rowsCase(page, url);
  await bannerCase(page, url);
  await destinationCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A page with the local profile, two terminals, the settings core, a frozen clock and a chat beside pane 1 (the chat is pane 3). */
async function chatBesidePane1(page, url, extra = {}) {
  await boot(page, url, { view: VIEW_P1, settings: true, clock: true, ...extra });
  await openAi(page);
  await page.waitForFunction(() => document.activeElement?.matches("textarea.chat-input"));
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length >= 1);
}

/** Script one `read_file` call of the tests, send a message and wait for its card. */
async function askForFile(page, extra = {}) {
  await scriptTool(page, { id: "call_1", tool: "read_file", path: TOOL_PATH, result: TOOL_TEXT, answer: "Done.", ...extra });
  await sendText(page, "read my notes");
  return openCard(page, "call_1");
}

/** The hash the mock holds for a waiting call. */
const storedHash = (page, id = "call_1") =>
  page.evaluate((call) => window.__MOCK__.ai.chat("chat-mock-1").pending.find((p) => p.call_id === call)?.hash ?? null, id);

/** Wait for the final answer and the end of the turn. */
async function finished(page) {
  await page.waitForFunction(
    () => [...document.querySelectorAll('.chat-msg[data-role="assistant"] .chat-text')].some((n) => n.textContent === "Done.") &&
      window.__MOCK__.ai.chat("chat-mock-1").ended?.end === "done",
  );
}

async function traceCase(page, url) {
  await chatBesidePane1(page, url);
  const card = await askForFile(page);

  // The card is in the strip between the transcript and the status line, and says what the core sent.
  assert.equal(await page.locator("div.chat-cards > section.chat-approval").count(), 1);
  const order = await page.evaluate(() => [...document.querySelector(".block-chat").children].map((c) => c.className.split(" ")[0]));
  assert.equal(order.indexOf("chat-cards"), order.indexOf("chat-log") + 1, `the strip follows the log: ${order}`);
  assert.equal(order.indexOf("chat-status"), order.indexOf("chat-cards") + 1, `the status line follows the strip: ${order}`);
  assert.equal(await card.getAttribute("data-mode"), "ask");
  assert.equal(await card.getAttribute("data-tool"), "read_file");
  assert.equal(await card.getAttribute("data-effect"), "read");
  const text = await card.textContent();
  for (const words of ["Approval needed", "Read a file", "pane 1 · This computer"]) assert.ok(text.includes(words), `${words} in ${text}`);
  const names = await card.locator("li.approval-row").evaluateAll((rows) => rows.map((r) => r.dataset.name));
  assert.deepEqual(names, ["tool", "pane", "target", "remote", "path", "offset", "limit", "resolved_path"]);
  assert.equal(await card.locator('li.approval-row[data-name="path"] .approval-value').textContent(), TOOL_PATH);
  assert.equal(await card.locator("button.approval-allow").textContent(), "Allow once");
  assert.equal(await card.locator("button.approval-deny").textContent(), "Deny");
  assert.equal(await card.locator("button.approval-allow").isEnabled(), true);
  assert.equal(await card.locator("button.approval-deny").isEnabled(), true);

  // The card never takes the keyboard, and nothing is decided yet.
  assert.equal(await page.evaluate(() => document.activeElement?.className), "chat-input");
  assert.deepEqual(await decisionCalls(page), []);

  // With the consent card open as well the approval card comes first, and closing the other leaves it.
  await page.locator("button.chat-ctx").click();
  await page.waitForSelector(".chat-consent");
  const strip = await page.evaluate(() => [...document.querySelector(".chat-cards").children].map((c) => c.className.split(" ")[0]));
  assert.deepEqual(strip, ["chat-approval", "chat-consent"]);
  await page.locator("button.chat-ctx").click();
  await page.waitForFunction(() => document.querySelector(".chat-consent") === null);
  assert.equal(await page.locator("section.chat-approval").count(), 1, "the approval card stays");

  // One real press after the window: one call with the four ids and the card's own hash.
  const hash = await storedHash(page);
  assert.match(hash, /^[0-9a-f]{64}$/);
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await page.waitForFunction(() => document.querySelector("section.chat-approval") === null);
  const made = await decisionCalls(page);
  assert.deepEqual(made, [{ conversation: "chat-mock-1", call_id: "call_1", decision: "allow", hash }]);
  assert.deepEqual(Object.keys(made[0]), ["conversation", "call_id", "decision", "hash"]);
  assert.equal(await page.locator("div.chat-cards").evaluate((n) => n.childElementCount), 0, "the strip is empty");
  await finished(page);
  assert.equal(await page.evaluate(() => window.__MOCK__.ai.chat("chat-mock-1").ended.end), "done");
  assert.equal(await page.evaluate(() => window.__MOCK__.ai.approval.decisions.length), 1);
}

/** The sorted member names of an object. */
const names = (o) => Object.keys(o).sort();

async function contractCase(page, url) {
  await chatBesidePane1(page, url);
  const card = await askForFile(page);
  const hash = await storedHash(page);

  // A snapshot taken while the card waits holds the card of the event.
  const snapshot = await page.evaluate(() => window.__TAURI__.core.invoke("ai_turn_snapshot", { conversation: "chat-mock-1" }));
  assert.equal(snapshot.running, true);
  assert.equal(snapshot.pending.length, 1);
  const sent = await page.evaluate(() => structuredClone(window.__MOCK__.ai.sent));
  const proposed = sent.find((e) => e.kind === "tool_proposed");
  assert.deepEqual(snapshot.pending[0], proposed.pending, "the snapshot holds the card of the event");

  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await finished(page);
  const events = await page.evaluate(() => structuredClone(window.__MOCK__.ai.sent));

  // Every kind of event, card and part has the member names of the recorded run.
  const first = (kind) => RUN.events.find((e) => e.kind === kind);
  for (const kind of ["turn_started", "usage", "tool_proposed", "tool_result", "message", "delta", "turn_ended"]) {
    for (const event of events.filter((e) => e.kind === kind)) assert.deepEqual(names(event), names(first(kind)), `an event ${kind}`);
  }
  const recordedCard = RUN.states.flatMap((s) => s.pending)[0];
  for (const event of events.filter((e) => e.kind === "tool_proposed")) {
    assert.deepEqual(names(event.pending), names(recordedCard), "a card");
    for (const row of event.pending.payload) assert.deepEqual(names(row), names(recordedCard.payload[0]), "a payload row");
  }
  const parts = (list, type) => list.flatMap((e) => (e.kind === "message" ? e.message.parts : [])).filter((p) => p.type === type);
  for (const type of ["tool_call", "tool_result"]) {
    const mine = parts(events, type);
    assert.ok(mine.length >= 1, type);
    for (const part of mine) assert.deepEqual(names(part), names(parts(RUN.events, type)[0]), type);
  }
  for (const m of events.filter((e) => e.kind === "message")) assert.deepEqual(names(m.message), names(first("message").message));

  // The page reads the card the mock sent without changing its hash or its field order.
  const read = await page.evaluate(async (pending) => {
    const m = await import(new URL("approvalstate.js", location.href).href);
    return m.readApproval(pending);
  }, proposed.pending);
  assert.equal(read.why, "ok");
  assert.equal(read.hash, hash);
  assert.deepEqual(read.approval.rows.map((r) => r.name), proposed.pending.payload.map((p) => p.name));

  // The decision names the arguments of the command table and one of its two verdicts.
  const made = await decisionCalls(page);
  assert.equal(made.length, 1);
  assert.deepEqual(Object.keys(made[0]), GOLDEN.commands.ai_decide);
  assert.ok(GOLDEN.ops.ai_decision.includes(made[0].decision));
}

async function idsCase(page, url) {
  await chatBesidePane1(page, url);
  // The canaries live only in the mock's source table and in the files of the tests.
  const card = await askForFile(page);
  assert.equal(await page.evaluate((c) => JSON.stringify(window.__MOCK__.ai.chat("chat-mock-1")).includes(c), TOOL_PATH), true);
  const hash = await storedHash(page);

  const surface = () =>
    page.evaluate(() => {
      const attrs = [];
      for (const n of document.querySelectorAll("*")) for (const a of n.attributes) attrs.push(a.value);
      const store = [];
      for (const area of [localStorage, sessionStorage]) for (let i = 0; i < area.length; i++) store.push(area.key(i), area.getItem(area.key(i)));
      return JSON.stringify({ attrs, store, cookie: document.cookie });
    });
  const seenWhileShown = await surface();
  for (const canary of [TOOL_PATH, TOOL_TEXT, "zq-approval-"]) assert.ok(!seenWhileShown.includes(canary), `${canary} is in an attribute or a store`);

  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await finished(page);

  const all = await page.evaluate(() => structuredClone(window.__MOCK__.calls));
  const text = JSON.stringify(all);
  for (const canary of [TOOL_PATH, TOOL_TEXT, "zq-approval-"]) assert.ok(!text.includes(canary), `${canary} is in a call`);
  const after = await surface();
  for (const canary of [TOOL_PATH, TOOL_TEXT, "zq-approval-"]) assert.ok(!after.includes(canary), `${canary} is in an attribute or a store`);

  // Every decision names the four ids; the hash occurs in the calls only as that value.
  const decisions = await decisionCalls(page);
  assert.equal(decisions.length, 1);
  assert.equal(Object.keys(decisions[0]).length, 4);
  assert.equal(text.split(hash).length - 1, decisions.length, "the hash occurs once per decision");
  // Among the commands of the chat, only the typed text and the rendering of the finished answer carry text of any kind.
  for (const [cmd, args] of all) {
    if (args === null || typeof args !== "object" || !(cmd.startsWith("ai_") || cmd === "markdown_render")) continue;
    const keys = Object.keys(args).filter((k) => ["text", "content", "path", "command"].includes(k));
    if (keys.length > 0) assert.ok(cmd === "ai_send" || cmd === "markdown_render", `${cmd} carries ${keys}`);
  }
  assert.equal((await calls(page, "ai_send")).length, 1);
}

/** The text of the status line of the chat. */
const status = (page) => page.locator(".chat-status").textContent();

/** Every event of the core that the mock produced, oldest first. */
const events = (page) => page.evaluate(() => structuredClone(window.__MOCK__.ai.sent));

async function denyCase(page, url) {
  await chatBesidePane1(page, url);
  const card = await askForFile(page);
  await tickClock(page, 1000);
  await card.locator("button.approval-deny").click();
  await page.waitForFunction(() => document.querySelector("section.chat-approval") === null);
  const made = await decisionCalls(page);
  assert.equal(made.length, 1);
  assert.equal(made[0].decision, "deny");
  assert.equal(made.filter((d) => d.decision === "allow").length, 0, "nothing was allowed");
  const result = (await events(page)).find((e) => e.kind === "tool_result");
  assert.equal(result.is_error, true);
  assert.equal(result.content, "The user denied this action.");
  await finished(page);
  assert.equal(await page.evaluate(() => window.__MOCK__.ai.chat("chat-mock-1").ended.end), "done");
}

async function guardCase(page, url) {
  // (a) A double click decides once.
  await chatBesidePane1(page, url);
  let card = await askForFile(page);
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").dblclick();
  await finished(page);
  assert.equal((await decisionCalls(page)).length, 1, "a double click makes one call");

  // (b) An allow right after the card appeared does nothing; after the window it goes. A deny is never held back.
  await chatBesidePane1(page, url);
  card = await askForFile(page);
  await card.locator("button.approval-allow").click();
  assert.deepEqual(await decisionCalls(page), [], "an allow inside the window makes no call");
  assert.equal(await card.getAttribute("data-mode"), "ask");
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await finished(page);
  assert.equal((await decisionCalls(page)).length, 1);
  await scriptTool(page, { id: "call_2", tool: "read_file", path: TOOL_PATH, answer: "Done." });
  await sendText(page, "once more");
  const second = await openCard(page, "call_2");
  await second.locator("button.approval-deny").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_decide").length === 2);
  assert.equal((await decisionCalls(page))[1].decision, "deny", "a deny goes at once");

  // (c) A click or a key that a script made decides nothing; a real click does.
  await chatBesidePane1(page, url);
  card = await askForFile(page);
  await tickClock(page, 1000);
  await page.evaluate(() => {
    const allow = document.querySelector(".approval-allow");
    allow.click();
    allow.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    document.querySelector(".approval-deny").click();
    document.querySelector(".approval-payload").dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  });
  assert.deepEqual(await decisionCalls(page), [], "a script decides nothing");
  assert.equal(await card.getAttribute("data-mode"), "ask");
  await card.locator("button.approval-allow").click();
  await finished(page);
  assert.equal((await decisionCalls(page)).length, 1, "the real click goes");

  // (d) A request that changed under the card is refused, the card is read again, and nothing is retried.
  await chatBesidePane1(page, url);
  card = await askForFile(page);
  const oldHash = await storedHash(page);
  await page.evaluate(() => window.__MOCK__.ai.approval.changeHash("call_1"));
  const newHash = await storedHash(page);
  assert.notEqual(oldHash, newHash);
  const before = (await calls(page, "ai_turn_snapshot")).length;
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await page.waitForFunction((n) => window.__MOCK__.callsOf("ai_turn_snapshot").length > n, before);
  await page.waitForFunction((h) => document.querySelector(".approval-hash")?.textContent === `hash ${h.slice(0, 12)}`, newHash);
  assert.equal((await calls(page, "ai_turn_snapshot")).length, before + 1, "one snapshot is asked for");
  assert.equal((await decisionCalls(page)).length, 1, "the page made no second call by itself");
  assert.equal((await decisionCalls(page))[0].hash, oldHash);
  card = page.locator('section.chat-approval[data-call="call_1"]');
  assert.equal(await card.getAttribute("data-mode"), "ask");
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await finished(page);
  const sentHashes = (await decisionCalls(page)).map((d) => d.hash);
  assert.deepEqual(sentHashes, [oldHash, newHash], "the next press repeats the new hash");

  // (e) A call that is no longer waiting turns the card into a quiet line and asks for one snapshot.
  await chatBesidePane1(page, url);
  card = await askForFile(page);
  const snapshots = (await calls(page, "ai_turn_snapshot")).length;
  await page.evaluate(() => window.__MOCK__.ai.approval.refuse("unknown_call"));
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await page.waitForSelector('section.chat-approval[data-mode="ended"]');
  assert.equal(await page.locator("section.chat-approval .approval-verdict").textContent(), "That call is no longer waiting.");
  assert.equal(await status(page), "That call is no longer waiting.");
  assert.equal(await page.locator("section.chat-approval button").count(), 0, "an ended card has no buttons");
  await page.waitForFunction((n) => window.__MOCK__.callsOf("ai_turn_snapshot").length > n, snapshots);
  assert.equal((await calls(page, "ai_turn_snapshot")).length, snapshots + 1);
  assert.equal((await decisionCalls(page)).length, 1);

  // (f) Any other refusal keeps the card with the core's text and asks for nothing.
  await chatBesidePane1(page, url);
  card = await askForFile(page);
  const quiet = (await calls(page, "ai_turn_snapshot")).length;
  await page.evaluate(() => window.__MOCK__.ai.approval.refuse("denied"));
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await page.waitForSelector("p.approval-fault");
  assert.equal(await page.locator("p.approval-fault").textContent(), "This location is protected and tools cannot use it.");
  assert.equal(await card.getAttribute("data-mode"), "ask");
  assert.equal(await card.locator("button.approval-allow").isEnabled(), true);
  assert.equal(await card.locator("button.approval-deny").isEnabled(), true);
  assert.equal((await calls(page, "ai_turn_snapshot")).length, quiet, "no snapshot is asked for");
}

async function gapCase(page, url) {
  await chatBesidePane1(page, url);
  // The page misses the first card; the second one shows the hole and the page asks for one snapshot.
  await scriptTool(page, {
    calls: [{ id: "call_1", tool: "read_file", path: TOOL_PATH }, { id: "call_2", tool: "read_file", path: TOOL_PATH }],
    answer: "Done.",
  });
  await page.evaluate(() => window.__MOCK__.ai.hold(3));
  await sendText(page, "read my notes");
  await page.waitForFunction(() => window.__MOCK__.ai.sent.length >= 3);
  await page.evaluate(() => {
    window.__MOCK__.ai.drop(1);
    window.__MOCK__.ai.release();
  });
  const card = await openCard(page, "call_1");
  await page.waitForSelector('section.chat-approval[data-call="call_2"]');
  const hash = await storedHash(page);
  assert.equal(await card.locator(".approval-hash").textContent(), `hash ${hash.slice(0, 12)}`);
  const names = await card.locator("li.approval-row").evaluateAll((rows) => rows.map((r) => r.dataset.name));
  assert.deepEqual(names, ["tool", "pane", "target", "remote", "path", "offset", "limit", "resolved_path"]);
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_decide").length === 1);
  assert.equal((await decisionCalls(page))[0].hash, hash, "the click sends the hash the snapshot carried");
}

async function reloadCase(page, url) {
  await chatBesidePane1(page, url);
  await askForFile(page);
  const hash = await storedHash(page);
  const rows = await page.locator("section.chat-approval li.approval-row .approval-value").allTextContents();

  await reloadKeeping(page);
  const card = await openCard(page, "call_1");
  assert.equal(await card.locator(".approval-hash").textContent(), `hash ${hash.slice(0, 12)}`);
  assert.deepEqual(await card.locator("li.approval-row .approval-value").allTextContents(), rows);
  assert.equal(await status(page), "waiting for your decision");
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await page.waitForFunction(() => document.querySelector("section.chat-approval") === null);
  assert.deepEqual(await decisionCalls(page), [{ conversation: "chat-mock-1", call_id: "call_1", decision: "allow", hash }]);
}

async function cancelCase(page, url) {
  for (const how of ["button", "escape"]) {
    await chatBesidePane1(page, url);
    await askForFile(page);
    if (how === "button") await page.locator('.chat-send[data-mode="stop"]').click();
    else await page.locator(".chat-input").press("Escape");
    await page.waitForFunction(() => window.__MOCK__.ai.chat("chat-mock-1").ended?.end === "cancelled");
    await page.waitForFunction(() => document.querySelector("section.chat-approval") === null);
    assert.equal((await calls(page, "ai_cancel")).length, 1, how);
    assert.deepEqual(await decisionCalls(page), [], `${how}: no decision`);
    const result = (await events(page)).find((e) => e.kind === "tool_result");
    assert.equal(result.content, "This call did not run: the turn was stopped.");
    assert.equal(result.is_error, true);
  }
}

async function keysCase(page, url, engine) {
  // WebKit on macOS visits buttons with Option+Tab only, as Safari does by default.
  const back = () => page.keyboard.press(engine === "webkit" && process.platform === "darwin" ? "Shift+Alt+Tab" : "Shift+Tab");
  const active = () => page.evaluate(() => document.activeElement?.className);
  const terminal = () => page.evaluate(() => window.__MOCK__.calls.filter((c) => c[0] === "key" || c[0] === "text").length);
  await chatBesidePane1(page, url);

  // A card that appears while the person types does not take the keys.
  await scriptTool(page, { id: "call_1", tool: "read_file", path: TOOL_PATH, answer: "Done." });
  await page.locator(".chat-input").fill("read my notes");
  await page.locator(".chat-input").press("Enter");
  await page.keyboard.type("abc");
  const card = await openCard(page, "call_1");
  assert.equal(await active(), "chat-input");
  assert.equal(await page.locator(".chat-input").inputValue(), "abc");
  assert.deepEqual(await decisionCalls(page), []);
  await page.keyboard.press("Enter");
  assert.deepEqual(await decisionCalls(page), [], "Enter in the message box decides nothing");
  await page.locator(".chat-input").fill("");

  // The order of the controls, and every control keeps its own keys.
  assert.equal(await card.getAttribute("role"), "group");
  const title = await card.evaluate((n) => document.getElementById(n.getAttribute("aria-labelledby")).textContent.replace("!", "").trim());
  assert.equal(title, "Approval needed");
  const loose = await page.evaluate(() => [...document.querySelectorAll(".chat-approval button, .chat-approval .approval-payload")].filter((n) => !n.hasAttribute("data-field")).length);
  assert.equal(loose, 0, "every control carries data-field");
  await back();
  assert.equal(await active(), "approval-deny");
  await back();
  assert.equal(await active(), "approval-allow");
  await back();
  assert.equal(await active(), "approval-payload");
  const typed = await terminal();
  await page.keyboard.type("x");
  assert.equal(await terminal(), typed, "a character typed on the card reaches no terminal");

  // Space on `Deny` decides.
  await page.locator(".approval-deny").focus();
  await page.keyboard.press("Space");
  await finished(page);
  assert.deepEqual((await decisionCalls(page)).map((d) => d.decision), ["deny"]);

  // Enter on `Allow once` decides once the window has passed.
  await scriptTool(page, { id: "call_2", tool: "read_file", path: TOOL_PATH, answer: "Done." });
  await sendText(page, "again");
  await openCard(page, "call_2");
  await tickClock(page, 1000);
  await page.locator('section[data-call="call_2"] .approval-allow').focus();
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_decide").length === 2);
  assert.equal((await decisionCalls(page))[1].decision, "allow");
  await page.waitForFunction(() => document.querySelector("section.chat-approval") === null);

  // `Escape` inside the card denies that call, gives the keyboard to the message box and does not stop the reply.
  await scriptTool(page, { id: "call_3", tool: "read_file", path: TOOL_PATH, answer: "Done." });
  await sendText(page, "third");
  await openCard(page, "call_3");
  await page.locator(".approval-payload").focus();
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_decide").length === 3);
  assert.equal((await decisionCalls(page))[2].decision, "deny");
  assert.equal(await active(), "chat-input", "the keyboard went back to the message box");
  assert.equal((await calls(page, "ai_cancel")).length, 0, "the reply was not stopped");
  await page.waitForFunction(() => document.querySelector("section.chat-approval") === null);

  // With the consent card open too: Escape in each closes or denies only its own.
  await scriptTool(page, { id: "call_4", tool: "read_file", path: TOOL_PATH, answer: "Done." });
  await sendText(page, "fourth");
  await openCard(page, "call_4");
  await page.locator("button.chat-ctx").click();
  await page.waitForSelector(".chat-consent");
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => document.querySelector(".chat-consent") === null);
  assert.equal(await page.locator("section.chat-approval").count(), 1, "Escape in the consent card leaves the approval card");
  assert.equal((await decisionCalls(page)).length, 3);
  await page.locator("button.chat-ctx").click();
  await page.waitForSelector(".chat-consent");
  await page.locator(".approval-payload").focus();
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_decide").length === 4);
  assert.equal((await decisionCalls(page))[3].call_id, "call_4");
  assert.equal(await page.locator(".chat-consent").count(), 1, "Escape in the approval card leaves the consent card");
  await finished(page);
}

async function badgeCase(page, url) {
  await chatBesidePane1(page, url);
  assert.equal(await page.locator(".ai-btn").getAttribute("data-waiting"), null);
  await scriptTool(page, { id: "call_1", tool: "read_file", path: TOOL_PATH, answer: "Done." });
  await sendText(page, "read my notes");
  await openCard(page, "call_1");
  const button = page.locator(".ai-btn");
  assert.equal(await button.getAttribute("data-waiting"), "1");
  assert.ok((await button.textContent()).includes("· 1"));
  assert.equal(await button.getAttribute("aria-label"), "AI chat, 1 call waits for your decision");
  assert.equal(await status(page), "waiting for your decision");
  await tickClock(page, 1000);
  await page.locator(".approval-allow").click();
  await finished(page);
  assert.equal((await button.textContent()).trim(), "✦ AI");
  assert.equal(await button.getAttribute("data-waiting"), null);
  assert.equal(await button.getAttribute("aria-label"), null);

  // Two calls in one turn.
  await scriptTool(page, {
    calls: [{ id: "call_2", tool: "read_file", path: TOOL_PATH }, { id: "call_3", tool: "read_file", path: TOOL_PATH }],
    answer: "Done.",
  });
  await sendText(page, "two files");
  await page.waitForSelector('section[data-call="call_3"]');
  assert.equal(await button.getAttribute("data-waiting"), "2");
  assert.ok((await button.textContent()).includes("· 2"));
  assert.equal(await button.getAttribute("aria-label"), "AI chat, 2 calls wait for your decision");
  await page.locator('section[data-call="call_2"] .approval-deny').click();
  await page.waitForFunction(() => document.querySelector(".ai-btn")?.dataset.waiting === "1");
  await page.locator('section[data-call="call_3"] .approval-deny').click();
  await page.waitForFunction(() => document.querySelector(".ai-btn")?.dataset.waiting === undefined);
  assert.equal((await button.textContent()).trim(), "✦ AI");
  assert.equal(await button.getAttribute("aria-label"), null);
}

/** A result that tries to be markup, a link and a handler: it must be shown as exactly this text. */
const HOSTILE = "zq-result-<img src=x onerror=alert(1)> [x](javascript:alert(1)) zqres-1d5c";

/** The row of a call in the transcript. */
const rowOf = (page, id) => page.locator(`.chat-msg[data-role="assistant"] .chat-tool[data-call="${id}"]`);

/** Press the toggle of a row and wait for its result to be drawn. */
async function opened(row) {
  await row.locator("button.tool-toggle").click();
  await row.locator("pre.tool-result").waitFor();
}

/** Ask for one file, allow it once the window has passed, and wait for the end of the turn. */
async function allowed(page, spec, text = "read it") {
  await scriptTool(page, { tool: "read_file", path: TOOL_PATH, answer: "Done.", ...spec });
  await sendText(page, text);
  const card = await openCard(page, spec.id);
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await page.waitForFunction(() => !window.__MOCK__.ai.chat("chat-mock-1").running);
  await page.waitForSelector(`.chat-tool[data-call="${spec.id}"]:not([data-state="waiting"])`);
}

async function rowsCase(page, url) {
  await chatBesidePane1(page, url);
  await allowed(page, { id: "call_1", result: TOOL_TEXT });
  let row = rowOf(page, "call_1");
  assert.equal(await row.getAttribute("data-state"), "ran");
  assert.equal(await row.locator(".tool-words").textContent(), "Read a file");
  assert.equal(await row.locator(".tool-summary").textContent(), JSON.stringify({ path: TOOL_PATH }));
  assert.equal(await row.locator(".tool-state").textContent(), "ran");
  const toggle = row.locator("button.tool-toggle");
  assert.equal(await toggle.getAttribute("aria-expanded"), "false");
  assert.match(await toggle.textContent(), /^Show result \(\d+(\.\d)? (B|KiB)\)$/);
  assert.equal(await page.locator("pre.tool-result").count(), 0, "the result is drawn only when it is opened");
  await toggle.click();
  await row.locator('button.tool-toggle[aria-expanded="true"]').waitFor();
  assert.equal(await toggle.getAttribute("aria-expanded"), "true");
  assert.equal(await toggle.textContent(), "Hide result");
  assert.equal(await row.locator(".tool-untrusted").textContent(), "Result · untrusted text");
  assert.ok((await row.locator("pre.tool-result").textContent()).includes(TOOL_TEXT));
  // Messages of the role tool draw nothing of their own.
  const messages = await page.evaluate(() => window.__MOCK__.ai.chat("chat-mock-1").messages.filter((m) => m.role !== "tool").length);
  assert.equal(await page.locator(".chat-msg").count(), messages);
  // The open state survives the next draw: a fresh snapshot replaces the text the store does not keep, and the box stays open.
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await page.waitForFunction(() => document.querySelector(".tool-result")?.textContent === "[tool output not stored]");
  assert.equal(await row.locator("button.tool-toggle").getAttribute("aria-expanded"), "true");

  // A hostile result is text and nothing else; a long one is cut at 20000 characters.
  await chatBesidePane1(page, url);
  await allowed(page, { id: "call_1", result: HOSTILE });
  await opened(rowOf(page, "call_1"));
  assert.equal(await rowOf(page, "call_1").locator("pre.tool-result").textContent(), HOSTILE);
  const markup = await page.evaluate(() => ({
    elements: document.querySelectorAll(".chat-tool img, .chat-tool a, .chat-tool script, .chat-tool iframe").length,
    images: document.querySelectorAll("img").length,
    handlers: [...document.querySelectorAll(".chat-tool *")].filter((n) => n.getAttributeNames().some((a) => a.startsWith("on"))).length,
  }));
  assert.deepEqual(markup, { elements: 0, images: 0, handlers: 0 });
  await allowed(page, { id: "call_2", result: "y".repeat(30000) }, "and a long one");
  await opened(rowOf(page, "call_2"));
  const long = await rowOf(page, "call_2").locator("pre.tool-result").textContent();
  assert.ok(long.startsWith("y".repeat(20000)) && !long.startsWith("y".repeat(20001)));
  assert.ok(long.endsWith("[cut for display]"));

  // A denied call and a stopped call are errors and say what happened.
  await chatBesidePane1(page, url);
  await scriptTool(page, { id: "call_1", tool: "read_file", path: TOOL_PATH, answer: "Done." });
  await sendText(page, "first");
  await (await openCard(page, "call_1")).locator("button.approval-deny").click();
  await page.waitForSelector('.chat-tool[data-call="call_1"][data-state="error"]');
  row = rowOf(page, "call_1");
  assert.equal(await row.locator(".tool-state").textContent(), "error");
  await opened(row);
  assert.equal(await row.locator("pre.tool-result").textContent(), "The user denied this action.");
  await scriptTool(page, { id: "call_2", tool: "read_file", path: TOOL_PATH, answer: "Done." });
  await sendText(page, "second");
  await openCard(page, "call_2");
  await page.locator('.chat-send[data-mode="stop"]').click();
  await page.waitForFunction(() => window.__MOCK__.ai.chat("chat-mock-1").ended?.end === "cancelled");
  row = rowOf(page, "call_2");
  await page.waitForSelector('.chat-tool[data-call="call_2"][data-state="error"]');
  await opened(row);
  assert.equal(await row.locator("pre.tool-result").textContent(), "This call did not run: the turn was stopped.");
}

async function bannerCase(page, url) {
  await chatBesidePane1(page, url);
  const finish = async () => {
    await page.waitForFunction(() => !window.__MOCK__.ai.chat("chat-mock-1").running);
  };
  const ask = async (spec, text) => {
    await scriptTool(page, { tool: "read_file", path: TOOL_PATH, answer: "Done.", ...spec });
    await sendText(page, text);
    return openCard(page, spec.id);
  };

  // No labels and no flag: no banner.
  let card = await ask({ id: "call_1" }, "one");
  assert.equal(await card.getAttribute("data-tainted"), "false");
  assert.equal(await card.locator(".approval-taint").count(), 0);
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await finish();

  // After a read in this chat the core marks the next card with the tool whose output is in the history.
  card = await ask({ id: "call_2" }, "two");
  assert.equal(await card.getAttribute("data-tainted"), "true");
  const history = card.locator('.approval-taint[data-tone="warn"]');
  assert.ok((await history.textContent()).includes("output of read_file"));
  await card.locator("button.approval-deny").click();
  await finish();

  // The flag and the labels of the core are shown as they are.
  card = await ask({ id: "call_3", tainted: true, taint: ["output of list_dir", "attached context"] }, "three");
  assert.equal(await card.getAttribute("data-tainted"), "true");
  const text = await card.locator(".approval-taint").textContent();
  for (const words of ["Untrusted text in this turn", "output of list_dir", "attached context", "can try to steer the assistant"]) {
    assert.ok(text.includes(words), `${words} in ${text}`);
  }
  await card.locator("button.approval-deny").click();
  await finish();

  // A list of labels without the flag still shows the banner.
  card = await ask({ id: "call_4", tainted: false, taint: ["output of list_dir"] }, "four");
  assert.equal(await card.getAttribute("data-tainted"), "true");
  assert.ok((await card.locator(".approval-taint").textContent()).includes("output of list_dir"));
  await card.locator("button.approval-deny").click();
  await finish();
}

async function destinationCase(page, url) {
  // The place comes from the route of the turn, in the core's word.
  await chatBesidePane1(page, url);
  let card = await askForFile(page);
  assert.equal(await card.locator("p.approval-dest").textContent(), `The result is sent to ${PROFILE.model} at ${PROFILE.host} (this computer).`);
  assert.equal(await card.locator(".approval-place").getAttribute("data-leaves"), "false");
  assert.equal(await card.getAttribute("data-remote"), "false");
  assert.equal(await card.locator(".approval-remote").count(), 0);

  // A cloud route leaves the computer, and says so in words.
  await chatBesidePane1(page, url, { profiles: [PROFILE, OTHER_PROFILE] });
  await page.locator(".chat-chip").click();
  await page.locator('.chat-choice[data-id="other-1"]').click();
  card = await askForFile(page);
  assert.equal(await card.locator("p.approval-dest").textContent(), `The result is sent to ${OTHER_PROFILE.model} at ${OTHER_PROFILE.host} (cloud).`);
  assert.equal(await card.locator(".approval-place").getAttribute("data-leaves"), "true");

  // After a reload the route is not known: the card does not guess a place.
  await reloadKeeping(page);
  card = await openCard(page, "call_1");
  assert.equal(await card.locator("p.approval-dest").textContent(), "The result is sent to the model of this chat.");
  assert.equal(await card.locator(".approval-place").count(), 0);

  // A remote target says so.
  await chatBesidePane1(page, url);
  card = await askForFile(page, { remote: true, target: "talos-1" });
  assert.equal(await card.getAttribute("data-remote"), "true");
  assert.equal(await card.locator(".approval-remote").textContent(), "remote host");
  assert.equal(await card.locator(".approval-where").textContent(), "pane 1 · talos-1");
}
