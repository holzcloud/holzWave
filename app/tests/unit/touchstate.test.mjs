// The pure model of the touch gestures on the compiled output in dist/: boundaries in milliseconds and pixels, rows, fling, pinch,
// handle and menu geometry. No DOM and no clock are needed.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  FLING_MS, HANDLE_PX, LONG_PRESS_MS, MENU_GAP_PX, MENU_WORDS, PINCH_RANGE, SLOP_PX,
  classify, dragRows, flingRows, handleBoxes, handleTarget, menuPlacement, orderCells, pinchScale, releaseVelocity,
  scaleLabel, smoothVelocity,
} from "../../dist/touchstate.js";
import { UI_SCALE_RANGE } from "../../dist/settings-model.js";

const spec = readFileSync(new URL("../../../docs/design.md", import.meta.url), "utf8");
const facts = (over) => ({ pointers: 1, movedPx: 0, elapsedMs: 0, released: false, ...over });

test("the constants are the spec's: half a second, ten pixels, 44 pixels", () => {
  assert.equal(LONG_PRESS_MS, 500);
  assert.equal(SLOP_PX, 10);
  assert.equal(HANDLE_PX, 44);
  assert.equal(MENU_GAP_PX, 8);
  assert.equal(FLING_MS, 600);
});

test("a lift inside the slop and before 500 ms is a tap", () => {
  assert.equal(classify(facts({ released: true, elapsedMs: 120, movedPx: 3 })), "tap");
  assert.equal(classify(facts({ released: true, elapsedMs: 499, movedPx: 10 })), "tap", "499 ms and exactly the slop is still a tap");
  assert.equal(classify(facts({ released: true, elapsedMs: 0, movedPx: 0 })), "tap");
});

test("a finger held 500 ms inside the slop is a long press, 499 ms is not yet", () => {
  assert.equal(classify(facts({ elapsedMs: 499 })), "pending");
  assert.equal(classify(facts({ elapsedMs: 500 })), "longpress");
  assert.equal(classify(facts({ elapsedMs: 800, movedPx: 10 })), "longpress");
  assert.equal(classify(facts({ elapsedMs: 500, released: true })), "longpress", "a lift after a timer that ran late is the long press, not a tap");
  assert.equal(classify(facts({ elapsedMs: 499, released: true })), "tap");
});

test("a move over 10 px is a drag, before and after half a second", () => {
  assert.equal(classify(facts({ movedPx: 10.01, elapsedMs: 100 })), "drag");
  assert.equal(classify(facts({ movedPx: 12, elapsedMs: 499 })), "drag");
  assert.equal(classify(facts({ movedPx: 12, elapsedMs: 900 })), "drag", "a drag stays a drag");
  assert.equal(classify(facts({ movedPx: 12, released: true })), "drag");
  assert.equal(classify(facts({ movedPx: 10 })), "pending", "exactly the slop has not moved");
});

test("a second finger makes a pinch and beats everything else", () => {
  assert.equal(classify(facts({ pointers: 2 })), "pinch");
  assert.equal(classify(facts({ pointers: 2, elapsedMs: 600, movedPx: 50 })), "pinch");
  assert.equal(classify(facts({ pointers: 3, released: true })), "pinch");
});

test("a distance that is not a number is never a tap", () => {
  assert.equal(classify(facts({ movedPx: Number.NaN, released: true })), "drag");
});

test("a finger moving down by three cells scrolls three rows towards older output", () => {
  assert.deepEqual(dragRows(60, 0, 20), { rows: -3, remainder: 0 });
  assert.deepEqual(dragRows(-60, 0, 20), { rows: 3, remainder: 0 }, "up goes to newer output");
  assert.equal(Object.is(dragRows(5, 0, 20).rows, -0), false, "no negative zero");
});

test("a fraction of a row is carried, never lost and never doubled", () => {
  let remainder = 0;
  let total = 0;
  for (let i = 0; i < 10; i++) {
    const step = dragRows(7, remainder, 20); // ten moves of 7 px are 70 px: 3.5 rows
    total += step.rows;
    remainder = step.remainder;
  }
  assert.equal(total, -3, "three whole rows came out");
  assert.ok(Math.abs(remainder + 0.5) < 1e-9, `half a row is left (${remainder})`);
  const more = dragRows(12, remainder, 20); // a little over the other half, so float rounding cannot decide it
  assert.equal(more.rows, -1, "the carried half and the next half make a row");
});

test("no cell height, or one that is not a length, gives no rows", () => {
  for (const cellH of [0, -1, Number.NaN]) assert.equal(dragRows(100, 0, cellH).rows, 0, String(cellH));
  assert.equal(dragRows(Number.NaN, 0, 20).rows, 0);
});

test("a fling decays to nothing within 600 ms, whole rows, one sign", () => {
  const v = 1.5; // a finger moving down
  let last = 0;
  for (let t = 0; t <= 700; t += 10) {
    const rows = flingRows(v, t, 20);
    assert.ok(Number.isInteger(rows), `whole rows at ${t} ms`);
    assert.ok(rows <= last, `never turns back: ${rows} after ${last} at ${t} ms`);
    last = rows;
  }
  assert.ok(last < 0, `the fling moved (${last})`);
  assert.equal(flingRows(v, 600, 20), flingRows(v, 5000, 20), "it is over at 600 ms");
  assert.equal(flingRows(v, 0, 20), 0);
  // The sign follows the finger: up gives positive rows.
  assert.ok(flingRows(-1.5, 600, 20) > 0);
  assert.equal(flingRows(-1.5, 600, 20), -flingRows(1.5, 600, 20));
});

test("a fling needs speed, motion and a cell height", () => {
  assert.equal(flingRows(0.049, 300, 20), 0, "under 0.05 px per ms");
  assert.notEqual(flingRows(0.05, 600, 5), 0, "0.05 px per ms over a short row still moves");
  assert.equal(flingRows(2, 300, 20, true), 0, "reduced motion");
  assert.equal(flingRows(2, 300, 0), 0);
  assert.equal(flingRows(Number.NaN, 300, 20), 0);
  assert.equal(flingRows(2, Number.NaN, 20), 0);
  assert.equal(flingRows(1e9, 600, 20), flingRows(4, 600, 20), "a speed is capped");
});

test("the speed of the finger is smoothed and capped, and a rest leaves no fling", () => {
  assert.equal(smoothVelocity(0, 20, 20), 1);
  assert.ok(Math.abs(smoothVelocity(1, 40, 20) - 1.7) < 1e-9);
  assert.equal(smoothVelocity(0, 1e6, 1), 4);
  assert.equal(smoothVelocity(0, -1e6, 1), -4);
  assert.equal(smoothVelocity(0.5, Number.NaN, 10), 0.5);
  assert.equal(releaseVelocity(1.2, 30), 1.2);
  assert.equal(releaseVelocity(1.2, 101), 0);
  assert.equal(releaseVelocity(Number.NaN, 10), 0);
});

test("a pinch changes the scale in steps of 5 within 75 to 200", () => {
  assert.equal(pinchScale(100, 100, 125), 125);
  assert.equal(pinchScale(100, 100, 102), 100, "a ratio of 1.02 is no step");
  assert.equal(pinchScale(100, 100, 103), 105);
  assert.equal(pinchScale(100, 100, 262), 200, "held to 200");
  assert.equal(pinchScale(100, 100, 30), 75, "held to 75");
  assert.equal(pinchScale(125, 100, 80), 100);
  assert.equal(pinchScale(150, 200, 100), 75);
  for (let d = 1; d < 600; d += 7) {
    const s = pinchScale(100, 100, d);
    assert.ok(s % 5 === 0 && s >= 75 && s <= 200, `${d} gives ${s}`);
  }
});

test("a start distance that is not a length gives the start scale", () => {
  for (const startDist of [0, -3, Number.NaN, Infinity]) assert.equal(pinchScale(130, startDist, 100), 130, String(startDist));
  assert.equal(pinchScale(130, 100, Number.NaN), 130);
});

test("the pinch range is the spec's and lies inside the page's UI scale range", () => {
  assert.deepEqual([...PINCH_RANGE], [75, 200]);
  assert.ok(PINCH_RANGE[0] >= UI_SCALE_RANGE[0] && PINCH_RANGE[1] <= UI_SCALE_RANGE[1]);
});

const GEO = { originX: 6, originY: 24, cellW: 10, cellH: 20 };
const STAGE = { w: 390, h: 700 };

test("two handles of at least 44 by 44 hang at the two edges of the selection, inside the stage", () => {
  const h = handleBoxes([4, 2], [9, 2], GEO, STAGE);
  for (const b of [h.start, h.end]) {
    assert.ok(b.w >= 44 && b.h >= 44, "a touch target");
    assert.ok(b.x >= 0 && b.y >= 0 && b.x + b.w <= STAGE.w && b.y + b.h <= STAGE.h, "inside the stage");
  }
  assert.equal(h.start.tipX, 6 + 4 * 10, "the left edge of the start cell");
  assert.equal(h.end.tipX, 6 + 10 * 10, "the right edge of the end cell");
  assert.equal(h.start.tipY, 24 + 3 * 20, "the bottom of the row");
  assert.equal(h.start.x + h.start.w / 2, h.start.tipX, "centred on the edge");
  assert.equal(h.end.x + h.end.w / 2, h.end.tipX);
  assert.ok(h.start.y <= h.start.tipY && h.start.tipY - h.start.y <= 4, "the stem reaches the row");
});

test("handles at the edge of the stage are pushed in, and an end before the start swaps", () => {
  const edge = handleBoxes([0, 0], [38, 0], { ...GEO, originX: 0 }, { w: 380, h: 700 });
  assert.equal(edge.start.x, 0, "pushed in on the left");
  assert.equal(edge.end.x + edge.end.w, 380, "pushed in on the right");
  const low = handleBoxes([0, 0], [1, 40], GEO, { w: 390, h: 300 });
  assert.ok(low.end.y + low.end.h <= 300, "pushed up at the bottom");
  const swapped = handleBoxes([9, 2], [4, 2], GEO, STAGE);
  assert.ok(swapped.start.tipX < swapped.end.tipX, "start is never after end");
  const rows = handleBoxes([2, 5], [30, 3], GEO, STAGE);
  assert.ok(rows.start.tipY < rows.end.tipY, "an earlier row is the start");
  assert.deepEqual(orderCells([3, 1], [2, 1]), [[2, 1], [3, 1]]);
  assert.deepEqual(orderCells([3, 1], [3, 1]), [[3, 1], [3, 1]]);
});

test("a handle names the cell beside its tip, inside the row above, on the side of the selection", () => {
  // The end handle: its tip is the right edge of cell 9 of row 2: x 106, y 84.
  assert.deepEqual(handleTarget({ x: 106, y: 100 }, { dx: 0, dy: 16 }, true, GEO), [9, 2]);
  assert.deepEqual(handleTarget({ x: 126, y: 100 }, { dx: 0, dy: 16 }, true, GEO), [11, 2], "two cells to the right");
  // The start handle: the left edge of cell 4 of row 2: x 46.
  assert.deepEqual(handleTarget({ x: 46, y: 100 }, { dx: 0, dy: 16 }, false, GEO), [4, 2]);
  assert.deepEqual(handleTarget({ x: 46, y: 100 + 20 }, { dx: 0, dy: 16 }, false, GEO), [4, 3], "a row lower");
  assert.deepEqual(handleTarget({ x: Number.NaN, y: 100 }, { dx: 0, dy: 0 }, true, GEO)[0], 0);
});

test("the menu sits 8 px above the selection, below when there is no room, and inside the stage", () => {
  const stage = { x: 0, y: 0, w: 390, h: 700 };
  const menu = { w: 120, h: 44 };
  const sel = { x: 100, y: 200, w: 60, h: 20 };
  assert.deepEqual(menuPlacement(sel, stage, menu), { x: 70, y: 200 - 8 - 44 }, "above, centred");
  const top = { x: 100, y: 30, w: 60, h: 20 };
  assert.deepEqual(menuPlacement(top, stage, menu), { x: 70, y: 30 + 20 + 8 }, "below: 30 - 8 - 44 would leave the stage");
  assert.equal(menuPlacement({ x: 0, y: 200, w: 20, h: 20 }, stage, menu).x, 0, "inside on the left");
  assert.equal(menuPlacement({ x: 380, y: 200, w: 10, h: 20 }, stage, menu).x, 390 - 120, "inside on the right");
  assert.equal(menuPlacement(sel, { ...stage, y: 100 }, menu).y, 200 - 8 - 44, "room above is measured from the stage's top");
  assert.equal(menuPlacement({ x: 100, y: 140, w: 60, h: 20 }, { ...stage, y: 100 }, menu).y, 140 + 20 + 8);
  assert.deepEqual(menuPlacement({ x: 0, y: 0, w: 390, h: 700 }, stage, menu), { x: 135, y: 8 }, "a selection that fills the stage: inside, at the top");
  assert.equal(menuPlacement({ x: 0, y: 0, w: 390, h: 690 }, { ...stage, h: 700 }, menu).y, 8, "no room below the handles either");
  assert.equal(menuPlacement({ x: 0, y: 0, w: 10, h: 10 }, { x: 0, y: 0, w: 100, h: 30 }, menu).y, 0, "a stage lower than the menu: at its top edge");
});

test("the words and the label are the design record's", () => {
  assert.deepEqual({ ...MENU_WORDS }, { copy: "Copy", paste: "Paste" });
  assert.ok(spec.includes("`Copy` and `Paste`"), "the spec names the two buttons");
  assert.ok(spec.includes("`UI scale 125 %`"), "the spec names the label");
  assert.equal(scaleLabel(125), "UI scale 125 %");
  assert.equal(scaleLabel(75), "UI scale 75 %");
  assert.ok(spec.includes("800 ms"), "the label stays 800 ms");
});
