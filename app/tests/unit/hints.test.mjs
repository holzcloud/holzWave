// Hint labels and key reading on the compiled output in dist/: no DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import { HINT_ALPHABET, MAX_HINTS, hintLabels, hintLetter, resolveHint } from "../../dist/hints.js";

const prefixFree = (labels) => {
  for (const a of labels) {
    for (const b of labels) {
      if (a !== b) assert.ok(!b.startsWith(a), `"${a}" is a prefix of "${b}"`);
    }
  }
};

test("up to the alphabet length every label is one letter, in alphabet order", () => {
  assert.equal(HINT_ALPHABET.length, 26);
  assert.deepEqual(hintLabels(0), []);
  assert.deepEqual(hintLabels(1), ["a"]);
  assert.deepEqual(hintLabels(3), ["a", "b", "c"]);
  assert.deepEqual(hintLabels(26), [...HINT_ALPHABET]);
});

test("labels are unique, prefix-free and as many as asked, for every count up to the limit", () => {
  for (const n of [1, 2, 25, 26, 27, 28, 50, 51, 52, 100, 300, 650, 675, 676]) {
    const labels = hintLabels(n);
    assert.equal(labels.length, n, `${n} targets get ${n} labels`);
    assert.equal(new Set(labels).size, n, `${n}: a label repeats`);
    for (const l of labels) assert.match(l, /^[a-z]{1,2}$/);
    prefixFree(labels);
  }
});

test("beyond the alphabet two letters are used, singles stay singles as far as possible", () => {
  const labels = hintLabels(27);
  assert.equal(labels.filter((l) => l.length === 2).length, 2, "one prefix letter, used as far as the count needs it");
  assert.equal(labels.filter((l) => l.length === 1).length, 25);
  assert.deepEqual(labels.slice(0, 25), [...HINT_ALPHABET].slice(0, 25), "the first targets keep the single letters");
  assert.deepEqual(labels.slice(25), ["za", "zb"]);
  assert.equal(hintLabels(676).every((l) => l.length === 2), true, "at the limit every label is two letters");
});

test("a count beyond the limit gets the limit's labels, and a count that is not a count gets none", () => {
  assert.equal(MAX_HINTS, 676);
  assert.equal(hintLabels(5000).length, MAX_HINTS);
  assert.deepEqual(hintLabels(-1), []);
  assert.deepEqual(hintLabels(Number.NaN), []);
  assert.deepEqual(hintLabels(Number.POSITIVE_INFINITY).length, MAX_HINTS);
  assert.deepEqual(hintLabels(2.9).length, 2, "a fraction counts whole targets");
});

test("typed letters resolve to a hit, a partial label or nothing", () => {
  const labels = hintLabels(30); // 4 singles a-d... and prefixes
  const single = labels.find((l) => l.length === 1);
  const double = labels.find((l) => l.length === 2);
  assert.deepEqual(resolveHint(labels, single), { state: "hit", index: labels.indexOf(single) });
  assert.deepEqual(resolveHint(labels, double[0]), { state: "partial" });
  assert.deepEqual(resolveHint(labels, double), { state: "hit", index: labels.indexOf(double) });
  assert.deepEqual(resolveHint(labels, double + "a"), { state: "none" });
  assert.deepEqual(resolveHint(labels, "1"), { state: "none" });
  assert.deepEqual(resolveHint([], "a"), { state: "none" });
  assert.deepEqual(resolveHint(labels, ""), { state: "partial" }, "nothing typed yet is still a partial label");
});

test("only a plain letter, with or without Shift, is a label letter", () => {
  assert.equal(hintLetter({ key: "a" }), "a");
  assert.equal(hintLetter({ key: "A" }), "a", "Shift gives the capital; the label is lower case");
  assert.equal(hintLetter({ key: "z" }), "z");
  for (const e of [
    { key: "a", metaKey: true }, { key: "a", ctrlKey: true }, { key: "a", altKey: true },
    { key: "1" }, { key: "Escape" }, { key: "Enter" }, { key: " " }, { key: "é" }, { key: "Shift" }, { key: "aa" },
  ]) {
    assert.equal(hintLetter(e), null, JSON.stringify(e));
  }
});
