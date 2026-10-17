// The approval card of a tool call (docs/design.md section 6.15): the card shows the exact request the core sent, one real
// press decides it with the four ids and the card's own hash, and the core's result removes it. The page decides nothing
// itself and sends ids only, and a mock that plays the core is held to the recorded tool run and to the command table.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { TOOL_PATH, TOOL_TEXT, VIEW_P1, boot, calls, decisionCalls, openAi, openCard, scriptTool, sendText, tickClock } from "./ai-lib.mjs";

const GOLDEN = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));
const RUN = JSON.parse(readFileSync(new URL("../fixtures/ai_tool_stream.json", import.meta.url), "utf8"));

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await traceCase(page, url);
  await contractCase(page, url);
  await idsCase(page, url);
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
