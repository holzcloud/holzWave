// Shared checker and drivers of the page gate (aigate-*.test.mjs): the corpus the core writes, the checker that finds
// what a hostile text could make of the page (an element, an attribute, a link or a global that the page does not own), the
// guards that watch for requests and windows, and the helper that proves the checker can fail.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
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
  const spec = {
    scope,
    mode: options.mode ?? "model",
    extra: options.extra ?? [],
    maxDepth: options.maxDepth ?? 66,
    inert: options.inert ?? null,
    safe: options.safe ?? null,
    forbidden: loadCorpus().forbidden,
    uiElements: UI_ELEMENTS,
    uiAttributes: UI_ATTRIBUTES,
  };
  return page.evaluate(async (s) => {
    const { ELEMENTS, appendSafe } = await import(new URL("safehtml.js", location.href).href);
    let roots;
    if (s.inert !== null) roots = [new DOMParser().parseFromString(`<!doctype html><body>${s.inert}`, "text/html").body];
    else if (s.safe !== null) {
      const node = document.createElement("div");
      appendSafe(s.safe, node);
      roots = [node];
    } else roots = [...document.querySelectorAll(s.scope)];
    const lines = [];
    const text = s.mode === "text";
    if (roots.length === 0) lines.push(`empty-scope: nothing matches ${s.scope}`);
    const badElements = new Set(s.forbidden.elements.filter((n) => !text || !s.uiElements.includes(n)));
    const badAttributes = new Set(s.forbidden.attributes.filter((n) => !text || !s.uiAttributes.includes(n)));
    const hostileValue = /__XSS|onerror|javascript:/i;
    const XHTML = "http://www.w3.org/1999/xhtml";
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
          if (text && hostileValue.test(attribute.value)) add("attribute-value", `${n}=${attribute.value.slice(0, 40)}`);
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
    if (lines.length > 40) return [...lines.slice(0, 40), `... and ${lines.length - 40} more`];
    return lines;
  }, spec);
}

/**
 * Elements and attributes that a control of the page needs and that a text-mode check lets through: an input, a select, a form
 * or a dialog is not an attack by itself (an element that loads or runs something still is), and the focus, the id and the
 * name of a control are its own. The value check still reads every one of them.
 */
export const UI_ELEMENTS = ["textarea", "select", "form", "dialog"];
export const UI_ATTRIBUTES = ["id", "name", "tabindex", "autofocus", "accesskey"];

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
