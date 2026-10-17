// Running a command (docs/design.md section 6.17): a `run_command` card shows the exact command the core sent, one real press decides
// it with the four ids and the card's own hash, and the core's outcome removes it. The page runs nothing and sends ids only; a mock
// that plays the core is held to the golden of the core (tests/fixtures/ai_command_card.json).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  COMMAND_GOLDEN, COMMAND_TEXT, OTHER_PROFILE, PROFILE, TOOL_FILE, TOOL_PATH, VIEW_P1, boot, calls, commandRuns, decisionCalls, holdCommand,
  openAi, openCard, releaseCommand, reloadKeeping, scriptCommand, sendText, tickClock,
} from "./ai-lib.mjs";

const PROTOCOL = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(5000);
  // A failing case names itself.
  const cases = { traceCase, contractCase, idsCase, commandCase, resultCase, runningCase, remoteCase, bannerCase };
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

const HOSTILE_COMMAND = "zq-cmd-<img src=x onerror=alert(1)> [x](javascript:alert(1)) zqcm-7a31";
const HOSTILE_OUTPUT = "<b>zq-out</b> javascript:alert(1)";
const LAST_SEGMENT = "zq-last-segment-91c4";

/** The computed `max-height` of the payload box of a card. */
const maxHeight = (card) => card.locator("div.approval-payload").evaluate((n) => getComputedStyle(n).maxHeight);

async function commandCase(page, url, engine) {
  // A hostile command is the exact text in the `command` row: no element, no link, no handler.
  await chatBesidePane1(page, url);
  let card = await askForCommand(page, { command: HOSTILE_COMMAND });
  assert.equal(await card.locator('li.approval-row[data-name="command"] .approval-value').textContent(), HOSTILE_COMMAND);
  assert.equal(await card.locator("img, a").count(), 0);
  assert.equal(await card.evaluate((c) => c.querySelector("[onerror]") === null && c.querySelector("[href]") === null), true);

  // A command with a bidirectional override shows it as an escape and never raw.
  await chatBesidePane1(page, url);
  const bidi = `echo a${String.fromCharCode(0x202e)}b`;
  card = await askForCommand(page, { command: bidi });
  const shownBidi = await card.locator('li.approval-row[data-name="command"] .approval-value').textContent();
  assert.equal(shownBidi, "echo a\\u{202e}b");
  assert.ok(!(await card.textContent()).includes(String.fromCharCode(0x202e)), "a raw bidi character is in the card");

  // A very long command is in the DOM whole, and its box has no height limit and no scroll of its own, as the box of a read
  // card and of a write card is limited. The three cards are in one strip, so the comparison is on one page.
  await chatBesidePane1(page, url);
  const long = `echo ok${"\n".repeat(2000)}${LAST_SEGMENT}`;
  await scriptCommand(page, {
    calls: [
      { id: "call_1", command: long },
      { id: "call_2", tool: "read_file", path: TOOL_PATH, result: "x" },
      { id: "call_3", tool: "write_file", path: TOOL_FILE, content: "one\ntwo\n" },
    ],
    answer: "Done.",
  });
  await sendText(page, "run it");
  await openCard(page, "call_3");
  card = page.locator('section.chat-approval[data-call="call_1"]');
  const shown = await card.locator('li.approval-row[data-name="command"] .approval-value').textContent();
  assert.equal(shown.length, "echo ok".length + 4000 + LAST_SEGMENT.length, "the command is whole");
  assert.ok(shown.endsWith(LAST_SEGMENT));
  assert.equal(await maxHeight(card), "none", "a command card has no height limit");
  const room = await card.locator("div.approval-payload").evaluate((n) => [n.scrollHeight, n.clientHeight]);
  assert.ok(room[0] <= room[1] + 1, `the box scrolls by itself: ${room}`);
  for (const id of ["call_2", "call_3"]) {
    const other = await maxHeight(page.locator(`section.chat-approval[data-call="${id}"]`));
    assert.match(other, /^\d+(\.\d+)?px$/, `${id} keeps its limit`);
  }

  // The strip scrolls, and is at most half the pane.
  const strip = await page.evaluate(() => {
    const el = document.querySelector("div.chat-cards");
    return { scroll: el.scrollHeight, client: el.clientHeight, pane: el.closest(".block-chat").getBoundingClientRect().height };
  });
  assert.ok(strip.scroll > strip.client, `the strip does not scroll: ${JSON.stringify(strip)}`);
  assert.ok(strip.client <= strip.pane / 2 + 1, `the strip is more than half the pane: ${JSON.stringify(strip)}`);

  // With one long command, scrolled to the end of the strip, the buttons are inside the strip and come after the end of the command.
  await chatBesidePane1(page, url);
  card = await askForCommand(page, { command: long });
  const layout = await page.evaluate(() => {
    const strip = document.querySelector("div.chat-cards");
    strip.scrollTop = strip.scrollHeight;
    const box = (selector) => document.querySelector(selector).getBoundingClientRect();
    return {
      strip: box("div.chat-cards"), command: box('li.approval-row[data-name="command"]'), dest: box("p.approval-dest"),
      allow: box("button.approval-allow"), hash: box("p.approval-hash"),
    };
  });
  assert.ok(layout.allow.top >= layout.strip.top - 1 && layout.allow.bottom <= layout.strip.bottom + 1, `the button is outside the strip: ${JSON.stringify(layout)}`);
  assert.ok(layout.command.bottom <= layout.dest.top + 1, "the command is above the destination line");
  assert.ok(layout.dest.top < layout.hash.top && layout.hash.top < layout.allow.top, "the destination, the hash and the buttons follow in order");

  // The keys of the card: from the message box `Shift+Tab` reaches Deny, then Allow once, then the payload box.
  const back = () => page.keyboard.press(engine === "webkit" && process.platform === "darwin" ? "Shift+Alt+Tab" : "Shift+Tab");
  const active = () => page.evaluate(() => document.activeElement?.className);
  await page.locator(".chat-input").focus();
  await back();
  assert.equal(await active(), "approval-deny");
  await back();
  assert.equal(await active(), "approval-allow");
  await back();
  assert.equal(await active(), "approval-payload");
  // `Escape` inside the card denies that call and does not stop the reply.
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_decide").length === 1);
  assert.equal((await decisionCalls(page))[0].decision, "deny");
  assert.equal((await calls(page, "ai_cancel")).length, 0, "the reply was not stopped");
  assert.deepEqual(await commandRuns(page), [], "a denied command did not run");
}

/** The row of a call in the transcript. */
const rowOf = (page, id) => page.locator(`.chat-msg[data-role="assistant"] .chat-tool[data-call="${id}"]`);

/** Press the toggle of a row and wait for its result to be drawn. */
async function opened(row) {
  await row.locator("button.tool-toggle").click();
  await row.locator("pre.tool-result").waitFor();
}

/** Ask for a command, allow it once the window has passed, and wait for the end of the turn. */
async function allowed(page, spec, text = "run it") {
  await scriptCommand(page, { answer: "Done.", ...spec });
  await sendText(page, text);
  const card = await openCard(page, spec.id);
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await page.waitForFunction(() => !window.__MOCK__.ai.chat("chat-mock-1").running);
  await page.waitForSelector(`.chat-tool[data-call="${spec.id}"]:not([data-state="waiting"])`);
}

async function resultCase(page, url) {
  await chatBesidePane1(page, url);
  const report = COMMAND_GOLDEN.texts.report;
  const outcomes = [
    ["a non-zero status", { output: report.failed, failed: true }, "error", "!", report.failed],
    ["a time out", { output: report.timed_out, failed: true }, "error", "!", report.timed_out],
    ["a lost connection", { output: report.lost, failed: true }, "error", "!", report.lost],
    ["no output", { output: report.no_output }, "ran", "\u2713", report.no_output],
    ["a status 0", { output: report.ok }, "ran", "\u2713", report.ok],
  ];
  let decided = 0;
  for (const [n, [what, spec, state, mark, text]] of outcomes.entries()) {
    const id = `call_${n + 1}`;
    await allowed(page, { id, ...spec });
    decided += 1;
    const row = rowOf(page, id);
    assert.equal(await row.getAttribute("data-state"), state, what);
    assert.equal(await row.locator(".tool-mark").textContent(), mark, `${what}: the mark`);
    assert.equal(await row.locator(".tool-state").textContent(), state, `${what}: the word`);
    assert.equal(await row.locator(".tool-words").textContent(), "Run a command");
    const toggles = row.locator("button.tool-toggle");
    assert.equal(await toggles.count(), 1, `${what}: one toggle`);
    assert.equal(await toggles.getAttribute("aria-expanded"), "false");
    assert.ok((await toggles.getAttribute("data-field")) !== null, `${what}: the toggle carries data-field`);
    await opened(row);
    assert.equal(await row.locator(".tool-untrusted").textContent(), "Result \u00b7 untrusted text");
    assert.equal(await row.locator("pre.tool-result").textContent(), text, what);
    assert.equal((await decisionCalls(page)).length, decided, `${what}: no second decision call`);
  }

  // A denial is an error with the core's sentence.
  await scriptCommand(page, { id: "call_6", answer: "Done." });
  await sendText(page, "run it too");
  await (await openCard(page, "call_6")).locator("button.approval-deny").click();
  await page.waitForSelector('.chat-tool[data-call="call_6"][data-state="error"]');
  const denied = rowOf(page, "call_6");
  assert.equal(await denied.locator(".tool-state").textContent(), "error");
  await opened(denied);
  assert.equal(await denied.locator("pre.tool-result").textContent(), "The user denied this action.");
  await page.waitForFunction(() => !window.__MOCK__.ai.chat("chat-mock-1").running);

  // A long output is cut at 20000 characters, and a hostile one is text.
  await allowed(page, { id: "call_7", output: "y".repeat(30000) });
  await opened(rowOf(page, "call_7"));
  const long = await rowOf(page, "call_7").locator("pre.tool-result").textContent();
  assert.ok(long.startsWith("y".repeat(20000)) && !long.startsWith("y".repeat(20001)));
  assert.ok(long.endsWith("[cut for display]"));
  await allowed(page, { id: "call_8", output: HOSTILE_OUTPUT });
  await opened(rowOf(page, "call_8"));
  assert.equal(await rowOf(page, "call_8").locator("pre.tool-result").textContent(), HOSTILE_OUTPUT);
  const markup = await page.evaluate(() => ({
    elements: document.querySelectorAll(".chat-tool b, .chat-tool a, .chat-tool img").length,
    handlers: [...document.querySelectorAll(".chat-tool *")].filter((n) => n.getAttributeNames().some((a) => a.startsWith("on"))).length,
  }));
  assert.deepEqual(markup, { elements: 0, handlers: 0 });
  assert.equal((await decisionCalls(page)).length, 8, "one decision for each card");
}

async function runningCase(page, url) {
  const stopped = COMMAND_GOLDEN.texts.stopped;
  for (const how of ["button", "escape"]) {
    await chatBesidePane1(page, url);
    await holdCommand(page);
    const card = await askForCommand(page);
    await tickClock(page, 1000);
    await card.locator("button.approval-allow").click();
    // The card says the command is running, has no buttons, and the chat is still at work.
    await page.waitForSelector('section.chat-approval[data-mode="sent"]');
    assert.ok((await card.textContent()).includes("Allowed \u00b7 running\u2026"), how);
    assert.equal(await card.locator("button").count(), 0, `${how}: no button while it runs`);
    assert.match(await page.locator(".chat-status").textContent(), /waiting for your decision|working/);
    assert.deepEqual(await commandRuns(page), [COMMAND_TEXT]);

    if (how === "button") await page.locator('.chat-send[data-mode="stop"]').click();
    else await page.locator(".chat-input").press("Escape");
    await page.waitForFunction(() => window.__MOCK__.ai.chat("chat-mock-1").ended?.end === "cancelled");
    await page.waitForFunction(() => document.querySelector("section.chat-approval") === null);
    assert.deepEqual(await calls(page, "ai_cancel"), [{ conversation: "chat-mock-1" }], how);
    const result = (await page.evaluate(() => structuredClone(window.__MOCK__.ai.sent))).find((e) => e.kind === "tool_result");
    assert.equal(result.content, stopped, how);
    assert.equal(result.is_error, true);
    const row = rowOf(page, "call_1");
    assert.equal(await row.getAttribute("data-state"), "error", how);
    await opened(row);
    assert.equal(await row.locator(".tool-untrusted").textContent(), "Result \u00b7 untrusted text");
    assert.equal(await row.locator("pre.tool-result").textContent(), stopped, how);
    assert.equal((await decisionCalls(page)).length, 1, `${how}: no second decision call`);
  }

  // Without a stop the result arrives when the command ends, and the row says it ran.
  await chatBesidePane1(page, url);
  await holdCommand(page);
  const card = await askForCommand(page);
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await page.waitForSelector('section.chat-approval[data-mode="sent"]');
  await releaseCommand(page);
  await finished(page);
  assert.equal(await rowOf(page, "call_1").getAttribute("data-state"), "ran");
  assert.equal((await calls(page, "ai_cancel")).length, 0);
}

async function remoteCase(page, url) {
  await chatBesidePane1(page, url);
  let card = await askForCommand(page, { remote: true, target: "talos-1", cwd: "/srv/app", timeout_secs: 120 });
  assert.equal(await card.getAttribute("data-remote"), "true");
  assert.equal(await card.locator(".approval-remote").textContent(), "remote host");
  assert.equal(await card.locator(".approval-where").textContent(), "pane 1 \u00b7 talos-1");
  const fields = await card.locator("li.approval-row").evaluateAll((r) => r.map((n) => n.dataset.name));
  assert.deepEqual(fields, ["tool", "pane", "target", "remote", "command", "cwd", "timeout_secs", "resolved_cwd"]);
  assert.equal(await card.locator("p.approval-dest").textContent(), `The command's output is sent to ${PROFILE.model} at ${PROFILE.host} (this computer).`);

  // A cloud route leaves the computer and says so; after a reload the route is not known and the card does not guess.
  await chatBesidePane1(page, url, { profiles: [PROFILE, OTHER_PROFILE] });
  await page.locator(".chat-chip").click();
  await page.locator('.chat-choice[data-id="other-1"]').click();
  card = await askForCommand(page, { remote: true, target: "talos-1" });
  assert.equal(await card.locator("p.approval-dest").textContent(), `The command's output is sent to ${OTHER_PROFILE.model} at ${OTHER_PROFILE.host} (cloud).`);
  assert.equal(await card.locator(".approval-place").getAttribute("data-leaves"), "true");
  await reloadKeeping(page);
  card = await openCard(page, "call_1");
  assert.equal(await card.locator("p.approval-dest").textContent(), "The command's output is sent to the model of this chat.");
  assert.equal(await card.locator(".approval-place").count(), 0);
}

async function bannerCase(page, url) {
  await chatBesidePane1(page, url);
  // A tainted command: the banner holds the title, the core's label and the sentence of a command.
  let card = await askForCommand(page, { tainted: true, taint: ["output of read_file"] });
  assert.equal(await card.getAttribute("data-tainted"), "true");
  const banner = card.locator('div.approval-taint[data-tone="warn"]');
  const text = await banner.textContent();
  for (const words of ["Untrusted text in this turn", "output of read_file", "A command will run. Read all of it before you allow it."]) {
    assert.ok(text.includes(words), `${words} in ${text}`);
  }
  assert.ok((await card.textContent()).includes("Runs one command in a separate process with your account's rights. Check the command below."));

  // A click at the moment the card appears decides nothing; after the window one real click decides once, with the card's hash.
  await card.locator("button.approval-allow").click({ force: true });
  assert.deepEqual(await decisionCalls(page), [], "a click at once is ignored");
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").evaluate((button) => button.click());
  assert.deepEqual(await decisionCalls(page), [], "a click made by a script is ignored");
  const hash = await storedHash(page);
  await card.locator("button.approval-allow").click();
  await finished(page);
  assert.deepEqual(await decisionCalls(page), [{ conversation: "chat-mock-1", call_id: "call_1", decision: "allow", hash }]);

  // A command that follows no untrusted text has no banner.
  await chatBesidePane1(page, url);
  card = await askForCommand(page, { tainted: false, taint: [] });
  assert.equal(await card.getAttribute("data-tainted"), "false");
  assert.equal(await card.locator(".approval-taint").count(), 0);
}
