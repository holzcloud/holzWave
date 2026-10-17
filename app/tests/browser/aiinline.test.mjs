// The palette actions of the chat (docs/design.md section 6.19): `Explain error` opens the chat where the person is, writes the
// message into its composer and opens the context card with one item chosen; nothing is sent, shared or read until the person
// presses Send. The ids and the chords agree with the core's table.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  VIEW_P1, boot, calls, commandNames, ctxCalls, draftOf, emitFrame, previewKinds, runAction, setPrivacy, settle,
} from "./ai-lib.mjs";

const GOLDEN = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));

export const EXPLAIN =
  "Explain the error in the attached terminal output: what went wrong, the most likely cause, and the command or change that fixes it.";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await traceCase(page, url);
  await quietCase(page, url);
  await kindsCase(page, url);
  await contractCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A page with two terminals and the focus on pane 1, and the local provider read without a share. */
async function ready(page, url, { auto = true, panes } = {}) {
  await boot(page, url, { view: VIEW_P1 });
  if (auto) await setPrivacy(page, { localContext: "auto" });
  if (panes !== undefined) await page.evaluate((table) => Object.assign(window.__MOCK__.ai.context.panes, table), panes);
  await settle(page);
}

const aiCalls = async (page) => (await commandNames(page)).filter((name) => name.startsWith("ai_"));

/** The rows of the palette for `query`: title, section word and chord. */
async function paletteRows(page, query) {
  await page.keyboard.press("Meta+k");
  await page.waitForFunction(() => document.activeElement?.classList.contains("palette-input") === true);
  await page.keyboard.type(query);
  const rows = await page.evaluate(() =>
    [...document.querySelectorAll(".palette-row")].map((r) => ({
      title: r.querySelector(".palette-title").textContent,
      section: r.querySelector(".palette-section").textContent,
      chord: r.querySelector(".palette-chord").textContent,
    })),
  );
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => document.activeElement?.id === "ime");
  return rows;
}

async function traceCase(page, url) {
  await ready(page, url);
  assert.deepEqual(await aiCalls(page), [], "no chat command before the action runs");

  assert.deepEqual(await paletteRows(page, "explain"), [{ title: "Explain error", section: "ai", chord: "Cmd+Shift+Y" }]);
  assert.deepEqual(await aiCalls(page), [], "listing the actions asks nothing");

  await page.keyboard.press("Meta+Shift+y");
  await page.waitForSelector('.chat-cards section.chat-consent[data-mode="offer"]');
  assert.equal((await calls(page, "ai_conversation_new")).length, 1);
  assert.deepEqual(await calls(page, "block_open"), [{ kind: "chat", source: "local", path: "chat-mock-1", placement: "right" }]);
  assert.equal(await page.locator(".block-chat").count(), 1);
  assert.equal(await page.evaluate(() => document.activeElement?.matches("textarea.chat-input")), true, "the composer has the keyboard");
  const input = page.locator("textarea.chat-input");
  assert.equal(await input.inputValue(), EXPLAIN);
  assert.equal(await input.evaluate((node) => node.selectionStart === node.value.length && node.selectionEnd === node.value.length), true);
  const tokens = Math.ceil(new TextEncoder().encode(EXPLAIN).length / 3);
  assert.equal(await page.locator(".chat-count").textContent(), `≈ ${tokens} tokens`);
  assert.equal(await page.locator('button.chat-send[data-mode="send"]').isEnabled(), true);

  const items = page.locator("li.consent-item");
  assert.equal(await items.count(), 1, "one item only");
  assert.equal(await items.first().getAttribute("data-kind"), "last_command");
  assert.equal(await page.locator("li.consent-item input:checked").count(), 1, "and it is ticked");

  const ctx = await ctxCalls(page);
  assert.deepEqual(ctx.map(([name]) => name), ["ai_context_state", "ai_context_preview"], "one state call and one preview call, no share");
  assert.equal(ctx[0][1].pane, 1);
  assert.equal(ctx[1][1].pane, 1);
  assert.deepEqual(ctx[1][1].kinds, ["last_command"]);
  const names = await commandNames(page);
  for (const never of ["ai_send", "ai_context_share", "selection_text", "paste", "key", "text"]) {
    assert.equal(names.includes(never), false, `${never} was called by the action`);
  }
  const status = page.locator(".chat-status");
  assert.equal(await status.textContent(), "Nothing is sent until you press Send.");
  assert.equal(await status.getAttribute("data-kind"), "note");

  await page.locator("textarea.chat-input").press("Enter");
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_send").length === 1);
  const sent = (await calls(page, "ai_send"))[0];
  assert.deepEqual(Object.keys(sent), ["conversation", "text", "pane", "profile", "model", "offer", "picked"]);
  assert.equal(sent.conversation, "chat-mock-1");
  assert.equal(sent.text, EXPLAIN);
  assert.equal(sent.pane, 1);
  assert.equal(sent.profile, "local-1");
  assert.deepEqual(sent.picked, [1]);
  await page.waitForFunction(
    () => document.querySelectorAll('.chat-msg[data-role="assistant"][data-status="complete"]').length === 1 && !document.querySelector(".chat-partial"),
  );
}

async function quietCase(page, url) {
  await ready(page, url, { auto: false });
  const ids = await page.evaluate(() => window.__registry.listActions().map((a) => [a.id, a.title]));
  assert.ok(ids.some(([id, title]) => id === "ai.explain_error" && title === "Explain error"));
  await page.evaluate(() => window.__MOCK__.emit("layout", window.__MOCK__.view()));
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await paletteRows(page, "ai");
  await page.waitForTimeout(100);
  assert.deepEqual(await aiCalls(page), [], "the actions exist and ask the core nothing");
  const names = await commandNames(page);
  assert.equal(names.includes("paste") || names.includes("key"), false);

  // A refusal that is expected makes no chat call either: a page with no terminal pane in focus.
  await page.evaluate(() => {
    window.__MOCK__.setView({ ...window.__MOCK__.view(), panes: [], focus: 0 });
    window.__MOCK__.emit("layout", window.__MOCK__.view());
  });
  await runAction(page, "ai.explain_error");
  await page.waitForTimeout(100);
  assert.deepEqual(await aiCalls(page), []);
}

async function kindsCase(page, url) {
  const offered = (kinds) => ({ 1: { kinds, items: { selection: { label: "Selection of pane 1", bytes: 12, findings: [], text: "x" }, screen: { label: "Screen of pane 1", bytes: 2457, findings: [], text: "s" }, last_command: { label: "Last command output of pane 1", bytes: 918, findings: [], text: "l" } } } });

  // (a) the last command and the screen are listed: the last command
  await ready(page, url, { panes: offered(["last_command", "screen"]) });
  await runAction(page, "ai.explain_error");
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  assert.deepEqual(await previewKinds(page), [["last_command"]]);

  // (b) no last command, no selection shown: the screen
  await ready(page, url, { panes: offered(["selection", "screen"]) });
  await runAction(page, "ai.explain_error");
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  assert.deepEqual(await previewKinds(page), [["screen"]]);

  // (c) a selection is shown in the pane: the selection, and the action reads no text
  await ready(page, url, { panes: offered(["selection", "screen", "last_command"]) });
  await emitFrame(page, 1, { selection: [[0, 0, 5]] });
  await page.waitForTimeout(50);
  const before = (await calls(page, "selection_text")).length;
  await runAction(page, "ai.explain_error");
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  assert.deepEqual(await previewKinds(page), [["selection"]]);
  assert.equal((await calls(page, "selection_text")).length, before, "only the presence of the spans was used");

  // (d) the default privacy asks first: no share, no preview, until the person's own click
  await ready(page, url, { auto: false });
  await runAction(page, "ai.explain_error");
  await page.waitForSelector('.chat-consent[data-mode="ask"]');
  assert.equal(await page.locator(".consent-share").textContent(), "Share pane 1");
  assert.equal((await calls(page, "ai_context_share")).length, 0);
  assert.deepEqual(await previewKinds(page), []);
  await page.locator(".consent-share").click();
  await page.waitForSelector('.chat-consent[data-mode="offer"]');
  assert.equal((await calls(page, "ai_context_share")).length, 1);
  assert.deepEqual(await previewKinds(page), [["last_command"]], "the first listed, ticked without a tick by the person");
  assert.equal((await calls(page, "ai_send")).length, 0);

  // (e) a pane that lists nothing: the card says so, the text stays, the status line is the same
  await ready(page, url, { panes: { 1: { kinds: [], items: {} } } });
  await runAction(page, "ai.explain_error");
  await page.waitForSelector('.chat-consent[data-mode="blocked"]');
  assert.deepEqual(await previewKinds(page), []);
  assert.equal(await draftOf(page), EXPLAIN);
  await page.waitForFunction(() => document.querySelector(".chat-status")?.textContent === "Nothing is sent until you press Send.");
}

/** The key rows of the golden: `[{ action, chord, is_default, title }]`, the table the core sends. */
function goldenRows() {
  const lists = [];
  (function walk(value) {
    if (Array.isArray(value)) {
      if (value.length > 0 && value.every((row) => row && typeof row === "object" && "action" in row && "chord" in row)) lists.push(value);
      value.forEach(walk);
    } else if (value && typeof value === "object") {
      Object.values(value).forEach(walk);
    }
  })(GOLDEN);
  return lists[0];
}

async function contractCase(page, url) {
  const rows = goldenRows();
  const wanted = ["ai.explain_error"].map((id) => rows.find((row) => row.action === id));
  assert.ok(wanted.every((row) => row !== undefined), "the golden has the rows");
  await boot(page, url, { view: VIEW_P1 });

  const registry = await page.evaluate(() => window.__registry.listActions().map((a) => [a.id, a.title]));
  for (const row of wanted) {
    assert.deepEqual(registry.find(([id]) => id === row.action), [row.action, row.title], `${row.action} is registered with the core's title`);
  }

  // The page's own table and the core's rows give one chord for each id, and each chord leads back to its id.
  const probe = (rowsToInstall) =>
    page.evaluate(async ([table, ids, chords]) => {
      const chordsModule = await import(new URL("chords.js", location.href).href);
      if (table !== null) chordsModule.setBindingTable(table);
      const out = { chord: ids.map((id) => chordsModule.chordFor(id)), id: chords.map((c) => chordsModule.actionIdFor(c)) };
      chordsModule.setBindingTable([]);
      return out;
    }, [rowsToInstall, wanted.map((row) => row.action), wanted.map((row) => row.chord)]);
  for (const table of [null, rows]) {
    const found = await probe(table);
    assert.deepEqual(found.chord, wanted.map((row) => row.chord), table === null ? "the page's table" : "the core's rows");
    assert.deepEqual(found.id, wanted.map((row) => row.action));
  }

  // With no pane in focus the action refuses and does not throw.
  await page.evaluate(() => {
    window.__MOCK__.setView({ ...window.__MOCK__.view(), panes: [], focus: 0 });
    window.__MOCK__.emit("layout", window.__MOCK__.view());
  });
  await runAction(page, "ai.explain_error");
  await page.waitForFunction(() => document.querySelector("#status .seg.notice")?.textContent === "Focus a terminal pane to explain its error.");
}
