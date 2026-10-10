// The pure mouse helpers on the compiled output in dist/: no DOM is needed to import or run them.
import test from "node:test";
import assert from "node:assert/strict";
import { gridPoint, modifierBits, wheelRows } from "../../dist/mouse.js";

const NONE = { shiftKey: false, altKey: false, ctrlKey: false, metaKey: false };

test("the module imports and runs without a DOM", () => {
  assert.equal(typeof document, "undefined");
  assert.equal(typeof window, "undefined");
  assert.equal(modifierBits(NONE), 0);
});

test("modifierBits gives shift 1, alt 2, ctrl 4, super 8 and their sums", () => {
  assert.equal(modifierBits({ ...NONE, shiftKey: true }), 1);
  assert.equal(modifierBits({ ...NONE, altKey: true }), 2);
  assert.equal(modifierBits({ ...NONE, ctrlKey: true }), 4);
  assert.equal(modifierBits({ ...NONE, metaKey: true }), 8);
  assert.equal(modifierBits({ shiftKey: true, altKey: false, ctrlKey: true, metaKey: false }), 5);
  assert.equal(modifierBits({ shiftKey: true, altKey: true, ctrlKey: true, metaKey: true }), 15);
});

// The canvas starts at (10, 20) on the page, the grid at (6, 6) inside it and is 100 x 50 CSS px.
const CANVAS = { left: 10, top: 20 };
const GRID = { x: 6, y: 6, w: 100, h: 50 };

test("gridPoint is relative to the grid origin in device pixels, rounded to integers", () => {
  assert.deepEqual(gridPoint(10 + 6, 20 + 6, CANVAS, GRID, 1), [0, 0]);
  assert.deepEqual(gridPoint(10 + 6 + 10, 20 + 6 + 5, CANVAS, GRID, 1), [10, 5]);
  assert.deepEqual(gridPoint(10 + 6 + 10.4, 20 + 6 + 5.6, CANVAS, GRID, 2), [21, 11]);
  assert.deepEqual(gridPoint(10 + 6 + 10.4, 20 + 6 + 5.6, CANVAS, GRID, 1), [10, 6]);
});

test("gridPoint is clamped to the grid size in device pixels", () => {
  assert.deepEqual(gridPoint(0, 0, CANVAS, GRID, 2), [0, 0], "above and left of the grid");
  assert.deepEqual(gridPoint(10 + 6 + 100, 20 + 6 + 50, CANVAS, GRID, 2), [199, 99], "on the far edge, inside by one");
  assert.deepEqual(gridPoint(5000, 5000, CANVAS, GRID, 2), [199, 99], "far past the grid");
  assert.deepEqual(gridPoint(5000, 5000, CANVAS, { x: 0, y: 0, w: 0, h: 0 }, 2), [0, 0], "an empty grid has only 0");
});

test("gridPoint never yields a non-integer or a negative number", () => {
  for (const v of [NaN, Infinity, -Infinity]) {
    const [x, y] = gridPoint(v, v, CANVAS, GRID, 2);
    assert.ok(Number.isInteger(x) && x >= 0 && x <= 199, "x for a non-finite position");
    assert.ok(Number.isInteger(y) && y >= 0 && y <= 99, "y for a non-finite position");
  }
  const [x, y] = gridPoint(30, 40, CANVAS, GRID, NaN);
  assert.ok(Number.isInteger(x) && Number.isInteger(y), "a non-finite ratio still gives integers");
});

test("wheelRows in pixel mode divides by the cell height and keeps the remainder", () => {
  assert.deepEqual(wheelRows(0, 9, 0, 18), { rows: 0, remainder: 0.5 });
  assert.deepEqual(wheelRows(0.5, 9, 0, 18), { rows: 1, remainder: 0 });
  assert.deepEqual(wheelRows(0, 45, 0, 18), { rows: 2, remainder: 0.5 });
  assert.deepEqual(wheelRows(0.5, 27, 0, 18), { rows: 2, remainder: 0 });
});

test("wheelRows carries the sign of deltaY and a zero is never negative", () => {
  assert.deepEqual(wheelRows(0, -36, 0, 18), { rows: -2, remainder: 0 });
  assert.deepEqual(wheelRows(0, -9, 0, 18), { rows: 0, remainder: -0.5 });
  assert.deepEqual(wheelRows(-0.5, -9, 0, 18), { rows: -1, remainder: 0 });
  assert.deepEqual(wheelRows(0.5, -9, 0, 18), { rows: 0, remainder: 0 });
});

test("wheelRows in line mode is three rows per line and a page is one grid page", () => {
  assert.deepEqual(wheelRows(0, 1, 1, 18), { rows: 3, remainder: 0 });
  assert.deepEqual(wheelRows(0, -1, 1, 18), { rows: -3, remainder: 0 });
  assert.deepEqual(wheelRows(0, 0.5, 1, 18), { rows: 1, remainder: 0.5 });
  assert.deepEqual(wheelRows(0, 1, 2, 18, 30), { rows: 30, remainder: 0 });
});

test("wheelRows ignores a delta that is not a number and a cell height of zero", () => {
  assert.deepEqual(wheelRows(0.25, NaN, 0, 18), { rows: 0, remainder: 0.25 });
  assert.deepEqual(wheelRows(0.25, Infinity, 0, 18), { rows: 0, remainder: 0.25 });
  assert.deepEqual(wheelRows(0.25, 100, 0, 0), { rows: 0, remainder: 0.25 });
});
