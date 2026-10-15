// Pure geometry on the compiled output in dist/: no DOM is needed to import or run it.
import test from "node:test";
import assert from "node:assert/strict";
import {
  DIVIDER_HIT,
  HEADER_REM,
  PAD,
  cellAt,
  dividerRect,
  fitCells,
  gridRect,
  headerHeight,
  paneAt,
  paneRect,
  ratioAt,
} from "../../dist/geometry.js";

const M = { cellW: 8, cellH: 18, dpr: 2, width: 1000, height: 600 };
const FULL = { x: 0, y: 0, w: 1, h: 1 };
const LEFT = { x: 0, y: 0, w: 0.5, h: 1 };
const RIGHT = { x: 0.5, y: 0, w: 0.5, h: 1 };
const HEADER = 20; // the header strip at the base font size

test("the module imports and runs without a DOM", () => {
  assert.equal(typeof document, "undefined");
  assert.equal(typeof window, "undefined");
  assert.deepEqual(paneRect(M, FULL), { x: 0, y: 0, w: 1000, h: 600 });
});

test("paneRect scales a unit rectangle by the canvas size", () => {
  assert.deepEqual(paneRect(M, { x: 0.5, y: 0.25, w: 0.5, h: 0.75 }), { x: 500, y: 150, w: 500, h: 450 });
});

test("gridRect starts a header below the pane's top edge and keeps the padding on the other sides", () => {
  assert.equal(PAD, 6);
  assert.deepEqual(gridRect(M, FULL, HEADER), { x: PAD, y: HEADER, w: 1000 - 2 * PAD, h: 600 - HEADER - PAD });
  assert.deepEqual(gridRect(M, RIGHT, HEADER), { x: 500 + PAD, y: HEADER, w: 500 - 2 * PAD, h: 600 - HEADER - PAD });
  const lower = { x: 0, y: 0.5, w: 1, h: 0.5 };
  assert.deepEqual(gridRect(M, lower, HEADER), { x: PAD, y: 300 + HEADER, w: 1000 - 2 * PAD, h: 300 - HEADER - PAD });
});

test("a taller header moves the grid down and makes it shorter by the same amount", () => {
  const small = gridRect(M, FULL, 20);
  const big = gridRect(M, FULL, 30);
  assert.equal(big.y, small.y + 10);
  assert.equal(big.h, small.h - 10);
  assert.equal(big.x, small.x);
  assert.equal(big.w, small.w);
});

test("gridRect never returns a negative size, whatever the header", () => {
  const g = gridRect(M, { x: 0.2, y: 0.2, w: 0.001, h: 0 }, HEADER);
  assert.equal(g.w, 0);
  assert.equal(g.h, 0);
  assert.equal(gridRect({ ...M, width: 0, height: 0 }, FULL, HEADER).w, 0);
  assert.equal(gridRect(M, FULL, 5000).h, 0, "a header taller than the pane leaves no grid");
});

test("a header height that is not a length counts as no header", () => {
  const none = { x: PAD, y: 0, w: 1000 - 2 * PAD, h: 600 - PAD };
  assert.deepEqual(gridRect(M, FULL, Number.NaN), none);
  assert.deepEqual(gridRect(M, FULL, -20), none);
  assert.deepEqual(gridRect(M, FULL, Number.POSITIVE_INFINITY), none);
});

test("the header height follows the root font size and snaps to device pixels", () => {
  assert.equal(HEADER_REM, 24 / 12);
  assert.equal(headerHeight(12, 1), 24, "UI scale 100 %");
  assert.equal(headerHeight(18, 1), 36, "UI scale 150 %");
  assert.equal(headerHeight(24, 1), 48);
  assert.equal(headerHeight(13.3, 1), 27, "26.6 px snaps to a whole device pixel");
  assert.equal(headerHeight(13.25, 2), 26.5, "a half CSS pixel is a whole device pixel at 2x");
  assert.equal(headerHeight(12, 2), 24);
});

test("the header height of a root size that is not a size is the base height", () => {
  assert.equal(headerHeight(Number.NaN, 1), 24);
  assert.equal(headerHeight(0, 1), 24);
  assert.equal(headerHeight(-12, 1), 24);
  assert.equal(headerHeight(12, 0), 24, "a device pixel ratio of 0 counts as 1");
});

test("cellAt floors to cell indices relative to the grid below the header", () => {
  assert.deepEqual(cellAt(M, FULL, HEADER, PAD, HEADER), [0, 0]);
  assert.deepEqual(cellAt(M, FULL, HEADER, PAD + 8 * 3 + 7.9, HEADER + 18 * 2 + 17.9), [3, 2]);
  assert.deepEqual(cellAt(M, FULL, HEADER, PAD + 8 * 3, HEADER + 18 * 2), [3, 2], "a cell starts on its left and top edge");
  assert.deepEqual(cellAt(M, RIGHT, HEADER, 500 + PAD + 8 * 10, HEADER + 18), [10, 1], "relative to the pane's own grid");
  assert.deepEqual(cellAt(M, FULL, 30, PAD, 30 + 18), [0, 1], "the header height is the one that was passed");
});

test("cellAt clamps at 0 on the left of, above and inside the header", () => {
  assert.deepEqual(cellAt(M, FULL, HEADER, 0, 0), [0, 0]);
  assert.deepEqual(cellAt(M, RIGHT, HEADER, 10, -50), [0, 0]);
  assert.deepEqual(cellAt(M, FULL, HEADER, PAD + 8 * 4, 5), [4, 0], "a point in the header is in row 0");
});

test("paneAt returns the pane that contains the point", () => {
  const view = { panes: [[1, LEFT], [2, RIGHT]] };
  assert.deepEqual(paneAt(M, view, 10, 10), [1, LEFT]);
  assert.deepEqual(paneAt(M, view, 499.9, 599), [1, LEFT]);
  assert.deepEqual(paneAt(M, view, 500, 0), [2, RIGHT], "the shared edge belongs to the right pane");
  assert.deepEqual(paneAt(M, view, 999, 300), [2, RIGHT]);
});

test("paneAt returns null outside every pane and without a view", () => {
  const view = { panes: [[1, LEFT], [2, RIGHT]] };
  assert.equal(paneAt(M, view, -1, 10), null);
  assert.equal(paneAt(M, view, 10, -1), null);
  assert.equal(paneAt(M, view, 1000, 10), null);
  assert.equal(paneAt(M, view, 10, 600), null);
  assert.equal(paneAt(M, { panes: [] }, 10, 10), null);
  assert.equal(paneAt(M, null, 10, 10), null);
});

test("fitCells counts the cells of the grid below the header", () => {
  assert.deepEqual(fitCells(M, FULL, HEADER), { cols: Math.floor(988 / 8), rows: Math.floor(574 / 18) });
  assert.deepEqual(fitCells(M, FULL, 100), { cols: 123, rows: Math.floor(494 / 18) }, "a taller header, fewer rows");
});

test("fitCells returns at least 2 columns and 1 row", () => {
  assert.deepEqual(fitCells(M, { x: 0, y: 0, w: 0.001, h: 0.001 }, HEADER), { cols: 2, rows: 1 });
  assert.deepEqual(fitCells({ ...M, width: 0, height: 0 }, FULL, HEADER), { cols: 2, rows: 1 });
  assert.deepEqual(fitCells(M, FULL, 5000), { cols: 123, rows: 1 }, "a header taller than the pane");
});

test("dividerRect is DIVIDER_HIT wide across its axis and as long as its area along it", () => {
  assert.equal(DIVIDER_HIT, 8);
  assert.deepEqual(
    dividerRect(M, { split: 1, axis: "Row", area: FULL, ratio: 0.5 }),
    { x: 500 - 4, y: 0, w: 8, h: 600 },
  );
  assert.deepEqual(
    dividerRect(M, { split: 2, axis: "Column", area: { x: 0, y: 0, w: 0.5, h: 1 }, ratio: 0.25 }),
    { x: 0, y: 150 - 4, w: 500, h: 8 },
  );
});

test("ratioAt maps a pointer position into the split's area", () => {
  const area = { x: 100, y: 50, w: 800, h: 400 };
  assert.equal(ratioAt(area, "Row", 500, 0), 0.5);
  assert.equal(ratioAt(area, "Column", 0, 250), 0.5);
  assert.equal(ratioAt(area, "Row", 100, 0), 0);
  assert.equal(ratioAt(area, "Row", 900, 0), 1);
  assert.ok(ratioAt(area, "Row", 0, 0) < 0, "left of the area stays outside 0..1 for the core to clamp");
});

test("ratioAt is not finite for an area without size", () => {
  assert.ok(!Number.isFinite(ratioAt({ x: 0, y: 0, w: 0, h: 10 }, "Row", 5, 5)));
  assert.ok(!Number.isFinite(ratioAt({ x: 0, y: 0, w: 10, h: 0 }, "Column", 5, 5)));
});
