// Shared checker and drivers of the page gate (aigate-*.test.mjs): the corpus the core writes, the checker that finds
// what a hostile text could make of the page (an element, an attribute, a link or a global that the page does not own), the
// guards that watch for requests and windows, and the helper that proves the checker can fail.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { PROFILE, VIEW_CHAT_ONLY, boot, chatBlock } from "./ai-lib.mjs";
import { APP_DIR } from "./lib.mjs";

/** The corpus fixture the core's test writes (core/tests/ai_xss_golden.rs): cases, the allowed elements and the forbidden lists. */
export function loadCorpus() {
  const corpus = JSON.parse(readFileSync(join(APP_DIR, "tests/fixtures/ai_xss.json"), "utf8"));
  assert.equal(corpus.version, 1, "the corpus fixture has another version");
  assert.ok(Array.isArray(corpus.cases) && corpus.cases.length > 0, "the corpus has no cases");
  return corpus;
}

/**
 * What a node of the page may not be, as one line per problem. The checker runs in the page (it needs the page's own list of
 * elements) over every element below each node that matches `scope`.
 *
 * `options.mode`:
 *   `model` (default): text that came from a model and was copied by the page. An element must be on the page's list (or match
 *     one of `options.extra`, the elements the page adds on purpose), a class must be the page's own or a language class,
 *     and an input must be a disabled checkbox.
 *   `text`: a card, a list or a dialog that must show a hostile text as text. The page's own controls are fine, so an element
 *     or an attribute that a control needs is not flagged (`UI_ELEMENTS`, `UI_ATTRIBUTES`), but every attribute value that
 *     carries a piece of the attack is.
 * `options.maxDepth` (66) is the deepest nesting below a scope node. `options.inert` is HTML parsed into an inert document with
 * the page's own `DOMParser` and checked instead of the page (the checker's own test), and `options.safe` is HTML copied
 * through the page's `appendSafe` into a detached node and checked.
 */
export async function domProblems(page, scope, options = {}) {
  await ensureChecker(page);
  return page.evaluate((spec) => window.__gate.run(spec), checkSpec(scope, options));
}

/** The arguments of one run of the checker in the page. */
function checkSpec(scope, options) {
  return {
    scope,
    mode: options.mode ?? "model",
    extra: options.extra ?? [],
    maxDepth: options.maxDepth ?? 66,
    inert: options.inert ?? null,
    safe: options.safe ?? null,
    checkIn: options.checkIn ?? null,
    textOnlyBelow: options.textOnlyBelow ?? null,
    forbidden: loadCorpus().forbidden,
    uiElements: UI_ELEMENTS,
    uiAttributes: UI_ATTRIBUTES,
    textAttributes: TEXT_ATTRIBUTES,
  };
}

/**
 * Put the checker into the page once per document, as `window.__gate`: `check(roots, spec)` answers the lines for every element
 * below each root, and `run(spec)` picks the roots (the nodes that match `scope`, or an inert or a copied document).
 */
async function ensureChecker(page) {
  const there = await page.evaluate(() => window.__gate !== undefined);
  if (there) return;
  await page.evaluate(async () => {
    const { ELEMENTS, appendSafe } = await import(new URL("safehtml.js", location.href).href);
    const XHTML = "http://www.w3.org/1999/xhtml";
    const hostileValue = /__XSS|onerror|javascript:/i;
    const check = (roots, s) => {
      const lines = [];
      const text = s.mode === "text";
      const badElements = new Set(s.forbidden.elements.filter((n) => !text || !s.uiElements.includes(n)));
      const badAttributes = new Set(s.forbidden.attributes.filter((n) => !text || !s.uiAttributes.includes(n)));
      const pathOf = (el, root) => {
        const parts = [];
        for (let n = el; n !== null && n !== root; n = n.parentElement) {
          const cls = typeof n.className === "string" && n.className !== "" ? `.${n.className.split(/\s+/)[0]}` : "";
          parts.unshift(`${n.localName}${cls}`);
        }
        return parts.slice(-4).join(">");
      };
      for (const root of roots) {
        let deepest = 0;
        for (const el of root.querySelectorAll("*")) {
          const name = el.localName.toLowerCase();
          const where = pathOf(el, root);
          const add = (klass, detail = "") => lines.push(`${klass}: ${where}${detail === "" ? "" : ` ${detail}`}`);
          let depth = 0;
          for (let n = el; n !== null && n !== root; n = n.parentElement) depth += 1;
          deepest = Math.max(deepest, depth);
          const extra = s.extra.some((sel) => el.matches(sel));
          if (el.namespaceURI !== XHTML) add("foreign-namespace", el.namespaceURI ?? "");
          if (badElements.has(name)) add("forbidden-element");
          else if (!text && !extra && !ELEMENTS.has(name)) add("unlisted-element");
          for (const attribute of el.attributes) {
            const n = attribute.name.toLowerCase();
            if (badAttributes.has(n)) add("forbidden-attribute", n);
            if (s.forbidden.attribute_prefixes.some((p) => n.startsWith(p))) add("handler-attribute", n);
            if (n.startsWith("xlink:") || n.startsWith("xmlns")) add("namespace-attribute", n);
            if (n === "href" && name !== "a") add("href-on-non-anchor", name);
            if (text && !s.textAttributes.includes(n) && hostileValue.test(attribute.value)) add("attribute-value", `${n}=${attribute.value.slice(0, 40)}`);
          }
          if (name === "a") {
            const href = el.getAttribute("href");
            if (href !== null && !/^(https?:\/\/|mailto:)\S*$/i.test(href)) add("bad-link", href.slice(0, 60));
            if (href !== null && !(el.getAttribute("rel") ?? "").split(/\s+/).includes("noopener")) add("link-without-rel");
          }
          if (!text && !extra) {
            for (const cls of el.classList) {
              if (!/^chat-[a-z-]+$/.test(cls) && !/^language-[A-Za-z0-9_+#.-]{1,32}$/.test(cls)) add("bad-class", cls.slice(0, 40));
            }
            if (name === "input" && !(el.getAttribute("type") === "checkbox" && el.hasAttribute("disabled"))) add("bad-input");
          }
        }
        if (deepest > s.maxDepth) lines.push(`too-deep: nesting of ${deepest} below ${s.scope}, the most is ${s.maxDepth}`);
      }
      if (s.textOnlyBelow !== null) {
        for (const holder of document.querySelectorAll(s.textOnlyBelow)) {
          if (holder.children.length > 0) lines.push(`element-below-text: ${holder.children[0].localName} below ${s.textOnlyBelow}`);
        }
      }
      return lines;
    };
    const run = (s) => {
      let roots;
      if (s.inert !== null) roots = [new DOMParser().parseFromString(`<!doctype html><body>${s.inert}`, "text/html").body];
      else if (s.safe !== null) {
        const node = document.createElement("div");
        appendSafe(s.safe, node);
        roots = [node];
      } else roots = [...document.querySelectorAll(s.scope)];
      const lines = roots.length === 0 ? [`empty-scope: nothing matches ${s.scope}`] : check(roots, s);
      return lines.length > 40 ? [...lines.slice(0, 40), `... and ${lines.length - 40} more`] : lines;
    };
    window.__gate = { check, run };
  });
}

/**
 * Watch the node that matches `scope`: after every change below it (a child, an attribute, a text) the checker runs over the
 * elements below every node that matches `options.checkIn` (the scope itself by default), and `options.textOnlyBelow` (a
 * selector) adds a line for every element found below a node that matches it. The lines are kept in `window.__WATCH`; read
 * them with `readWatch`. A change is seen whatever happens next, so a state that lasts a moment is not missed.
 */
export async function watchDom(page, scope, options = {}) {
  await ensureChecker(page);
  const spec = checkSpec(scope, options);
  await page.evaluate(
    ([sel, s]) => {
      const root = document.querySelector(sel);
      if (root === null) throw new Error(`watchDom: nothing matches ${sel}`);
      window.__WATCH = [];
      window.__WATCH_OBSERVER?.disconnect();
      const look = () => {
        const roots = s.checkIn === null ? [root] : [...root.querySelectorAll(s.checkIn)];
        for (const line of window.__gate.check(roots, s)) if (window.__WATCH.length < 200) window.__WATCH.push(line);
      };
      window.__WATCH_OBSERVER = new MutationObserver(look);
      window.__WATCH_OBSERVER.observe(root, { subtree: true, childList: true, attributes: true, characterData: true });
    },
    [scope, spec],
  );
}

/** The lines the watch found so far (the observer is flushed first). */
export const readWatch = (page) =>
  page.evaluate(() => {
    window.__WATCH_OBSERVER?.takeRecords();
    return [...(window.__WATCH ?? [])];
  });

/**
 * Elements and attributes that a control of the page needs and that a text-mode check lets through: an input, a select, a form
 * or a dialog is not an attack by itself (an element that loads or runs something still is), and the focus, the id and the
 * name of a control are its own. The value check still reads every one of them.
 */
export const UI_ELEMENTS = ["textarea", "select", "form", "dialog"];
export const UI_ATTRIBUTES = ["id", "name", "tabindex", "autofocus", "accesskey", "style"];

/**
 * The two attributes that carry a person-readable name of a control and nothing else: a tooltip (`title`) and an accessible name
 * (`aria-label`). The page sets them with `setAttribute`, so the text in them is never parsed, and the header of a pane and the
 * block of a chat repeat the title of the chat there on purpose (the tooltip shows what the header cuts, the name is what a
 * screen reader reads). Every other attribute must not carry a string of the attack, and an attribute breaker would still show
 * as a new attribute (a handler, `autofocus`), which is checked on its own.
 */
export const TEXT_ATTRIBUTES = ["title", "aria-label"];

/** The names of the problem classes `domProblems` reports (the first word of a line). */
export const PROBLEM_CLASSES = [
  "foreign-namespace", "forbidden-element", "unlisted-element", "forbidden-attribute", "handler-attribute", "namespace-attribute",
  "href-on-non-anchor", "bad-link", "link-without-rel", "bad-class", "bad-input", "too-deep", "attribute-value", "empty-scope",
];

/** What is wrong with the page itself, as lines: a global the hostile text sets, a polluted prototype, a clobbered document. */
export async function pageClean(page) {
  return page.evaluate(() => {
    const lines = [];
    if (window.__XSS !== undefined) lines.push("window.__XSS is set: something ran");
    if (Object.prototype.polluted !== undefined || ({}).polluted !== undefined) lines.push("the prototype of Object has a member `polluted`");
    if (!(document.body instanceof HTMLBodyElement)) lines.push("document.body is not the body element");
    if (typeof document.getElementById !== "function") lines.push("document.getElementById was replaced");
    if (typeof document.querySelector !== "function") lines.push("document.querySelector was replaced");
    if (typeof window.fetch !== "function" || typeof window.open !== "function") lines.push("a global function was replaced");
    return lines;
  });
}

/**
 * Watch the requests and windows of a page: every request to another origin than `url` and every popup. `assertQuiet(what)`
 * throws, naming `what`, when one occurred or when `pageClean` finds a line.
 */
export function armGuards(page, url) {
  const origin = new URL(url).origin;
  const foreign = [];
  const popups = [];
  page.on("request", (request) => {
    const address = request.url();
    if (address.startsWith("about:")) return;
    let from = "";
    try {
      from = new URL(address).origin;
    } catch {
      from = address;
    }
    if (from !== origin) foreign.push(address.slice(0, 120));
  });
  page.on("popup", (popup) => popups.push(popup.url().slice(0, 120)));
  return {
    foreign: () => [...foreign],
    popups: () => [...popups],
    async assertQuiet(what) {
      const lines = await pageClean(page);
      if (foreign.length > 0 || popups.length > 0 || lines.length > 0) {
        throw new Error(`${what}: foreign requests ${JSON.stringify(foreign)}, popups ${JSON.stringify(popups)}, page problems ${JSON.stringify(lines)}`);
      }
    },
  };
}

/** Throw when the page does not answer a trivial call within `ms`. */
export async function responsive(page, ms = 5000) {
  let timer;
  const late = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`the page did not answer within ${ms} ms`)), ms);
  });
  try {
    await Promise.race([page.evaluate(() => 1 + 1), late]);
  } finally {
    clearTimeout(timer);
  }
}

/**
 * The checker's own proof: an inert hostile document gets one line for every problem class, and a document the page's own
 * copy made gets none. A checker that finds nothing would make every other row pass for the wrong reason.
 */
export async function checkerCase(page, errors = []) {
  const before = errors.length;
  const hostile =
    '<p>ok</p><script>window.__XSS = 1</script><img src="https://evil.example/x.png" onerror="window.__XSS = 1">' +
    '<iframe src="https://evil.example/f"></iframe><svg onload="window.__XSS = 1"><a xlink:href="https://evil.example/">s</a></svg>' +
    '<form action="https://evil.example/p"><input type="image" src="https://evil.example/i.png"></form><style>p { color: red }</style>' +
    '<math><mi>x</mi></math><a href="javascript:window.__XSS = 1">j</a><a href="https://example.org/">no rel</a>' +
    '<div style="position:fixed" onclick="window.__XSS = 1" class="x">d</div><section>unlisted</section><p id="clobber">c</p>' +
    '<link href="https://evil.example/s.css" rel="stylesheet"><input type="text">' +
    `${"<div>".repeat(80)}deep${"</div>".repeat(80)}`;
  const found = await domProblems(page, "", { inert: hostile });
  const klass = (line) => line.split(":")[0];
  const seen = new Set(found.map(klass));
  for (const want of [
    "forbidden-element", "foreign-namespace", "unlisted-element", "forbidden-attribute", "handler-attribute", "namespace-attribute",
    "href-on-non-anchor", "bad-link", "link-without-rel", "bad-class", "bad-input", "too-deep",
  ]) {
    assert.ok(seen.has(want), `the checker did not report ${want} in the hostile document; it reported ${[...seen].join(", ")}`);
  }
  const named = (what) => found.filter((l) => l.includes(what)).length;
  assert.ok(named("script") > 0 && named("iframe") > 0 && named("form") > 0 && named("style") > 0, `the checker missed an element: ${found.join(" | ")}`);
  const textual = await domProblems(page, "", { inert: '<p title="javascript:alert(1)" data-x="__XSS">t</p><img src="x">', mode: "text" });
  assert.ok(textual.some((l) => l.startsWith("attribute-value")), `text mode did not read an attribute value: ${textual.join(" | ")}`);
  assert.ok(textual.some((l) => l.startsWith("forbidden-element")), "text mode did not flag an image element");
  const none = await domProblems(page, "", { safe: '<p>ok <a href="https://example.org/">x</a></p>' });
  assert.deepEqual(none, [], "a document the page's own copy made has problems");
  // Whatever a scope is, a scope that matches nothing is a line of its own: a surface that was not reached must not pass.
  const empty = await domProblems(page, "#no-such-node");
  assert.ok(empty.some((l) => l.startsWith("empty-scope")), "an empty scope was not reported");
  await dropCheckerReports(page, errors, before);
}

/**
 * Take the policy reports that an inert hostile document causes out of `errors`, from index `from` on: the page's policy also
 * applies in the inert document that the checker (or `appendSafe`) parses hostile markup in, with its inline style and
 * handlers, and nothing of it reaches the page. Only the three directives of that case are taken, whatever file the engine
 * names as the source; any other report stays an error. The reports arrive a moment after the call that caused them.
 */
export async function dropCheckerReports(page, errors, from = 0) {
  await page.waitForTimeout(150);
  const expected = /^csp violation: (style-src-(elem|attr)|script-src-attr) blocked inline /;
  const kept = errors.slice(0, from).concat(errors.slice(from).filter((e) => !expected.test(e)));
  errors.splice(0, errors.length, ...kept);
}

/** Characters that hide or change what a text says, built from code points so that no file holds one raw. */
export const HIDDEN = {
  bidi: String.fromCodePoint(0x202e),
  zeroWidthSpace: String.fromCodePoint(0x200b),
  zeroWidthJoiner: String.fromCodePoint(0x200d),
  tag: String.fromCodePoint(0xe0041),
  nul: String.fromCodePoint(0),
  escape: String.fromCodePoint(0x1b),
  c1: String.fromCodePoint(0x9b),
  lineSeparator: String.fromCodePoint(0x2028),
  noBreakSpace: String.fromCodePoint(0xa0),
  byteOrderMark: String.fromCodePoint(0xfeff),
};

/** The code points of `text`, one string each: the chunks of a reply that streams one character at a time. */
export const chars = (text) => Array.from(text);

/**
 * Put one finished reply of the assistant per text into the mock's state of `chat` and make the page read that state, as a
 * focus does after a reload: it asks for a snapshot. Then wait until the page has drawn them all, which is when the number of
 * `markdown_render` calls has stood still for 100 ms and every message is on the page. `tag` makes the ids of the messages, so a
 * second set of the same chat is new to the page and is asked for again.
 */
export async function installMessages(page, chat, texts, tag = "m") {
  const asked = await page.evaluate(() => window.__MOCK__.callsOf("ai_turn_snapshot").length);
  await page.evaluate(
    ([id, list, prefix]) => {
      const ts = 1800000000;
      const messages = list.map((text, i) => ({
        id: `${prefix}-${i + 1}`, role: "assistant", ts, model: "llama3.2", status: "complete", usage: null,
        parts: [{ type: "text", text }],
      }));
      window.__MOCK__.ai.setState(id, { messages, title: "corpus" });
      window.dispatchEvent(new Event("focus"));
    },
    [chat, texts, tag],
  );
  await page.waitForFunction((n) => window.__MOCK__.callsOf("ai_turn_snapshot").length > n, asked);
  await page.waitForFunction(([prefix, count]) => document.querySelectorAll(`.chat-msg[data-id^="${prefix}-"]`).length === count, [tag, texts.length]);
  let last = -1;
  for (let still = 0; still < 3; ) {
    await page.waitForTimeout(100);
    const now = await page.evaluate(() => window.__MOCK__.callsOf("markdown_render").length);
    still = now === last ? still + 1 : 0;
    last = now;
  }
}

/** The elements the chat adds around a finished reply on purpose: the bar under a one-line block of code and its note. */
export const PAGE_ADDED = ["div.chat-codebar", "button.chat-insert", "p.chat-codenote", "div.chat-codebar *"];

/** A chat that fills the screen, with one stored chat `chat-x`, so that its messages can be put into the mock's state. */
export async function openCorpusChat(page, url) {
  await boot(page, url, {
    profiles: [PROFILE],
    view: VIEW_CHAT_ONLY,
    blocks: [chatBlock("chat-x")],
    states: { "chat-x": { title: "corpus", profile: "local-1", model: "llama3.2" } },
    starts: 1,
  });
  await page.waitForSelector(".block-chat");
}

/**
 * Take the policy reports that `safehtml.js` causes by parsing hostile markup in its inert document out of `errors`: the inline
 * style, the inline handlers and a `base` element (Chromium reports each; the engine that does not report it is no better or
 * worse). Nothing of it reaches the page, which is what the rows assert. Any other report, and any from another file, stays
 * an error. The reports arrive a moment after the call that caused them.
 */
export async function dropRawReports(page, errors) {
  await page.waitForTimeout(150);
  const expected = /^csp violation: (style-src-(elem|attr)|script-src-attr|base-uri) blocked .* \(.*safehtml\.js\)$/;
  errors.splice(0, errors.length, ...errors.filter((e) => !expected.test(e)));
}

// ---- the surfaces: one set of hostile strings, every place the page draws text that is not its own ---------------------------

/** The pollution of a prototype as the JSON a hostile answer would carry: an own `__proto__` member and a `constructor.prototype` one. */
export const POLLUTION_JSON = '{"__proto__":{"polluted":true},"constructor":{"prototype":{"polluted":true}}}';

const WORD = "q";

/**
 * The fourteen hostile strings every surface is fed. `shows` is the part that must be visible when the surface shows the value
 * (for the string of hidden characters the word between them, for the long line its last 64 letters), `head` is the start that a
 * surface which cuts a value must still show, and `markup` marks the strings that must never be found in an attribute.
 */
export const HOSTILE = [
  { id: "img-onerror", text: '<img src=x onerror="window.__XSS=1">', markup: true },
  { id: "script-tag", text: "<script>window.__XSS=1</script>", markup: true },
  { id: "svg-onload", text: '<svg onload="window.__XSS=1"></svg>', markup: true },
  { id: "js-scheme", text: "javascript:window.__XSS=1", markup: false },
  { id: "textarea-break", text: '</textarea><img src=x onerror="window.__XSS=1">', markup: true },
  { id: "quote-break", text: '" onfocus="window.__XSS=1" autofocus="', markup: true },
  { id: "template", text: "{{constructor.constructor('window.__XSS=1')()}}", markup: true },
  { id: "anchor-js", text: '<a href="javascript:window.__XSS=1">x</a>', markup: true },
  {
    id: "hidden-run",
    text: `${HIDDEN.bidi}${HIDDEN.zeroWidthSpace}hidden-word${HIDDEN.zeroWidthJoiner}${HIDDEN.tag}${HIDDEN.nul}${HIDDEN.escape}${HIDDEN.c1}${HIDDEN.lineSeparator}`,
    shows: "hidden-word",
    head: "hidden-word",
    markup: false,
  },
  { id: "long-line", text: `${WORD.repeat(4995)}__XSS`, shows: `${WORD.repeat(59)}__XSS`, markup: false },
  { id: "proto-key", text: "__proto__", markup: false },
  { id: "data-host", text: 'data:text/html,<script>window.__XSS=1</script>', markup: true },
  { id: "style-tag", text: "<style>*{display:none}</style>", markup: true },
  { id: "double-escape", text: "&lt;img src=x onerror=window.__XSS=1&gt; &amp;lt;", markup: true },
].map((h) => ({ ...h, shows: h.shows ?? h.text, head: h.head ?? Array.from(h.text).slice(0, 32).join("") }));

/** The strings by id. */
export const hostile = (id) => HOSTILE.find((h) => h.id === id);

/**
 * Run one surface against the hostile strings. `run(probe)` boots what it needs (the committed helpers, with the options of the
 * surface), puts the strings where the surface draws them and answers `{ scope, shown }` or calls `probe` itself when the surface
 * shows its strings one after the other. A probe checks, for the nodes that match `scope`:
 *   - `domProblems` in text mode finds nothing and `pageClean` finds nothing;
 *   - every string of `put` (all of them by default) that is in `shown` (`true`: all of `put`) occurs in the text of the scope
 *     by its `shows` part, and every one in `cut` by its `head`;
 *   - no attribute value anywhere in the scope contains a markup string;
 *   - the guards are quiet and the page is responsive.
 * A failure is a line `name: ...` in `ctx.failures`; `finish` throws all of them together.
 */
export async function surface(ctx, name, run) {
  const probe = async ({ scope, put = HOSTILE.map((h) => h.id), shown = true, cut = [], label = "", options = {} }) => {
    const here = label === "" ? name : `${name} (${label})`;
    const fail = (line) => ctx.failures.push(`${here}: ${line}`);
    const { page } = ctx;
    const found = await domProblems(page, scope, { mode: "text", ...options });
    for (const line of found) fail(line);
    for (const line of await pageClean(page)) fail(line);
    const text = (await page.locator(scope).allTextContents()).join("\n");
    const expected = shown === true ? put : shown;
    for (const id of expected) {
      const h = hostile(id);
      if (!text.includes(h.shows)) fail(`the string ${id} is not shown (${JSON.stringify(h.shows.slice(0, 60))})`);
    }
    for (const id of cut) {
      const h = hostile(id);
      if (!text.includes(h.head)) fail(`the start of the string ${id} is not shown (${JSON.stringify(h.head.slice(0, 40))})`);
    }
    const carried = await page.evaluate(
      ([sel, words, names]) =>
        [...document.querySelectorAll(sel)].flatMap((root) =>
          [root, ...root.querySelectorAll("*")].flatMap((n) =>
            [...n.attributes].filter((a) => !names.includes(a.name) && words.some((w) => a.value.includes(w))).map((a) => `${n.localName}[${a.name}]`),
          ),
        ),
      [scope, put.map(hostile).filter((h) => h.markup).map((h) => h.shows), TEXT_ATTRIBUTES],
    );
    for (const holder of new Set(carried)) fail(`an attribute carries the text: ${holder}`);
    for (const address of ctx.guards.foreign()) fail(`a request went to another origin: ${address}`);
    for (const address of ctx.guards.popups()) fail(`a window opened: ${address}`);
    try {
      await responsive(page, 5000);
    } catch (e) {
      fail(e.message);
    }
    ctx.probes += 1;
    ctx.scanned.push([name, text.length, put.length]);
    return found.length;
  };
  probe.fail = (line) => ctx.failures.push(`${name}: ${line}`);
  try {
    const result = await run(probe);
    if (result !== undefined && result !== null) await probe(result);
  } catch (e) {
    ctx.failures.push(`${name}: exception: ${String(e?.message ?? e).split("\n")[0].slice(0, 300)}`);
  }
  ctx.surfaces.push(name);
}

/** The state of one page run of the surfaces: the page, the guards, the failures so far and the surfaces that ran. */
export function surfaceContext(page, url, guards) {
  return { page, url, guards, failures: [], surfaces: [], probes: 0, scanned: [], corpus: loadCorpus() };
}

/** Throw one error that lists every failure of every surface, or nothing when there is none. */
export function finish(ctx, expected) {
  const per = {};
  for (const [name, length, strings] of ctx.scanned) {
    per[name] ??= { probes: 0, characters: 0, strings: 0 };
    per[name].probes += 1;
    per[name].characters += length;
    per[name].strings += strings;
  }
  console.log(`aigate surfaces: ${expected.map((n) => `${n} ${per[n]?.probes ?? 0} probes, ${per[n]?.strings ?? 0} strings, ${per[n]?.characters ?? 0} characters`).join("; ")}`);
  if (ctx.failures.length > 0) {
    throw new Error(`${ctx.failures.length} problems in the surfaces:\n${ctx.failures.slice(0, 60).join("\n")}`);
  }
  assert.deepEqual(ctx.surfaces, expected, "every surface of the file ran");
  assert.ok(ctx.probes >= expected.length, "every surface was probed");
}
