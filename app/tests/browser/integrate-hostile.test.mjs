// The release gate of the integration block on the page: models that other parties' names and outputs could shape, drawn on
// both engines. A cell, a key, a title, a note and an action label that look like an element, a link or a handler are shown as
// those characters and nothing else; no raw control, direction, zero-width, format or tag character reaches the text or an
// attribute of the block; a cell is at most 240 characters; nothing leaves the page; and the page reports no error, which
// includes a Content-Security-Policy violation. The models are built here from the shapes of the chat's page gate and a few
// more, not read from a file the core writes. A model that is not the contract (3000 rows, 70 columns, a number for a cell,
// members the contract does not have, a prototype in a member) is drawn within the caps.
import assert from "node:assert/strict";
import { HOSTILE as SHAPES, HIDDEN, armGuards, domProblems, pageClean, responsive } from "./aigate-lib.mjs";
import { KUBE, answer, openBlock, podsView, rowsShown } from "./integrate-lib.mjs";

const cp = (...codes) => String.fromCodePoint(...codes);

/** The shapes that matter to a cell and are not in the chat's list: sequences, direction, format and tag characters, a long line. */
const MORE = [
  { id: "escape-sequence", text: `${HIDDEN.escape}[31mred${HIDDEN.escape}[0m ok` },
  { id: "title-sequence", text: `${HIDDEN.escape}]0;owned${cp(7)}name` },
  { id: "rlo-name", text: `${HIDDEN.bidi}gnp.exe` },
  { id: "isolates", text: `a${cp(0x2066)}b${cp(0x2069)}c${cp(0x2067)}d` },
  { id: "format", text: `a${cp(0x206f)}b${cp(0x180e)}c${cp(0xfff9)}d${cp(0x1d173)}e` },
  { id: "tag", text: `a${HIDDEN.tag}b${cp(0xe007f)}c` },
  { id: "nul", text: `a${HIDDEN.nul}b` },
  { id: "line-breaks", text: `a\r\nb\nc\rd${HIDDEN.lineSeparator}e` },
  { id: "bom", text: `${HIDDEN.byteOrderMark}name` },
  { id: "long-5000", text: "a".repeat(5000) },
  { id: "long-70000", text: "x".repeat(70000) },
  { id: "emoji", text: `a${cp(0x1f600)}${HIDDEN.zeroWidthJoiner}${cp(0x1f600)}b` },
];

const SHAPE_LIST = [...SHAPES.map((s) => ({ id: s.id, text: s.text })), ...MORE];

/** The characters the core removes: controls, separators, direction, zero-width, format and tag characters. Written as ranges. */
const RANGES = [
  [0x00, 0x1f], [0x7f, 0x9f], [0xad, 0xad], [0x600, 0x605], [0x61c, 0x61c], [0x6dd, 0x6dd], [0x70f, 0x70f], [0x890, 0x891],
  [0x8e2, 0x8e2], [0x180e, 0x180e], [0x200b, 0x200f], [0x2028, 0x202e], [0x2060, 0x206f], [0xfeff, 0xfeff], [0xfff9, 0xfffb],
  [0x110bd, 0x110bd], [0x110cd, 0x110cd], [0x13430, 0x1343f], [0x1bca0, 0x1bca3], [0x1d173, 0x1d17a], [0xe0001, 0xe0001],
  [0xe0020, 0xe007f],
];
const isHidden = (code, keepLines) => {
  if (keepLines && (code === 9 || code === 10)) return false;
  return RANGES.some(([lo, hi]) => code >= lo && code <= hi);
};
const without = (text, keepLines = false) =>
  Array.from(text)
    .filter((c) => !isHidden(c.codePointAt(0), keepLines))
    .join("");
const hiddenIn = (text, keepLines = false) => Array.from(text).find((c) => isHidden(c.codePointAt(0), keepLines)) ?? null;
/** What the block shows of a cell: the cell without the hidden characters, cut at 240 characters with an ellipsis. */
const shown = (text) => {
  const chars = Array.from(without(text));
  return chars.length <= 240 ? chars.join("") : `${chars.slice(0, 239).join("")}…`;
};

/** A text without hidden characters, cut at `max` characters (the reader's cap for a note), no ellipsis. */
const capped = (text, max, keepLines = false) => Array.from(without(text, keepLines)).slice(0, max).join("");

const BLOCK_ONLY = "img, script, a, iframe, object, embed, svg, form, input, style, link, [onerror], [onclick], [onload], [style]";

/** Everything that must hold about the block after a model is drawn. */
async function inert(page, guards, errors, what, { keepLines = false } = {}) {
  assert.deepEqual(await domProblems(page, ".block-integration", { mode: "text" }), [], `${what}: the block has problems`);
  const counts = await page.evaluate((selector) => ({
    inBlock: document.querySelectorAll(`.block-integration :is(${selector})`).length,
    inPage: document.querySelectorAll("img, iframe, object, embed").length,
  }), BLOCK_ONLY);
  assert.deepEqual(counts, { inBlock: 0, inPage: 0 }, `${what}: an element was made`);
  const { text, attributes, cells } = await page.evaluate(() => {
    const block = document.querySelector(".block-integration");
    const attributes = [];
    for (const node of block.querySelectorAll("*")) for (const a of node.attributes) attributes.push(a.value);
    const cells = [...block.querySelectorAll(".ig-cell, .ig-v, .ig-k, .ig-th")].map((n) => n.textContent ?? "");
    return { text: block.textContent ?? "", attributes, cells };
  });
  const bad = hiddenIn(text, keepLines);
  assert.equal(bad, null, `${what}: the text of the block holds U+${bad?.codePointAt(0).toString(16)}`);
  for (const value of attributes) {
    const found = hiddenIn(value);
    assert.equal(found, null, `${what}: an attribute holds U+${found?.codePointAt(0).toString(16)}`);
  }
  for (const cell of cells) assert.ok(Array.from(cell).length <= 240, `${what}: a cell has ${Array.from(cell).length} characters`);
  assert.deepEqual(await pageClean(page), [], `${what}: the page is not clean`);
  await guards.assertQuiet(what);
  await responsive(page);
  assert.deepEqual(errors, [], `${what}: the page reported errors (a Content-Security-Policy violation is one)`);
}

/** Put `view` (with a selection) in place of the pods answer, and read again. */
async function show(page, view, selection, marker) {
  await page.evaluate(([v, s]) => {
    window.__IG__.runs.pods = v;
    window.__IG__.selection = s;
  }, [answer(view, selection), selection]);
  await page.locator(".block-integration .ig-refresh").click();
  await page.waitForFunction((m) => (document.querySelector(".block-integration")?.textContent ?? "").includes(m), marker);
}

const action = (label) => ({ verb: "logs", label, effect: "read", args: { pod: "api-0" } });

const tableOf = (i, shape) => ({
  kind: "table",
  title: `t${i} ${shape}`,
  columns: [{ id: "name", label: `NAME ${shape}` }, { id: "status", label: "STATUS" }],
  rows: [
    { key: `k${i}-${shape}`, cells: [`s${i}:${shape}`, shape], tone: "warn", actions: [action(shape)] },
    { key: `k${i}-b`, cells: [shape, `s${i}:${shape}`], tone: "danger", actions: [] },
  ],
  truncated: { shown: 2, note: shape },
  note: shape,
  fetched: null,
});

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(8000);
  await openBlock(page, url, { tools: [KUBE], runs: { pods: answer(podsView()) } });
  const guards = armGuards(page, url);
  await rowsShown(page);

  await hostileModelsAreDrawnAsInertText(page, guards, errors);
  await aModelOutsideTheContractIsCutToTheCaps(page, guards, errors);

  assert.deepEqual(errors, [], "the page reported errors");
}

async function hostileModelsAreDrawnAsInertText(page, guards, errors) {
  let tables = 0;
  for (const [i, shape] of SHAPE_LIST.entries()) {
    const text = shape.text;
    // The selection of the pane is a name as well: it is drawn in the header and on the chips.
    await show(page, tableOf(i, text), { context: text, namespace: text }, `s${i}:`);
    await inert(page, guards, errors, `table ${shape.id}`);
    // What the block shows of the cell is the cell, as those characters: the markup is not interpreted.
    const first = await page.locator(".block-integration .ig-row .ig-cell").first().textContent();
    assert.ok(first === shown(`s${i}:${text}`), `table ${shape.id}: the first cell`);
    const second = await page.locator(".block-integration .ig-row .ig-cell").nth(1).textContent();
    assert.ok(second === shown(text), `table ${shape.id}: the second cell`);
    assert.equal(await page.locator(".block-integration .ig-row").count(), 2, `table ${shape.id}: two rows`);
    assert.equal(await page.locator(".block-integration .ig-note").count(), 2, `table ${shape.id}: the cut note and the note`);
    for (const note of await page.locator(".block-integration .ig-note").allTextContents()) {
      assert.ok(note === capped(text, 128), `table ${shape.id}: a note is the text without hidden characters, at most 128 characters`);
    }
    tables += 1;

    await show(
      page,
      {
        kind: "kv",
        title: `v${i} ${text}`,
        rows: [{ label: `l${i} ${text}`, value: `s${i}:${text}`, tone: "warn" }, { label: text, value: text, tone: "normal" }],
        actions: [action(text)],
        note: text,
        fetched: null,
      },
      { context: text },
      `s${i}:`,
    );
    await inert(page, guards, errors, `kv ${shape.id}`);
    assert.ok((await page.locator(".block-integration .ig-v").first().textContent()) === shown(`s${i}:${text}`), `kv ${shape.id}: the first value`);

    await show(
      page,
      { kind: "text", title: `x${i} ${text}`, text: `s${i}:\n${text}\n\tindented`, format: "plain", truncated: false, note: text, fetched: null },
      {},
      `s${i}:`,
    );
    await inert(page, guards, errors, `text ${shape.id}`, { keepLines: true });
    assert.ok(
      (await page.locator(".block-integration .ig-text").textContent()) === capped(`s${i}:\n${text}\n\tindented`, 65536, true),
      `text ${shape.id}: the text is the text without hidden characters`,
    );
  }
  assert.ok(tables >= 20, `${tables} shapes were drawn, at least 20 are asserted`);
  console.log(`integrate-hostile: ${tables} shapes drawn as a table, a list and a text, each in the cells, keys, titles, notes, labels and the selection`);

  // The checker can fail: an element put into the block by hand is found, and a hidden character in an attribute is found.
  await page.evaluate(() => {
    const p = document.createElement("p");
    p.innerHTML = "<embed>";
    p.className = "probe";
    document.querySelector(".block-integration .ig-body").append(p);
  });
  const problems = await domProblems(page, ".block-integration", { mode: "text" });
  assert.ok(problems.some((l) => l.startsWith("forbidden-element")), `the checker did not find an embedded object: ${problems.join(" | ")}`);
  assert.equal(await page.locator(".block-integration embed").count(), 1, "the probe element is there");
  await page.evaluate(() => document.querySelector(".block-integration .probe").remove());
  assert.equal(hiddenIn(`a${HIDDEN.bidi}b`), HIDDEN.bidi, "the hidden-character check finds a direction mark");
  assert.equal(hiddenIn("a\nb", true), null, "a line feed is allowed in a text");
  assert.equal(hiddenIn("a\nb"), "\n", "a line feed is not allowed in a cell");
  await responsive(page);
}

async function aModelOutsideTheContractIsCutToTheCaps(page, guards, errors) {
  // 3000 rows: the reader keeps 2000.
  await page.evaluate(() => {
    const columns = ["a", "b", "c"].map((id) => ({ id, label: id.toUpperCase() }));
    const rows = Array.from({ length: 3000 }, (_, i) => ({ key: `r${i}`, cells: [`rows3000-${i}`, "x", "y"], tone: "normal", actions: [] }));
    window.__IG__.runs.pods = {
      view: { kind: "table", title: "rows", columns, rows, truncated: null, note: null, fetched: null },
      selection: { selection: { context: "prod" }, production: null },
      fetched: null,
    };
  });
  await page.locator(".block-integration .ig-refresh").click();
  await page.waitForFunction(() => (document.querySelector(".block-integration")?.textContent ?? "").includes("rows3000-0"));
  assert.equal(await page.locator(".block-integration .ig-row").count(), 2000, "3000 rows are drawn as 2000");
  assert.ok(!(await page.locator(".block-integration").textContent()).includes("rows3000-2000"), "the 2001st row is not drawn");
  await inert(page, guards, errors, "3000 rows");

  // 70 columns: the reader keeps 64, and a row of fewer cells is padded.
  await page.evaluate(() => {
    const columns = Array.from({ length: 70 }, (_, i) => ({ id: `c${i}`, label: `COL${i}` }));
    const rows = [
      { key: "full", cells: columns.map((_, i) => `wide${i}`), tone: "normal", actions: [] },
      { key: "short", cells: ["only"], tone: "normal", actions: [] },
      { key: "long", cells: Array.from({ length: 90 }, (_, i) => `more${i}`), tone: "normal", actions: [] },
    ];
    window.__IG__.runs.pods.view = { kind: "table", title: "wide", columns, rows, truncated: null, note: null, fetched: null };
  });
  await page.locator(".block-integration .ig-refresh").click();
  await page.waitForFunction(() => (document.querySelector(".block-integration")?.textContent ?? "").includes("COL0"));
  assert.equal(await page.locator(".block-integration .ig-th").count(), 64, "70 columns are drawn as 64");
  assert.equal(await page.locator(".block-integration .ig-row").nth(1).locator(".ig-cell").count(), 64, "a short row is padded to the columns");
  assert.equal(await page.locator(".block-integration .ig-row").nth(2).locator(".ig-cell").count(), 64, "a long row is cut to the columns");
  await inert(page, guards, errors, "70 columns");

  // A number, null, an object and an array in place of a cell are empty cells; members the contract does not have are not kept;
  // a prototype in a member changes nothing.
  await page.evaluate(() => {
    const model = JSON.parse(
      '{"kind":"table","title":"odd","columns":[{"id":"a","label":"A"},{"id":"b","label":"B"},{"id":"c","label":"C"},{"id":"d","label":"D"}],' +
        '"rows":[{"key":"odd1","cells":[42,null,{"x":"<img src=x onerror=alert(1)>"},["y"]],"tone":"loud","actions":[],"onclick":"alert(1)","__proto__":{"polluted":true}},' +
        '{"key":7,"cells":["skipped"]},{"cells":["no key"]},"a string","skipped too",null,' +
        '{"key":"odd2","cells":["ok","ok","ok","ok"],"tone":"warn","actions":[{"verb":"logs","label":"L","effect":"read","args":{"a":1}},' +
        '{"verb":"logs","label":"M","effect":"nope","args":{}},{"verb":"logs","label":"N","effect":"read","args":{"p":"v"}}]}],' +
        '"truncated":{"shown":"many","note":5},"note":7,"fetched":-1,"constructor":{"prototype":{"polluted":true}},"__proto__":{"polluted":true}}',
    );
    window.__IG__.runs.pods.view = model;
  });
  await page.locator(".block-integration .ig-refresh").click();
  await page.waitForFunction(() => (document.querySelector(".block-integration")?.textContent ?? "").includes("odd2") || document.querySelector(".block-integration .ig-row"));
  await page.waitForFunction(() => [...document.querySelectorAll(".block-integration .ig-cell")].some((n) => n.textContent === "ok"));
  const rows = await page.locator(".block-integration .ig-row").evaluateAll((trs) => trs.map((tr) => [...tr.querySelectorAll(".ig-cell")].map((td) => td.textContent)));
  assert.equal(rows.length, 2, "only the rows with a text key and a list of cells are drawn");
  assert.deepEqual(rows[0].slice(0, 4), ["", "", "", ""], "a number, null, an object and an array are empty cells");
  assert.deepEqual(rows[1].slice(0, 4), ["ok", "ok", "ok", "ok"]);
  assert.equal(await page.locator(".block-integration .ig-row").first().getAttribute("onclick"), null, "a member the contract does not have is not kept");
  assert.equal(await page.locator(".block-integration .ig-row").first().getAttribute("data-tone"), null, "an unknown tone is the normal one");
  assert.equal(await page.evaluate(() => ({}).polluted === undefined && Object.prototype.polluted === undefined), true, "the prototype is not changed");
  await inert(page, guards, errors, "members outside the contract");

  // A cell of 5000 characters and a text of 100000 characters: the cell is cut at 240 with the full text in the tooltip (at most
  // 4096), and the text is cut at 65536.
  await page.evaluate(() => {
    window.__IG__.runs.pods.view = {
      kind: "table", title: "long", columns: [{ id: "a", label: "A" }],
      rows: [{ key: "long", cells: ["L".repeat(5000)], tone: "normal", actions: [] }], truncated: null, note: null, fetched: null,
    };
  });
  await page.locator(".block-integration .ig-refresh").click();
  await page.waitForFunction(() => (document.querySelector(".block-integration")?.textContent ?? "").includes("LLLL"));
  const cell = page.locator(".block-integration .ig-cell").first();
  assert.equal(Array.from((await cell.textContent()) ?? "").length, 240, "a cell is shown at 240 characters");
  assert.ok(((await cell.textContent()) ?? "").endsWith("…"), "the cut ends in an ellipsis");
  assert.equal(Array.from((await cell.getAttribute("title")) ?? "").length, 4096, "the tooltip holds at most 4096 characters");
  await inert(page, guards, errors, "a long cell");
  await page.evaluate(() => {
    window.__IG__.runs.pods.view = { kind: "text", title: "t", text: "T".repeat(100000), format: "plain", truncated: false, note: null, fetched: null };
  });
  await page.locator(".block-integration .ig-refresh").click();
  await page.waitForSelector(".block-integration .ig-text");
  assert.equal(Array.from((await page.locator(".block-integration .ig-text").textContent()) ?? "").length, 65536, "a text is cut at 65536 characters");
  await inert(page, guards, errors, "a long text");

  // Things that are no view at all leave the block without rows and the page alive: a string, a number, an array, an unknown kind.
  for (const [what, view] of [["a string", "text"], ["a number", 7], ["an array", [1, 2]], ["an unknown kind", { kind: "html", html: "<img src=x onerror=alert(1)>" }]]) {
    await page.evaluate((v) => {
      window.__IG__.runs.pods = { view: v, selection: { selection: {}, production: null }, fetched: null };
    }, view);
    await page.locator(".block-integration .ig-refresh").click();
    await page.waitForTimeout(150);
    assert.equal(await page.locator(".block-integration .ig-row").count(), 0, `${what}: no rows`);
    await inert(page, guards, errors, what);
  }
}
