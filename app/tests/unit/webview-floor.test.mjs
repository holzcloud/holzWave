// The oldest Android System WebView the page runs on, and the guard that keeps it honest. The table below lists the features
// newer than Chrome 80 that the page uses or may soon use, each with the first version of the "WebView Android" column of
// MDN's compatibility data and the MDN page it was read from. The tests read the built page (app/dist, after `npm run build`),
// find which rows it uses, compute the floor as the highest version among them, and hold dist/floor.js (the document's one
// script, which loads the application only above the floor) to that number and to one check for every used row.
//
// The versions were read on 2026-11-21 from MDN's compatibility data (the tables of the MDN pages, @mdn/browser-compat-data
// 8.1.10 of 2026-11-19, and for the rows of the page itself also from MDN's live data behind each page). When a row is added,
// read its version from the MDN page now, never from memory.
//
// TypeScript's lib is ES2022, so a newer ECMAScript built-in (findLast, toSorted, iterator helpers, Set methods, ...) does not
// compile without this table moving with it; the DOM lib is current, so DOM and CSS features are found by the patterns below.
import test from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { transformSync } from "esbuild";

const APP = fileURLToPath(new URL("../../", import.meta.url));
const DIST = join(APP, "dist");
const REPO = join(APP, "..");

/** The check of a row that no feature test can make (syntax, a layout context, a vendored library): the version gate. */
const VERSION = "version < FLOOR";
const MDN = "https://developer.mozilla.org/en-US/docs/";

/**
 * One row per feature. `webview` is the first WebView Android version with the feature, or null when MDN records none (then
 * `outside` says why the page may still use it and the row cannot set the floor). `css`, `js` and `html` are the patterns that
 * find the feature in the built stylesheets, scripts and documents; `check` is the text dist/floor.js evaluates for it: a
 * JavaScript expression, a quoted `CSS.supports` or media query string, or VERSION.
 */
const FEATURES = [
  // ---- JavaScript ----
  {
    name: "Logical assignment (||=, &&=, ??=)", webview: 85, mdn: `${MDN}Web/JavaScript/Reference/Operators/Nullish_coalescing_assignment`,
    js: /(\|\||&&|\?\?)=/, check: VERSION,
  },
  {
    name: "String.prototype.replaceAll", webview: 85, mdn: `${MDN}Web/JavaScript/Reference/Global_Objects/String/replaceAll`,
    js: /\.replaceAll\(/, check: 'typeof "".replaceAll === "function"',
  },
  {
    name: "Element.replaceChildren", webview: 86, mdn: `${MDN}Web/API/Element/replaceChildren`,
    js: /\.replaceChildren\(/, check: 'typeof Element.prototype.replaceChildren === "function"',
  },
  {
    name: "Array.prototype.at and String.prototype.at", webview: 92, mdn: `${MDN}Web/JavaScript/Reference/Global_Objects/Array/at`,
    js: /\.at\((-?\d|[A-Za-z_$])/, check: 'typeof [].at === "function" && typeof "".at === "function"',
  },
  {
    name: "Object.hasOwn", webview: 93, mdn: `${MDN}Web/JavaScript/Reference/Global_Objects/Object/hasOwn`,
    js: /\bObject\.hasOwn\(/, check: 'typeof Object.hasOwn === "function"',
  },
  {
    // The chat calls a link of no kind "inert"; only the attribute or the property is the feature.
    name: "The inert attribute", webview: 102, mdn: `${MDN}Web/API/HTMLElement/inert`,
    js: /\.inert\b|Attribute\(\s*["']inert["']/, html: /<[a-z][^>]*\sinert[\s>=]/, check: '"inert" in HTMLElement.prototype',
  },
  // ---- CSS ----
  {
    name: "color-scheme", webview: 81, mdn: `${MDN}Web/CSS/Reference/Properties/color-scheme`,
    css: /(^|[;{\s])color-scheme\s*:/, js: /["']color-scheme["']/, check: '"color-scheme: dark"',
  },
  {
    name: "appearance (unprefixed)", webview: 84, mdn: `${MDN}Web/CSS/Reference/Properties/appearance`,
    css: /(^|[;{\s])appearance\s*:/, check: '"appearance: none"',
  },
  {
    // `CSS.supports` says yes to `gap` since grid (57), so no test tells the flex context apart.
    name: "gap in a flex container", webview: 84, mdn: `${MDN}Web/CSS/Reference/Properties/gap`,
    css: /(^|[;{\s])(row-|column-)?gap\s*:/, check: VERSION,
  },
  {
    name: ":focus-visible", webview: 86, mdn: `${MDN}Web/CSS/Reference/Selectors/:focus-visible`,
    css: /:focus-visible\b/, js: /:focus-visible\b/, check: '"selector(:focus-visible)"',
  },
  {
    name: "inset", webview: 87, mdn: `${MDN}Web/CSS/Reference/Properties/inset`,
    css: /(^|[;{\s])inset\s*:/, check: '"inset: 0"',
  },
  {
    name: "text-underline-offset", webview: 87, mdn: `${MDN}Web/CSS/Reference/Properties/text-underline-offset`,
    css: /text-underline-offset\s*:/, check: '"text-underline-offset: 1px"',
  },
  {
    name: ":is()", webview: 88, mdn: `${MDN}Web/CSS/Reference/Selectors/:is`,
    css: /:is\(/, js: /:is\(/, check: '"selector(:is(a))"',
  },
  {
    // 87 and 88 have it only when the offset, colour or style of the decoration is set too; 89 has it always.
    name: "text-decoration-thickness", webview: 89, mdn: `${MDN}Web/CSS/Reference/Properties/text-decoration-thickness`,
    css: /text-decoration-thickness\s*:/, check: '"text-decoration-thickness: 1px"',
  },
  {
    name: "The forced-colors media feature", webview: 89, mdn: `${MDN}Web/CSS/Reference/At-rules/@media/forced-colors`,
    css: /\(forced-colors\s*:/, js: /\(forced-colors\s*:/, check: '"(forced-colors: none), (forced-colors: active)"',
  },
  {
    name: "forced-color-adjust", webview: 89, mdn: `${MDN}Web/CSS/Reference/Properties/forced-color-adjust`,
    css: /forced-color-adjust\s*:/, check: '"forced-color-adjust: none"',
  },
  {
    name: "overflow: clip", webview: 90, mdn: `${MDN}Web/CSS/Reference/Properties/overflow`,
    css: /(^|[;{\s])overflow(-x|-y)?\s*:\s*clip\b/, check: '"overflow: clip"',
  },
  {
    // MDN marks it partial: the WebView does not keep a minimum contrast of the control by itself.
    name: "accent-color", webview: 93, mdn: `${MDN}Web/CSS/Reference/Properties/accent-color`,
    css: /accent-color\s*:/, check: '"accent-color: auto"',
  },
  {
    // `CSS.supports` says yes to it from the grid context, so no test tells the newer contexts apart.
    name: "align-self: start", webview: 93, mdn: `${MDN}Web/CSS/Reference/Properties/align-self`,
    css: /align-self\s*:\s*start\b/, check: VERSION,
  },
  {
    name: "The prefers-contrast media feature", webview: 96, mdn: `${MDN}Web/CSS/Reference/At-rules/@media/prefers-contrast`,
    css: /\(prefers-contrast\b/, js: /\(prefers-contrast\b/,
    check: '"(prefers-contrast: no-preference), (prefers-contrast: more), (prefers-contrast: less), (prefers-contrast: custom)"',
  },
  {
    name: ":has()", webview: 105, mdn: `${MDN}Web/CSS/Reference/Selectors/:has`,
    css: /:has\(/, js: /:has\(/, check: '"selector(:has(a))"',
  },
  {
    name: "Dynamic, small and large viewport units (dvh, svh, lvh)", webview: 108, mdn: `${MDN}Web/CSS/Reference/Values/length`,
    css: /\d[dsl]v(h|w|i|b|min|max)\b/, js: /\d[dsl]v(h|w|i|b|min|max)\b/, check: '"height: 1dvh"',
  },
  {
    name: "color-mix()", webview: 111, mdn: `${MDN}Web/CSS/Reference/Values/color_value/color-mix`,
    css: /color-mix\(/, js: /color-mix\(/, check: '"color: color-mix(in srgb, currentcolor, transparent)"',
  },
  {
    name: "scrollbar-width", webview: null, mdn: `${MDN}Web/CSS/Reference/Properties/scrollbar-width`,
    outside: "MDN records no WebView with it; the page uses it only to hide the scroll bar of a strip, the WebView draws overlay scroll bars and ignores the declaration, and nothing depends on it",
    css: /scrollbar-width\s*:/, check: null,
  },
];

/**
 * The vendored libraries, by what their builds target. The editor bundle (scripts/vendor-editor.mjs) is built for `es2022` and
 * `safari16`, so it may hold any syntax of ES2022; the newest of it in the WebView column is a class static initialization block
 * (94). PDF.js 6.4.299 ships the legacy build, which its FAQ states supports Chrome 125 and later (pdf.js wiki "Frequently Asked
 * Questions", section "Which browsers/environments are supported?", and `ENV_TARGETS` in gulpfile.mjs of v6.4.299).
 */
const VENDORED = [
  {
    name: "The editor bundle (esbuild target es2022, safari16)", webview: 94,
    mdn: `${MDN}Web/JavaScript/Reference/Classes/Static_initialization_blocks`,
    path: "vendor/editor", pinned: () => /target:\s*\["es2022",\s*"safari16"\]/.test(readFileSync(join(APP, "scripts", "vendor-editor.mjs"), "utf8")),
    pinnedWhat: 'scripts/vendor-editor.mjs builds for target ["es2022", "safari16"]', check: VERSION,
  },
  {
    name: "PDF.js 6.4.299, legacy build (Chrome 125 and later)", webview: 125,
    mdn: "https://github.com/mozilla/pdf.js/wiki/Frequently-Asked-Questions#faq-support",
    path: "vendor/pdfjs/pdf.min.mjs",
    pinned: () => JSON.parse(readFileSync(join(APP, "package.json"), "utf8")).devDependencies["pdfjs-dist"] === "6.4.299",
    pinnedWhat: "app/package.json pins pdfjs-dist 6.4.299", check: VERSION,
  },
];

/** The rows every version of this table must have, whether the page uses them today or not. */
const REQUIRED = [
  "Object.hasOwn", "Array.prototype.at and String.prototype.at", "Element.replaceChildren", "The inert attribute", ":has()", "color-mix()",
  "Dynamic, small and large viewport units (dvh, svh, lvh)", "scrollbar-width", "overflow: clip", "accent-color",
];

/** The boot script and its stylesheet are the gate, not the page: they are scanned on their own rules below. */
const GATE = new Set(["floor.js", "floor.css"]);

/** Replace a comment by as many newlines as it spans, so a line number of the stripped text is the line of the file. */
const blank = (m) => m.replace(/[^\n]/g, "");
const stripCss = (text) => text.replace(/\/\*[\s\S]*?\*\//g, blank);
const stripJs = (text) =>
  text.replace(/\/\*[\s\S]*?\*\//g, blank).replace(/(^|[;{}),]\s|^\s+)\/\/[^\n]*/gm, (m, lead) => lead);

/** The files of the built page, each with its kind and its text without comments. */
function pageFiles() {
  assert.ok(existsSync(join(DIST, "index.html")), "app/dist is missing: run `npm run build` first");
  const files = [];
  for (const name of readdirSync(DIST).sort()) {
    if (GATE.has(name)) continue;
    const raw = () => readFileSync(join(DIST, name), "utf8");
    if (name.endsWith(".js")) files.push({ name, kind: "js", text: stripJs(raw()) });
    else if (name.endsWith(".css")) files.push({ name, kind: "css", text: stripCss(raw()) });
    else if (name.endsWith(".html")) files.push({ name, kind: "html", text: raw() });
  }
  return files;
}

/** Where the page first uses `row` (`file:line`), or null. */
function firstUse(row, files) {
  for (const file of files) {
    const re = row[file.kind];
    if (!re) continue;
    const m = re.exec(file.text);
    if (m) return `${file.name}:${file.text.slice(0, m.index).split("\n").length}`;
  }
  return null;
}

/** Every row the built page uses, with where. */
function usedRows() {
  const files = pageFiles();
  const used = [];
  for (const row of FEATURES) {
    const at = firstUse(row, files);
    if (at) used.push({ ...row, at });
  }
  for (const row of VENDORED) if (existsSync(join(DIST, row.path))) used.push({ ...row, at: `dist/${row.path}` });
  return used;
}

/** The highest first version among the rows the page uses. */
const floorOf = (used) => Math.max(...used.filter((r) => r.webview !== null).map((r) => r.webview));

const floorJs = () => {
  const path = join(DIST, "floor.js");
  assert.ok(existsSync(path), "dist/floor.js is missing: the boot script was not built");
  return readFileSync(path, "utf8");
};

test("the_page_is_scanned_for_every_feature_of_the_table_and_the_floor_is_the_highest_version_it_uses", (t) => {
  const names = new Set([...FEATURES, ...VENDORED].map((r) => r.name));
  for (const name of REQUIRED) assert.ok(names.has(name), `the table has no row ${name}`);
  for (const row of [...FEATURES, ...VENDORED]) {
    assert.ok(row.webview === null || (Number.isInteger(row.webview) && row.webview > 80), `${row.name}: a version newer than Chrome 80, or null`);
    assert.ok(/^https:\/\//.test(row.mdn), `${row.name} names its source`);
    if (row.webview === null) assert.ok(typeof row.outside === "string" && row.outside.length > 40, `${row.name} has no WebView version and says why the page may use it`);
  }
  const used = usedRows();
  for (const row of used) t.diagnostic(`${row.webview ?? "none"}  ${row.name}  (${row.at})`);
  assert.ok(used.some((r) => r.name === "Object.hasOwn"), "the scan finds Object.hasOwn in the page");
  const floor = floorOf(used);
  t.diagnostic(`floor: ${floor}`);
  assert.ok(floor >= 93, `the floor ${floor} is below Object.hasOwn (93)`);
  for (const row of used) if (row.webview !== null) assert.ok(row.webview <= floor, `${row.name} is above the floor`);
});

test("the_patterns_find_the_feature_and_not_a_word_that_looks_like_it", () => {
  const row = (name) => FEATURES.find((r) => r.name === name);
  const inert = row("The inert attribute");
  assert.ok(!inert.js.test('if (linkKind(href) === "inert") {'), "the chat's link kind is not the attribute");
  assert.ok(!inert.js.test('node.closest("[hidden], [inert]")'), "a selector that reads the attribute does not need it");
  assert.ok(inert.js.test("sheet.inert = true;") && inert.js.test('node.setAttribute("inert", "")') && inert.html.test('<div id="x" inert>'));
  const clip = row("overflow: clip");
  assert.ok(!clip.css.test(".a { text-overflow: clip; }") && clip.css.test(".a { overflow: clip; }") && clip.css.test(".a{overflow-x:clip}"));
  const at = row("Array.prototype.at and String.prototype.at");
  assert.ok(at.js.test("done.failed.at(-1)") && !at.js.test("// look at (this)") && !at.js.test("const flat = ["));
  const gap = row("gap in a flex container");
  assert.ok(gap.css.test(".a { display: flex; gap: .5rem; }") && !gap.css.test("--hw-gap-x: 1px"));
  const dvh = row("Dynamic, small and large viewport units (dvh, svh, lvh)");
  assert.ok(dvh.css.test("height: 100dvh;") && !dvh.css.test("height: 100vh;"));
  // Comments are not uses: the page's own comments name features it does not use.
  assert.equal(stripJs("const a = 1; // a.inert = true\n/* x.inert */ b();").includes("inert"), false);
  assert.equal(stripJs('const u = "https://example.org/";'), 'const u = "https://example.org/";', "an address in a string is not a comment");
});

test("the_vendored_libraries_are_rows_of_their_own_and_the_floor_covers_them", () => {
  for (const row of VENDORED) {
    assert.ok(row.pinned(), `${row.name}: ${row.pinnedWhat}; the row was read for that, so read the support of the new build and change the row`);
    assert.ok(existsSync(join(DIST, row.path)), `${row.path} is in the build`);
  }
  const floor = floorOf(usedRows());
  for (const row of VENDORED) assert.ok(row.webview <= floor, `${row.name} needs ${row.webview}, above the floor ${floor}`);
  // The PDF preview ships the legacy build of PDF.js; its stated support is the one the row records.
  assert.match(readFileSync(join(APP, "scripts", "copy-static.mjs"), "utf8"), /"legacy\/build\/pdf\.min\.mjs"/);
});

test("the_boot_script_holds_the_floor_and_a_check_for_every_used_feature_newer_than_chrome_80", () => {
  const js = floorJs();
  const used = usedRows();
  const floor = floorOf(used);
  const declared = js.match(/\bFLOOR = (\d+);/);
  assert.ok(declared, "dist/floor.js declares FLOOR");
  assert.equal(Number(declared[1]), floor, `FLOOR in dist/floor.js is ${declared[1]}, the page needs ${floor}`);
  for (const row of used) {
    if (row.webview === null) continue;
    assert.ok(typeof row.check === "string", `${row.name} is used and has no check`);
    assert.ok(js.includes(row.check), `dist/floor.js does not check ${row.name} (${row.check})`);
  }
  // No check of the boot script stands for a feature above the floor, so no check can lock out a WebView at or above it.
  const checks = [...js.matchAll(/"((?:selector\(|\()?[a-z-]+[:(][^"]*)"/g)].map((m) => `"${m[1]}"`).filter((s) => !s.startsWith('"/'));
  for (const check of checks) {
    const row = [...FEATURES, ...VENDORED].find((r) => r.check === check);
    assert.ok(row, `dist/floor.js checks ${check}, which is no row of the table`);
    assert.ok(row.webview <= floor, `dist/floor.js checks ${row.name}, above the floor`);
  }
  // It loads the application as the module it is, from the root, and nothing else.
  assert.match(js, /\.type = "module";/);
  assert.match(js, /\.src = "\/main\.js";/);
  assert.match(js, /\.href = "\/floor\.css";/);
  assert.doesNotMatch(js, /innerHTML|outerHTML|insertAdjacentHTML|document\.write|\.style\b|setAttribute\("style"|eval\(|new Function/);
});

test("typescript_compiles_against_es2022_so_a_newer_built_in_cannot_slip_in", () => {
  const conf = JSON.parse(readFileSync(join(APP, "tsconfig.json"), "utf8")).compilerOptions;
  assert.equal(conf.target, "ES2022");
  assert.deepEqual([...conf.lib].sort(), ["DOM", "ES2022"]);
});

test("the_boot_script_is_es2017_so_an_old_webview_can_parse_it", () => {
  const js = floorJs();
  // esbuild lowers whatever is newer than the target; when the output for ES2017 equals the output for the newest syntax,
  // the file has nothing newer.
  const newest = transformSync(js, { loader: "js", target: "esnext" }).code;
  const es2017 = transformSync(js, { loader: "js", target: "es2017" }).code;
  assert.equal(es2017, newest, "dist/floor.js uses syntax newer than ES2017");
  const code = stripJs(js);
  assert.doesNotMatch(code, /\?\.|\?\?|#[A-Za-z_]|\bcatch\s*\{/, "no optional chaining, nullish operator, private name or bare catch");
  assert.doesNotMatch(code, /^\s*(import|export)\b|\bimport\s*\(|import\.meta/m, "a classic script: no import or export");
  assert.match(code, /^\(function \(\) \{/m, "the script is wrapped in a function so nothing leaks into the global scope");
});

test("the_words_of_the_notice_are_the_words_of_the_design_record", () => {
  const design = readFileSync(join(REPO, "docs", "design.md"), "utf8");
  const entry = design.slice(design.indexOf("**Web view too old.**"));
  assert.ok(design.includes("**Web view too old.**"), "docs/design.md 6.6 has the entry Web view too old");
  for (const words of [
    "`Web view too old`",
    "`holzWave needs Android System WebView N or later. This phone has version M.`",
    "`Update Android System WebView in the Play Store, then open holzWave again.`",
  ]) assert.ok(entry.split("\n")[0].includes(words), `the entry names ${words}`);
  const js = floorJs();
  for (const words of ['"Web view too old"', '"holzWave needs Android System WebView "', '" or later."', '" This phone has version "', '"Update Android System WebView in the Play Store, then open holzWave again."']) {
    assert.ok(js.includes(words), `dist/floor.js says ${words}`);
  }
});
