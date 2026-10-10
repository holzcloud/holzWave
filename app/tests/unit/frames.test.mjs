// The frame patcher against the golden sequence the Rust tests write (tests/fixtures/frames.json): every step is
// applied in order to one mirror and the grid it shows must equal the grid of the engine. Runs on dist/ like the
// other unit tests; nothing is imported from the DOM side of the app.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { applyFrame, gridText, toRenderFrame } from "../../dist/frames.js";

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
