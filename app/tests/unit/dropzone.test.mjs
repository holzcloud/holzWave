// The drop-zone maths on the compiled output in dist/: no DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import { EDGE_BAND, beyondSlop, dropZone, neighbour, outlineOf } from "../../dist/dropzone.js";

const R = { x: 100, y: 50, w: 400, h: 200 };

test("the middle of a pane is a swap, each outer quarter is its side", () => {
  assert.equal(dropZone(R, 300, 150), "center");
  assert.equal(dropZone(R, 110, 150), "left");
  assert.equal(dropZone(R, 490, 150), "right");
  assert.equal(dropZone(R, 300, 55), "top");
  assert.equal(dropZone(R, 300, 245), "bottom");
});

test("the band is a quarter of the width and of the height, so a wide pane is split by the same share", () => {
  assert.equal(EDGE_BAND, 0.25);
  assert.equal(dropZone(R, 100 + 400 * 0.24, 150), "left");
  assert.equal(dropZone(R, 100 + 400 * 0.26, 150), "center");
  assert.equal(dropZone(R, 300, 50 + 200 * 0.24), "top");
  assert.equal(dropZone(R, 300, 50 + 200 * 0.26), "center");
});

test("in a corner the nearer side, by share of the pane, wins", () => {
  assert.equal(dropZone(R, 104, 70), "left", "4 px of 400 is nearer than 20 px of 200");
  assert.equal(dropZone(R, 150, 52), "top");
});

test("a point outside, a rectangle without area and numbers that are not numbers have no zone", () => {
  assert.equal(dropZone(R, 99, 150), null);
  assert.equal(dropZone(R, 300, 251), null);
  assert.equal(dropZone({ ...R, w: 0 }, 100, 100), null);
  assert.equal(dropZone(R, Number.NaN, 100), null);
  assert.equal(dropZone({ ...R, h: Number.POSITIVE_INFINITY }, 300, 100), null);
});

test("the outline is the half a side would take and the whole pane for a swap", () => {
  assert.deepEqual(outlineOf(R, "left"), { x: 100, y: 50, w: 200, h: 200 });
  assert.deepEqual(outlineOf(R, "right"), { x: 300, y: 50, w: 200, h: 200 });
  assert.deepEqual(outlineOf(R, "top"), { x: 100, y: 50, w: 400, h: 100 });
  assert.deepEqual(outlineOf(R, "bottom"), { x: 100, y: 150, w: 400, h: 100 });
  assert.deepEqual(outlineOf(R, "center"), R);
  assert.notEqual(outlineOf(R, "center"), R, "a copy, not the argument");
});

test("four pixels make a drag, a click that wobbles does not", () => {
  assert.equal(beyondSlop({ x: 0, y: 0 }, { x: 3, y: 0 }), false);
  assert.equal(beyondSlop({ x: 0, y: 0 }, { x: 3, y: 3 }), true);
  assert.equal(beyondSlop({ x: 0, y: 0 }, { x: 0, y: 4 }), true);
});

test("the neighbour wraps and is missing when there is no other item", () => {
  assert.equal(neighbour([1, 2, 3], 3, 1), 1);
  assert.equal(neighbour([1, 2, 3], 1, -1), 3);
  assert.equal(neighbour([1], 1, 1), undefined);
  assert.equal(neighbour([1, 2], 9, 1), undefined);
});
