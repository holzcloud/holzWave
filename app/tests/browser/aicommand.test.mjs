// Running a command (docs/design.md section 6.17): a `run_command` card shows the exact command the core sent, one real press decides
// it with the four ids and the card's own hash, and the core's outcome removes it. The page runs nothing and sends ids only; a mock
// that plays the core is held to the golden of the core (tests/fixtures/ai_command_card.json).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  COMMAND_GOLDEN, COMMAND_TEXT, VIEW_P1, boot, calls, commandRuns, decisionCalls, openAi, openCard, scriptCommand, sendText, tickClock,
} from "./ai-lib.mjs";

const PROTOCOL = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(5000);
  // A failing case names itself.
  const cases = { traceCase, contractCase, idsCase };
  for (const [name, run] of Object.entries(cases)) {
    try {
      await run(page, url, engine);
    } catch (e) {
      e.message = `${name}: ${e.message}`;
      throw e;
    }
  }
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A page with the local profile, two terminals, the settings core, a frozen clock and a chat beside pane 1 (the chat is pane 3). */
async function chatBesidePane1(page, url, extra = {}) {
  await boot(page, url, { view: VIEW_P1, settings: true, clock: true, ...extra });
  await openAi(page);
  await page.waitForFunction(() => document.activeElement?.matches("textarea.chat-input"));
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length >= 1);
}

/** Script one command call, send a message and wait for its card. */
async function askForCommand(page, extra = {}) {
  await scriptCommand(page, { id: "call_1", command: COMMAND_TEXT, output: COMMAND_GOLDEN.texts.report.ok, answer: "Done.", ...extra });
  await sendText(page, "run the checks");
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

/** The command as the card writes it: the line break is the two characters `\n`. */
const SHOWN = COMMAND_TEXT.replace("\n", "\\n");

async function traceCase(page, url) {
  await chatBesidePane1(page, url);
  const card = await askForCommand(page);

  assert.equal(await page.locator("div.chat-cards > section.chat-approval").count(), 1, "the card is in the strip");
  assert.equal(await card.getAttribute("data-mode"), "ask");
  assert.equal(await card.getAttribute("data-tool"), "run_command");
  assert.equal(await card.getAttribute("data-effect"), "exec");
  const text = await card.textContent();
  for (const words of [
    "Approval needed", "Run a command", "pane 1 · This computer",
    "Runs one command in a separate process with your account's rights. Check the command below.",
  ]) {
    assert.ok(text.includes(words), `${words} in ${text}`);
  }

  // The payload keeps every field the core sent, in the core's order, and the command is the core's escaped text.
  const fields = await card.locator("li.approval-row").evaluateAll((r) => r.map((n) => n.dataset.name));
  assert.deepEqual(fields, ["tool", "pane", "target", "remote", "command", "timeout_secs", "resolved_cwd"]);
  assert.equal(await card.locator('li.approval-row[data-name="command"] .approval-value').textContent(), SHOWN);

  // The card never takes the keyboard, and nothing is decided yet.
  assert.equal(await page.evaluate(() => document.activeElement?.className), "chat-input");
  assert.deepEqual(await decisionCalls(page), []);
  assert.deepEqual(await commandRuns(page), [], "nothing ran before the decision");

  // One real press after the window: one call with the four ids and the card's own hash, and then the command runs once.
  const hash = await storedHash(page);
  assert.match(hash, /^[0-9a-f]{64}$/);
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await page.waitForFunction(() => document.querySelector("section.chat-approval") === null);
  const made = await decisionCalls(page);
  assert.deepEqual(made, [{ conversation: "chat-mock-1", call_id: "call_1", decision: "allow", hash }]);
  assert.deepEqual(Object.keys(made[0]), ["conversation", "call_id", "decision", "hash"]);
  await finished(page);
  assert.deepEqual(await commandRuns(page), [COMMAND_TEXT], "the command ran once");

  // The row says what the core said: it ran, and its result is the core's report under the label of untrusted text.
  const row = page.locator('div.chat-tool[data-call="call_1"]');
  assert.equal(await row.getAttribute("data-state"), "ran");
  assert.ok((await row.textContent()).includes("Run a command"));
  await row.locator("button.tool-toggle").click();
  await row.locator("pre.tool-result").waitFor();
  assert.equal(await row.locator(".tool-untrusted").textContent(), "Result · untrusted text");
  assert.equal(await row.locator("pre.tool-result").textContent(), COMMAND_GOLDEN.texts.report.ok);
  assert.equal(await page.evaluate(() => window.__MOCK__.ai.chat("chat-mock-1").ended.end), "done");
}

async function contractCase(page, url) {
  await chatBesidePane1(page, url);
  const card = await askForCommand(page);
  const hash = await storedHash(page);
  const sent = await page.evaluate(() => structuredClone(window.__MOCK__.ai.sent));
  const proposed = sent.find((e) => e.kind === "tool_proposed").pending;

  // Every member name of the card and each payload row equals the core's, and the card keeps the order of the members.
  const golden = COMMAND_GOLDEN.cards.local;
  assert.deepEqual(names(proposed), names(golden), "a card");
  assert.deepEqual(Object.keys(proposed), Object.keys(golden), "a card keeps the order of the members");
  for (const row of proposed.payload) assert.deepEqual(names(row), names(golden.payload[0]), "a payload row");
  assert.deepEqual(proposed.payload.map((p) => p.name), golden.payload.map((p) => p.name), "the payload names");
  assert.equal(proposed.effect, golden.effect);

  // The page reads the card the mock sent without changing its hash or its field order.
  const read = await page.evaluate(async (pending) => {
    const m = await import(new URL("approvalstate.js", location.href).href);
    return m.readApproval(pending);
  }, proposed);
  assert.equal(read.why, "ok");
  assert.equal(read.hash, hash);
  assert.deepEqual(read.approval.rows.map((r) => r.name), proposed.payload.map((p) => p.name));

  // The decision names the arguments of the command table.
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await finished(page);
  const made = await decisionCalls(page);
  assert.equal(made.length, 1);
  assert.deepEqual(Object.keys(made[0]), PROTOCOL.commands.ai_decide);
}

async function idsCase(page, url) {
  await chatBesidePane1(page, url);
  const command = "echo zq-cmd-text-5e17a3";
  const card = await askForCommand(page, { command });
  const hash = await storedHash(page);

  // The command is in text nodes of the card; no attribute, no title and no storage holds it.
  const surface = () =>
    page.evaluate(() => {
      const attrs = [];
      for (const n of document.querySelectorAll("*")) for (const a of n.attributes) attrs.push(a.value);
      const store = [];
      for (const area of [localStorage, sessionStorage]) for (let i = 0; i < area.length; i++) store.push(area.key(i), area.getItem(area.key(i)));
      return JSON.stringify({ attrs, store, cookie: document.cookie });
    });
  const canary = "zq-cmd-text-5e17a3";
  assert.ok(!(await surface()).includes(canary), "the command is in an attribute or a store");
  assert.ok((await card.textContent()).includes(canary), "the command is shown as text");

  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await finished(page);

  const all = await page.evaluate(() => structuredClone(window.__MOCK__.calls));
  const sentText = JSON.stringify(all.filter(([cmd]) => cmd !== "ai_send"));
  assert.ok(!sentText.includes(canary), "the command is in a call");
  assert.ok(!(await surface()).includes(canary), "the command is in an attribute or a store after the run");
  const titles = await page.evaluate(() => [...document.querySelectorAll("[title]")].map((n) => n.title).join("|"));
  assert.ok(!titles.includes(canary), "the command is in a title");

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
