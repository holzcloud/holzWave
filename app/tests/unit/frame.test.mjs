// Unit tests run on the compiled output in dist/ with node:test: no test framework, no @types/node.
import test from "node:test";
import assert from "node:assert/strict";
import { BOLD, ITALIC, UNDERLINE, INVERSE, MAX_CELLS, MAX_TAIL, css, font, gridCells, rowExtras, visibleCursor } from "../../dist/frame.js";

test("css pads a colour to six hex digits", () => {
  assert.equal(css(0xff8fb8), "#ff8fb8");
  assert.equal(css(1), "#000001");
  assert.equal(css(0), "#000000");
});

test("font puts italic before bold and names JetBrains Mono", () => {
  const f = font(BOLD | ITALIC, 14);
  assert.ok(f.startsWith("italic bold 14px"), f);
  assert.ok(f.includes("JetBrains Mono"), f);
  assert.ok(font(0, 12).startsWith("12px "), font(0, 12));
});

test("the style flags are distinct powers of two", () => {
  const flags = [BOLD, ITALIC, UNDERLINE, INVERSE];
  for (const f of flags) assert.ok(f > 0 && (f & (f - 1)) === 0, `${f} is not a power of two`);
  assert.equal(new Set(flags).size, flags.length);
});

const frame = (over = {}) => ({
  cols: 10, rows: 4, cursor: { x: 2, y: 1, style: "block", blink: false }, focused: true, fg: 1, bg: 2, lines: [], ...over,
});

test("gridCells is the cell count of a sane grid and null for anything else", () => {
  assert.equal(gridCells(frame()), 40);
  assert.equal(gridCells({ cols: MAX_CELLS, rows: 1 }), MAX_CELLS);
  for (const [cols, rows] of [[0, 4], [10, 0], [-1, 4], [2.5, 4], [NaN, 4], [10, Infinity], [MAX_CELLS, 2], [1e6, 1e6]]) {
    assert.equal(gridCells({ cols, rows }), null, `${cols} x ${rows}`);
  }
});

test("visibleCursor is null without a cursor and outside the grid, and hollow without focus", () => {
  assert.deepEqual(visibleCursor(frame()), { x: 2, y: 1, style: "block", blink: false });
  assert.equal(visibleCursor(frame({ cursor: null })), null);
  assert.deepEqual(visibleCursor(frame({ focused: false })), { x: 2, y: 1, style: "hollow", blink: false });
  assert.deepEqual(
    visibleCursor(frame({ focused: false, cursor: { x: 0, y: 0, style: "bar", blink: true } })),
    { x: 0, y: 0, style: "hollow", blink: true },
  );
  for (const [x, y] of [[10, 0], [0, 4], [-1, 0], [0, -1], [NaN, 0], [0, 1.5]]) {
    assert.equal(visibleCursor(frame({ cursor: { x, y, style: "block", blink: false } })), null, `${x},${y}`);
  }
  assert.ok(visibleCursor(frame({ cursor: { x: 9, y: 3, style: "bar", blink: true } })));
});

test("rowExtras maps columns to tails, ignores malformed entries and cuts absurd tails", () => {
  assert.equal(rowExtras(frame(), 0), null);
  assert.equal(rowExtras(frame({ extras: [[]] }), 0), null);
  const f = frame({ extras: [[[1, "\u0308"], [3, ""], [-1, "x"], [1.5, "x"], "no", [4, 7], [5, "a".repeat(MAX_TAIL * 3)]], []] });
  const m = rowExtras(f, 0);
  assert.deepEqual([...m.keys()], [1, 5]);
  assert.equal(m.get(1), "\u0308");
  assert.equal(Array.from(m.get(5)).length, MAX_TAIL);
  assert.equal(rowExtras(f, 1), null);
  assert.equal(rowExtras(f, 7), null);
});
