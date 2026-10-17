// Changing a file (docs/design.md section 6.16): a write tool's card shows the changes the core worked out from the file's current
// text, one real press decides it with the four ids and the card's own hash, and the core's outcome removes it. The page works out
// no diff and sends ids only; a mock that plays the core is held to the golden of the core (tests/fixtures/ai_write_card.json).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  FILE_NEW, FILE_OLD, TOOL_FILE, VIEW_P1, boot, calls, decisionCalls, fileText, openAi, openCard, scriptWrite, seedFile, sendText, tickClock,
} from "./ai-lib.mjs";

const GOLDEN = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));
const WRITE = JSON.parse(readFileSync(new URL("../fixtures/ai_write_card.json", import.meta.url), "utf8"));

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

/** Seed the file, script one `write_file` call over it, send a message and wait for its card. */
async function askForWrite(page, extra = {}) {
  await seedFile(page, TOOL_FILE, FILE_OLD);
  await scriptWrite(page, { id: "call_1", tool: "write_file", path: TOOL_FILE, content: FILE_NEW, answer: "Done.", ...extra });
  await sendText(page, "update my todo list");
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

/** The sorted member names of an object. */
const names = (o) => Object.keys(o).sort();

async function traceCase(page, url) {
  await chatBesidePane1(page, url);
  const card = await askForWrite(page);

  assert.equal(await card.getAttribute("data-mode"), "ask");
  assert.equal(await card.getAttribute("data-tool"), "write_file");
  assert.equal(await card.getAttribute("data-effect"), "write");
  const text = await card.textContent();
  for (const words of ["Approval needed", "Write a file", "pane 1 · This computer", "Changes one file. Check the changes below."]) {
    assert.ok(text.includes(words), `${words} in ${text}`);
  }

  // The diff comes before the payload box, as the core worked it out: the unchanged line, the removed one, the added one, the unchanged one.
  const diff = card.locator('div.approval-diff[data-kind="replace"]');
  assert.equal(await diff.count(), 1);
  const order = await card.evaluate((c) => {
    const d = c.querySelector("div.approval-diff");
    const p = c.querySelector("div.approval-payload");
    return d !== null && p !== null && Boolean(d.compareDocumentPosition(p) & Node.DOCUMENT_POSITION_FOLLOWING);
  });
  assert.equal(order, true, "the diff box comes before the payload box");
  const rows = diff.locator("li.diff-row");
  assert.deepEqual(await rows.evaluateAll((r) => r.map((n) => n.dataset.op)), ["ctx", "del", "add", "ctx"]);
  assert.deepEqual(await diff.locator(".diff-sign").allTextContents(), ["", "-", "+", ""]);
  assert.deepEqual(await diff.locator(".diff-text").allTextContents(), ["alpha", "beta zq-old-text-4d2e91", "beta zq-new-text-b83f17", "gamma"]);
  assert.deepEqual(await diff.locator(".diff-old").allTextContents(), ["1", "2", "", "3"]);
  assert.deepEqual(await diff.locator(".diff-new").allTextContents(), ["1", "", "2", "3"]);
  assert.equal(await diff.locator(".diff-kind").textContent(), "Replaces the whole file");
  assert.equal(await diff.locator(".diff-count").textContent(), "1 line added, 1 removed");

  // The payload keeps every field the core sent, with the digest of the old text as the last row.
  const fields = await card.locator("li.approval-row").evaluateAll((r) => r.map((n) => n.dataset.name));
  assert.deepEqual(fields, ["tool", "pane", "target", "remote", "path", "content", "resolved_path", "stamp", "base"]);
  assert.match(await card.locator('li.approval-row[data-name="base"] .approval-value').textContent(), /^sha256:[0-9a-f]{64}$/);

  // The card never takes the keyboard, and nothing is decided yet.
  assert.equal(await page.evaluate(() => document.activeElement?.className), "chat-input");
  assert.deepEqual(await decisionCalls(page), []);

  // One real press after the window: one call with the four ids and the card's own hash, and then the file is replaced.
  const hash = await storedHash(page);
  assert.match(hash, /^[0-9a-f]{64}$/);
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await page.waitForFunction(() => document.querySelector("section.chat-approval") === null);
  const made = await decisionCalls(page);
  assert.deepEqual(made, [{ conversation: "chat-mock-1", call_id: "call_1", decision: "allow", hash }]);
  assert.deepEqual(Object.keys(made[0]), ["conversation", "call_id", "decision", "hash"]);
  await finished(page);
  assert.equal(await fileText(page, TOOL_FILE), FILE_NEW, "the file holds the new text");
  const row = page.locator('div.chat-tool[data-call="call_1"]');
  assert.equal(await row.getAttribute("data-state"), "ran");
  assert.ok((await row.textContent()).includes("Write a file"));
  assert.equal(await page.evaluate(() => window.__MOCK__.ai.chat("chat-mock-1").ended.end), "done");
}

async function contractCase(page, url) {
  await chatBesidePane1(page, url);
  const card = await askForWrite(page);
  const hash = await storedHash(page);
  const sent = await page.evaluate(() => structuredClone(window.__MOCK__.ai.sent));
  const proposed = sent.find((e) => e.kind === "tool_proposed").pending;

  // Every member name of the card, its diff, each row by operation and each payload row equals the core's.
  const golden = WRITE.cards.replace;
  assert.deepEqual(names(proposed), names(golden), "a card");
  assert.deepEqual(Object.keys(proposed), Object.keys(golden), "a card keeps the order of the members");
  assert.deepEqual(names(proposed.diff), names(golden.diff), "the diff");
  for (const row of proposed.payload) assert.deepEqual(names(row), names(golden.payload[0]), "a payload row");
  assert.deepEqual(proposed.payload.map((p) => p.name), golden.payload.map((p) => p.name), "the payload names");
  const shapes = new Map();
  for (const one of Object.values(WRITE.cards)) for (const row of one.diff.rows) shapes.set(row.op, Object.keys(row));
  for (const row of proposed.diff.rows) assert.deepEqual(Object.keys(row), shapes.get(row.op), `a row ${row.op}`);

  // The page reads the card the mock sent without changing its hash or its field order.
  const read = await page.evaluate(async (pending) => {
    const m = await import(new URL("approvalstate.js", location.href).href);
    return m.readApproval(pending);
  }, proposed);
  assert.equal(read.why, "ok");
  assert.equal(read.hash, hash);
  assert.deepEqual(read.approval.rows.map((r) => r.name), proposed.payload.map((p) => p.name));

  // The decision names the arguments of the command table, and the mock's sentences are the core's.
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await finished(page);
  const made = await decisionCalls(page);
  assert.equal(made.length, 1);
  assert.deepEqual(Object.keys(made[0]), GOLDEN.commands.ai_decide);
  const texts = await page.evaluate(() => window.__MOCK__.ai.write.texts);
  assert.equal(texts.payload_changed, WRITE.texts.payload_changed);
  for (const [code, phrase] of Object.entries(texts.faults)) assert.equal(phrase, WRITE.texts.faults[code], code);
}

async function idsCase(page, url) {
  await chatBesidePane1(page, url);
  const card = await askForWrite(page);
  const hash = await storedHash(page);

  // The texts of the file are in text nodes of the card; no attribute and no storage holds them.
  const surface = () =>
    page.evaluate(() => {
      const attrs = [];
      for (const n of document.querySelectorAll("*")) for (const a of n.attributes) attrs.push(a.value);
      const store = [];
      for (const area of [localStorage, sessionStorage]) for (let i = 0; i < area.length; i++) store.push(area.key(i), area.getItem(area.key(i)));
      return JSON.stringify({ attrs, store, cookie: document.cookie });
    });
  const canaries = ["zq-old-text-4d2e91", "zq-new-text-b83f17"];
  const shown = await surface();
  for (const canary of canaries) assert.ok(!shown.includes(canary), `${canary} is in an attribute or a store`);
  const inText = await card.textContent();
  for (const canary of canaries) assert.ok(inText.includes(canary), `${canary} is shown as text`);

  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await finished(page);

  const all = await page.evaluate(() => structuredClone(window.__MOCK__.calls));
  const sentText = JSON.stringify(all.filter(([cmd]) => cmd !== "ai_send"));
  for (const canary of [...canaries, FILE_OLD, FILE_NEW]) assert.ok(!sentText.includes(canary), `${canary} is in a call`);
  const after = await surface();
  for (const canary of canaries) assert.ok(!after.includes(canary), `${canary} is in an attribute or a store`);

  // Every decision names the four ids; the hash occurs in the calls only as that value.
  const decisions = await decisionCalls(page);
  assert.equal(decisions.length, 1);
  assert.equal(Object.keys(decisions[0]).length, 4);
  assert.equal(JSON.stringify(all).split(hash).length - 1, decisions.length, "the hash occurs once per decision");
  // Among the commands of the chat only the typed text and the rendering of the finished answer carry text of any kind.
  for (const [cmd, args] of all) {
    if (args === null || typeof args !== "object" || !(cmd.startsWith("ai_") || cmd === "markdown_render")) continue;
    const keys = Object.keys(args).filter((k) => ["text", "content", "path", "command"].includes(k));
    if (keys.length > 0) assert.ok(cmd === "ai_send" || cmd === "markdown_render", `${cmd} carries ${keys}`);
  }
  assert.equal((await calls(page, "ai_send")).length, 1);
}
