// The page-side contract gate: pure file reads and compiled modules, no browser. The two allow-lists (the core's sanitiser and the
// page's copy) and the corpus golden are compared; no page module builds markup from a string or runs dynamic code; the modules of
// the AI chat store, fetch and print nothing; the commands that decide, undo, answer, key and send each have one call site in their own
// module; the arguments of the AI commands are ids and typed text; the AI stylesheets use tokens, rem and the pixel whitelist. Every
// scan has a self-test on a synthetic source, and every test first asserts that its scan found something: a vacuous pass is a failure.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { ELEMENTS, safeHref } from "../../dist/safehtml.js";

const here = dirname(fileURLToPath(import.meta.url));
const src = join(here, "../../src");
const read = (path) => readFileSync(path, "utf8");
const corpus = JSON.parse(read(join(here, "../fixtures/ai_xss.json")));
const protocol = JSON.parse(read(join(here, "../fixtures/protocol.json")));
const previewRs = read(join(here, "../../../core/src/preview.rs"));

// ---- scanners ---------------------------------------------------------------------------------------------------------------

/** `source` without block comments and without line comments (a `//` counts only at the start of a line or after white space, so an address in a string survives). */
function stripComments(source) {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .split("\n")
    .map((line) => line.replace(/(^|\s)\/\/.*$/, "$1"))
    .join("\n");
}

/** How many times `source` calls the command `name` through `call` or `invoke`, with an optional type argument. */
function callSites(source, name) {
  return [...stripComments(source).matchAll(new RegExp(`\\b(?:call|invoke)\\s*(?:<[^()]*?>)?\\s*\\(\\s*["']${name}["']`, "g"))].length;
}

/** Every command name that `source` calls through `call` or `invoke`. */
function calledNames(source) {
  return [...stripComments(source).matchAll(/\b(?:call|invoke)\s*(?:<[^()]*?>)?\s*\(\s*["']([a-z_]+)["']/g)].map((m) => m[1]);
}

/** The modules of the page: file name to source text, comments removed. */
function appModules() {
  return Object.fromEntries(readdirSync(src).filter((f) => f.endsWith(".ts")).map((f) => [f, stripComments(read(join(src, f)))]));
}

/** The modules that draw or talk for the AI chat: by the prefix of their name. */
const AI_PREFIX = /^(chat|ai|consent|approval|ask|undo|inline|ondevice)/;
const AI_REQUIRED = [
  "chat.ts", "chatview.ts", "chatstate.ts", "chatlist.ts", "aisettings.ts", "aistate.ts", "aisidebar.ts", "consent.ts", "consentstate.ts",
  "consentcard.ts", "approval.ts", "approvalstate.ts", "approvalcard.ts", "undo.ts", "ask.ts", "askstate.ts", "askcard.ts", "inline.ts",
  "inlinestate.ts", "inlinecode.ts", "ondevice.ts",
];

/** Patterns that build markup from a string or run dynamic code. */
const MARKUP = [/\.innerHTML\b/, /\.outerHTML\b/, /\binsertAdjacentHTML\b/, /\bdocument\.write\b/, /\bcreateContextualFragment\b/, /\bsrcdoc\b/, /\bnew Function\b/, /\beval\s*\(/];

/** Patterns that store, fetch, print or name an address. */
const QUIET = [
  [/\blocalStorage\b/, "localStorage"], [/\bsessionStorage\b/, "sessionStorage"], [/\bindexedDB\b/, "indexedDB"], [/\bdocument\.cookie\b/, "document.cookie"],
  [/\bfetch\s*\(/, "fetch("], [/\bXMLHttpRequest\b/, "XMLHttpRequest"], [/\bWebSocket\b/, "WebSocket"], [/\bEventSource\b/, "EventSource"],
  [/\bsendBeacon\b/, "sendBeacon"], [/\bconsole\./, "console."], [/\bwindow\.open\b/, "window.open"], [/\blocation\.assign\b/, "location.assign"],
  [/\blocation\.href\s*=/, "location.href ="], [/\bimport\s*\(/, "dynamic import("], [/\.on[a-z]+\s*=[^=]/, "an event handler property"],
  [/allow_unsafe:\s*true/, "allow_unsafe: true"], [/https?:\/\//, "an address"],
];

/** The problems of a set of modules against `patterns`, as `file: what` lines. */
function scan(modules, patterns) {
  const found = [];
  for (const [file, text] of Object.entries(modules)) {
    for (const entry of patterns) {
      const [pattern, what] = Array.isArray(entry) ? entry : [entry, String(entry)];
      if (pattern.test(text)) found.push(`${file}: ${what}`);
    }
  }
  return found;
}

/** The quoted names between the first `[` after `open` and its `]`. */
const quotedAfter = (text, open) => {
  const start = text.indexOf(open);
  assert.ok(start >= 0, `${open} is not in core/src/preview.rs`);
  const end = text.indexOf("]", start);
  return [...text.slice(start, end).matchAll(/"([^"]+)"/g)].map((m) => m[1]);
};

/** The rules of a style sheet without comments: `{ selector, body, at }` for every rule with declarations, `at` the enclosing at-rules. */
function cssRules(text) {
  const clean = text.replace(/\/\*[\s\S]*?\*\//g, "");
  const rules = [];
  const walk = (from, to, at) => {
    let i = from;
    let start = from;
    while (i < to) {
      const c = clean[i];
      if (c === ";") start = i + 1;
      if (c === "{") {
        let depth = 1;
        let j = i + 1;
        while (j < to && depth > 0) {
          if (clean[j] === "{") depth += 1;
          if (clean[j] === "}") depth -= 1;
          j += 1;
        }
        const prelude = clean.slice(start, i).trim();
        const inner = clean.slice(i + 1, j - 1);
        if (prelude.startsWith("@media") || prelude.startsWith("@supports")) walk(i + 1, j - 1, [...at, prelude]);
        else rules.push({ selector: prelude, body: inner, at });
        i = j;
        start = j;
        continue;
      }
      i += 1;
    }
  };
  walk(0, clean.length, []);
  return rules;
}

const STYLE_SHEETS = ["chat.css", "consent.css", "approval.css", "aisettings.css", "aisidebar.css", "inline.css", "ask.css"];
const PX_WHITELIST = new Set([1, 2, 16, 28, 44, 640]);
/** The only places a shadow may be drawn: a sheet and the rule it is drawn in, with the reason. */
const SHADOW_OK = [
  { sheet: "chat.css", at: "(max-width: 640px)", reason: "the picker of the chat is a sheet fixed to the bottom edge of a phone; a floating layer may have a shadow (design 4)" },
];

/** What is wrong with a style sheet: tokens, rem, the pixel whitelist, motion, shadows, and the two blocks every sheet needs. */
function styleProblems(name, text, shadows = SHADOW_OK) {
  const found = [];
  const clean = text.replace(/\/\*[\s\S]*?\*\//g, "");
  if (/#[0-9a-fA-F]{3,8}\b(?![\w-])/.test(clean.replace(/#[a-z][\w-]*\s*[{,.:\[>+~ ]/gi, ""))) found.push(`${name}: a hex colour`);
  for (const [pattern, what] of [[/gradient\(/, "a gradient"], [/blur\(/, "a blur"], [/backdrop-filter/, "backdrop-filter"], [/(^|[\s;{])filter\s*:/, "filter:"]]) {
    if (pattern.test(clean)) found.push(`${name}: ${what}`);
  }
  for (const rule of cssRules(text)) {
    if (/(^|[\s;])(transition|animation)\s*:/.test(rule.body) && !rule.selector.split(",").every((s) => s.trim().startsWith(":root:not(.motion-reduce)"))) {
      found.push(`${name}: motion outside :root:not(.motion-reduce) in ${rule.selector.slice(0, 60)}`);
    }
    const shadow = rule.body.match(/box-shadow\s*:\s*([^;}]+)/);
    if (shadow && shadow[1].trim() !== "none") {
      const allowed = shadows.some((s) => s.sheet === name && rule.at.some((a) => a.includes(s.at)));
      if (!allowed) found.push(`${name}: a shadow outside the allowed places in ${rule.selector.slice(0, 60)}`);
    }
  }
  for (const m of clean.matchAll(/(\d+(?:\.\d+)?)px\b/g)) {
    if (!PX_WHITELIST.has(Number(m[1]))) found.push(`${name}: the pixel length ${m[0]} is not on the whitelist`);
  }
  if (!clean.includes("forced-colors")) found.push(`${name}: no forced-colors block`);
  if (!/max-width:\s*640px/.test(clean)) found.push(`${name}: no narrow block (max-width: 640px)`);
  return [...new Set(found)];
}

// ---- the tests --------------------------------------------------------------------------------------------------------------

test("the page copy and the core renderer allow the same elements and the same link schemes", () => {
  const core = quotedAfter(previewRs, "let tags: HashSet<&str> = [");
  const page = [...ELEMENTS];
  const golden = corpus.elements;
  assert.equal(core.length, 29, "the core's list was found");
  assert.deepEqual([...core].sort(), [...page].sort(), "the page copy and the core allow the same elements");
  assert.deepEqual([...golden].sort(), [...core].sort(), "the golden holds the list of the core");
  for (const name of page) assert.ok(!corpus.forbidden.elements.includes(name), `${name} is allowed and forbidden`);
  assert.ok(page.every((name) => core.includes(name)), "the page is never wider than the core");
  const schemes = quotedAfter(previewRs, ".url_schemes([");
  assert.deepEqual(schemes, ["http", "https", "mailto"]);

  for (const ok of ["https://example.org/a", "HTTP://example.org", "mailto:a@example.org", " https://example.org/ "]) assert.ok(safeHref(ok), ok);
  const refused = [
    ...corpus.forbidden.schemes.map((s) => `${s}:x`), "//evil.example/x", "../a", "#a", "", "https://exa mple.org", "\u0001javascript:x", "ftp://example.org/",
    "javascript:alert(1)", "JaVaScRiPt:alert(1)", "data:text/html,x", "relative/path",
  ];
  for (const bad of refused) assert.ok(!safeHref(bad), `safeHref accepted ${JSON.stringify(bad)}`);
  // The page is stricter on purpose: the core passes a relative link, the page never does.
  assert.ok(corpus.cases.some((c) => c.id === "relative-path" && c.core_html.includes("../notes.md")), "the core passes a relative link");
  assert.ok(!safeHref("../notes.md"));
});

test("no page module builds markup from a string or runs dynamic code", () => {
  const modules = appModules();
  assert.ok(Object.keys(modules).length > 60, "the scan found the modules of the page");
  assert.ok("safehtml.ts" in modules && "chat.ts" in modules);
  assert.deepEqual(scan(modules, MARKUP), []);
  const parsers = Object.entries(modules).filter(([, text]) => /\bDOMParser\b/.test(text)).map(([file]) => file);
  assert.deepEqual(parsers, ["safehtml.ts"], "the only parser of markup is the one of safehtml.ts");
});

test("the AI modules store nothing, fetch nothing, print nothing and name no address", () => {
  const modules = appModules();
  const ai = Object.fromEntries(Object.entries(modules).filter(([file]) => AI_PREFIX.test(file)));
  for (const required of AI_REQUIRED) assert.ok(required in ai, `${required} was not found`);
  assert.deepEqual(scan(ai, QUIET), []);
});

test("each decision, undo, answer and key command has one call site in its own module", () => {
  const modules = Object.fromEntries(readdirSync(src).filter((f) => f.endsWith(".ts")).map((f) => [f, read(join(src, f))]));
  const sites = (name) => Object.entries(modules).flatMap(([file, text]) => Array.from({ length: callSites(text, name) }, () => file));
  assert.deepEqual(sites("ai_decide"), ["approval.ts"]);
  assert.deepEqual(sites("ai_undo"), ["undo.ts"]);
  assert.deepEqual(sites("ai_ask_answer"), ["ask.ts"]);
  assert.deepEqual(sites("ai_key_set"), ["aisettings.ts"]);
  assert.deepEqual(sites("ai_send"), ["chat.ts"]);
  const holders = Object.entries(modules).filter(([, text]) => /\bapi_key\b/.test(stripComments(text))).map(([file]) => file).sort();
  assert.ok(holders.includes("aisettings.ts"), "the key is typed in the providers dialog");
  assert.deepEqual(holders.filter((f) => !["aisettings.ts", "aistate.ts"].includes(f)), [], "the word api_key is confined to the two modules of the providers dialog");
});

test("the commands the AI modules call take ids and typed text only", () => {
  const commands = protocol.commands;
  const ai = Object.keys(commands).filter((n) => n.startsWith("ai_") || n === "markdown_render");
  assert.ok(ai.length >= 20, `the golden has ${ai.length} AI commands`);
  // An argument name is read word by word (`call_id` is `call` and `id`), so `profile` is not `file`.
  const forbiddenWords = new Set("context content screen selection output stdin snippet excerpt body payload file data blob bytes html markup secret password token passphrase".split(" "));
  const forbidden = { test: (arg) => arg.toLowerCase().split("_").some((word) => forbiddenWords.has(word)) };
  const only = { text: ["ai_send", "markdown_render"], input: ["ai_profile_save"], api_key: ["ai_key_set"], hash: ["ai_decide"], decision: ["ai_decide"], seal: ["ai_ask_answer"], verdict: ["ai_ask_answer"], call_id: ["ai_decide", "ai_undo"] };
  for (const name of ai) {
    for (const arg of commands[name]) {
      assert.ok(!forbidden.test(arg), `${name} takes an argument named ${arg}`);
      if (arg in only) assert.ok(only[arg].includes(name), `${name} takes ${arg}, which only ${only[arg].join(" and ")} may take`);
    }
  }
  const called = new Set();
  for (const f of readdirSync(src).filter((x) => x.endsWith(".ts"))) for (const n of calledNames(read(join(src, f)))) if (n.startsWith("ai_") || n === "markdown_render") called.add(n);
  assert.deepEqual([...called].filter((n) => !ai.includes(n)), [], "the page calls only commands of the golden");
  // `ai_has_key` is a reviewed command that no page code calls (the state of a key comes with the list of profiles).
  assert.deepEqual(ai.filter((n) => !called.has(n)), ["ai_has_key"], "every other command of the golden is called by the page");
});

test("the AI stylesheets use tokens, rem and the pixel whitelist, and have the narrow and forced-colour blocks", () => {
  for (const name of STYLE_SHEETS) {
    const text = read(join(src, name));
    assert.ok(cssRules(text).length > 5, `${name}: the scan found rules`);
    assert.deepEqual(styleProblems(name, text), []);
  }
  assert.ok(SHADOW_OK.every((s) => s.reason.length > 20), "every shadow place names its reason");
  assert.ok(/box-shadow:\s*0/.test(read(join(src, "chat.css"))), "the narrow sheet of chat.css still has the one shadow the table allows");
});

test("the corpus fixture is whole", () => {
  assert.equal(corpus.version, 1);
  const ids = corpus.cases.map((c) => c.id);
  assert.equal(new Set(ids).size, ids.length, "ids are unique");
  assert.ok(corpus.cases.length >= 60);
  assert.deepEqual([...new Set(corpus.cases.map((c) => c.class))].sort(), ["clobber", "handler", "image", "markup", "nest", "scheme", "script", "soup", "table", "text"]);
  for (const c of corpus.cases) {
    assert.deepEqual(Object.keys(c), ["class", "core_html", "expect", "id", "marker", "raw_html", "source", "visible"], c.id);
    assert.ok(["removed", "text", "open"].includes(c.expect), c.id);
    assert.ok(c.raw_html.includes(c.marker), `${c.id}: the marker is not live`);
  }
  assert.deepEqual(Object.keys(corpus), ["cases", "elements", "forbidden", "limits", "version"]);
  const text = read(join(here, "../fixtures/ai_xss.json"));
  const hiddenAt = (n) =>
    (n < 32 && n !== 10) || (n >= 127 && n <= 160) || n === 0x1680 || (n >= 0x2000 && n <= 0x200f) || (n >= 0x2028 && n <= 0x202f) ||
    (n >= 0x205f && n <= 0x206f) || n === 0x3000 || n === 0xfeff || (n >= 0xe000 && n <= 0xf8ff) || (n >= 0xe0000 && n <= 0xe007f);
  assert.deepEqual([...text].filter((c) => hiddenAt(c.codePointAt(0))), [], "a raw hidden character in the fixture");
  const hosts = new Set([...text.matchAll(/https?:\/\/([^\s"'<>)\\/:?#]+)/g)].map((m) => m[1]));
  assert.ok(hosts.size >= 2);
  for (const host of hosts) assert.ok(["evil.example", "example.org", "localhost"].includes(host), `the corpus names ${host}`);
});

test("the scanners can fail", () => {
  // Markup from a string, dynamic code, a parser outside safehtml.
  const bad = { "a.ts": stripComments("el.innerHTML = x; new Function('1'); eval(y); document.write(z); const p = new DOMParser();") };
  const found = scan(bad, MARKUP);
  assert.ok(found.length >= 4, found.join("; "));
  assert.deepEqual(scan({ "ok.ts": stripComments("// el.innerHTML = x\n/* eval(x) */ el.textContent = x;") }, MARKUP), [], "a comment is not code");
  // Storage, network, print, address, handler property.
  const quiet = scan({ "b.ts": stripComments("localStorage.setItem(a, b); fetch(u); console.log(1); const u = 'https://example.org'; el.onclick = f; import('./x.js'); window.open(u);") }, QUIET);
  for (const want of ["localStorage", "fetch(", "console.", "an address", "an event handler property", "dynamic import(", "window.open"]) {
    assert.ok(quiet.some((l) => l.includes(want)), `the quiet scan missed ${want}: ${quiet}`);
  }
  assert.equal(stripComments("const a = 'https://x'; // https://y").includes("https://x"), true, "a scheme in a string survives");
  assert.equal(stripComments("a // https://y").includes("https://y"), false, "a comment after code goes");
  // A second call site and an argument named like context.
  assert.equal(callSites('call("ai_decide", {}); call<string>("ai_decide", {}); // call("ai_decide")', "ai_decide"), 2);
  assert.deepEqual(calledNames('call("ai_a"); invoke<X>("ai_b", 1);'), ["ai_a", "ai_b"]);
  assert.ok(["context", "file_text", "Payload"].every((a) => a.toLowerCase().split("_").some((w) => "context file payload".split(" ").includes(w))));
  assert.ok(!"profile".split("_").some((w) => w === "file"), "profile is not file");
  // A widened element list and an element the corpus forbids.
  const widened = new Set([...ELEMENTS, "img"]);
  assert.ok(corpus.forbidden.elements.some((n) => widened.has(n)), "a list with img in it meets the forbidden list");
  assert.ok(!ELEMENTS.has("img"));
  // Stylesheets: a hex colour, a gradient, an odd pixel length, motion outside the rule, a shadow outside the narrow block.
  const sheet = "@media (forced-colors: active) { a { color: CanvasText; } } @media (max-width: 640px) { b { min-height: 44px; } }";
  assert.deepEqual(styleProblems("t.css", sheet), []);
  assert.ok(styleProblems("t.css", `${sheet} .x { color: #ff00aa; }`).some((l) => l.includes("hex colour")));
  assert.ok(styleProblems("t.css", `${sheet} .x { background: linear-gradient(red, blue); }`).some((l) => l.includes("gradient")));
  assert.ok(styleProblems("t.css", `${sheet} .x { width: 13px; }`).some((l) => l.includes("13px")));
  assert.ok(styleProblems("t.css", `${sheet} .x { transition: color 1s; }`).some((l) => l.includes("motion")));
  assert.deepEqual(styleProblems("t.css", `${sheet} :root:not(.motion-reduce) .x { transition: color 1s; }`), []);
  assert.ok(styleProblems("t.css", `${sheet} .x { box-shadow: 0 0 3rem black; }`).some((l) => l.includes("shadow")));
  assert.ok(styleProblems("chat.css", "@media (forced-colors: active) { a { color: CanvasText; } } @media (max-width: 640px) { .x { box-shadow: 0 0 3rem black; } }").length === 0);
  assert.ok(styleProblems("t.css", "a { color: CanvasText; }").some((l) => l.includes("forced-colors")));
  assert.ok(styleProblems("t.css", "#sidebar-x .y { color: red; }\n@media (forced-colors: active) { a { color: CanvasText; } }").every((l) => !l.includes("hex")), "an id selector is no colour");
});
