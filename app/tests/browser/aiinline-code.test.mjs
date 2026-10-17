// The reply action of the chat (docs/design.md section 6.19): a one-line code block of a finished reply gets `Insert at prompt`,
// and only a real click puts that line at the prompt of the pane the chat acts for, once, through the paste command with the
// unsafe flag off, with no line break and no key press. Hostile blocks get no button.
import assert from "node:assert/strict";
import {
  VIEW3, VIEW_P1, boot, calls, chatBlock, codeBars, commandNames, emitFrame, openAi, openSide, pastesOf, scriptReply, sendSide, sendText, settle,
} from "./ai-lib.mjs";

const CLEAN = "find . -type f -size +1G -exec ls -lh {} +";
const REPLY = `<p>Use this:</p><pre><code class="language-sh">${CLEAN}</code></pre><p>It lists the files.</p>`;
const STATUS = ".chat-status";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await insertCase(page, url);
  await refusedCase(page, url);
  await hostileCase(page, url);
  await streamCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A page with two terminals, the focus on pane 1 and a chat pane beside it; `paste` answers `answer`. */
async function chatPage(page, url, answer = "written") {
  await boot(page, url, { view: VIEW_P1 });
  await page.evaluate((a) => window.__MOCK__.on("paste", () => a), answer);
  await settle(page);
  await openAi(page);
}

const statusOf = async (page, scope = "") => ({
  text: await page.locator(`${scope} ${STATUS}`.trim()).first().textContent(),
  kind: await page.locator(`${scope} ${STATUS}`.trim()).first().getAttribute("data-kind"),
});

/** The pane the chat acts for becomes `pane`: the view's focus moves and the page sees two layouts. */
async function focusPane(page, pane) {
  await page.evaluate((p) => {
    const v = window.__MOCK__.view();
    v.focus = p;
    v.tabs[0].focus = p;
    window.__MOCK__.setView(v);
  }, pane);
  await settle(page);
}

async function insertCase(page, url) {
  await chatPage(page, url);
  await page.evaluate(() => (document.getElementById("ime").blur(), 0));
  await scriptReply(page, { text: "Use this: ...", html: REPLY, task: "run ```ls``` for me" });

  const bars = await codeBars(page);
  assert.deepEqual(bars, [{ kind: "bar", text: "Insert at prompt", before: "pre" }], "one bar directly after the block");
  const button = page.locator(".chat-msg[data-role=\"assistant\"] button.chat-insert");
  assert.equal(await button.count(), 1);
  assert.equal(await button.getAttribute("type"), "button");
  assert.equal(await button.evaluate((n) => n.hasAttribute("data-field")), true);
  const attributes = await page.evaluate(() =>
    [...document.querySelectorAll(".chat-codebar, .chat-codebar *")].flatMap((n) => [...n.attributes].map((a) => a.value)));
  assert.equal(attributes.some((value) => value.includes("find") || value.includes("-size")), false, "no command text in any attribute");
  assert.equal(await page.locator('.chat-msg[data-role="user"] .chat-codebar').count(), 0, "a user message has no bar");

  await button.click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("paste").length === 1);
  const pastes = await pastesOf(page);
  assert.deepEqual(pastes, [{ pane: 1, text: CLEAN, allow_unsafe: false }]);
  assert.deepEqual(Object.keys(pastes[0]), ["pane", "text", "allow_unsafe"]);
  assert.ok(!/[\r\n]/.test(pastes[0].text), "no line break");
  await page.waitForFunction(() => document.querySelector(".chat-status")?.textContent?.startsWith("Inserted"));
  assert.deepEqual(await statusOf(page), { text: "Inserted at the prompt of pane 1. It was not run.", kind: "note" });
  assert.deepEqual(await calls(page, "focus_pane"), [{ pane: 1 }]);
  assert.equal(await page.evaluate(() => document.activeElement?.id), "ime", "the terminal has the keyboard");
  const names = await commandNames(page);
  for (const never of ["key", "text", "ai_send_key"]) assert.equal(names.includes(never), false, `${never} was called`);
  assert.equal((await pastesOf(page)).length, 1, "exactly one paste");

  // The pane is taken at the click: after the focus moved to terminal pane 2 the next click goes there.
  await focusPane(page, 2);
  await button.click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("paste").length === 2);
  assert.deepEqual((await pastesOf(page))[1], { pane: 2, text: CLEAN, allow_unsafe: false });

  // The keyboard presses it too (a real key event on the focused button), once each.
  for (const [key, count] of [["Enter", 3], ["Space", 4]]) {
    await button.focus();
    await page.keyboard.press(key);
    await page.waitForFunction((n) => window.__MOCK__.callsOf("paste").length === n, count);
  }
  assert.equal((await commandNames(page)).includes("key"), false, "no key press was sent to a pane");

  // The same reply in the column.
  await chatPage(page, url);
  await focusPane(page, 1);
  await openSide(page);
  await scriptReply(page, { text: "Use this: ...", html: REPLY, send: sendSide });
  await page.locator("#sidebar button.chat-insert").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("paste").length === 1);
  assert.deepEqual(await pastesOf(page), [{ pane: 1, text: CLEAN, allow_unsafe: false }]);
}

async function refusedCase(page, url) {
  // The terminal refuses, has nothing to take, fails, or answers something unknown: said in words, never repeated.
  for (const answer of ["rejected", "empty", "done"]) {
    await chatPage(page, url, answer);
    await scriptReply(page, { text: "x", html: REPLY });
    await page.locator("button.chat-insert").click();
    await page.waitForFunction(() => document.querySelector(".chat-status")?.dataset.kind === "error");
    assert.equal((await pastesOf(page)).length, 1, `one paste for ${answer}`);
    assert.equal((await pastesOf(page))[0].allow_unsafe, false);
    assert.equal((await calls(page, "focus_pane")).length, 0, `no focus move for ${answer}`);
    assert.deepEqual(await statusOf(page), { text: "The terminal did not take the text, so nothing was inserted.", kind: "error" });
  }
  await chatPage(page, url);
  await page.evaluate(() => window.__MOCK__.on("paste", () => {
    throw "boom";
  }));
  await scriptReply(page, { text: "x", html: REPLY });
  await page.locator("button.chat-insert").click();
  await page.waitForFunction(() => document.querySelector(".chat-status")?.dataset.kind === "error");
  assert.equal((await pastesOf(page)).length, 1);

  // A program on the alternate screen is not at a prompt.
  await chatPage(page, url);
  await scriptReply(page, { text: "x", html: REPLY });
  await emitFrame(page, 1, { modes: { alt: true, bracketed_paste: false, mouse: false } });
  await page.locator("button.chat-insert").click();
  await page.waitForFunction(() => document.querySelector(".chat-status")?.textContent?.startsWith("That pane"));
  assert.deepEqual(await statusOf(page), { text: "That pane shows a full-screen program, so nothing was inserted.", kind: "error" });
  assert.equal((await pastesOf(page)).length, 0);

  // The acting pane is a preview, or is gone from the view.
  const PREVIEW = { pane: 3, kind: "preview", source: "local", path: "/work/notes.md", source_label: "This computer" };
  await boot(page, url, { view: VIEW3, blocks: [PREVIEW] });
  await page.evaluate(() => window.__MOCK__.on("paste", () => "written"));
  await settle(page);
  await focusPane(page, 3);
  await openSide(page);
  await scriptReply(page, { text: "x", html: REPLY, send: sendSide });
  await page.locator("#sidebar button.chat-insert").click();
  await page.waitForFunction(() => document.querySelector("#sidebar .chat-status")?.dataset.kind === "error");
  assert.deepEqual(await statusOf(page, "#sidebar"), { text: "There is no terminal pane to insert into.", kind: "error" });
  assert.equal((await pastesOf(page)).length, 0);

  await chatPage(page, url);
  await scriptReply(page, { text: "x", html: REPLY });
  await page.evaluate(() => {
    const v = window.__MOCK__.view();
    v.panes = v.panes.filter(([id]) => id === 3);
    v.focus = 3;
    v.tabs[0].focus = 3;
    v.tabs[0].panes = 1;
    v.dividers = [];
    window.__MOCK__.setView(v);
  });
  await settle(page);
  await page.locator("button.chat-insert").click();
  await page.waitForFunction(() => document.querySelector(".chat-status")?.dataset.kind === "error");
  assert.deepEqual(await statusOf(page), { text: "There is no terminal pane to insert into.", kind: "error" });
  assert.equal((await pastesOf(page)).length, 0);

  // An event that a script made is not a press: nothing is pasted and nothing is said.
  await chatPage(page, url);
  await scriptReply(page, { text: "x", html: REPLY });
  const before = await statusOf(page);
  await page.locator("button.chat-insert").evaluate((node) => node.dispatchEvent(new MouseEvent("click", { bubbles: true })));
  await page.waitForTimeout(100);
  assert.equal((await pastesOf(page)).length, 0);
  assert.deepEqual(await statusOf(page), before);
}

const HOSTILE = [
  CLEAN,
  "echo \u001b[31mred",
  "printf \u001b]52;c;Zm9v\u0007",
  "\u009b2J",
  "echo ‮evil",
  "ec​ho hi",
  "\u{e0041}x",
  "ls&#13;pwd",
  "a b",
  "a b",
  "a\tb",
  "ls\npwd",
  "z".repeat(1025),
  "",
  "rm\u007f",
];

async function hostileCase(page, url) {
  const requests = [];
  page.on("request", (r) => requests.push(r.url()));
  await chatPage(page, url);
  const html = HOSTILE.map((code) => `<pre><code>${code.replace(/&(?!#13;)/g, "&amp;").replace(/</g, "&lt;")}</code></pre>`).join("");
  await scriptReply(page, { text: "x", html });
  const marks = await page.evaluate(() =>
    [...document.querySelectorAll(".chat-md pre")].map((pre) => {
      const next = pre.nextElementSibling;
      if (next === null) return "";
      if (next.classList.contains("chat-codebar")) return `bar:${next.textContent}`;
      return next.classList.contains("chat-codenote") ? `note:${next.textContent}` : "";
    }));
  const note = "note:! Not insertable: it holds characters that are not shown.";
  assert.equal(marks.length, 15, "every block is drawn");
  assert.deepEqual(marks, [
    "bar:Insert at prompt", note, note, note, note, note, note, note, note, note, "", "", "", "", note,
  ]);
  assert.equal(await page.locator("button.chat-insert").count(), 1);
  assert.equal(await page.locator(".chat-codebar").count(), 1);

  await page.locator("button.chat-insert").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("paste").length === 1);
  assert.deepEqual(await pastesOf(page), [{ pane: 1, text: CLEAN, allow_unsafe: false }], "its own text, not a neighbour's");

  const found = await page.evaluate(() => {
    const log = document.querySelector(".chat-log");
    const texts = [...log.querySelectorAll("pre")].map((p) => p.textContent).filter((t) => t.length > 3 && t.length < 100);
    const attributes = [...log.querySelectorAll("*")].flatMap((n) => [...n.attributes].map((a) => a.value));
    return {
      xss: window.__XSS ?? null,
      media: log.querySelectorAll("img, script, iframe").length,
      leaked: attributes.filter((value) => texts.some((t) => value.includes(t))).length,
    };
  });
  assert.deepEqual(found, { xss: null, media: 0, leaked: 0 });
  assert.deepEqual(requests.filter((u) => !u.startsWith("http://127.0.0.1") && !u.startsWith("http://localhost") && !u.startsWith("data:") && !u.startsWith("blob:")), []);

  // At most sixteen buttons in one reply.
  const many = Array.from({ length: 20 }, (_, i) => `<pre><code>echo ${i}</code></pre>`).join("");
  await scriptReply(page, { text: "y", html: many, task: "more" });
  assert.equal(await page.locator('.chat-msg[data-role="assistant"]').last().locator("button.chat-insert").count(), 16);
  assert.equal(await page.locator("body").evaluate((n) => n.isConnected), true, "the page stayed responsive");
}

async function streamCase(page, url) {
  await chatPage(page, url);
  await page.evaluate(() => {
    window.__MOCK__.ai.script({ chunks: ["```\nls -la\n", "```"] });
    window.__MOCK__.on("markdown_render", () => "<pre><code>ls -la</code></pre>");
    window.__MOCK__.ai.hold(2);
  });
  await sendText(page, "list");
  await page.waitForFunction(() => document.querySelector(".chat-partial .chat-text")?.textContent?.startsWith("```"));
  assert.equal(await page.locator(".chat-partial .chat-text").evaluate((n) => n.children.length), 0, "plain text while it streams");
  assert.deepEqual(await codeBars(page), [], "no bar on a partial message");
  await page.evaluate(() => window.__MOCK__.ai.release());
  await page.waitForSelector(".chat-msg[data-role=\"assistant\"] .chat-codebar");
  assert.equal((await codeBars(page)).length, 1);

  // A redraw of the finished message (a window focus asks for a snapshot) leaves one bar per block.
  const snapshots = (await calls(page, "ai_turn_snapshot")).length;
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await page.waitForFunction((n) => window.__MOCK__.callsOf("ai_turn_snapshot").length > n, snapshots);
  await page.waitForTimeout(150);
  assert.equal((await codeBars(page)).length, 1, "one bar, not two");
  assert.equal(await page.locator(".chat-md pre").count(), 1);

  // A cancelled turn leaves a plain message without a bar.
  await page.evaluate(() => {
    window.__MOCK__.ai.script({ chunks: ["```\nrm -rf x\n", "```"] });
    window.__MOCK__.ai.hold(2);
  });
  await sendText(page, "again");
  await page.waitForFunction(() => document.querySelector(".chat-partial .chat-text")?.textContent?.startsWith("```"));
  await page.locator(".chat-send").click();
  await page.waitForFunction(() => document.querySelector('.chat-msg[data-status="cancelled"]'));
  assert.equal(await page.locator('.chat-msg[data-status="cancelled"] .chat-codebar').count(), 0);
  assert.equal((await codeBars(page)).length, 1, "only the finished reply has its bar");
}
