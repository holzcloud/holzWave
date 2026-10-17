// The monitor's data and words (src/monitorstate.ts, built into dist/).
import test from "node:test";
import assert from "node:assert/strict";
import { bar, bytes, memory, rate, readSample, uptime } from "../../dist/monitorstate.js";
import { blockContext, blockTitle } from "../../dist/blockstate.js";

test("a sample is read defensively", () => {
  assert.equal(readSample(null), null);
  assert.equal(readSample("x"), null);
  const s = readSample({ cpu: 250, cores: 8, mem_used: -5, mem_total: 16, net_rx: "x", procs: [{ pid: 4, name: "a".repeat(99), cpu: 12.5, mem: 3 }, 7, null] });
  assert.equal(s.cpu, 100, "a percentage is capped");
  assert.equal(s.mem_used, 0, "a negative number is zero");
  assert.equal(s.net_rx, 0);
  assert.equal(s.procs.length, 1);
  assert.equal(s.procs[0].name.length, 40);
  assert.equal(readSample({ procs: Array.from({ length: 100 }, () => ({ pid: 1, name: "x", cpu: 0, mem: 0 })) }).procs.length, 32);
});

test("a bar lights segments in proportion, never none above zero, and has a level", () => {
  assert.deepEqual(bar(0), { on: 0, level: "ok" });
  assert.deepEqual(bar(1), { on: 1, level: "ok" });
  assert.deepEqual(bar(38), { on: 3, level: "ok" });
  assert.deepEqual(bar(75), { on: 6, level: "warn" });
  assert.deepEqual(bar(100), { on: 8, level: "danger" });
  assert.deepEqual(bar(-4), { on: 0, level: "ok" });
  assert.deepEqual(bar(900), { on: 8, level: "danger" });
});

test("sizes, rates, memory and uptime read like the design", () => {
  assert.equal(bytes(0), "0B");
  assert.equal(bytes(84 * 1024), "84K");
  assert.equal(bytes(1.5 * 1024 * 1024), "1.5M");
  assert.equal(rate(1.2 * 1024 * 1024), "1.2 MB/s");
  assert.equal(rate(84 * 1024), "84 KB/s");
  assert.equal(rate(12), "12 B/s");
  assert.equal(memory(6.1 * 1024 ** 3, 16 * 1024 ** 3), "6.1/16G");
  assert.equal(uptime(90 * 60), "1 h 30 min");
  assert.equal(uptime(3 * 86400 + 4 * 3600), "3 d 4 h");
  assert.equal(uptime(5 * 60), "5 min");
});

test("the monitor's header says what it is", () => {
  const b = { pane: 1, kind: "monitor", source: "local", path: "", source_label: "This computer" };
  assert.equal(blockTitle(b), "system");
  assert.equal(blockContext(b), "local · 1 s");
  const remote = { pane: 2, kind: "monitor", source: "talos", path: "", source_label: "Talos" };
  assert.equal(blockContext(remote), "Talos · 1 s", "another computer says its name");
  assert.equal(blockContext({ ...remote, source_label: "" }), "talos · 1 s", "an empty label falls back to the source id");
  assert.equal(blockTitle(remote), "system");
});
