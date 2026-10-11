// Scrollback search through the core (DESK-21, F6): Cmd+F opens a bar in the focused pane, the needle is sent
// after a quiet period, Enter and Shift+Enter walk the matches, the counter and the highlights come from the
// frame header, and no key typed in the bar reaches the terminal.
import assert from "node:assert/strict";
import { callsOf, emit } from "./lib.mjs";

const BG = 0x14091a;
const MODES = { alt: false, bracketed_paste: false, mouse: false };

/** A frame message for `pane`: 20 x 4 cells with the search fields of the header set from `over`. */
const frame = (pane, seq, over = {}) => ({
  pane, seq, full: false, cols: 20, rows: 4, fg: 0xf6efe6, bg: BG, cursor: null, scrollbar: [0, 4, 4],
  selection: [], matches: [], search: null, copy_cursor: null, modes: MODES, changed: [], ...over,
});
const fullFrame = (pane, seq, over = {}) => frame(pane, seq, { full: true, ...over });

/** Open the page against the stub with a mirror for both panes (pane 2 is focused, the right half). */
async function open(page, url) {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(() => document.getElementById("term").focus());
  await emit(page, "frame", fullFrame(1, 1));
  await emit(page, "frame", fullFrame(2, 1));
}

const searchOps = async (page) => (await callsOf(page, "search")).map((a) => ({ pane: a.pane, ...a.op }));
const bar = (page) => page.getByLabel("Search scrollback");
const counter = (page) => page.locator(".search-count").textContent();
const reset = (page) => page.evaluate(() => window.__MOCK__.reset());

/** Open the bar with the shortcut and wait until its input exists and has the keyboard. */
async function openBar(page) {
  await page.keyboard.press("Meta+f");
  await page.waitForFunction(() => document.activeElement?.getAttribute("aria-label") === "Search scrollback");
}

/** The calls that would send something to the shell. */
const shellCalls = (page) =>
  page.evaluate(() => window.__MOCK__.calls.filter(([c]) => ["key", "text", "paste"].includes(c)).map(([c]) => c));

export default async function ({ page, url, errors }) {
  await openCase(page, url);
  await debounceCase(page, url);
  await emptyAndTrimCase(page, url);
  await clipCase(page, url);
  await navigateCase(page, url);
  await counterCase(page, url);
  await highlightCase(page, url);
  await closeCase(page, url);
  await moveCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** Cmd+F shows the labelled bar in the focused pane, top right, and the input has the keyboard. */
async function openCase(page, url) {
  await open(page, url);
  assert.equal(await page.locator(".search-bar").count(), 0, "no bar before the shortcut");
  await openBar(page);

  const input = bar(page);
  assert.equal(await input.count(), 1, "one input labelled Search scrollback");
  assert.equal(await input.evaluate((n) => n.tagName), "INPUT");
  assert.ok(await page.locator("#stage .search-bar").isVisible(), "the bar is inside the stage and visible");
  for (const label of ["Previous match", "Next match", "Close search"]) {
    assert.equal(await page.getByLabel(label).count(), 1, `a "${label}" button`);
  }
  assert.equal(await counter(page), "0/0", "no search, no matches");

  // The bar sits in the focused pane (the right half of the stub's layout), at its top right.
  const geo = await page.evaluate(() => {
    const c = document.getElementById("term").getBoundingClientRect();
    const b = document.querySelector(".search-bar").getBoundingClientRect();
    return { cLeft: c.left, cTop: c.top, cW: c.width, cH: c.height, left: b.left, right: b.right, top: b.top, bottom: b.bottom };
  });
  assert.ok(geo.left >= geo.cLeft + geo.cW / 2, `the bar is in the right pane (left ${geo.left})`);
  assert.ok(geo.right <= geo.cLeft + geo.cW + 0.5, "the bar does not leave the pane");
  assert.ok(geo.right >= geo.cLeft + geo.cW - 60, `the bar is at the top right (right ${geo.right})`);
  assert.ok(geo.top >= geo.cTop + 20, "the bar is below the pane header");
  assert.ok(geo.top < geo.cTop + 80, `the bar is at the top (top ${geo.top})`);
  assert.equal(await page.evaluate(() => document.activeElement.getAttribute("aria-label")), "Search scrollback");
}

/** The needle goes out once, whole, after the quiet period; nothing goes out per keystroke. */
async function debounceCase(page, url) {
  await open(page, url);
  await openBar(page);
  await reset(page);
  await page.keyboard.type("abc", { delay: 10 });
  assert.deepEqual(await searchOps(page), [], "nothing is sent while the typing goes on");
  await page.waitForFunction(() => window.__MOCK__.callsOf("search").length >= 1);
  await page.waitForTimeout(300);
  assert.deepEqual(await searchOps(page), [{ pane: 2, kind: "set", needle: "abc" }], "one set with the whole needle");
  assert.deepEqual(await shellCalls(page), [], "typing in the bar reaches no program");

  // A burst that changes its mind inside the quiet period sends only the end of it.
  await reset(page);
  await page.keyboard.type("def", { delay: 5 });
  await page.keyboard.press("Backspace");
  await page.waitForTimeout(400);
  const ops = await searchOps(page);
  assert.deepEqual(ops, [{ pane: 2, kind: "set", needle: "abcde" }], `one set after the burst, got ${JSON.stringify(ops)}`);
}

/** The needle is trimmed; an empty one clears the search. */
async function emptyAndTrimCase(page, url) {
  await open(page, url);
  await openBar(page);
  await reset(page);
  await page.keyboard.type("  hi there  ");
  await page.waitForFunction(() => window.__MOCK__.callsOf("search").length >= 1);
  assert.deepEqual(await searchOps(page), [{ pane: 2, kind: "set", needle: "hi there" }], "leading and trailing blanks are cut");

  await reset(page);
  await bar(page).fill("");
  await page.waitForFunction(() => window.__MOCK__.callsOf("search").length >= 1);
  await page.waitForTimeout(250);
  assert.deepEqual(await searchOps(page), [{ pane: 2, kind: "clear" }], "an empty needle sends clear, once");

  // Blanks only are empty too.
  await reset(page);
  await page.keyboard.type("x");
  await page.waitForFunction(() => window.__MOCK__.callsOf("search").length >= 1);
  await reset(page);
  await page.keyboard.press("Backspace");
  await page.keyboard.type("   ");
  await page.waitForFunction(() => window.__MOCK__.callsOf("search").length >= 1);
  assert.deepEqual((await searchOps(page)).map((o) => o.kind), ["clear"], "blanks only is an empty needle");
}

/** A needle over 1024 bytes is cut on a character boundary before it is sent. */
async function clipCase(page, url) {
  await open(page, url);
  await openBar(page);
  await reset(page);
  await bar(page).fill("é".repeat(700)); // 1400 bytes
  await page.waitForFunction(() => window.__MOCK__.callsOf("search").length >= 1);
  const [op] = await searchOps(page);
  assert.equal(op.kind, "set");
  assert.ok(Buffer.byteLength(op.needle, "utf8") <= 1024, `${Buffer.byteLength(op.needle, "utf8")} bytes were sent`);
  assert.equal(op.needle, "é".repeat(512), "cut after whole characters, not in the middle of one");
}

/** Enter is next, Shift+Enter is previous; the buttons do the same; Enter on a needle not yet sent sends it. */
async function navigateCase(page, url) {
  await open(page, url);
  await openBar(page);
  await page.keyboard.type("foo");
  await page.waitForFunction(() => window.__MOCK__.callsOf("search").length >= 1);
  await page.waitForTimeout(50);
  await reset(page);

  await page.keyboard.press("Enter");
  await page.keyboard.press("Shift+Enter");
  await page.getByLabel("Next match").click();
  await page.getByLabel("Previous match").click();
  assert.deepEqual(
    await searchOps(page),
    [{ pane: 2, kind: "next" }, { pane: 2, kind: "prev" }, { pane: 2, kind: "next" }, { pane: 2, kind: "prev" }],
    "Enter and the next button go down, Shift+Enter and the previous button go up",
  );
  assert.equal(await page.evaluate(() => document.activeElement.getAttribute("aria-label")), "Search scrollback", "the input keeps the keyboard after a button");
  assert.deepEqual(await shellCalls(page), [], "Enter in the bar is not a key for the program");

  // Enter before the quiet period is over sends the needle first.
  await reset(page);
  await page.keyboard.type("bar");
  await page.keyboard.press("Enter");
  const ops = await searchOps(page);
  assert.deepEqual(ops, [{ pane: 2, kind: "set", needle: "foobar" }], "Enter sends a needle that was not sent yet, and no next");
  await page.waitForTimeout(300);
  assert.equal((await searchOps(page)).length, 1, "the quiet timer does not send it a second time");

  // The palette actions exist.
  await reset(page);
  await page.evaluate(async () => {
    const { runAction } = await import(new URL("actions.js", location.href).href);
    runAction("search.next");
    runAction("search.prev");
  });
  assert.deepEqual((await searchOps(page)).map((o) => o.kind), ["next", "prev"], "search.next and search.prev are actions");
}

/** The counter reads current/total, 1-based, from the focused pane's frame header and follows later frames. */
async function counterCase(page, url) {
  await open(page, url);
  await openBar(page);
  assert.equal(await counter(page), "0/0");

  await emit(page, "frame", frame(2, 2, { search: { total: 7, current: 2 } }));
  assert.equal(await counter(page), "3/7", "current 2 in reading order is the third of seven");
  await emit(page, "frame", frame(2, 3, { search: { total: 9, current: 8 } }));
  assert.equal(await counter(page), "9/9", "a later frame changes the total and the place");
  await emit(page, "frame", frame(2, 4, { search: { total: 0, current: null } }));
  assert.equal(await counter(page), "0/0", "no matches");
  await emit(page, "frame", frame(2, 5, { search: { total: 4, current: null } }));
  assert.equal(await counter(page), "0/4", "matches but no current one");
  await emit(page, "frame", frame(2, 6, { search: null }));
  assert.equal(await counter(page), "0/0", "a header without a search");

  // Another pane's header does not change the counter.
  await emit(page, "frame", frame(1, 2, { search: { total: 50, current: 10 } }));
  assert.equal(await counter(page), "0/0", "pane 1 is not the focused pane");
}

/** Match spans of the focused pane are boxes; the current one has an outline besides its colour. */
async function highlightCase(page, url) {
  await open(page, url);
  const boxes = () =>
    page.evaluate(() =>
      [...document.querySelectorAll("#overlay .match")].map((n) => {
        const r = n.getBoundingClientRect();
        const s = getComputedStyle(n);
        return {
          x: r.left, y: r.top, w: r.width, h: r.height, cls: n.className,
          outline: s.outlineStyle === "none" ? 0 : parseFloat(s.outlineWidth), background: s.backgroundColor,
        };
      }),
    );
  assert.deepEqual(await boxes(), [], "no search, no boxes");

  await emit(page, "frame", frame(2, 2, { matches: [[1, 2, 5, false], [2, 0, 3, true]], search: { total: 2, current: 1 } }));
  let b = await boxes();
  assert.equal(b.length, 2, "one box per match");
  const plain = b.find((x) => !/\bcurrent\b/.test(x.cls));
  const current = b.find((x) => /\bcurrent\b/.test(x.cls));
  assert.ok(plain && current, "one plain and one current box");
  assert.equal(plain.outline, 0, "a plain match has no outline");
  assert.ok(current.outline >= 2, `the current match has an outline of 2 px or more (${current.outline})`);
  assert.notEqual(plain.background, "rgba(0, 0, 0, 0)", "a match is filled");

  // [2, 5] and [0, 3] are four columns each; the second is one row lower and two columns to the left.
  assert.ok(Math.abs(plain.w - current.w) < 0.6, "the width is the span in cells");
  assert.ok(current.y > plain.y, "row 2 is below row 1");
  assert.ok(plain.x > current.x, "column 2 is right of column 0");

  // Matches in an unfocused pane draw nothing; matches that leave with the frame leave the page.
  await emit(page, "frame", frame(1, 2, { matches: [[0, 0, 3, true]] }));
  assert.equal((await boxes()).length, 2, "pane 1 is not focused");
  await emit(page, "frame", frame(2, 3, { matches: [], search: null }));
  assert.deepEqual(await boxes(), [], "the boxes are removed with the matches");

  // Spans that are not on the grid draw nothing and throw nothing (the page reports errors at the end).
  await emit(page, "frame", frame(2, 4, { matches: [[99, 0, 3, false], ["a", 0, 1, false], [1, 0, 9999, true]] }));
  b = await boxes();
  assert.equal(b.length, 1, "only the span that can be clamped is drawn");
}

/** Escape and the close button clear the search, hide the bar and give the keyboard back to the terminal. */
async function closeCase(page, url) {
  await open(page, url);
  await openBar(page);
  await page.keyboard.type("zz");
  await page.waitForFunction(() => window.__MOCK__.callsOf("search").length >= 1);
  await reset(page);

  await page.keyboard.press("Escape");
  assert.deepEqual(await searchOps(page), [{ pane: 2, kind: "clear" }], "Escape clears the search");
  assert.equal(await page.locator(".search-bar").isVisible(), false, "the bar is hidden");
  assert.equal(await page.evaluate(() => document.activeElement.id), "ime", "the terminal input has the keyboard");
  assert.deepEqual(await shellCalls(page), [], "Escape closed the bar and was not sent to the program");

  // With the bar closed the terminal has its keys again.
  await page.keyboard.press("a");
  assert.ok((await shellCalls(page)).includes("key"), "a key goes to the program once the bar is closed");

  // The close button does the same.
  await openBar(page);
  await reset(page);
  await page.getByLabel("Close search").click();
  assert.deepEqual(await searchOps(page), [{ pane: 2, kind: "clear" }]);
  assert.equal(await page.locator(".search-bar").isVisible(), false);
  assert.equal(await page.evaluate(() => document.activeElement.id), "ime");

  // Reopening shows the last needle, selected, and searches it again.
  await reset(page);
  await openBar(page);
  await page.waitForFunction(() => window.__MOCK__.callsOf("search").length >= 1);
  assert.deepEqual(await searchOps(page), [{ pane: 2, kind: "set", needle: "zz" }], "the needle comes back and is searched");
  assert.equal(await bar(page).inputValue(), "zz");
}

/** Opening the bar on another pane moves it there and ends the search of the old pane. */
async function moveCase(page, url) {
  await open(page, url);
  await openBar(page);
  await page.keyboard.type("moved");
  await page.waitForFunction(() => window.__MOCK__.callsOf("search").length >= 1);
  await reset(page);

  const left = await page.evaluate(() => {
    const v = window.__MOCK__.view();
    v.focus = 1;
    window.__MOCK__.setView(v);
    return window.__MOCK__.emit("layout", v);
  });
  assert.ok(left >= 1, "the page listens to layout events");
  await page.waitForFunction(() => document.querySelector(".search-bar")?.offsetParent === null);
  assert.deepEqual(await searchOps(page), [{ pane: 2, kind: "clear" }], "the search of the pane that lost the focus ended");

  await openBar(page);
  await page.waitForFunction(() => window.__MOCK__.callsOf("search").length >= 2);
  assert.deepEqual(
    await searchOps(page),
    [{ pane: 2, kind: "clear" }, { pane: 1, kind: "set", needle: "moved" }],
    "the same needle is searched in the new pane",
  );
  const geo = await page.evaluate(() => {
    const c = document.getElementById("term").getBoundingClientRect();
    const b = document.querySelector(".search-bar").getBoundingClientRect();
    return { mid: c.left + c.width / 2, right: b.right };
  });
  assert.ok(geo.right <= geo.mid + 0.5, "the bar is in the left pane now");
}
