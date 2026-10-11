// The palette's matcher and the pure helpers of palette.ts on the compiled output in dist/: none of it needs a DOM.
import test from "node:test";
import assert from "node:assert/strict";
import { fuzzyScore, rank } from "../../dist/fuzzy.js";
import { chordLabel, sectionOf } from "../../dist/palette.js";

test("a query that is not a subsequence of the text has no score", () => {
  assert.equal(fuzzyScore("xyz", "Split right"), null);
  assert.equal(fuzzyScore("spl", "Select tab 1"), null, "no p in the text");
  assert.equal(fuzzyScore("tsp", "Split right"), null, "the order of the characters counts");
  assert.equal(fuzzyScore("splitt", "Split"), null, "a query longer than the text cannot match");
  assert.equal(fuzzyScore("aa", "a"), null, "a character cannot match twice");
});

test("matching ignores case in both directions", () => {
  assert.notEqual(fuzzyScore("spl", "Split right"), null);
  assert.notEqual(fuzzyScore("SPL", "split right"), null);
  assert.ok(fuzzyScore("Spl", "Split right") >= fuzzyScore("spl", "Split right"), "the same case is never worse");
});

test("a match scores higher than a text that does not match at all", () => {
  const hit = fuzzyScore("spl", "Split right");
  assert.equal(typeof hit, "number");
  assert.ok(Number.isFinite(hit));
  assert.equal(fuzzyScore("spl", "Select tab 1"), null);
});

test("consecutive characters score higher than scattered ones", () => {
  assert.ok(fuzzyScore("tab", "New tab") > fuzzyScore("tab", "Toggle a bar"));
  assert.ok(fuzzyScore("spl", "Split right") > fuzzyScore("spl", "Show pane left"));
});

test("characters that start words score higher than characters inside them", () => {
  assert.ok(fuzzyScore("sr", "Split right") > fuzzyScore("sr", "Sprint"));
  assert.ok(fuzzyScore("np", "New pane") > fuzzyScore("np", "Snippet"));
  // a camelCase hump and a character after punctuation start words too
  assert.ok(fuzzyScore("fb", "fooBar") > fuzzyScore("fb", "fabulous"));
  assert.ok(fuzzyScore("tp", "Theme: Papier") > fuzzyScore("tp", "Attempt"));
});

test("an earlier match beats a later one for the same text shape", () => {
  assert.ok(fuzzyScore("a", "ab") > fuzzyScore("a", "ba"));
});

test("an empty query matches everything with one equal score", () => {
  assert.equal(fuzzyScore("", "Split right"), 0);
  assert.equal(fuzzyScore("", ""), 0);
  assert.equal(fuzzyScore("", "A much longer title than the other one"), 0);
});

test("the matcher handles astral characters, empty text and very long input", () => {
  assert.equal(fuzzyScore("a", ""), null);
  assert.notEqual(fuzzyScore("\u{1F600}", "smile \u{1F600}"), null);
  const long = "x".repeat(50_000);
  assert.equal(fuzzyScore(long, "Split"), null);
  assert.notEqual(fuzzyScore("x", long), null);
});

test("rank sorts by score descending and keeps the registration order for equal scores", () => {
  const items = [
    { title: "Sprout" },
    { title: "Split pane right" },
    { title: "Select tab 1" },
    { title: "Show pane left" },
    { title: "Split pane right" },
  ];
  const out = rank("spl", items, (i) => i.title);
  assert.deepEqual(
    out.map((r) => r.index),
    [1, 4, 3],
    "the two equal titles keep their order, the scattered match follows, non-matches are gone",
  );
  for (let n = 1; n < out.length; n++) assert.ok(out[n - 1].score >= out[n].score);
});

test("an empty query keeps every item in its registration order", () => {
  const items = ["b", "a", "c"].map((title) => ({ title }));
  const out = rank("", items, (i) => i.title);
  assert.deepEqual(out.map((r) => r.item.title), ["b", "a", "c"]);
  assert.deepEqual(out.map((r) => r.index), [0, 1, 2]);
  assert.equal(new Set(out.map((r) => r.score)).size, 1, "equal scores");
});

test("rank does not modify the items it is given", () => {
  const items = ["b", "a"];
  rank("a", items, (s) => s);
  assert.deepEqual(items, ["b", "a"]);
});

test("the section of an action id is its prefix, underscores as spaces", () => {
  assert.equal(sectionOf("pane.split_right"), "pane");
  assert.equal(sectionOf("copy_mode.enter"), "copy mode");
  assert.equal(sectionOf("theme.set.Papier"), "theme");
  assert.equal(sectionOf("noprefix"), "noprefix");
});

test("the chord label is the table's chord and empty for an action nothing is bound to", () => {
  assert.equal(chordLabel("pane.split_right"), "Cmd+D");
  assert.equal(chordLabel("palette.open"), "Cmd+K");
  assert.equal(chordLabel("no.such.action"), "");
});
