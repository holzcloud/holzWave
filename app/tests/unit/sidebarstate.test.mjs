// The sidebar's data, the Pomodoro clock and the state of a host (src/sidebarstate.ts, built into dist/).
import test from "node:test";
import assert from "node:assert/strict";
import { IDLE, hostState, left, mmss, pause, phaseWord, progress, readWidgets, resume, start, tick } from "../../dist/sidebarstate.js";

test("widgets are read defensively: only what the page can draw", () => {
  assert.deepEqual(readWidgets(null), { widgets: [], notes: {}, warnings: [] });
  const got = readWidgets({
    widgets: [
      { id: "notes", title: "Notes", kind: "note" },
      { id: "Bad Id", title: "x", kind: "note" },
      { id: "p", title: "P", kind: "timer", work_min: 0, break_min: 5 },
      { id: "l", title: "L", kind: "list", items: [{ label: "a", command: "ls" }, { label: "b", url: "javascript:x" }, { label: "c", url: "https://x.y" }, { label: "d", command: "x", url: "https://x.y" }, { label: "", command: "x" }] },
      { id: "s", title: "S", kind: "script" },
      7,
    ],
    notes: { notes: "hello", "Bad Key": "x", n: 5 },
    warnings: ["widgets/x.json: bad", 3],
  });
  assert.deepEqual(got.widgets.map((w) => w.id), ["notes", "p", "l"]);
  assert.equal(got.widgets[1].work_min, 1, "a timer works at least a minute");
  assert.deepEqual(got.widgets[2].items, [{ label: "a", command: "ls" }, { label: "c", url: "https://x.y" }], "a link must be http(s); an item has one of command or url");
  assert.deepEqual(got.notes, { notes: "hello" });
  assert.deepEqual(got.warnings, ["widgets/x.json: bad"]);
});

test("the clock runs work, then break, then ends; pause keeps what is left", () => {
  const def = { work_min: 25, break_min: 5 };
  const t0 = 1_000_000;
  let s = start(def, t0);
  assert.equal(phaseWord(s), "working");
  assert.equal(mmss(left(s, def, t0)), "25:00");
  assert.equal(mmss(left(s, def, t0 + 61_000)), "23:59");
  assert.equal(progress(s, def, t0 + 12.5 * 60_000), 0.5);
  s = pause(s, t0 + 60_000);
  assert.equal(phaseWord(s), "working · paused");
  assert.equal(tick(s, def, t0 + 99 * 60_000), s, "a paused clock does not run out");
  assert.equal(mmss(left(s, def, t0 + 99 * 60_000)), "24:00");
  s = resume(s, t0 + 100 * 60_000);
  assert.equal(mmss(left(s, def, t0 + 100 * 60_000)), "24:00");
  const end = s.endsAt;
  s = tick(s, def, end + 1);
  assert.equal(s.mode, "break");
  assert.equal(s.endsAt, end + 5 * 60_000, "the break follows without a gap");
  assert.deepEqual(tick(s, def, s.endsAt + 1), IDLE);
  assert.deepEqual(tick(start({ work_min: 1 }, 0), { break_min: 0 }, 60_001), IDLE, "no break: it ends");
  assert.equal(phaseWord(IDLE), "ready");
  assert.equal(mmss(-5), "00:00");
});

test("a host is connected when any pane runs it, waiting while it reconnects, failed when it could not", () => {
  const c = (profile, state, attempt = 0, of = 0) => ({ pane: 1, profile, name: profile, state, attempt, of, secs: 0 });
  assert.deepEqual(hostState("a", []), { state: "idle", note: "" });
  assert.deepEqual(hostState("a", [c("a", "connected")]), { state: "connected", note: "" });
  assert.deepEqual(hostState("a", [c("a", "reconnecting", 2, 5)]), { state: "wait", note: "⟳ 2/5" });
  assert.deepEqual(hostState("a", [c("a", "connecting")]), { state: "wait", note: "connecting" });
  assert.deepEqual(hostState("a", [c("a", "failed")]), { state: "failed", note: "failed" });
  assert.equal(hostState("a", [c("a", "failed"), c("a", "connected")]).state, "connected");
  assert.equal(hostState("a", [c("b", "connected")]).state, "idle");
});
