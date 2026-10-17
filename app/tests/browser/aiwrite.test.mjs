// Changing a file (docs/design.md section 6.16): a write tool's card shows the changes the core worked out from the file's current
// text, one real press decides it with the four ids and the card's own hash, and the core's outcome removes it. The page works out
// no diff and sends ids only; a mock that plays the core is held to the golden of the core (tests/fixtures/ai_write_card.json).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  FILE_NEW, FILE_OLD, OTHER_PROFILE, PROFILE, TOOL_FILE, VIEW_P1, addRows, boot, calls, changeFile, chars, decisionCalls, diffOf, fileText,
  openAi, openCard, payloadOf, scriptWrite, seedFile, sendText, tickClock,
} from "./ai-lib.mjs";

const GOLDEN = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));
const WRITE = JSON.parse(readFileSync(new URL("../fixtures/ai_write_card.json", import.meta.url), "utf8"));

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(5000);
  // A failing case names itself.
  const cases = { traceCase, contractCase, idsCase, diffCase, editCase, remoteCase, capsCase, staleCase };
  for (const [name, run] of Object.entries(cases)) {
    try {
      await run(page, url);
    } catch (e) {
      e.message = `${name}: ${e.message}`;
      throw e;
    }
  }
  await keysCase(page, url, engine).catch((e) => {
    e.message = `keysCase: ${e.message}`;
    throw e;
  });
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

/** The lines `1..n` of a file as text, with the numbered lines in `changed` replaced. */
const lines = (n, changed = {}, final = true) => Array.from({ length: n }, (_, i) => changed[i + 1] ?? `line ${i + 1}`).join("\n") + (final ? "\n" : "");

/** The card of a write over a seeded file, for the cases that look at the diff. */
async function writeOver(page, url, before, after, extra = {}) {
  await chatBesidePane1(page, url);
  await seedFile(page, TOOL_FILE, before);
  await scriptWrite(page, { id: "call_1", tool: "write_file", path: TOOL_FILE, content: after, answer: "Done.", ...extra });
  await sendText(page, "change it");
  return openCard(page, "call_1");
}

async function diffCase(page, url) {
  // Thirty lines with line 15 replaced: three unchanged lines each side, and the core's numbers for the gaps.
  let card = await writeOver(page, url, lines(30), lines(30, { 15: "line 15 changed" }));
  let diff = card.locator("div.approval-diff");
  const ops = await diff.locator("li.diff-row").evaluateAll((r) => r.map((n) => n.dataset.op));
  assert.deepEqual(ops, ["gap", "ctx", "ctx", "ctx", "del", "add", "ctx", "ctx", "ctx", "gap"]);
  assert.deepEqual(await diff.locator(".diff-gap").allTextContents(), ["\u22ef 11 unchanged lines", "\u22ef 12 unchanged lines"]);
  assert.deepEqual(await diff.locator("li.diff-row[data-op='ctx'] .diff-old").allTextContents(), ["12", "13", "14", "16", "17", "18"]);
  assert.equal(await diff.locator(".diff-kind").textContent(), "Replaces the whole file");
  assert.equal(await diff.locator(".diff-count").textContent(), "1 line added, 1 removed");
  assert.deepEqual(await diff.locator(".diff-sign").allTextContents(), ["", "", "", "-", "+", "", "", ""].slice(0, 0).concat(await diff.locator(".diff-sign").allTextContents()));
  const signs = await diff.locator("li.diff-row:not([data-op='gap']):not([data-op='note']) .diff-sign").allTextContents();
  assert.deepEqual(signs, ["", "", "", "-", "+", "", "", ""], "the sign is a character: + for an added line, - for a removed one, none otherwise");

  // The member names of a gap row and of a note row are the core's.
  const proposed = (await page.evaluate(() => structuredClone(window.__MOCK__.ai.sent))).find((e) => e.kind === "tool_proposed").pending;
  const golden = new Map();
  for (const one of Object.values(WRITE.cards)) for (const row of one.diff.rows) golden.set(row.op, Object.keys(row));
  for (const row of proposed.diff.rows) assert.deepEqual(Object.keys(row), golden.get(row.op), `a row ${row.op}`);

  // A file without a final newline: the core's note after the last line.
  card = await writeOver(page, url, "a\nb", "a\nb\nc");
  diff = card.locator("div.approval-diff");
  assert.deepEqual(await diff.locator("li.diff-row").evaluateAll((r) => r.map((n) => n.dataset.op)), ["ctx", "del", "note", "add", "add", "note"]);
  assert.deepEqual(await diff.locator("li.diff-row[data-op='note'] .diff-note").allTextContents(), ["No newline at end of file", "No newline at end of file"]);
  const proposedNote = (await page.evaluate(() => structuredClone(window.__MOCK__.ai.sent))).find((e) => e.kind === "tool_proposed").pending;
  for (const row of proposedNote.diff.rows) assert.deepEqual(Object.keys(row), golden.get(row.op), `a row ${row.op}`);

  // A hidden character is shown as an escape, never raw; text that looks like markup is only text.
  const bidi = `a${String.fromCharCode(0x202e)}b`;
  const hostile = "zq-diff-<img src=x onerror=alert(1)> [x](javascript:alert(1)) zqdf-7a30";
  card = await writeOver(page, url, "one\ntwo\n", `one\n${bidi}\n${hostile}\n`);
  diff = card.locator("div.approval-diff");
  const texts = await diff.locator(".diff-text").allTextContents();
  assert.ok(texts.includes("a\\u{202e}b"), `${texts}`);
  assert.equal(texts.includes(hostile), true, "the hostile line is shown as that exact text");
  const box = await diff.textContent();
  assert.ok(!box.includes(String.fromCharCode(0x202e)), "no raw bidi character in the box");
  assert.equal(await card.locator("img, a").count(), 0);
  assert.equal(await card.evaluate((c) => c.querySelector("[onerror]") === null), true);

  // A line of 5000 characters is in the DOM whole and wraps inside the box.
  card = await writeOver(page, url, "short\n", `${"w".repeat(5000)}\n`);
  diff = card.locator("div.approval-diff");
  assert.ok((await diff.locator(".diff-text").allTextContents()).some((t) => t.length === 5000));
  const fits = await diff.evaluate((box) => box.scrollWidth <= box.clientWidth + 1);
  assert.equal(fits, true, "the row is not wider than the box");

  // Two hundred rows: the box is at most 16 rem high and scrolls inside.
  card = await writeOver(page, url, "", lines(200));
  diff = card.locator("div.approval-diff");
  const sizes = await diff.evaluate((box) => {
    const rem = parseFloat(getComputedStyle(document.documentElement).fontSize);
    return { high: box.getBoundingClientRect().height / rem, scroll: box.scrollHeight > box.clientHeight };
  });
  assert.ok(sizes.high <= 16.01, `${sizes.high} rem`);
  assert.equal(sizes.scroll, true);

  // A diff with an operation the page does not know can only be denied.
  const odd = diffOf([{ op: "move", old: 1, new: 2, text: "x" }]);
  card = await writeOver(page, url, "one\n", "two\n", { diff: odd });
  assert.equal(await card.getAttribute("data-mode"), "cannot");
  assert.equal(await card.locator("button.approval-deny").count(), 1);
  assert.equal(await card.locator("button.approval-allow").count(), 0);
}

async function editCase(page, url) {
  const before = "foo one\nbar\nfoo two\nbaz\nfoo three\n";
  await chatBesidePane1(page, url);
  await seedFile(page, TOOL_FILE, before);
  await scriptWrite(page, { id: "call_1", tool: "edit_file", path: TOOL_FILE, old_text: "bar", new_text: "BAR", answer: "Done." });
  await sendText(page, "edit it");
  let card = await openCard(page, "call_1");
  assert.ok((await card.textContent()).includes("Edit a file"));
  assert.equal(await card.getAttribute("data-effect"), "write");
  assert.equal(await card.locator(".diff-kind").textContent(), "Changes parts of the file");
  assert.deepEqual(
    await card.locator("li.approval-row").evaluateAll((r) => r.map((n) => n.dataset.name)),
    ["tool", "pane", "target", "remote", "path", "old_text", "new_text", "replace_all", "resolved_path", "stamp", "base"],
  );
  assert.equal(await card.locator('li.approval-row[data-name="replace_all"] .approval-value').textContent(), "false");
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await finished(page);
  assert.equal(await fileText(page, TOOL_FILE), before.replace("bar", "BAR"), "the file holds the replaced text");
  const row = page.locator('div.chat-tool[data-call="call_1"]');
  assert.equal(await row.getAttribute("data-state"), "ran");
  assert.ok((await row.textContent()).includes("Edit a file"));

  // Every match on request: three changes.
  await chatBesidePane1(page, url);
  await seedFile(page, TOOL_FILE, before);
  await scriptWrite(page, { id: "call_1", tool: "edit_file", path: TOOL_FILE, old_text: "foo", new_text: "FOO", replace_all: true, answer: "Done." });
  await sendText(page, "edit it all");
  card = await openCard(page, "call_1");
  const ops = await card.locator("li.diff-row").evaluateAll((r) => r.map((n) => n.dataset.op));
  assert.equal(ops.filter((o) => o === "del").length, 3);
  assert.equal(ops.filter((o) => o === "add").length, 3);
  assert.equal(await card.locator('li.approval-row[data-name="replace_all"] .approval-value').textContent(), "true");

  // An edit whose text is not in the file has no card: the row is an error with the core's phrase.
  await chatBesidePane1(page, url);
  await seedFile(page, TOOL_FILE, before);
  await scriptWrite(page, { id: "call_2", tool: "edit_file", path: TOOL_FILE, old_text: "absent text", new_text: "x", answer: "Done." });
  await sendText(page, "edit nothing");
  await finished(page);
  assert.equal(await page.locator("section.chat-approval").count(), 0, "no card");
  const failed = page.locator('div.chat-tool[data-call="call_2"]');
  assert.equal(await failed.getAttribute("data-state"), "error");
  await failed.locator("button.tool-toggle").click();
  const result = await failed.locator("pre.tool-result").textContent();
  assert.ok(result.includes(WRITE.texts.faults.no_match), result);
  assert.equal(await fileText(page, TOOL_FILE), before, "the file is untouched");
}

async function remoteCase(page, url) {
  await chatBesidePane1(page, url);
  await scriptWrite(page, {
    id: "call_1", tool: "write_file", path: "/srv/app/deploy.sh", content: "#!/bin/sh\nset -eu\n", target: "talos-1", remote: true, answer: "Done.",
  });
  await sendText(page, "make the script");
  let card = await openCard(page, "call_1");
  assert.equal(await card.getAttribute("data-remote"), "true");
  assert.equal(await card.locator(".approval-remote").textContent(), "remote host");
  assert.ok((await card.locator(".approval-where").textContent()).includes("pane 1 \u00b7 talos-1"));
  const diff = card.locator('div.approval-diff[data-kind="create"]');
  assert.equal(await diff.count(), 1);
  assert.equal(await diff.locator(".diff-kind").textContent(), "Creates a new file");
  assert.ok((await diff.locator(".diff-size").textContent()).startsWith("new \u00b7 "));
  assert.deepEqual(await diff.locator("li.diff-row").evaluateAll((r) => r.map((n) => n.dataset.op)), ["add", "add"]);
  assert.deepEqual(await diff.locator(".diff-old").allTextContents(), ["", ""]);
  assert.deepEqual(
    await card.locator("li.approval-row").evaluateAll((r) => r.map((n) => n.dataset.name)),
    ["tool", "pane", "target", "remote", "path", "content", "resolved_path", "base"],
  );
  assert.equal(await card.locator('li.approval-row[data-name="base"] .approval-value').textContent(), "none");
  assert.equal(
    await card.locator("p.approval-dest").textContent(),
    `Only the outcome (saved or not) is sent to ${PROFILE.model} at ${PROFILE.host} (this computer).`,
  );

  // A cloud route leaves the computer, and says so in words.
  await chatBesidePane1(page, url, { profiles: [PROFILE, OTHER_PROFILE] });
  await page.locator(".chat-chip").click();
  await page.locator('.chat-choice[data-id="other-1"]').click();
  await scriptWrite(page, { id: "call_1", tool: "write_file", path: "/srv/app/new.txt", content: "x\n", answer: "Done." });
  await sendText(page, "make a file");
  card = await openCard(page, "call_1");
  assert.equal(
    await card.locator("p.approval-dest").textContent(),
    `Only the outcome (saved or not) is sent to ${OTHER_PROFILE.model} at ${OTHER_PROFILE.host} (cloud).`,
  );
  assert.equal(await card.locator(".approval-place").getAttribute("data-leaves"), "true");

  // A turn that holds text the user did not write: the banner names it and says that a file will be changed.
  await chatBesidePane1(page, url);
  await scriptWrite(page, {
    id: "call_1", tool: "write_file", path: "/srv/app/new.txt", content: "x\n", tainted: true, taint: ["output of read_file"], answer: "Done.",
  });
  await sendText(page, "make a file after reading");
  card = await openCard(page, "call_1");
  const banner = await card.locator(".approval-taint").textContent();
  for (const words of ["output of read_file", "can try to steer the assistant", "A file will be changed. Check the changes below before you allow it."]) {
    assert.ok(banner.includes(words), `${words} in ${banner}`);
  }
}

async function capsCase(page, url) {
  const ask = async (spec) => {
    await chatBesidePane1(page, url);
    await scriptWrite(page, { id: "call_1", tool: "write_file", path: TOOL_FILE, content: "x\n", answer: "Done.", ...spec });
    await sendText(page, "big one");
    return openCard(page, "call_1");
  };
  const payload = (...rest) => payloadOf("write_file", rest);

  // One value of 393216 characters is shown; one more is not, and only `Deny` is left.
  let card = await ask({ payload: payload(["path", TOOL_FILE], ["content", chars(393216)]) });
  assert.equal(await card.getAttribute("data-mode"), "ask");
  assert.equal(await card.locator("button.approval-allow").count(), 1);
  card = await ask({ payload: payload(["path", TOOL_FILE], ["content", chars(393217)]) });
  assert.equal(await card.getAttribute("data-mode"), "cannot");
  assert.ok((await card.textContent()).includes("Cannot show in full"));
  assert.equal(await card.locator("button.approval-allow").count(), 0);
  assert.equal(await card.locator("button.approval-deny").count(), 1);

  // More than 524288 characters in all, over several rows (the name of the tool is one of the values: ten characters).
  card = await ask({ payload: payload(["a", chars(180000)], ["b", chars(180000)], ["c", chars(164279)]) });
  assert.equal(await card.getAttribute("data-mode"), "cannot");
  card = await ask({ payload: payload(["a", chars(180000)], ["b", chars(180000)], ["c", chars(164278)]) });
  assert.equal(await card.getAttribute("data-mode"), "ask");

  // A diff of 4000 rows is shown, and one of 4001 rows is not.
  card = await ask({ diff: diffOf(addRows(4000)) });
  assert.equal(await card.getAttribute("data-mode"), "ask");
  assert.equal(await card.locator("li.diff-row").count(), 4000);
  card = await ask({ diff: diffOf(addRows(4001)) });
  assert.equal(await card.getAttribute("data-mode"), "cannot");
  assert.ok((await card.textContent()).includes("Cannot show in full"));
  assert.equal(await card.locator("button.approval-allow").count(), 0);
  assert.equal(await card.locator("li.diff-row").count(), 0, "a diff that is not shown in full is not shown at all");
  // The strip still scrolls.
  assert.equal(await page.locator("div.chat-cards").evaluate((n) => getComputedStyle(n).overflowY !== "visible"), true);
}

async function staleCase(page, url) {
  // The file changed after the card: other text of the same length. The core refuses the call and says so.
  const other = FILE_OLD.replace("old", "oth");
  assert.equal(other.length, FILE_OLD.length);
  let card = await writeOver(page, url, FILE_OLD, FILE_NEW);
  const hash = await storedHash(page);
  await changeFile(page, TOOL_FILE, other);
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await finished(page);
  const made = await decisionCalls(page);
  assert.deepEqual(made, [{ conversation: "chat-mock-1", call_id: "call_1", decision: "allow", hash }], "the card's own hash");
  assert.equal(await page.locator("section.chat-approval").count(), 0, "the card is gone");
  let row = page.locator('div.chat-tool[data-call="call_1"]');
  assert.equal(await row.getAttribute("data-state"), "error");
  await row.locator("button.tool-toggle").click();
  assert.equal(await row.locator(".tool-untrusted").textContent(), "Result \u00b7 untrusted text");
  assert.equal(await row.locator("pre.tool-result").textContent(), WRITE.texts.payload_changed);
  assert.equal(await fileText(page, TOOL_FILE), other, "the file is as the other program left it");

  // The same when the file is removed meanwhile.
  card = await writeOver(page, url, FILE_OLD, FILE_NEW);
  await changeFile(page, TOOL_FILE, null);
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").click();
  await finished(page);
  row = page.locator('div.chat-tool[data-call="call_1"]');
  assert.equal(await row.getAttribute("data-state"), "error");
  await row.locator("button.tool-toggle").click();
  assert.equal(await row.locator("pre.tool-result").textContent(), WRITE.texts.payload_changed);
  assert.equal(await fileText(page, TOOL_FILE), null, "nothing was written");
}

async function keysCase(page, url, engine) {
  // WebKit on macOS visits buttons with Option+Tab only, as Safari does by default.
  const mac = engine === "webkit" && process.platform === "darwin";
  const back = () => page.keyboard.press(mac ? "Shift+Alt+Tab" : "Shift+Tab");
  const forward = () => page.keyboard.press(mac ? "Alt+Tab" : "Tab");
  const active = () => page.evaluate(() => document.activeElement?.className);
  const terminal = () => page.evaluate(() => window.__MOCK__.calls.filter((c) => c[0] === "key" || c[0] === "text").length);
  await chatBesidePane1(page, url);
  await seedFile(page, TOOL_FILE, FILE_OLD);
  await scriptWrite(page, { id: "call_1", tool: "write_file", path: TOOL_FILE, content: FILE_NEW, answer: "Done." });
  await page.locator(".chat-input").fill("update my todo list");
  await page.locator(".chat-input").press("Enter");
  await page.keyboard.type("abc");
  const card = await openCard(page, "call_1");
  assert.equal(await active(), "chat-input");
  assert.equal(await page.locator(".chat-input").inputValue(), "abc", "typing keeps the focus and the text");
  assert.deepEqual(await decisionCalls(page), []);
  await page.locator(".chat-input").fill("");

  // The order: from the message box `Shift+Tab` reaches `Deny`, `Allow once`, the payload box and then the diff box.
  const loose = await page.evaluate(
    () => [...document.querySelectorAll(".chat-approval button, .chat-approval .approval-payload, .chat-approval .approval-diff")].filter((n) => !n.hasAttribute("data-field")).length,
  );
  assert.equal(loose, 0, "every control carries data-field");
  await back();
  assert.equal(await active(), "approval-deny");
  await back();
  assert.equal(await active(), "approval-allow");
  await back();
  assert.equal(await active(), "approval-payload");
  await back();
  assert.equal(await active(), "approval-diff");
  await forward();
  assert.equal(await active(), "approval-payload");
  await forward();
  assert.equal(await active(), "approval-allow");
  await forward();
  assert.equal(await active(), "approval-deny");

  // The diff box is a group named by its head line.
  const diff = card.locator("div.approval-diff");
  assert.equal(await diff.getAttribute("role"), "group");
  const name = await diff.evaluate((n) => document.getElementById(n.getAttribute("aria-labelledby")).textContent);
  assert.equal(name, await diff.locator("p.diff-head").textContent());
  assert.ok(name.startsWith("Replaces the whole file 1 line added, 1 removed"), name);

  // A character typed on the card reaches no terminal; `Escape` in the diff box denies that call and gives the keyboard back.
  await diff.focus();
  const typed = await terminal();
  await page.keyboard.type("x");
  assert.equal(await terminal(), typed, "a character typed on the card reaches no terminal");
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_decide").length === 1);
  assert.equal((await decisionCalls(page))[0].decision, "deny");
  assert.equal(await active(), "chat-input", "the keyboard went back to the message box");
  assert.equal((await calls(page, "ai_cancel")).length, 0, "the reply was not stopped");
  await page.waitForFunction(() => document.querySelector("section.chat-approval") === null);
  assert.equal(await fileText(page, TOOL_FILE), FILE_OLD, "a denied write leaves the file alone");
}
