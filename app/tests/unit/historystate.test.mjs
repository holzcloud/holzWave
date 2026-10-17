// The history on the page (src/historystate.ts, built into dist/).
import test from "node:test";
import assert from "node:assert/strict";
import { ago, meta, pieces, readHits, readOpenPane, shortDir } from "../../dist/historystate.js";

test("hits are read defensively", () => {
  assert.deepEqual(readHits(null), []);
  assert.deepEqual(readHits({}), []);
  const got = readHits([{ cmd: "ls", cwd: "/", host: "local", last: 5, count: 0, exit: 1.5, marks: [0, -1, "x", 2] }, { cmd: "" }, 7, { nocmd: 1 }]);
  assert.equal(got.length, 1);
  assert.deepEqual(got[0], { cmd: "ls", cwd: "/", host: "local", last: 5, count: 1, exit: null, marks: [0, 2] });
  assert.equal(readHits(Array.from({ length: 500 }, () => ({ cmd: "x" }))).length, 200);
});

test("the matched letters are cut into runs, counted in characters", () => {
  assert.deepEqual(pieces("kubectl get", [0, 8]), [["k", true], ["ubectl ", false], ["g", true], ["et", false]]);
  assert.deepEqual(pieces("abc", [0, 1]), [["ab", true], ["c", false]]);
  assert.deepEqual(pieces("a😀b", [1]), [["a", false], ["😀", true], ["b", false]]);
  assert.deepEqual(pieces("", []), []);
});

test("ages and the line under a command", () => {
  const now = 1_000_000_000;
  assert.equal(ago(now - 5, now), "now");
  assert.equal(ago(now - 5 * 60, now), "5 min ago");
  assert.equal(ago(now - 2 * 3600, now), "2 h ago");
  assert.equal(ago(now - 3 * 86400, now), "3 d ago");
  assert.equal(ago(now - 14 * 86400, now), "2 w ago");
  assert.equal(ago(now - 90 * 86400, now), "3 mo ago");
  assert.equal(ago(now + 100, now), "now", "a clock ahead is not the future");
  assert.equal(shortDir("/home/holz/infra", "/home/holz"), "~/infra");
  assert.equal(shortDir("/home/holzer", "/home/holz"), "/home/holzer");
  assert.equal(meta({ cmd: "x", cwd: "/home/holz/infra", host: "local", last: now - 7200, count: 14, exit: 2, marks: [] }, now, "/home/holz"), "~/infra · 2 h ago · ×14 · exit 2");
  assert.equal(meta({ cmd: "x", cwd: "", host: "local", last: now, count: 1, exit: 0, marks: [] }, now), "now");
});

test("the_open_event_payload_is_read_defensively", () => {
  assert.equal(readOpenPane({ pane: 1 }), 1);
  assert.equal(readOpenPane({ pane: 0 }), 0);
  assert.equal(readOpenPane({ pane: 1, extra: 2 }), 1, "other members are ignored, the pane is read");
  for (const bad of [null, undefined, [], "1", 1, true, {}, { pane: "1" }, { pane: -1 }, { pane: 1.5 }, { pane: NaN }, { pane: Infinity }, { pane: 2 ** 60 }, { pane: null }, { pane: [1] }, [{ pane: 1 }]]) {
    assert.equal(readOpenPane(bad), null, JSON.stringify(bad));
  }
});
