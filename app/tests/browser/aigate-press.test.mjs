// What a person can do and a script cannot. The page sends ids and typed text only: the text and the key of a shared pane are in
// no call. A click, a dispatched mouse event and a dispatched Enter that a script made decide nothing, answer nothing, undo
// nothing and insert nothing, while one real press makes exactly one call with exactly the keys the protocol golden lists. A key
// typed into the providers dialog goes out in one call and is never shown again.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  ASK, FILE_NEW, FILE_OLD, PANE_KEY, PANE_TEXT, PROFILE, TOOL_FILE, TOOL_PATH, TOOL_TEXT, VIEW_P1, askCalls, askCard, boot, calls, commandNames,
  decisionCalls, emitAsk, fillField, openAi, openCard, openContext, openDialog, pastesOf, scriptReply, scriptTool, scriptWrite, seedFile, sendText,
  settle, tickClock, undoCalls,
} from "./ai-lib.mjs";
import { APP_DIR } from "./lib.mjs";
import { armGuards, pageClean } from "./aigate-lib.mjs";

const GOLDEN = JSON.parse(readFileSync(`${APP_DIR}tests/fixtures/protocol.json`, "utf8"));

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(8000);
  const guards = armGuards(page, url);
  await idsCase(page, url, guards);
  await pressCase(page, url, guards);
  await keyCase(page, url, guards);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** The argument names the protocol golden lists for `command`, read at run time. */
const keysOf = (command) => {
  const keys = GOLDEN.commands[command];
  assert.ok(Array.isArray(keys) && keys.length > 0, `the golden lists ${command}`);
  return keys;
};

/** Assert that a call has exactly the keys of the golden, in its order. */
const assertKeys = (command, args) => assert.deepEqual(Object.keys(args), keysOf(command), `${command} has exactly the documented keys`);

/** A page with two terminals, the focus on pane 1, a frozen clock and a chat beside it. */
async function chatBeside(page, url, extra = {}) {
  await boot(page, url, { view: VIEW_P1, settings: true, clock: true, profiles: [PROFILE], ...extra });
  await openAi(page);
  await page.waitForFunction(() => document.activeElement?.matches("textarea.chat-input"));
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_profiles").length >= 1);
}

/** Wait for the final answer and the end of the turn. */
const finished = (page) =>
  page.waitForFunction(
    () =>
      [...document.querySelectorAll('.chat-msg[data-role="assistant"] .chat-text')].some((n) => n.textContent === "Done.") &&
      window.__MOCK__.ai.chat("chat-mock-1").ended?.end === "done",
  );

/** Everything a person or a script could read from the page, as one text: markup, text, inputs and both stores. */
const everything = (page) =>
  page.evaluate(() => {
    const parts = [document.documentElement.outerHTML, document.body.innerText, document.cookie];
    for (const store of [localStorage, sessionStorage]) for (const key of Object.keys(store)) parts.push(key, store.getItem(key));
    for (const field of document.querySelectorAll("input, textarea")) parts.push(field.value);
    const walker = document.createTreeWalker(document.documentElement, NodeFilter.SHOW_TEXT);
    for (let n = walker.nextNode(); n !== null; n = walker.nextNode()) parts.push(n.data);
    return parts.join("\n");
  });

// ---- ids only --------------------------------------------------------------------------------------------------------------

/**
 * A pane is shared, its screen previewed and a message sent. The text and the key on the pane's screen occur in no call of any
 * command; the send has only the documented keys, the typed text, the offer id and the numbers of the picked items.
 */
async function idsCase(page, url, guards) {
  await chatBeside(page, url, { clock: false });
  assert.ok(await page.evaluate(([text, key]) => JSON.stringify(window.__MOCK__.ai.context.panes[1]).includes(text) && JSON.stringify(window.__MOCK__.ai.context.panes[1]).includes(key), [PANE_TEXT, PANE_KEY]), "the canaries are on the mock's pane");
  await openContext(page);
  if ((await page.locator(".chat-consent").getAttribute("data-mode")) === "ask") await page.locator(".consent-share").click();
  await page.waitForSelector('.chat-consent[data-mode="pick"]');
  await page.locator('label.consent-kind[data-kind="screen"] input').check();
  await page.locator(".consent-preview").click();
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  await sendText(page, "explain this");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);

  const all = JSON.stringify(await page.evaluate(() => window.__MOCK__.calls));
  for (const canary of [PANE_TEXT, PANE_KEY, PANE_KEY.slice(0, 24), PANE_TEXT.slice(0, 20)]) assert.ok(!all.includes(canary), `${canary} is in a call`);
  const sends = await calls(page, "ai_send");
  const last = sends.at(-1);
  for (const key of Object.keys(last)) assert.ok(keysOf("ai_send").includes(key), `ai_send has the key ${key}`);
  assert.equal(last.text, "explain this", "the text is what was typed");
  assert.equal(typeof last.offer, "string", "the offer is an id");
  assert.ok(Array.isArray(last.picked) && last.picked.length > 0 && last.picked.every((n) => Number.isInteger(n)), "the picked items are numbers");
  assert.ok(!JSON.stringify(last).includes("screen"), "no kind of text rides on the send");
  const wanted = keysOf("ai_send");
  assert.ok(Object.keys(last).every((k) => wanted.includes(k)));
  for (const [command, args] of await page.evaluate(() => window.__MOCK__.calls.map((c) => [c[0], structuredClone(c[1])]))) {
    if (command.startsWith("ai_context_")) assert.deepEqual(Object.keys(args), keysOf(command), `${command} has the documented keys`);
  }
  await guards.assertQuiet("ids");
  console.log(`aigate ids: ${(await page.evaluate(() => window.__MOCK__.calls.length))} calls, the canaries in none`);
}

// ---- a script cannot press, a person can ---------------------------------------------------------------------------------

/** What a script can do to a control: click it, dispatch a click, dispatch the key that activates it. */
const scriptPresses = (page, selector) =>
  page.evaluate((sel) => {
    const node = document.querySelector(sel);
    if (node === null) throw new Error(`no ${sel}`);
    node.click();
    node.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    node.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
    node.dispatchEvent(new KeyboardEvent("keyup", { key: "Enter", bubbles: true, cancelable: true }));
  }, selector);

/** Four controls decide, answer, undo and insert. A script's press makes no call, one real press makes one, a double click makes one. */
async function pressCase(page, url, guards) {
  // `Allow once` of an approval card.
  await chatBeside(page, url);
  await scriptTool(page, { id: "call_1", tool: "read_file", path: TOOL_PATH, result: TOOL_TEXT, answer: "Done." });
  await sendText(page, "read my notes");
  let card = await openCard(page, "call_1");
  await tickClock(page, 1000);
  await scriptPresses(page, "button.approval-allow");
  await page.waitForTimeout(100);
  assert.deepEqual(await decisionCalls(page), [], "Allow once: a script's press makes no call");
  assert.equal(await card.getAttribute("data-mode"), "ask", "the card still waits");
  const hash = await page.evaluate(() => window.__MOCK__.ai.chat("chat-mock-1").pending.find((p) => p.call_id === "call_1").hash);
  await card.locator("button.approval-allow").click();
  await finished(page);
  let made = await decisionCalls(page);
  assert.equal(made.length, 1, "Allow once: one real press makes one call");
  assertKeys("ai_decide", made[0]);
  assert.deepEqual(made[0], { conversation: "chat-mock-1", call_id: "call_1", decision: "allow", hash }, "the call repeats the pending entry's hash");
  await chatBeside(page, url);
  await scriptTool(page, { id: "call_1", tool: "read_file", path: TOOL_PATH, result: TOOL_TEXT, answer: "Done." });
  await sendText(page, "read again");
  card = await openCard(page, "call_1");
  await tickClock(page, 1000);
  await card.locator("button.approval-allow").dblclick();
  await finished(page);
  assert.equal((await decisionCalls(page)).length, 1, "Allow once: a double click makes one call");

  // `Send once` of the card of a `wsh ai` request.
  await boot(page, url, { clock: true });
  await emitAsk(page, ASK);
  await askCard(page).waitFor();
  await tickClock(page, 700);
  await scriptPresses(page, "section.ask .ask-yes");
  await page.waitForTimeout(100);
  assert.deepEqual(await askCalls(page), [], "Send once: a script's press makes no call");
  assert.equal(await askCard(page).count(), 1, "the card stays");
  await askCard(page).locator(".ask-yes").click();
  await page.waitForFunction(() => document.querySelector("section.ask") === null);
  made = await askCalls(page);
  assert.equal(made.length, 1, "Send once: one real press makes one call");
  assertKeys("ai_ask_answer", made[0]);
  assert.deepEqual(made[0], { ask: ASK.ask, verdict: "send", seal: ASK.seal });
  await emitAsk(page, { ...ASK, ask: "00000000000000000000000000000002" });
  await askCard(page).waitFor();
  await tickClock(page, 1000);
  await askCard(page).locator(".ask-yes").dblclick();
  await page.waitForFunction(() => document.querySelector("section.ask") === null);
  assert.equal((await askCalls(page)).length, 2, "Send once: a double click makes one more call");

  // `Undo` of a write that ran.
  const finishedWrite = async () => {
    await chatBeside(page, url);
    await seedFile(page, TOOL_FILE, FILE_OLD);
    await scriptWrite(page, { id: "call_1", tool: "write_file", path: TOOL_FILE, content: FILE_NEW, answer: "Done." });
    await sendText(page, "update my todo list");
    const open = await openCard(page, "call_1");
    await tickClock(page, 1000);
    await open.locator("button.approval-allow").click();
    await finished(page);
    const row = page.locator('div.chat-tool[data-call="call_1"]');
    await row.locator("button.tool-undo").waitFor();
    return row;
  };
  let row = await finishedWrite();
  await scriptPresses(page, ".tool-undo");
  await page.waitForTimeout(100);
  assert.deepEqual(await undoCalls(page), [], "Undo: a script's press makes no call");
  await row.locator("button.tool-undo").click();
  await page.waitForFunction(() => document.querySelector(".chat-tool[data-call='call_1'] .tool-undone") !== null);
  made = await undoCalls(page);
  assert.equal(made.length, 1, "Undo: one real press makes one call");
  assertKeys("ai_undo", made[0]);
  assert.deepEqual(made[0], { conversation: "chat-mock-1", call_id: "call_1" });
  row = await finishedWrite();
  await row.locator("button.tool-undo").dblclick();
  await page.waitForFunction(() => document.querySelector(".chat-tool[data-call='call_1'] .tool-undone") !== null);
  assert.equal((await undoCalls(page)).length, 1, "Undo: a double click makes one call");

  // `Insert at prompt` under a clean one-line command of a reply.
  const COMMAND = "find . -type f -size +1G";
  await boot(page, url, { view: VIEW_P1, profiles: [PROFILE] });
  await page.evaluate(() => window.__MOCK__.on("paste", () => "written"));
  await settle(page);
  await openAi(page);
  await scriptReply(page, { text: "x", html: `<pre><code>${COMMAND}</code></pre>` });
  await page.locator("button.chat-insert").waitFor();
  await scriptPresses(page, "button.chat-insert");
  await page.waitForTimeout(100);
  assert.deepEqual(await pastesOf(page), [], "Insert at prompt: a script's press makes no call");
  await page.locator("button.chat-insert").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("paste").length === 1);
  await page.waitForTimeout(100);
  made = await pastesOf(page);
  assert.equal(made.length, 1, "Insert at prompt: one real press makes one call");
  assertKeys("paste", made[0]);
  assert.deepEqual(made[0], { pane: 1, text: COMMAND, allow_unsafe: false });
  assert.ok(!/[\r\n]/.test(made[0].text), "no line break");
  assert.ok(!(await commandNames(page)).some((n) => ["key", "text", "ai_send_key"].includes(n)), "no key was sent");
  await page.locator("button.chat-insert").dblclick();
  await page.waitForTimeout(300);
  assert.equal((await pastesOf(page)).length, 2, "Insert at prompt: a double click makes one call");
  await guards.assertQuiet("press");
}

// ---- a key goes out once and never comes back ---------------------------------------------------------------------------

const KEY = "zq-gate-key-5d83a1c7e940";

/**
 * A key typed into the providers dialog: it is in exactly one call, `ai_key_set`, which has the documented keys; the field is empty
 * again; no input value, attribute, text node, store or console message holds it; a reloaded page says only `key saved`.
 */
async function keyCase(page, url, guards) {
  const consoleText = [];
  page.on("console", (m) => consoleText.push(m.text()));
  await boot(page, url, { profiles: [] });
  await openAi(page);
  await page.waitForFunction(() => document.querySelector(".chat-setup")?.textContent === "Set up…");
  await page.locator(".chat-setup").click();
  await openDialog(page);
  await fillField(page, "#aiset-name", "Claude");
  await fillField(page, "#aiset-model", "claude-x");
  await fillField(page, "#aiset-key", KEY);
  await page.locator(".aiset-save").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_key_set").length === 1);
  await page.waitForFunction(() => document.querySelector(".aiset-status")?.textContent === "Saved.");

  const all = await page.evaluate(() => window.__MOCK__.calls.map((c) => [c[0], structuredClone(c[1])]));
  const holders = all.filter(([, args]) => JSON.stringify(args ?? null).includes(KEY)).map(([command]) => command);
  assert.deepEqual(holders, ["ai_key_set"], "the key is in exactly one call");
  const sets = all.filter(([command]) => command === "ai_key_set");
  assert.equal(sets.length, 1);
  assertKeys("ai_key_set", sets[0][1]);
  assert.equal(sets[0][1].api_key, KEY);
  assert.ok(!JSON.stringify(all.filter(([command]) => command === "ai_profile_save")).includes(KEY), "the profile call has no key");
  assert.equal(await page.locator("#aiset-key").inputValue(), "", "the field is empty again");
  assert.ok(!(await everything(page)).includes(KEY), "the key is in no input, attribute, text node or store");
  assert.ok(!consoleText.some((t) => t.includes(KEY)), "the key is in no console message");
  assert.equal(await page.locator("label.aiset-row .aiset-key").textContent(), "key saved");

  // Every command of the case is one the golden knows, and none of them reads a key.
  const names = [...new Set(all.map(([command]) => command))];
  for (const name of names) {
    if (name === "markdown_render") continue;
    assert.ok(Object.hasOwn(GOLDEN.commands, name), `${name} is a command of the golden`);
    assert.ok(!/(get|read|show|reveal|export)/i.test(name) || !/key/i.test(name), `${name} reads a key`);
  }

  // A reloaded page: the core keeps the profile and the key, the page shows the words and nothing else.
  const kept = await page.evaluate(() => ({ profiles: window.__MOCK__.ai.profiles, keys: window.__MOCK__.ai.keys }));
  await page.addInitScript((saved) => {
    window.__MOCK__.ai.profiles = saved.profiles;
    window.__MOCK__.ai.keys = saved.keys;
  }, kept);
  await page.reload();
  await page.waitForSelector(".divider");
  await openAi(page);
  await page.locator(".chat-setup").click();
  await openDialog(page);
  await page.waitForSelector("label.aiset-row");
  assert.equal(await page.locator("label.aiset-row .aiset-key").textContent(), "key saved", "a reloaded page says only that a key is saved");
  assert.equal(await page.locator("#aiset-key").inputValue(), "");
  assert.ok(!(await everything(page)).includes(KEY), "the key is nowhere in the reloaded page");
  assert.ok(!JSON.stringify(await page.evaluate(() => window.__MOCK__.calls)).includes(KEY), "no call of the reloaded page holds the key");
  assert.deepEqual(await pageClean(page), []);
  await guards.assertQuiet("key");
}
