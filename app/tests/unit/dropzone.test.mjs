// The drop-zone maths on the compiled output in dist/: no DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import {
  EDGE_BAND,
  LEAVE_MS,
  Leaving,
  TEAR_MARGIN,
  attachFor,
  beyondSlop,
  dropZone,
  leavingPanes,
  neighbour,
  outlineOf,
  outsideBeyond,
} from "../../dist/dropzone.js";
import { leavePane } from "../../dist/frames.js";

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

// ---- between windows -------------------------------------------------------------------------------------------

test("a release tears off only 24 px beyond the edge, in any direction; a slip over the edge cancels", () => {
  assert.equal(TEAR_MARGIN, 24);
  const out = (x, y) => outsideBeyond(x, y, 1000, 640);
  assert.equal(out(500, 300), false, "inside");
  assert.equal(out(-1, 300), false, "just over the left edge");
  assert.equal(out(-23, 300), false);
  assert.equal(out(-24, 300), true, "24 px is enough");
  assert.equal(out(1023, 300), false);
  assert.equal(out(1024, 300), true);
  assert.equal(out(500, -24), true);
  assert.equal(out(500, 663), false);
  assert.equal(out(500, 664), true);
  assert.equal(out(-10, -10), false, "a corner is measured by the farther side, not added up");
  assert.equal(out(-100, 700), true);
  assert.equal(outsideBeyond(-30, 0, 1000, 640, 40), false, "the margin can be given");
});

test("a release outside that is not a number tears nothing off", () => {
  assert.equal(outsideBeyond(Number.NaN, 0, 1000, 640), false);
  assert.equal(outsideBeyond(-100, Number.POSITIVE_INFINITY, 1000, 640), false);
  assert.equal(outsideBeyond(-100, 0, Number.NaN, 640), false);
});

const TABS = [{ focus: 7 }, { focus: 8 }, { focus: 9 }];

test("a pane released over another window lands on a zone of a pane, a tab, the bar or a chip", () => {
  const on = (under) => attachFor("pane", 5, under, TABS, 1);
  assert.deepEqual(on({ part: "pane", pane: 2, zone: "left" }), { cmd: "attach_pane", args: { pane: 5, target: 2, edge: "left" } });
  assert.deepEqual(on({ part: "pane", pane: 2, zone: "center" }), { cmd: "attach_pane", args: { pane: 5, target: 2, edge: "center" } });
  assert.deepEqual(on({ part: "tab", index: 2 }), { cmd: "attach_pane", args: { pane: 5, target: 9, edge: "right" } }, "next to that tab's focus pane, as in one window");
  assert.deepEqual(on({ part: "bar" }), { cmd: "attach_pane", args: { pane: 5, workspace: 1 } }, "a tab of its own in the shown workspace");
  assert.deepEqual(on({ part: "chip", index: 0 }), { cmd: "attach_pane", args: { pane: 5, workspace: 0 } });
});

test("a tab released over another window lands in the workspace of a chip, and in the shown one anywhere else", () => {
  const on = (under) => attachFor("tab", 5, under, TABS, 1);
  assert.deepEqual(on({ part: "chip", index: 2 }), { cmd: "attach_tab", args: { pane: 5, workspace: 2 } });
  for (const under of [{ part: "bar" }, { part: "tab", index: 0 }, { part: "pane", pane: 2, zone: "top" }, { part: "pane", pane: 2, zone: null }]) {
    assert.deepEqual(on(under), { cmd: "attach_tab", args: { pane: 5, workspace: 1 } }, JSON.stringify(under));
  }
});

test("a release over no zone answers nothing", () => {
  assert.equal(attachFor("pane", 5, { part: "none" }, TABS, 0), null);
  assert.equal(attachFor("tab", 5, { part: "none" }, TABS, 0), null);
  assert.equal(attachFor("pane", 5, { part: "pane", pane: 2, zone: null }, TABS, 0), null, "a pane's own edge outside its grid");
  assert.equal(attachFor("pane", 5, { part: "tab", index: 7 }, TABS, 0), null, "a tab that is not there");
});

const VIEW = {
  tabs: [{ active: false, focus: 3 }, { active: true, focus: 2 }],
  panes: [[1, {}], [2, {}]],
};

test("the panes a gesture lets go of: the pane; the shown tab's panes; one pane of another tab", () => {
  assert.deepEqual(leavingPanes({ kind: "pane", pane: 2 }, VIEW), { kind: "pane", pane: 2, ids: [2] });
  assert.deepEqual(leavingPanes({ kind: "tab", index: 1 }, VIEW), { kind: "tab", pane: 2, ids: [1, 2] });
  assert.deepEqual(leavingPanes({ kind: "tab", index: 0 }, VIEW), { kind: "tab", pane: 3, ids: [3] }, "the layout lists no panes of a tab that is not shown");
  assert.equal(leavingPanes({ kind: "tab", index: 5 }, VIEW), null);
});

test("a pane that was let go of is forgotten when a layout without it arrives, and not before", () => {
  const leaving = new Leaving();
  leaving.mark([1, 2], 1000);
  assert.deepEqual(leaving.take(new Set([1, 2, 3]), 1100), [], "still shown: the other window has not answered");
  assert.equal(leaving.size, 2);
  assert.deepEqual(leaving.take(new Set([2, 3]), 1200), [1], "the layout without 1");
  assert.deepEqual(leaving.take(new Set([3]), 1300), [2]);
  assert.equal(leaving.size, 0);
  assert.deepEqual(leaving.take(new Set(), 1400), [], "once, not again");
});

test("a mark that nothing confirmed lapses, and a cancelled gesture unmarks", () => {
  const leaving = new Leaving();
  leaving.mark([1], 0);
  assert.deepEqual(leaving.take(new Set([1]), LEAVE_MS + 1), [], "the other window never answered");
  assert.equal(leaving.size, 0);
  assert.deepEqual(leaving.take(new Set(), LEAVE_MS + 2), [], "and a pane that then goes for other reasons is not forgotten by it");
  leaving.mark([4], 0);
  leaving.unmark([4]);
  assert.deepEqual(leaving.take(new Set(), 10), []);
  leaving.mark([5], 0);
  assert.deepEqual(leaving.take(new Set(), LEAVE_MS + 1), [], "too late to count");
});

test("a pane that moved to another window loses mirror, size, title and cwd, is not an exited pane and is away", () => {
  const sets = () => ({ frames: new Map([[3, {}]]), sizes: new Map([[3, {}]]), titles: new Map([[3, "vim"]]), cwds: new Map([[3, "/x"]]), exited: new Set([3, 9]), away: new Set() });
  const stores = sets();
  leavePane(stores, 3);
  assert.equal(stores.frames.has(3) || stores.sizes.has(3) || stores.titles.has(3) || stores.cwds.has(3), false);
  assert.deepEqual([...stores.exited], [9], "only exits are exits: a pane that comes back is not one");
  assert.deepEqual([...stores.away], [3]);
  const other = sets();
  other.frames.set(4, {});
  leavePane(other, 3);
  assert.equal(other.frames.has(4), true, "the other panes are untouched");
});
