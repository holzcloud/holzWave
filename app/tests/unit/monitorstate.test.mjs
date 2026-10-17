// The monitor's data and words (src/monitorstate.ts, built into dist/).
import test from "node:test";
import assert from "node:assert/strict";
import { bar, bytes, memory, noteWords, rate, readReading, readSample, uptime } from "../../dist/monitorstate.js";
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

test("a reading is read by its state word and anything else is no reading", () => {
  const live = { state: "live", cpu: 38, cores: 4, mem_used: 1, mem_total: 2, net_rx: 3, net_tx: 4, uptime: 5, procs: [] };
  assert.deepEqual(readReading(live), { state: "live", sample: readSample(live), system: "" });
  assert.deepEqual(readReading({ state: "unsupported", system: "Darwin" }), { state: "unsupported", sample: null, system: "Darwin" });
  const markup = readReading({ state: "unsupported", system: "<i>x y</i>" });
  assert.equal(markup.system, "ixyi", "markup, spaces and slashes are cut out of the word");
  assert.equal(readReading({ state: "unsupported", system: "x".repeat(40) }).system, "x".repeat(24));
  assert.equal(readReading({ state: "unsupported" }).system, "", "no word is an empty word");
  assert.equal(readReading({ state: "unsupported", system: 7 }).system, "");
  for (const bad of [null, undefined, "live", 3, [], [live], {}, { cpu: 1 }, { state: "LIVE" }, { state: "weird" }, { state: 1 }]) {
    assert.equal(readReading(bad), null, JSON.stringify(bad));
  }
});

test("the note words name the host and bound the detail", () => {
  assert.deepEqual(noteWords("connecting", "Talos", ""), { text: "Connecting to Talos …", tone: "plain" });
  assert.deepEqual(noteWords("no_answer", "Talos", "the connection was lost"), { text: "No answer from Talos: the connection was lost", tone: "warn" });
  assert.deepEqual(noteWords("no_answer", "Talos", ""), { text: "No answer from Talos.", tone: "warn" });
  assert.deepEqual(noteWords("unsupported", "Mac", "Darwin"), { text: "Not supported: Mac does not run Linux (it says Darwin).", tone: "warn" });
  assert.deepEqual(noteWords("unsupported", "Mac", ""), { text: "Not supported: Mac did not answer like a Linux computer.", tone: "warn" });
  const long = noteWords("no_answer", "n".repeat(100), "r".repeat(300)).text;
  assert.equal(long, `No answer from ${"n".repeat(64)}…: ${"r".repeat(200)}…`, "the name is cut to 64 and the reason to 200, each with an ellipsis");
  assert.equal(noteWords("no_answer", "a\u0007b\n", "c\u001bd\u0000").text, "No answer from ab: cd", "control characters are removed");
  assert.equal(noteWords("no_answer", "<b>x</b>", "<script>1</script>").text, "No answer from <b>x</b>: <script>1</script>", "markup stays plain characters");
  assert.equal(noteWords("unsupported", "Mac", "Da rwin<>").text, "Not supported: Mac does not run Linux (it says Darwin).");
});
