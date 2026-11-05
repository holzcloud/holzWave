// The frame patcher against the golden sequence the Rust tests write (tests/fixtures/frames.json): every step is
// applied in order to one mirror and the grid it shows must equal the grid of the engine. Runs on dist/ like the
// other unit tests; nothing is imported from the DOM side of the app.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { setFlagsFromString } from "node:v8";
import { runInNewContext } from "node:vm";
import {
  RateMeter, applyFrame, decorationSignature, forgetPane, gridText, rowCells, spansSignature, toRenderFrame,
  visibleMatches, visibleSpans,
} from "../../dist/frames.js";

const fixture = JSON.parse(readFileSync(new URL("../fixtures/frames.json", import.meta.url), "utf8"));

/** A minimal full frame for pane 1 with `rows` rows of `text`. */
const full = (over = {}) => ({
  pane: 1, seq: 1, full: true, cols: 10, rows: 4, fg: 0xffffff, bg: 0,
  cursor: null, scrollbar: [0, 4, 4], selection: [], matches: [], search: null, copy_cursor: null,
  modes: { alt: false, bracketed_paste: false, mouse: false }, changed: [], ...over,
});
const row = (y, text, extras) => ({ y, runs: [[text, text.length, 0xffffff, 0, 0]], ...(extras ? { extras } : {}) });

test("every step of the golden sequence patches the mirror to the engine's grid", () => {
  assert.equal(fixture.version, 1);
  assert.ok(fixture.steps.length >= 19, `only ${fixture.steps.length} steps`);
  let mirror;
  for (const step of fixture.steps) {
    if (step.frame) {
      const out = applyFrame(mirror, step.frame);
      assert.equal(out.gap, false, `${step.name}: the golden sequence has no gap`);
      mirror = out.mirror;
    }
    assert.ok(mirror, `${step.name}: no mirror`);
    assert.deepEqual(gridText(mirror), step.grid, `${step.name}: the grid differs`);
  }
});

test("the header of every golden frame is the mirror's header", () => {
  let mirror;
  for (const step of fixture.steps) {
    const f = step.frame;
    if (!f) continue;
    mirror = applyFrame(mirror, f).mirror;
    assert.equal(mirror.seq, f.seq, step.name);
    assert.equal(mirror.cols, f.cols, step.name);
    assert.equal(mirror.rows, f.rows, step.name);
    assert.deepEqual(mirror.cursor, f.cursor, step.name);
    assert.deepEqual(mirror.scrollbar, f.scrollbar, step.name);
    assert.deepEqual(mirror.selection, f.selection, step.name);
    assert.deepEqual(mirror.matches, f.matches, step.name);
    assert.deepEqual(mirror.search, f.search, step.name);
    assert.deepEqual(mirror.copyCursor, f.copy_cursor, step.name);
    assert.deepEqual(mirror.modes, f.modes, step.name);
    assert.equal(mirror.fg, f.fg, step.name);
    assert.equal(mirror.bg, f.bg, step.name);
  }
});

test("a frame that does not follow the sequence is a gap and leaves the mirror unchanged", () => {
  const first = applyFrame(undefined, full({ changed: [row(0, "abc")] })).mirror;
  const before = structuredClone(first);

  const skipped = applyFrame(first, full({ full: false, seq: 3, changed: [row(0, "zzz")] }));
  assert.equal(skipped.gap, true, "seq 3 after 1");
  assert.equal(skipped.mirror, first, "the same mirror comes back");
  assert.deepEqual(first, before);

  for (const seq of [1, 0, -1, 1.5, NaN]) {
    const out = applyFrame(first, full({ full: false, seq, changed: [row(0, "zzz")] }));
    assert.equal(out.gap, true, `seq ${seq} after 1`);
    assert.deepEqual(out.mirror, before);
  }
  const next = applyFrame(first, full({ full: false, seq: 2, changed: [row(1, "def")] }));
  assert.equal(next.gap, false, "seq 2 after 1 patches");
  assert.deepEqual(gridText(next.mirror), ["abc", "def", "", ""]);
});

test("a frame that is not full for an unknown pane is a gap", () => {
  const out = applyFrame(undefined, full({ full: false, seq: 1, changed: [row(0, "abc")] }));
  assert.equal(out.gap, true);
  assert.equal(out.mirror, undefined);
});

test("a non-full frame of another size is a gap: it cannot be patched", () => {
  const first = applyFrame(undefined, full()).mirror;
  assert.equal(applyFrame(first, full({ full: false, seq: 2, cols: 12 })).gap, true);
  assert.equal(applyFrame(first, full({ full: false, seq: 2, rows: 5 })).gap, true);
});

test("a full frame always replaces the mirror and restarts the sequence", () => {
  let mirror = applyFrame(undefined, full({ seq: 1, changed: [row(0, "old"), row(2, "keep?")] })).mirror;
  mirror = applyFrame(mirror, full({ full: false, seq: 2, changed: [row(1, "mid")] })).mirror;
  assert.equal(mirror.seq, 2);

  const out = applyFrame(mirror, full({ seq: 7, cols: 8, rows: 3, changed: [row(0, "new")] }));
  assert.equal(out.gap, false);
  assert.equal(out.mirror.seq, 7);
  assert.equal(out.mirror.cols, 8);
  assert.equal(out.mirror.rows, 3);
  assert.deepEqual(gridText(out.mirror), ["new", "", ""], "rows a full frame does not send are blank");

  const after = applyFrame(out.mirror, full({ full: false, seq: 8, cols: 8, rows: 3, changed: [row(2, "x")] }));
  assert.equal(after.gap, false);
  assert.deepEqual(gridText(after.mirror), ["new", "", "x"]);

  const fromNothing = applyFrame(undefined, full({ seq: 41, changed: [row(0, "a")] }));
  assert.equal(fromNothing.gap, false);
  assert.equal(fromNothing.mirror.seq, 41);
});

test("applying a frame does not change the mirror it was applied to", () => {
  const first = applyFrame(undefined, full({ changed: [row(0, "abc")] })).mirror;
  const before = structuredClone(first);
  const next = applyFrame(first, full({ full: false, seq: 2, changed: [row(0, "xyz")] })).mirror;
  assert.deepEqual(first, before);
  assert.notEqual(next, first);
  assert.deepEqual(gridText(next)[0], "xyz");
});

test("rows outside the grid and malformed rows are ignored without throwing", () => {
  const first = applyFrame(undefined, full()).mirror;
  const hostile = [
    row(4, "below"), row(99999, "far"), row(-1, "negative"), row(1.5, "fraction"), row(NaN, "nan"),
    { y: 0, runs: "not a list" }, { y: 1 }, null, 7, "row",
    row(2, "ok"),
  ];
  const out = applyFrame(first, full({ full: false, seq: 2, changed: hostile }));
  assert.equal(out.gap, false);
  assert.deepEqual(gridText(out.mirror), ["", "", "ok", ""]);
});

test("a header that is not a grid is ignored and allocates nothing", () => {
  const first = applyFrame(undefined, full({ changed: [row(0, "abc")] })).mirror;
  for (const size of [{ cols: 0 }, { rows: -1 }, { cols: 1e9, rows: 1e9 }, { cols: 2.5 }, { cols: NaN }, { rows: 2 ** 40 }]) {
    const out = applyFrame(first, full({ seq: 5, ...size }));
    assert.equal(out.mirror, first, JSON.stringify(size));
    assert.equal(out.gap, false);
  }
  const fromNothing = applyFrame(undefined, full({ cols: 1e9, rows: 1e9 }));
  assert.equal(fromNothing.mirror, undefined);
});

test("toRenderFrame has the grid size, blank rows as empty lines, extras by row and the focus flag", () => {
  const msg = full({
    cols: 6, rows: 3, cursor: { x: 1, y: 0, style: "bar", blink: true },
    changed: [row(0, "a\0 b", [[0, "́"]]), row(2, "cd")],
  });
  const mirror = applyFrame(undefined, msg).mirror;

  const f = toRenderFrame(mirror, true);
  assert.equal(f.cols, 6);
  assert.equal(f.rows, 3);
  assert.equal(f.focused, true);
  assert.deepEqual(f.cursor, { x: 1, y: 0, style: "bar", blink: true });
  assert.equal(f.fg, 0xffffff);
  assert.equal(f.bg, 0);
  assert.equal(f.lines.length, 3);
  assert.deepEqual(f.lines[0], [["a\0 b", 4, 0xffffff, 0, 0]]);
  assert.deepEqual(f.lines[1], [], "a row no frame sent is an empty line");
  assert.deepEqual(f.lines[2], [["cd", 2, 0xffffff, 0, 0]]);
  assert.deepEqual(f.extras, [[[0, "́"]], [], []]);
  assert.equal(toRenderFrame(mirror, false).focused, false);
});

test("the render frame of the cluster step carries the tail on the cell of its row", () => {
  let mirror;
  for (const step of fixture.steps) {
    if (step.frame) mirror = applyFrame(mirror, step.frame).mirror;
    if (step.name === "emoji with clustering") break;
  }
  const f = toRenderFrame(mirror, true);
  assert.equal(f.extras.length, f.rows);
  assert.deepEqual(f.extras[4], [[0, "‍👩‍👧"]]);
  assert.deepEqual(f.extras[0], []);
});

test("gridText decodes spacers, tails and trailing blanks", () => {
  const mirror = applyFrame(
    undefined,
    full({ cols: 8, rows: 2, changed: [row(0, "日\0本\0!  ", [[4, "̈"]]), row(1, "   ")] }),
  ).mirror;
  assert.deepEqual(gridText(mirror), ["日本!̈", ""]);
});

test("spansSignature is equal for equal spans and different for any difference", () => {
  assert.ok(spansSignature([]) === "", "an empty list has the empty signature");
  assert.ok(spansSignature(undefined) === "", "a missing list has the empty signature");
  const a = spansSignature([[1, 0, 3]]);
  assert.ok(a !== "", "a span has a signature");
  assert.ok(spansSignature([[1, 0, 3]]) === a, "equal lists, equal signatures");
  for (const other of [[[1, 0, 4]], [[1, 1, 3]], [[2, 0, 3]], [[1, 0, 3], [2, 0, 3]], [[2, 0, 3], [1, 0, 3]]]) {
    assert.ok(spansSignature(other) !== a, `${JSON.stringify(other)} has the signature of [[1, 0, 3]]`);
  }
  // The row boundary is not ambiguous: [1, 20, 3] and [12, 0, 3] must not collide.
  assert.ok(spansSignature([[1, 20, 3]]) !== spansSignature([[12, 0, 3]]), "the fields run together");
});

test("visibleSpans keeps spans inside the grid and clamps their ends", () => {
  const spans = [
    [1, 0, 3], [2, 5, 99], [3, -4, 2], [4, 0, 0], // kept or clamped; row 4 is outside a 4-row grid
    [0, 5, 2], [-1, 0, 3], [1.5, 0, 3], [0, NaN, 3], [0, 0, Infinity], "row", null, [1, 2],
  ];
  const mirror = applyFrame(undefined, full({ cols: 10, rows: 4, selection: spans })).mirror;
  const got = JSON.stringify(visibleSpans(mirror));
  assert.ok(got === JSON.stringify([[1, 0, 3], [2, 5, 9], [3, 0, 2]]), `visibleSpans gave ${got}`);
});

test("visibleSpans of a mirror without a selection is empty", () => {
  assert.deepEqual(visibleSpans(applyFrame(undefined, full()).mirror), []);
});

test("forgetPane deletes every entry of the pane and records the exit", () => {
  const stores = () => ({
    frames: new Map([[1, "a"], [2, "b"]]),
    sizes: new Map([[1, "a"], [2, "b"]]),
    titles: new Map([[1, "vim"], [2, "zsh"]]),
    cwds: new Map([[1, "/a"], [2, "/b"]]),
    exited: new Set(),
  });
  const s = stores();
  forgetPane(s, 2);
  for (const name of ["frames", "sizes", "titles", "cwds"]) {
    assert.ok(!s[name].has(2), `${name} still has pane 2`);
    assert.ok(s[name].has(1), `${name} lost pane 1`);
  }
  assert.deepEqual([...s.exited], [2]);

  forgetPane(s, 2); // twice is the same as once
  forgetPane(s, 99); // a pane nobody heard of
  assert.deepEqual([...s.exited].sort((a, b) => a - b), [2, 99]);
  assert.equal(s.frames.size, 1);
});

test("RateMeter counts the events of the last window and scales to one second", () => {
  const m = new RateMeter(1000);
  assert.equal(m.perSecond(0), 0);
  for (const t of [0, 100, 200]) m.note(t);
  assert.equal(m.perSecond(250), 3);
  assert.equal(m.perSecond(1100), 2, "the event at 0 is out of the window, 100 is on its edge");
  assert.equal(m.perSecond(1150), 1);
  assert.equal(m.perSecond(5000), 0);

  const half = new RateMeter(500);
  half.note(0);
  half.note(100);
  assert.equal(half.perSecond(200), 4, "two events in half a second is four per second");

  const burst = new RateMeter(1000);
  for (let i = 0; i < 500; i++) burst.note(i); // 500 events in 500 ms
  assert.equal(burst.perSecond(499), 500);
});

// ---- search matches, the overlay signature and per-cell rows (plan 21) ----------------------------------------

test("visibleMatches keeps whole-number spans on the grid, clamps the ends and the current flag is true or false", () => {
  const mirror = applyFrame(undefined, full({
    matches: [[1, 2, 5, false], [2, -3, 99, true], [9, 0, 1, false], ["a", 0, 1, false], [0, 4, 2, false], [3, 0, 1, 1]],
  })).mirror;
  assert.deepEqual(visibleMatches(mirror), [[1, 2, 5, false], [2, 0, 9, true], [3, 0, 1, false]]);
});

test("decorationSignature differs when the matches, the counter or the copy cursor differ and not otherwise", () => {
  const base = applyFrame(undefined, full()).mirror;
  const sig = (over) => decorationSignature(applyFrame(undefined, full(over)).mirror);
  assert.equal(decorationSignature(base), sig({}), "equal headers, equal signatures");
  const variants = [
    { matches: [[0, 0, 1, true]] },
    { matches: [[0, 0, 1, false]] },
    { search: { total: 1, current: 0 } },
    { search: { total: 2, current: 0 } },
    { copy_cursor: [1, 2] },
    { selection: [[0, 0, 3]] },
  ];
  const seen = new Set([decorationSignature(base)]);
  for (const v of variants) seen.add(sig(v));
  assert.equal(seen.size, variants.length + 1, "every variant has its own signature");
  assert.equal(decorationSignature(undefined), "");
});

test("rowCells gives one string per column with the wide character's spacer empty and cluster tails joined", () => {
  const wide = { y: 0, runs: [["a", 1, 1, 0, 0], ["\u4e2d\0", 2, 1, 0, 0], ["b ", 2, 1, 0, 0]], extras: [[3, "\u0301"]] };
  const mirror = applyFrame(undefined, full({ changed: [wide] })).mirror;
  assert.deepEqual(rowCells(mirror, 0), ["a", "\u4e2d", "", "b\u0301", " "]);
  assert.deepEqual(rowCells(mirror, 1), [], "a row no frame sent is empty");
  assert.deepEqual(rowCells(mirror, 99), [], "a row outside the grid is empty");
  assert.deepEqual(rowCells(mirror, -1), []);
  assert.equal(gridText(mirror)[0], "a\u4e2db\u0301", "gridText is the same row joined and trimmed");
});

test("the link spans of a row are kept with the row, malformed ones are dropped and at most eight stay", () => {
  const spans = [
    [4, 7, "https://example.org/docs"],
    [-1, 3, "https://a.example"], // starts left of the grid
    [5, 2, "https://b.example"], // backwards
    [0, 10, "https://c.example"], // ends past the last column (cols = 10)
    [1.5, 2, "https://d.example"],
    ["1", 2, "https://e.example"],
    [1, 2, 3], // not an address
    [1, 2, ""], // blank address
    [1, 2], // too short
    [1, 2, "https://f.example", "extra"], // too long
    "nope",
    null,
  ];
  const mirror = applyFrame(undefined, full({ changed: [{ ...row(1, "see docs  "), links: spans }] })).mirror;
  assert.deepEqual(mirror.rows_[1].links, [[4, 7, "https://example.org/docs"]]);
  const many = Array.from({ length: 12 }, (_, i) => [i % 5, (i % 5) + 1, `https://h${i}.example`]);
  const capped = applyFrame(undefined, full({ changed: [{ ...row(0, "x"), links: many }] })).mirror;
  assert.equal(capped.rows_[0].links.length, 8, "eight spans at most");
  assert.deepEqual(capped.rows_[0].links.map((s) => s[2]), many.slice(0, 8).map((s) => s[2]), "the first eight, in order");
  const long = applyFrame(undefined, full({ changed: [{ ...row(0, "x"), links: [[0, 1, `https://x.example/${"a".repeat(2040)}`]] }] })).mirror;
  assert.equal(long.rows_[0].links, undefined, "an address past 2048 units is not from the core");
  const none = applyFrame(undefined, full({ changed: [{ ...row(0, "x"), links: "https://x.example" }] })).mirror;
  assert.equal(none.rows_[0].links, undefined, "a value that is not a list is no links");
});

test("a row keeps its links over later frames until it is sent again", () => {
  const first = applyFrame(undefined, full({ changed: [{ ...row(1, "see docs  "), links: [[4, 7, "https://example.org/docs"]] }, row(2, "other")] })).mirror;
  const next = applyFrame(first, full({ seq: 2, full: false, changed: [row(2, "changed")] })).mirror;
  assert.deepEqual(next.rows_[1].links, [[4, 7, "https://example.org/docs"]], "an untouched row keeps its links");
  const again = applyFrame(next, full({ seq: 3, full: false, changed: [row(1, "see docs  ")] })).mirror;
  assert.equal(again.rows_[1].links, undefined, "a row sent again without links has none");
});

// ---- the resting cue of links: the page keeps the flag only where it treats the span as a link ----------------

const LINK = 16, BOLD = 1, UNDERLINE = 4;
const FG = 0xffffff, BG = 0;
/** A filter that accepts the addresses in `ok` and counts every call. */
const filterOf = (ok, key = "k") => {
  const f = { key, calls: [], allow: (uri) => (f.calls.push(uri), ok.includes(uri)) };
  return f;
};
/** A mirror of one row of `cols` cells made of `runs` and `links` (the page's own `usableLinks` reads the spans). */
const linkMirror = (runs, links, cols = 20) => applyFrame(undefined, full({ cols, rows: 1, changed: [{ y: 0, runs, links }] })).mirror;
const run = (text, flags = 0, fg = FG, bg = BG) => [text, Array.from(text).length, fg, bg, flags];
const cellsOfRuns = (runs) => runs.reduce((n, r) => n + r[1], 0);
const OK = "https://ok.example/", NO = "ssh://no.example/";
const REST = " ".repeat(10);

test("a row with no flagged run comes back as the same array of runs and allow is not asked", () => {
  const mirror = linkMirror([run("plain text"), run(REST, UNDERLINE | BOLD)], [[0, 3, NO]]);
  const filter = filterOf([]);
  const lines = toRenderFrame(mirror, true, filter).lines;
  assert.equal(lines[0], mirror.rows_[0].runs, "the very array of the mirror's row");
  assert.deepEqual(filter.calls, [], "no cell is flagged: nothing to ask");
});

test("a row whose flagged cells all lie in accepted spans keeps its runs and allow is asked once per span", () => {
  const runs = [run("see "), run("docs", LINK), run(" and "), run("more", LINK), run(" ")];
  const mirror = linkMirror(runs, [[4, 7, OK], [13, 16, OK + "b"]], 18);
  const filter = filterOf([OK, OK + "b"]);
  const lines = toRenderFrame(mirror, true, filter).lines;
  assert.equal(lines[0], mirror.rows_[0].runs, "the same array");
  assert.deepEqual(filter.calls, [OK, OK + "b"], "one call for each span of the row");
});

test("a refused span loses LINK and keeps every other flag, colour and character, and the cells add up", () => {
  const runs = [run("see ", 0, 1, 2), run("docs", LINK | BOLD, 3, 4), run(" here" + REST, 0, 5, 6)];
  const mirror = linkMirror(runs, [[4, 7, NO]], 19);
  const out = toRenderFrame(mirror, true, filterOf([])).lines[0];
  assert.deepEqual(out, [run("see ", 0, 1, 2), run("docs", BOLD, 3, 4), run(" here" + REST, 0, 5, 6)]);
  assert.equal(cellsOfRuns(out), cellsOfRuns(runs));
  assert.deepEqual(mirror.rows_[0].runs[1], run("docs", LINK | BOLD, 3, 4), "the mirror's own row is not changed");
});

test("a span that covers part of a run splits it at the edges of the span, the rest of the run loses the flag", () => {
  // One run of ten flagged cells, a span over cells 3 to 5 that is accepted: the other cells lose the flag.
  const mirror = linkMirror([run("0123456789", LINK | BOLD, 7, 8), run(" ".repeat(10))], [[3, 5, OK]]);
  const out = toRenderFrame(mirror, true, filterOf([OK])).lines[0];
  assert.deepEqual(out, [run("012", BOLD, 7, 8), run("345", LINK | BOLD, 7, 8), run("6789", BOLD, 7, 8), run(" ".repeat(10))]);
  for (const r of out) assert.equal(Array.from(r[0]).length, r[1], "one code point per cell");
});

test("a wide character keeps its spacer at the edge of a span and when a span ends between the two", () => {
  const wide = "a\u4e2d\0b"; // a, a wide character, its spacer, b: four cells
  const rest = run(" ".repeat(16));
  // The span is the wide character's cell alone: the spacer goes with it.
  let out = toRenderFrame(linkMirror([run(wide, LINK), rest], [[1, 1, OK]]), true, filterOf([OK])).lines[0];
  assert.deepEqual(out, [run("a", 0), run("\u4e2d\0", LINK), run("b", 0), rest]);
  // The span ends on the wide character and not on its spacer, and it starts at the first cell.
  out = toRenderFrame(linkMirror([run("\u4e2d\0b", LINK), rest], [[0, 0, OK]], 19), true, filterOf([OK])).lines[0];
  assert.deepEqual(out, [run("\u4e2d\0", LINK), run("b", 0), rest]);
  // The span names only the spacer: the wide character is not inside it, so neither of the two keeps the cue.
  out = toRenderFrame(linkMirror([run(wide, LINK), rest], [[2, 2, OK]]), true, filterOf([OK])).lines[0];
  assert.deepEqual(out, [run(wide, 0), rest]);
  for (const r of out) assert.equal(Array.from(r[0]).length, r[1], "one code point per cell, the spacer included");
});

test("with two spans in one row only the accepted one keeps the flag, and two refused neighbours clear both", () => {
  const runs = [run("aaaa", LINK), run("bbbb", LINK), run(" ".repeat(12))];
  const mixed = linkMirror(runs, [[0, 3, OK], [4, 7, NO]]);
  assert.deepEqual(toRenderFrame(mixed, true, filterOf([OK])).lines[0], [run("aaaa", LINK), run("bbbb", 0), run(" ".repeat(12))]);
  const both = linkMirror(runs, [[0, 3, NO], [4, 7, NO + "2"]]);
  assert.deepEqual(toRenderFrame(both, true, filterOf([])).lines[0], [run("aaaa", 0), run("bbbb", 0), run(" ".repeat(12))]);
});

test("a flagged cell outside every valid span loses the flag, a row with no links at all loses it everywhere", () => {
  const runs = [run("xxxx", LINK), run(" ".repeat(16))];
  const filter = filterOf([OK]);
  // No links member at all.
  assert.deepEqual(toRenderFrame(linkMirror(runs, undefined), true, filter).lines[0], [run("xxxx", 0), run(" ".repeat(16))]);
  // The page dropped the span as malformed (backwards) and so there is none to accept.
  assert.deepEqual(toRenderFrame(linkMirror(runs, [[3, 0, OK]]), true, filter).lines[0], [run("xxxx", 0), run(" ".repeat(16))]);
  // Past the eighth: nine spans of one cell each, all of them would be accepted, the page holds eight.
  const nine = Array.from({ length: 9 }, (_, i) => [i, i, OK + i]);
  const mirror = linkMirror([run("012345678", LINK), run(" ".repeat(11))], nine);
  assert.equal(mirror.rows_[0].links.length, 8);
  const out = toRenderFrame(mirror, true, filterOf(nine.map((s) => s[2]))).lines[0];
  assert.deepEqual(out, [run("01234567", LINK), run("8", 0), run(" ".repeat(11))], "the ninth span is not held, so its cell has no cue");
});

test("the program's own underline is untouched, with LINK or without", () => {
  const runs = [run("under", UNDERLINE), run("both ", UNDERLINE | LINK), run(" ".repeat(10))];
  const out = toRenderFrame(linkMirror(runs, [[5, 9, NO]]), true, filterOf([])).lines[0];
  assert.deepEqual(out, [run("under", UNDERLINE), run("both ", UNDERLINE), run(" ".repeat(10))]);
  const kept = toRenderFrame(linkMirror(runs, [[5, 9, OK]]), true, filterOf([OK])).lines[0];
  assert.deepEqual(kept, runs, "an accepted span keeps both flags");
});

test("a second frame of the same mirror under an equal key asks allow nothing, another key asks again, a new row once", () => {
  const runs = [run("docs", LINK), run(" ".repeat(16))];
  const mirror = linkMirror(runs, [[0, 3, OK]]);
  const filter = filterOf([OK]);
  toRenderFrame(mirror, true, filter);
  toRenderFrame(mirror, false, filter);
  toRenderFrame(mirror, true, { ...filter, calls: filter.calls });
  assert.equal(filter.calls.length, 1, "asked once for the three frames");
  const other = filterOf([], "other");
  assert.deepEqual(toRenderFrame(mirror, true, other).lines[0][0], run("docs", 0), "another key gives another answer");
  assert.equal(other.calls.length, 1, "and is asked again");
  // Back to the first key: the answer is the first again (one more evaluation, the memo holds the last key).
  assert.equal(toRenderFrame(mirror, true, filter).lines[0], mirror.rows_[0].runs);
  // A frame replaces the row: the new row is asked once, and the untouched rows are not asked again.
  const f2 = filterOf([OK]);
  const first = applyFrame(undefined, full({ cols: 20, rows: 2, changed: [{ y: 0, runs, links: [[0, 3, OK]] }, { y: 1, runs, links: [[0, 3, OK]] }] })).mirror;
  toRenderFrame(first, true, f2);
  assert.equal(f2.calls.length, 2);
  const next = applyFrame(first, full({ cols: 20, rows: 2, seq: 2, full: false, changed: [{ y: 1, runs, links: [[0, 3, OK]] }] })).mirror;
  toRenderFrame(next, true, f2);
  assert.equal(f2.calls.length, 3, "the replaced row is asked once; the row the frame did not touch is not asked again");
});

test("sixty draws of a pane with twenty link rows under one key ask allow for the first draw only", () => {
  const changed = Array.from({ length: 20 }, (_, y) => ({ y, runs: [run("docs", LINK), run(" ".repeat(16))], links: [[0, 3, OK + y]] }));
  const mirror = applyFrame(undefined, full({ cols: 20, rows: 20, changed })).mirror;
  const filter = filterOf(changed.map((r) => r.links[0][2]));
  for (let i = 0; i < 60; i++) toRenderFrame(mirror, i % 2 === 0, filter);
  assert.equal(filter.calls.length, 20, `20 calls for 60 draws, not ${20 * 60}`);
  console.log(`frames: ${filter.calls.length} allow calls for 60 draws of 20 link rows under one key`);
});

test("the memo does not keep a row alive after the mirror drops it", async () => {
  setFlagsFromString("--expose-gc");
  const gc = runInNewContext("gc");
  const runs = [run("docs", LINK), run(" ".repeat(16))];
  const filter = filterOf([OK]);
  let ref;
  {
    let mirror = linkMirror(runs, [[0, 3, OK]]);
    toRenderFrame(mirror, true, filter);
    ref = new WeakRef(mirror.rows_[0]);
    mirror = null;
  }
  for (let i = 0; i < 10 && ref.deref() !== undefined; i++) {
    await new Promise((done) => setImmediate(done));
    gc();
  }
  assert.equal(ref.deref(), undefined, "the row was collected: the memo holds it weakly");
});

test("the header, the cursor, the cluster tails and the sizes are what the call without a filter gives", () => {
  const changed = [{ y: 0, runs: [run("docs", LINK), run(" ".repeat(16))], links: [[0, 3, NO]], extras: [[1, "\u0301"]] }];
  const mirror = applyFrame(undefined, full({ cols: 20, rows: 3, cursor: { x: 2, y: 0, shape: "block", visible: true }, fg: 0xabcdef, bg: 0x123456, changed })).mirror;
  const plain = toRenderFrame(mirror, true);
  const masked = toRenderFrame(mirror, true, filterOf([]));
  assert.deepEqual({ ...masked, lines: null }, { ...plain, lines: null });
  assert.notDeepEqual(masked.lines[0], plain.lines[0], "only the runs differ");
  assert.deepEqual(masked.lines.slice(1), plain.lines.slice(1), "and only in the row with a link");
  assert.deepEqual(toRenderFrame(mirror, false, filterOf([])), { ...masked, focused: false });
});
