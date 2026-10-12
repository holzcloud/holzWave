// The maths and the per-pane state of smooth scrolling and the cursor trail (DESK-28), on the compiled output in
// dist/. The clock is injected, so nothing here waits and no DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import {
  MAX_GHOSTS, MAX_LAYERS, Motion, SCROLL_MS, TRAIL_MS, easeOffset, easeOutCubic, scrolledRows, slideOffset,
  trailCells, trailGhosts,
} from "../../dist/scroll.js";
import { effectiveEffect } from "../../dist/a11y.js";

/** A clock the test moves by hand. */
function clock(start = 1000) {
  let t = start;
  const now = () => t;
  now.advance = (ms) => void (t += ms);
  return now;
}

/** The part of a pane mirror the effects look at. */
const mirror = (offset, { total = 100, len = 24, cols = 80, rows = 24, cursor = null, alt = false } = {}) => ({
  cols, rows, cursor, scrollbar: [offset, total, len], modes: { alt, bracketed_paste: false, mouse: false },
});
const at = (x, y) => ({ x, y, style: "block", blink: false });

// ---- the easing -------------------------------------------------------------------------------------

test("the module imports and runs without a DOM", () => {
  assert.equal(typeof document, "undefined");
  assert.equal(typeof window, "undefined");
  assert.equal(SCROLL_MS, 120, "a scroll settles within 120 ms");
});

test("easeOutCubic goes from 0 to 1, rises monotonically and clamps outside 0..1", () => {
  assert.equal(easeOutCubic(0), 0);
  assert.equal(easeOutCubic(1), 1);
  assert.equal(easeOutCubic(-3), 0);
  assert.equal(easeOutCubic(7), 1);
  assert.equal(easeOutCubic(Number.NaN), 0);
  let last = 0;
  for (let i = 1; i <= 100; i++) {
    const v = easeOutCubic(i / 100);
    assert.ok(v >= last, `not monotone at ${i}%`);
    last = v;
  }
  assert.ok(easeOutCubic(0.5) > 0.5, "an ease-out is ahead of linear at the half");
});

test("easeOffset starts at the distance, only decreases, and is exactly 0 from 120 ms on", () => {
  assert.equal(easeOffset(34, 0), 34);
  let last = 34;
  for (let ms = 1; ms < SCROLL_MS; ms++) {
    const v = easeOffset(34, ms);
    assert.ok(v <= last, `grew at ${ms} ms`);
    assert.ok(v > 0, `already 0 at ${ms} ms`);
    last = v;
  }
  assert.equal(easeOffset(34, SCROLL_MS), 0);
  assert.equal(easeOffset(34, SCROLL_MS + 1), 0);
  assert.equal(easeOffset(34, 1e9), 0, "clamped: a late frame does not extrapolate");
});

test("easeOffset is never negative and never larger than the distance", () => {
  for (const distance of [0.5, 1, 17, 1e6]) {
    for (let ms = -50; ms <= 300; ms += 7) {
      const v = easeOffset(distance, ms);
      assert.ok(v >= 0 && v <= distance, `${distance} at ${ms} ms gave ${v}`);
    }
  }
  assert.equal(easeOffset(10, -50), 10, "a clock that went backwards starts the slide over");
});

test("no distance, no work: zero, negative, NaN and infinite distances are an offset of 0", () => {
  for (const d of [0, -0, -5, Number.NaN, Infinity, -Infinity]) assert.equal(easeOffset(d, 10), 0, String(d));
  assert.equal(easeOffset(10, Number.NaN), 0);
  assert.equal(easeOffset(10, 10, 0), 0, "a duration of 0 has no slide");
  assert.equal(easeOffset(10, 10, -4), 0);
});

test("the duration is a parameter and the offset still ends exactly at it", () => {
  assert.equal(easeOffset(10, 49, 50) > 0, true);
  assert.equal(easeOffset(10, 50, 50), 0);
});

test("slideOffset keeps the sign and gives a plain 0 (not -0) when nothing is left", () => {
  assert.ok(slideOffset(-20, 30) < 0 && slideOffset(-20, 30) > -20);
  assert.ok(slideOffset(20, 30) > 0 && slideOffset(20, 30) < 20);
  assert.equal(slideOffset(-20, SCROLL_MS), 0);
  assert.ok(Object.is(slideOffset(-20, SCROLL_MS), 0), "no negative zero");
  assert.ok(Object.is(slideOffset(0, 5), 0));
  assert.equal(slideOffset(-20, 0), -20);
});

test("reduced motion: effectiveEffect is on only when the setting is on and motion is not reduced", () => {
  assert.equal(effectiveEffect(true, false), true);
  assert.equal(effectiveEffect(true, true), false, "reduced motion wins over the setting");
  assert.equal(effectiveEffect(false, false), false);
  assert.equal(effectiveEffect(false, true), false);
});

// ---- what counts as a scroll ----------------------------------------------------------------------

test("scrolledRows is the move of the viewport, and 0 for output, a resize and nonsense", () => {
  assert.equal(scrolledRows([50, 100, 24], [47, 100, 24]), -3, "towards older output");
  assert.equal(scrolledRows([50, 100, 24], [53, 100, 24]), 3, "towards newer output");
  assert.equal(scrolledRows([50, 100, 24], [50, 100, 24]), 0);
  assert.equal(scrolledRows([50, 100, 24], [51, 101, 24]), 0, "the history grew: output, not a jump");
  assert.equal(scrolledRows([50, 100, 24], [50, 100, 30]), 0, "a resize");
  assert.equal(scrolledRows(undefined, [1, 100, 24]), 0);
  assert.equal(scrolledRows([1, 100, 24], undefined), 0);
  assert.equal(scrolledRows([1.5, 100, 24], [3, 100, 24]), 0, "not whole rows");
  assert.equal(scrolledRows([Number.NaN, 100, 24], [3, 100, 24]), 0);
});

// ---- the trail --------------------------------------------------------------------------------------

test("trailCells: nothing for a move of one cell or none, else the cells the line crosses without the target", () => {
  assert.deepEqual(trailCells({ x: 5, y: 5 }, { x: 5, y: 5 }), []);
  assert.deepEqual(trailCells({ x: 5, y: 5 }, { x: 6, y: 5 }), []);
  assert.deepEqual(trailCells({ x: 5, y: 5 }, { x: 6, y: 6 }), []);
  assert.deepEqual(trailCells({ x: 2, y: 4 }, { x: 5, y: 4 }), [{ x: 2, y: 4 }, { x: 3, y: 4 }, { x: 4, y: 4 }]);
  assert.deepEqual(trailCells({ x: 5, y: 4 }, { x: 2, y: 4 }), [{ x: 5, y: 4 }, { x: 4, y: 4 }, { x: 3, y: 4 }]);
  assert.deepEqual(trailCells({ x: 0, y: 0 }, { x: 2, y: 2 }), [{ x: 0, y: 0 }, { x: 1, y: 1 }]);
});

test("trailCells never has more than six cells, starts at the old cell and stays on the line", () => {
  assert.equal(MAX_GHOSTS, 6);
  const cells = trailCells({ x: 0, y: 0 }, { x: 79, y: 23 });
  assert.equal(cells.length, 6);
  assert.deepEqual(cells[0], { x: 0, y: 0 });
  for (const c of cells) {
    assert.ok(c.x >= 0 && c.x < 79 && c.y >= 0 && c.y <= 23);
    assert.ok(Math.abs(c.y - (c.x * 23) / 79) <= 1, "off the line");
  }
  const xs = cells.map((c) => c.x);
  assert.deepEqual(xs, [...xs].sort((a, b) => a - b), "in order from the old cell towards the new");
  assert.equal(trailCells({ x: 0, y: 0 }, { x: 40, y: 0 }, 3).length, 3, "the cap is a parameter");
  assert.deepEqual(trailCells({ x: 0, y: 0 }, { x: 40, y: 0 }, 0), []);
  assert.deepEqual(trailCells({ x: 0, y: 0 }, { x: Number.NaN, y: 0 }), []);
});

test("trailGhosts fade over time, the one nearest the cursor is brightest, and none is left at the end", () => {
  const from = { x: 0, y: 0 }, to = { x: 10, y: 0 };
  const start = trailGhosts(from, to, 0);
  assert.equal(start.length, MAX_GHOSTS);
  for (const g of start) assert.ok(g.alpha > 0 && g.alpha <= 1);
  const alphas = start.map((g) => g.alpha);
  assert.deepEqual(alphas, [...alphas].sort((a, b) => a - b), "brighter towards the cursor");
  let last = Math.max(...alphas);
  for (let ms = 10; ms < TRAIL_MS; ms += 10) {
    const now = Math.max(...trailGhosts(from, to, ms).map((g) => g.alpha));
    assert.ok(now <= last, `brighter again at ${ms} ms`);
    last = now;
  }
  assert.deepEqual(trailGhosts(from, to, TRAIL_MS), []);
  assert.deepEqual(trailGhosts(from, to, 1e6), []);
  assert.deepEqual(trailGhosts(from, { x: 1, y: 0 }, 0), [], "no trail for a step of one cell");
  assert.deepEqual(trailGhosts(from, to, Number.NaN), []);
});

// ---- the state of the panes -------------------------------------------------------------------------

test("an idle Motion does no work: nothing is active, nothing to draw", () => {
  const m = new Motion(clock());
  m.setEnabled(true, true);
  assert.equal(m.active(), false);
  assert.equal(m.of(1, 24), null);
  m.frame(1, mirror(50), mirror(50), false, false);
  assert.equal(m.active(), false, "a frame that is not a scroll starts nothing");
  assert.equal(m.of(1, 24), null);
});

test("a scroll slides from the rows just scrolled to 0 within 120 ms and then the loop can stop", () => {
  const now = clock();
  const m = new Motion(now);
  m.setEnabled(true, false);
  m.frame(1, mirror(50), mirror(47), false, true); // three rows towards older output
  assert.equal(m.active(), true);
  assert.equal(m.of(1, 24).rows, -3, "starts at the rows scrolled; the new rows are drawn higher");

  now.advance(40);
  const mid = m.of(1, 24).rows;
  assert.ok(mid < 0 && mid > -3, `mid-slide offset ${mid}`);
  assert.equal(m.active(), true);

  now.advance(SCROLL_MS - 40 - 1);
  assert.equal(m.active(), true, "1 ms before the end there is still a frame to draw");
  now.advance(1);
  assert.equal(m.active(), false, "at 120 ms no animation frame is asked for");
  assert.equal(m.of(1, 24), null, "and the plain frame is what is drawn");
});

test("a scroll towards newer output slides the other way", () => {
  const m = new Motion(clock());
  m.setEnabled(true, false);
  m.frame(1, mirror(50), mirror(74), false, true);
  assert.equal(m.of(1, 24).rows, 24, "a page: 24 rows, the viewport");
});

test("a jump of more than a viewport slides in from one viewport away, not from the far end", () => {
  const m = new Motion(clock());
  m.setEnabled(true, false);
  m.frame(1, mirror(500), mirror(10), false, true);
  assert.equal(m.of(1, 24).rows, -24);
});

test("the frame before is kept under the slide, at the offset that keeps it where it was", () => {
  const now = clock();
  const m = new Motion(now);
  m.setEnabled(true, false);
  const old = mirror(50);
  m.frame(1, old, mirror(47), false, true);
  let fx = m.of(1, 24);
  assert.equal(fx.under.length, 1);
  assert.equal(fx.under[0].mirror, old);
  assert.equal(fx.under[0].rows, 0, "at the start the old rows are exactly where they were");
  now.advance(60);
  fx = m.of(1, 24);
  assert.ok(Math.abs(fx.under[0].rows - (fx.rows + 3)) < 1e-9, "old rows and new rows stay three rows apart");
});

test("a second scroll in the middle continues from what is left instead of jumping", () => {
  const now = clock();
  const m = new Motion(now);
  m.setEnabled(true, false);
  m.frame(1, mirror(50), mirror(48), false, true);
  now.advance(30);
  const left = m.of(1, 24).rows;
  m.frame(1, mirror(48), mirror(47), false, true);
  assert.ok(Math.abs(m.of(1, 24).rows - (left - 1)) < 1e-9, "left plus the new row");
  assert.equal(m.of(1, 24).under.length, 2, "both earlier frames fill the gap");
  now.advance(SCROLL_MS);
  assert.equal(m.active(), false);
});

test("at most MAX_LAYERS earlier frames are kept", () => {
  const m = new Motion(clock());
  m.setEnabled(true, false);
  for (let i = 0; i < MAX_LAYERS + 5; i++) m.frame(1, mirror(100 - i), mirror(99 - i), false, true);
  assert.ok(m.of(1, 24).under.length <= MAX_LAYERS);
});

test("output, a resize, a full frame and the alternate screen are not scrolls", () => {
  const m = new Motion(clock());
  m.setEnabled(true, false);
  m.frame(1, mirror(50), mirror(51, { total: 101 }), false, true);
  assert.equal(m.of(1, 24), null, "the history grew: output");
  m.frame(1, mirror(50), mirror(47), true, true);
  assert.equal(m.of(1, 24), null, "a full frame replaces everything");
  m.frame(1, mirror(50), mirror(47, { rows: 30, len: 24 }), false, true);
  assert.equal(m.of(1, 24), null, "another grid size");
  m.frame(1, mirror(0, { total: 24, alt: true }), mirror(3, { total: 24, alt: true }), false, true);
  assert.equal(m.of(1, 24), null, "the alternate screen has no history to slide through");
  m.frame(1, undefined, mirror(3), false, true);
  assert.equal(m.of(1, 24), null, "the first frame of a pane");
  assert.equal(m.active(), false);
});

test("a frame that changes the grid and is not a scroll cancels the slide; a cursor-only frame does not", () => {
  const m = new Motion(clock());
  m.setEnabled(true, false);
  m.frame(1, mirror(50), mirror(47), false, true);
  m.frame(1, mirror(47), mirror(47), false, false);
  assert.equal(m.active(), true, "no rows changed: the slide goes on");
  m.frame(1, mirror(47), mirror(47), false, true);
  assert.equal(m.active(), false, "rows changed without a scroll: what slides is no longer what is shown");
  assert.equal(m.of(1, 24), null);
});

test("each pane has its own slide, and a pane that is gone loses it", () => {
  const m = new Motion(clock());
  m.setEnabled(true, false);
  m.frame(1, mirror(50), mirror(47), false, true);
  m.frame(2, mirror(50), mirror(52), false, true);
  assert.equal(m.of(1, 24).rows, -3);
  assert.equal(m.of(2, 24).rows, 2);
  m.forget(1);
  assert.equal(m.of(1, 24), null);
  assert.equal(m.of(2, 24).rows, 2);
});

test("an effect that is not allowed does no work and holds no state; turning it off drops what it had", () => {
  const m = new Motion(clock());
  m.setEnabled(false, false);
  m.frame(1, mirror(50), mirror(47), false, true);
  m.frame(1, mirror(47, { cursor: at(0, 0) }), mirror(47, { cursor: at(20, 5) }), false, false);
  assert.equal(m.active(), false);
  assert.equal(m.of(1, 24), null);

  m.setEnabled(true, true);
  m.frame(1, mirror(50), mirror(47), false, true);
  m.frame(2, mirror(0, { cursor: at(0, 0) }), mirror(0, { cursor: at(20, 5) }), false, false);
  assert.equal(m.active(), true);
  m.setEnabled(false, true); // reduced motion arrives, live
  assert.equal(m.of(1, 24), null, "the slide is gone at once");
  assert.ok(m.of(2, 24).ghosts.length > 0, "the other effect is untouched");
  m.setEnabled(false, false);
  assert.equal(m.active(), false);
  assert.equal(m.of(2, 24), null);
});

test("the effects are independent: scrolling alone, and the trail alone", () => {
  const m = new Motion(clock());
  m.setEnabled(true, false);
  m.frame(1, mirror(0, { cursor: at(0, 0) }), mirror(0, { cursor: at(20, 5) }), false, true);
  assert.equal(m.of(1, 24), null, "no trail while it is off");
  m.setEnabled(false, true);
  m.frame(1, mirror(50), mirror(47), false, true);
  assert.equal(m.of(1, 24), null, "no slide while it is off");
});

// ---- the trail's state ------------------------------------------------------------------------------

test("a cursor that jumps more than one cell starts a trail of at most six ghosts that fades within TRAIL_MS", () => {
  const now = clock();
  const m = new Motion(now);
  m.setEnabled(false, true);
  m.frame(1, mirror(0, { cursor: at(2, 2) }), mirror(0, { cursor: at(60, 10) }), false, false);
  assert.equal(m.active(), true);
  const fx = m.of(1, 24);
  assert.equal(fx.rows, 0, "a trail alone does not slide the grid");
  assert.equal(fx.ghosts.length, MAX_GHOSTS);
  assert.deepEqual({ x: fx.ghosts[0].x, y: fx.ghosts[0].y }, { x: 2, y: 2 }, "from the old cell");
  now.advance(TRAIL_MS - 1);
  assert.equal(m.active(), true);
  now.advance(1);
  assert.equal(m.active(), false);
  assert.equal(m.of(1, 24), null);
});

test("typing a character, a steady cursor and a hidden cursor leave no trail", () => {
  const m = new Motion(clock());
  m.setEnabled(false, true);
  m.frame(1, mirror(0, { cursor: at(2, 2) }), mirror(0, { cursor: at(3, 2) }), false, true);
  assert.equal(m.of(1, 24), null, "one cell is typing");
  m.frame(1, mirror(0, { cursor: at(2, 2) }), mirror(0, { cursor: at(2, 2) }), false, false);
  assert.equal(m.of(1, 24), null, "no move");
  m.frame(1, mirror(0, { cursor: at(2, 2) }), mirror(0, { cursor: null }), false, true);
  assert.equal(m.of(1, 24), null, "a program that hides the cursor");
  m.frame(1, mirror(0, { cursor: null }), mirror(0, { cursor: at(40, 9) }), false, true);
  assert.equal(m.of(1, 24), null, "no cursor to come from: the cursor appears, it does not jump");
  assert.equal(m.active(), false);
});

test("a hidden cursor and a full frame cancel a trail that is running", () => {
  const m = new Motion(clock());
  m.setEnabled(false, true);
  m.frame(1, mirror(0, { cursor: at(2, 2) }), mirror(0, { cursor: at(40, 9) }), false, false);
  assert.equal(m.active(), true);
  m.frame(1, mirror(0, { cursor: at(40, 9) }), mirror(0, { cursor: null }), false, false);
  assert.equal(m.active(), false);
  m.frame(1, mirror(0, { cursor: at(2, 2) }), mirror(0, { cursor: at(40, 9) }), false, false);
  m.frame(1, mirror(0, { cursor: at(40, 9) }), mirror(0, { cursor: at(40, 9) }), true, true);
  assert.equal(m.active(), false);
});

test("the next frame that changes the grid without a jump ends the trail; a jump starts a new one", () => {
  const now = clock();
  const m = new Motion(now);
  m.setEnabled(false, true);
  m.frame(1, mirror(0, { cursor: at(2, 2) }), mirror(0, { cursor: at(40, 9) }), false, false);
  m.frame(1, mirror(0, { cursor: at(40, 9) }), mirror(0, { cursor: at(41, 9) }), false, true);
  assert.equal(m.of(1, 24), null, "output after the jump");
  m.frame(1, mirror(0, { cursor: at(41, 9) }), mirror(0, { cursor: at(5, 3) }), false, true);
  const fx = m.of(1, 24);
  assert.deepEqual({ x: fx.ghosts[0].x, y: fx.ghosts[0].y }, { x: 41, y: 9 }, "from where the cursor was");
});

test("a scroll is not a cursor jump: the cursor's row moves with the content", () => {
  const m = new Motion(clock());
  m.setEnabled(false, true);
  m.frame(1, mirror(50, { cursor: at(2, 20) }), mirror(47, { cursor: at(2, 23) }), false, true);
  assert.equal(m.of(1, 24), null);
  assert.equal(m.active(), false);
});
