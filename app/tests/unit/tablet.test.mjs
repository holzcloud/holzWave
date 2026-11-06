// The per-layout numbers on the compiled output in dist/: the tablet's touch sizes, the desktop's unchanged, and where a window can be torn off.
import test from "node:test";
import assert from "node:assert/strict";
import { DIVIDER_HIT, HEADER_REM, dividerRect, headerHeight } from "../../dist/geometry.js";
import { dividerHit, headerRem, tearOffAllowed } from "../../dist/tablet.js";

const M = { cellW: 8, cellH: 18, dpr: 2, width: 1000, height: 600 };
const ROW = { split: 1, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 0.25 };
const COLUMN = { split: 2, axis: "Column", area: { x: 0.5, y: 0.5, w: 0.5, h: 0.5 }, ratio: 0.5 };

test("the divider's touch area is 24 px on the tablet and the desktop's number everywhere else", () => {
  assert.equal(dividerHit("tablet"), 24);
  assert.equal(dividerHit("desktop"), DIVIDER_HIT);
  assert.equal(dividerHit("phone"), DIVIDER_HIT);
  assert.equal(DIVIDER_HIT, 8);
});

test("the pane header is 44 px at the base size on the tablet and the desktop's 24 px elsewhere", () => {
  assert.equal(headerRem("tablet"), 44 / 12);
  assert.equal(headerRem("desktop"), HEADER_REM);
  assert.equal(headerRem("phone"), HEADER_REM);
  assert.equal(HEADER_REM, 24 / 12);
});

test("headerHeight follows the layout and is unchanged without one", () => {
  assert.equal(headerHeight(12, 1, "tablet"), 44);
  assert.equal(headerHeight(12, 2, "tablet"), 44, "whole device pixels");
  assert.equal(headerHeight(15, 1, "tablet"), 55);
  assert.equal(headerHeight(9, 1, "tablet"), 44, "a UI scale of 75 % does not make the touch strip smaller than 44 px");
  assert.equal(headerHeight(13.25, 2, "tablet"), 48.5, "snapped to a half CSS pixel at 2x");
  // The same table the geometry test holds, with the layout left out and with the other two layouts.
  for (const layout of [undefined, "desktop", "phone"]) {
    assert.equal(headerHeight(12, 1, layout), 24);
    assert.equal(headerHeight(18, 1, layout), 36);
    assert.equal(headerHeight(24, 1, layout), 48);
    assert.equal(headerHeight(13.3, 1, layout), 27);
    assert.equal(headerHeight(13.25, 2, layout), 26.5);
    assert.equal(headerHeight(12, 2, layout), 24);
    assert.equal(headerHeight(Number.NaN, 1, layout), 24);
    assert.equal(headerHeight(0, 1, layout), 24);
  }
  assert.equal(headerHeight(Number.NaN, 1, "tablet"), 44, "a root size that is not a size counts as the base size");
});

test("dividerRect without a width is the rectangle of today; with 24 it is 24 wide across its axis, centred on the line", () => {
  assert.deepEqual(dividerRect(M, ROW), { x: 250 - DIVIDER_HIT / 2, y: 0, w: DIVIDER_HIT, h: 600 });
  assert.deepEqual(dividerRect(M, COLUMN), { x: 500, y: 450 - DIVIDER_HIT / 2, w: 500, h: DIVIDER_HIT });
  assert.deepEqual(dividerRect(M, ROW, 24), { x: 238, y: 0, w: 24, h: 600 });
  assert.deepEqual(dividerRect(M, COLUMN, 24), { x: 500, y: 438, w: 500, h: 24 });
  assert.deepEqual(dividerRect(M, ROW, dividerHit("tablet")), dividerRect(M, ROW, 24));
});

test("only a browser and Android cannot give a pane a window", () => {
  assert.equal(tearOffAllowed("web"), false);
  assert.equal(tearOffAllowed("android"), false);
  for (const os of ["macos", "windows", "linux", ""]) assert.equal(tearOffAllowed(os), true, os || "(empty)");
});
