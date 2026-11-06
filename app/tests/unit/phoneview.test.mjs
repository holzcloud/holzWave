// The phone's one-pane view on the compiled output in dist/: a pure transform of the core's view.
import test from "node:test";
import assert from "node:assert/strict";
import { phoneView, shownCount, shownIndex } from "../../dist/phoneview.js";

const deepFreeze = (o) => {
  for (const v of Object.values(o)) if (v && typeof v === "object") deepFreeze(v);
  return Object.freeze(o);
};

const THIRDS = (focus) => ({
  rev: 7,
  workspaces: [{ name: "main", active: true, panes: 3, pane_ids: [1, 2, 3] }],
  tabs: [{ title: "shell", active: true, focus, panes: 3, pane_ids: [1, 2, 3] }],
  focus,
  panes: [
    [1, { x: 0, y: 0, w: 1 / 3, h: 1 }],
    [2, { x: 1 / 3, y: 0, w: 1 / 3, h: 1 }],
    [3, { x: 2 / 3, y: 0, w: 1 / 3, h: 1 }],
  ],
  dividers: [
    { split: 10, axis: "Row", area: { x: 0, y: 0, w: 1, h: 1 }, ratio: 1 / 3 },
    { split: 11, axis: "Row", area: { x: 1 / 3, y: 0, w: 2 / 3, h: 1 }, ratio: 0.5 },
  ],
  pane_names: [[2, "logs"]],
  windows: 1,
});

test("the focused pane alone at the full rectangle, no dividers, the rest as the core sent it", () => {
  const input = deepFreeze(THIRDS(2));
  const out = phoneView(input);
  assert.deepEqual(out.panes, [[2, { x: 0, y: 0, w: 1, h: 1 }]]);
  assert.deepEqual(out.dividers, []);
  for (const key of ["rev", "workspaces", "tabs", "focus", "pane_names", "windows"]) {
    assert.deepEqual(out[key], input[key], key);
  }
  assert.equal(input.panes.length, 3, "the input is not changed");
  assert.equal(input.dividers.length, 2, "the input is not changed");
});

test("each pane of the tab can be the one that is shown", () => {
  for (const focus of [1, 2, 3]) assert.deepEqual(phoneView(THIRDS(focus)).panes.map(([id]) => id), [focus]);
});

test("a view whose focus is not among its panes is left as it is", () => {
  const empty = deepFreeze({ ...THIRDS(2), focus: 9 });
  assert.equal(phoneView(empty), empty);
  const none = deepFreeze({ ...THIRDS(2), panes: [], dividers: [] });
  assert.deepEqual(phoneView(none).panes, [], "no pane is invented");
});

test("a view with one pane gets the full rectangle", () => {
  const one = deepFreeze({ ...THIRDS(1), panes: [[1, { x: 0.2, y: 0.1, w: 0.5, h: 0.5 }]], dividers: [] });
  assert.deepEqual(phoneView(one).panes, [[1, { x: 0, y: 0, w: 1, h: 1 }]]);
});

test("the full rectangle is a new object each time", () => {
  const a = phoneView(THIRDS(1)).panes[0][1];
  const b = phoneView(THIRDS(1)).panes[0][1];
  assert.notEqual(a, b);
});

test("the place of the focus among the panes of the shown tab, and their count", () => {
  assert.equal(shownIndex(THIRDS(2)), 2);
  assert.equal(shownCount(THIRDS(2)), 3);
  assert.equal(shownIndex(THIRDS(1)), 1);
  assert.equal(shownIndex(THIRDS(3)), 3);
});

test("the shown tab is the active one, in the order of pane_ids", () => {
  const v = THIRDS(30);
  v.tabs = [
    { title: "a", active: false, focus: 1, panes: 1, pane_ids: [1] },
    { title: "b", active: true, focus: 30, panes: 3, pane_ids: [10, 30, 20] },
  ];
  assert.equal(shownIndex(v), 2);
  assert.equal(shownCount(v), 3);
});

test("a view with no shown tab, no ids or a focus that is not listed counts nothing", () => {
  const v = THIRDS(2);
  v.tabs = [{ title: "a", active: false, focus: 1, panes: 1, pane_ids: [1] }];
  assert.equal(shownIndex(v), 0);
  assert.equal(shownCount(v), 0);
  const w = THIRDS(2);
  w.tabs[0].pane_ids = undefined;
  assert.equal(shownIndex(w), 0);
  assert.equal(shownCount(w), 3, "the tab's own count when the ids are missing");
  const x = THIRDS(2);
  x.focus = 99;
  assert.equal(shownIndex(x), 0);
});
