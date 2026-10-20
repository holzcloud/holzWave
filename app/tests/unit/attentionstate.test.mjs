// The attention marks of the bar (attentionstate.ts, docs/design.md section 6.23): which kinds wait on which tab and pill.
// No DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import { Attention, MARKS, MAX_LATCHED, PRIORITY, idsOf } from "../../dist/attentionstate.js";

/** Two workspaces: alpha (shown) with tabs of panes [1, 2], [3] and [4]; beta with tabs [5] and [6, 7]. */
const view = (over = {}) => ({
  workspaces: [
    { active: true, pane_ids: [1, 2, 3, 4] },
    { active: false, pane_ids: [5, 6, 7] },
  ],
  tabs: [
    { active: true, pane_ids: [1, 2] },
    { active: false, pane_ids: [3] },
    { active: false, pane_ids: [4] },
  ],
  ...over,
});

/** A store that has seen `view()` with the first tab shown. */
function store() {
  const a = new Attention();
  a.follow([1, 2], [1, 2, 3, 4, 5, 6, 7]);
  return a;
}

const glyphs = (m) => m?.marks.map((x) => x.glyph).join("") ?? null;

test("the glyphs and the words are the contract of the design spec", () => {
  assert.deepEqual(PRIORITY, ["ask", "bell", "failed", "done"]);
  assert.deepEqual(Object.keys(MARKS), PRIORITY);
  assert.equal(MARKS.ask.glyph, "?");
  assert.equal(MARKS.bell.glyph, "▲");
  assert.equal(MARKS.failed.glyph, "!");
  assert.equal(MARKS.done.glyph, "✓");
  assert.equal(MARKS.ask.words, "waits for you");
  assert.equal(MARKS.bell.words, "rang the bell");
  assert.equal(MARKS.failed.words, "command failed");
  assert.equal(MARKS.done.words, "command finished");
});

test("a bell on a pane in view latches nothing", () => {
  const a = store();
  let told = 0;
  a.subscribe(() => told++);
  assert.equal(a.note(1, "bell"), false);
  assert.equal(a.note(2, "bell"), false, "an unfocused split of the shown tab is in view too");
  assert.equal(told, 0);
  assert.deepEqual(a.marks(view()), { tabs: [null, null, null], workspaces: [null, null] });
});

test("a bell on a hidden pane marks its inactive tab and its inactive workspace, with its words", () => {
  const a = store();
  assert.equal(a.note(3, "bell"), true);
  assert.equal(a.note(6, "bell"), true);
  const m = a.marks(view());
  assert.equal(m.tabs[0], null, "the shown tab never has a mark");
  assert.equal(glyphs(m.tabs[1]), "▲");
  assert.equal(m.tabs[1].words, "rang the bell");
  assert.equal(m.tabs[1].marks[0].kind, "bell");
  assert.equal(m.tabs[2], null);
  assert.equal(m.workspaces[0], null, "the shown workspace never has a mark");
  assert.equal(glyphs(m.workspaces[1]), "▲");
  assert.equal(m.workspaces[1].words, "rang the bell");
});

test("the shown tab and the shown workspace are null even when a latched pane sits in them", () => {
  const a = store();
  a.note(3, "bell");
  // The person shows the third pane's tab: the bell goes with it.
  a.follow([3], [1, 2, 3, 4, 5, 6, 7]);
  const shown = view({
    tabs: [
      { active: false, pane_ids: [1, 2] },
      { active: true, pane_ids: [3] },
      { active: false, pane_ids: [4] },
    ],
  });
  assert.deepEqual(a.marks(shown), { tabs: [null, null, null], workspaces: [null, null] });
});

test("follow clears the latched kinds of every shown pane and drops panes that are in no tab", () => {
  const a = store();
  a.note(3, "bell");
  a.note(4, "done");
  a.note(5, "failed");
  a.follow([3], [1, 2, 3, 4]); // pane 3 is shown now, pane 5 left every tab
  assert.deepEqual(a.marks(view()).tabs.map(glyphs), [null, null, "✓"]);
  assert.deepEqual(a.marks(view()).workspaces, [null, null], "the dropped pane marks nothing");
  // A dropped pane stays dropped when it comes back.
  a.follow([1, 2], [1, 2, 3, 4, 5, 6, 7]);
  assert.deepEqual(a.marks(view()).workspaces, [null, null]);
});

test("follow does not notify: the caller draws the bar next", () => {
  const a = store();
  a.note(3, "bell");
  let told = 0;
  a.subscribe(() => told++);
  a.follow([3], [1, 2, 3]);
  assert.equal(told, 0);
});

test("forget drops a pane and tells once", () => {
  const a = store();
  a.note(3, "bell");
  let told = 0;
  a.subscribe(() => told++);
  assert.equal(a.forget(3), true);
  assert.equal(a.forget(3), false, "a pane that holds nothing changes nothing");
  assert.equal(told, 1);
  assert.deepEqual(a.marks(view()).tabs, [null, null, null]);
});

test("subscribers are told once per real change and not for a repeat", () => {
  const a = store();
  let told = 0;
  const off = a.subscribe(() => told++);
  assert.equal(a.note(3, "bell"), true);
  assert.equal(a.note(3, "bell"), false, "a repeat changes nothing");
  assert.equal(a.note(3, "bell"), false);
  assert.equal(told, 1);
  assert.equal(a.note(3, "done"), true, "a second kind is a change");
  assert.equal(told, 2);
  off();
  a.note(4, "bell");
  assert.equal(told, 2, "an unsubscribed listener hears nothing");
});

test("several kinds on one tab are shown side by side, one symbol per kind, in the fixed order", () => {
  const a = store();
  // Latched out of order; the order of the symbols is the order of the kinds, not the order of arrival.
  a.note(3, "done");
  a.note(3, "failed");
  a.note(3, "bell");
  a.provide("chat", () => [3]);
  const m = a.marks(view()).tabs[1];
  assert.equal(glyphs(m), "?▲!✓");
  assert.deepEqual(m.marks.map((x) => x.kind), ["ask", "bell", "failed", "done"]);
  assert.equal(m.words, "waits for you, rang the bell, command failed, command finished");
});

test("a pane that holds both done and failed shows both", () => {
  const a = store();
  a.note(3, "done");
  a.note(3, "failed");
  assert.equal(glyphs(a.marks(view()).tabs[1]), "!✓");
});

test("a pill shows the kinds of all the panes of its workspace together, once each", () => {
  const a = store();
  a.note(6, "bell");
  a.note(7, "bell");
  a.note(5, "done");
  const m = a.marks(view()).workspaces[1];
  assert.equal(glyphs(m), "▲✓");
});

test("a waiting question is derived, not latched: it follows the provider and the pane being out of view", () => {
  const a = store();
  let waiting = [3];
  a.provide("chat", () => waiting);
  assert.equal(glyphs(a.marks(view()).tabs[1]), "?");
  assert.equal(glyphs(a.marks(view()).workspaces[0]), null);
  // Shown: hidden. Leaving again before the answer: back.
  a.follow([3], [1, 2, 3, 4, 5, 6, 7]);
  const shown = view({ tabs: [{ active: false, pane_ids: [1, 2] }, { active: true, pane_ids: [3] }, { active: false, pane_ids: [4] }] });
  assert.equal(a.marks(shown).tabs[1], null);
  a.follow([1, 2], [1, 2, 3, 4, 5, 6, 7]);
  assert.equal(glyphs(a.marks(view()).tabs[1]), "?");
  // Answered: gone for good.
  waiting = [];
  assert.equal(a.marks(view()).tabs[1], null);
});

test("changed tells subscribers once when who waits changes and not for a repeat; a provider that throws names none", () => {
  const a = store();
  let told = 0;
  a.subscribe(() => told++);
  let waiting = [];
  const off = a.provide("chat", () => waiting);
  a.changed();
  assert.equal(told, 0, "nobody waits and nobody did");
  waiting = [3];
  a.changed();
  a.changed();
  assert.equal(told, 1);
  waiting = [3, 4];
  a.changed();
  assert.equal(told, 2);
  off();
  assert.equal(told, 3, "taking a provider away changes who waits");
  a.provide("bad", () => {
    throw new Error("boom");
  });
  assert.doesNotThrow(() => a.marks(view()));
  assert.deepEqual(a.marks(view()).tabs, [null, null, null]);
});

test("a view without pane_ids marks nothing and does not throw", () => {
  const a = store();
  a.note(3, "bell");
  a.provide("chat", () => [3]);
  const bare = {
    workspaces: [{ active: true }, { active: false }],
    tabs: [{ active: true }, { active: false }],
  };
  assert.deepEqual(a.marks(bare), { tabs: [null, null], workspaces: [null, null] });
  const odd = { workspaces: [{ active: false, pane_ids: "3" }], tabs: [{ active: false, pane_ids: [null, "3", 1.5] }] };
  assert.deepEqual(a.marks(odd), { tabs: [null], workspaces: [null] });
});

test("pane ids are read as safe integers only, at most 1024", () => {
  assert.deepEqual(idsOf([1, 2.5, "3", null, 4, Number.MAX_SAFE_INTEGER + 2, -1]), [1, 4, -1]);
  assert.deepEqual(idsOf(undefined), []);
  assert.deepEqual(idsOf({ length: 3 }), []);
  assert.equal(idsOf(Array.from({ length: 5000 }, (_, i) => i)).length, 1024);
});

test("a pane id that is not a whole number latches nothing", () => {
  const a = store();
  assert.equal(a.note(Number.NaN, "bell"), false);
  assert.equal(a.note(1.5, "bell"), false);
});

test("at most 256 latched panes are kept, the oldest dropped first", () => {
  const a = new Attention();
  const ids = Array.from({ length: MAX_LATCHED + 40 }, (_, i) => 1000 + i);
  a.follow([], ids);
  for (const id of ids) a.note(id, "bell");
  const v = { workspaces: [{ active: false, pane_ids: ids.slice(0, 1024) }], tabs: [] };
  // The newest panes are still latched, the first ones are not: the pill holds a mark either way, so look per tab.
  const tabs = ids.map((id) => ({ active: false, pane_ids: [id] }));
  const m = a.marks({ workspaces: v.workspaces, tabs });
  const marked = m.tabs.map((t) => t !== null);
  assert.equal(marked.filter(Boolean).length, MAX_LATCHED);
  assert.equal(marked[0], false, "the oldest was dropped");
  assert.equal(marked[marked.length - 1], true, "the newest stays");
});
