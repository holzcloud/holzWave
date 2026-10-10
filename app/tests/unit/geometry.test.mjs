// Pure geometry on the compiled output in dist/: no DOM is needed to import or run it.
import test from "node:test";
import assert from "node:assert/strict";
import {
  DIVIDER_HIT,
  PAD,
  cellAt,
  dividerRect,
  fitCells,
  gridRect,
  ordered,
  paneAt,
  paneRect,
  ratioAt,
} from "../../dist/geometry.js";

const M = { cellW: 8, cellH: 18, dpr: 2, width: 1000, height: 600 };
const FULL = { x: 0, y: 0, w: 1, h: 1 };
const LEFT = { x: 0, y: 0, w: 0.5, h: 1 };
const RIGHT = { x: 0.5, y: 0, w: 0.5, h: 1 };

test("the module imports and runs without a DOM", () => {
  assert.equal(typeof document, "undefined");
  assert.equal(typeof window, "undefined");
  assert.deepEqual(paneRect(M, FULL), { x: 0, y: 0, w: 1000, h: 600 });
});

test("paneRect scales a unit rectangle by the canvas size", () => {
  assert.deepEqual(paneRect(M, { x: 0.5, y: 0.25, w: 0.5, h: 0.75 }), { x: 500, y: 150, w: 500, h: 450 });
});

test("gridRect subtracts the padding on every side", () => {
  assert.equal(PAD, 6);
  assert.deepEqual(gridRect(M, FULL), { x: PAD, y: PAD, w: 1000 - 2 * PAD, h: 600 - 2 * PAD });
  assert.deepEqual(gridRect(M, RIGHT), { x: 500 + PAD, y: PAD, w: 500 - 2 * PAD, h: 600 - 2 * PAD });
});

test("gridRect never returns a negative size", () => {
  const g = gridRect(M, { x: 0.2, y: 0.2, w: 0.001, h: 0 });
  assert.equal(g.w, 0);
  assert.equal(g.h, 0);
  assert.equal(gridRect({ ...M, width: 0, height: 0 }, FULL).w, 0);
});

test("cellAt floors to cell indices relative to the grid", () => {
  assert.deepEqual(cellAt(M, FULL, PAD, PAD), [0, 0]);
  assert.deepEqual(cellAt(M, FULL, PAD + 8 * 3 + 7.9, PAD + 18 * 2 + 17.9), [3, 2]);
  assert.deepEqual(cellAt(M, FULL, PAD + 8 * 3, PAD + 18 * 2), [3, 2], "a cell starts on its left and top edge");
  assert.deepEqual(cellAt(M, RIGHT, 500 + PAD + 8 * 10, PAD + 18), [10, 1], "relative to the pane's own grid");
});

test("cellAt clamps at 0 on the left of and above the grid", () => {
  assert.deepEqual(cellAt(M, FULL, 0, 0), [0, 0]);
  assert.deepEqual(cellAt(M, RIGHT, 10, -50), [0, 0]);
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

test("fitCells counts the cells of the grid", () => {
  assert.deepEqual(fitCells(M, FULL), { cols: Math.floor(988 / 8), rows: Math.floor(588 / 18) });
});

test("fitCells returns at least 2 columns and 1 row", () => {
  assert.deepEqual(fitCells(M, { x: 0, y: 0, w: 0.001, h: 0.001 }), { cols: 2, rows: 1 });
  assert.deepEqual(fitCells({ ...M, width: 0, height: 0 }, FULL), { cols: 2, rows: 1 });
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

test("ordered puts the selection's ends in reading order", () => {
  const same = { pane: 1, a: [2, 3], b: [5, 3] };
  assert.deepEqual(ordered(same), [[2, 3], [5, 3]]);
  assert.deepEqual(ordered({ pane: 1, a: [5, 3], b: [2, 3] }), [[2, 3], [5, 3]]);
  assert.deepEqual(ordered({ pane: 1, a: [9, 7], b: [1, 2] }), [[1, 2], [9, 7]]);
  assert.deepEqual(ordered({ pane: 1, a: [1, 2], b: [9, 7] }), [[1, 2], [9, 7]]);
});
