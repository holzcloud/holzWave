// The pure half of the approval card (approvalstate.ts): how a pending call is read from the core's card, what the page
// shows of it, and which characters are written as escapes. The recorded tool run (tests/fixtures/ai_tool_stream.json) is
// the card the core really sent. No DOM and no bridge are needed.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { canAllow, canDeny, readApproval, showText } from "../../dist/approvalstate.js";

const RUN = JSON.parse(readFileSync(new URL("../fixtures/ai_tool_stream.json", import.meta.url), "utf8"));
const HASH = "9f3a41c2d7be5a60b1c4e8f2d3a7094b6c5d1e0f8a2b3c4d5e6f708192a3b4c5";

/** The eleven members of the core's card, with the payload of a `read_file` call in the core's order. */
function card(extra = {}) {
  const rows = [
    ["tool", "read_file"], ["pane", "1"], ["target", "This computer"], ["remote", "false"], ["path", "/home/u/notes.txt"],
    ["offset", "0"], ["limit", "16384"], ["resolved_path", "/home/u/notes.txt"],
  ];
  return {
    conversation: "chat-1", call_id: "call_1", tool: "read_file", effect: "read", pane: 1, target: "This computer", remote: false,
    payload: rows.map(([name, value]) => ({ name, value, bytes: new TextEncoder().encode(value).length })), hash: HASH,
    tainted: false, taint: [], ...extra,
  };
}

test("a pending entry is read member by member and the hash stays exactly as sent", () => {
  const seen = readApproval(card());
  assert.equal(seen.why, "ok");
  assert.equal(seen.id, "call_1");
  assert.equal(seen.tool, "read_file");
  assert.equal(seen.hash, HASH);
  assert.deepEqual(
    Object.keys(seen.approval).sort(),
    ["call_id", "conversation", "effect", "hash", "pane", "remote", "rows", "taint", "tainted", "target", "tool"],
  );
  assert.equal(seen.approval.hash, HASH);
  assert.deepEqual(seen.approval.rows.map((r) => r.name), ["tool", "pane", "target", "remote", "path", "offset", "limit", "resolved_path"]);
  assert.deepEqual(seen.approval.rows[4], { name: "path", value: "/home/u/notes.txt", bytes: 17 });
  assert.equal(canAllow(seen), true);
  assert.equal(canDeny(seen), true);

  // A member that is not part of the card is never copied: not by name, and an own `__proto__` never.
  const hostile = JSON.parse(
    `{"text":"t","command":"c","api_key":"k","__proto__":{"polluted":true},${JSON.stringify(card()).slice(1)}`,
  );
  const read = readApproval(hostile);
  assert.equal(read.why, "ok");
  const text = JSON.stringify(read);
  for (const word of ["\"text\"", "\"command\"", "api_key", "polluted", "__proto__"]) assert.ok(!text.includes(word), word);
  assert.equal(Object.getPrototypeOf(read.approval), Object.prototype);

  // A hash that is not 64 lower-case hexadecimal characters is no hash: no card, no way to answer.
  for (const bad of [HASH.toUpperCase(), HASH.slice(1), `${HASH}0`, "", 1234, null, undefined]) {
    const entry = readApproval(card({ hash: bad }));
    assert.equal(entry.hash, null, String(bad));
    assert.equal(entry.approval, null);
    assert.equal(entry.why, "shape");
    assert.equal(canAllow(entry), false);
    assert.equal(canDeny(entry), false);
  }

  // Anything that is not an entry with a call id and a tool is nothing.
  for (const nothing of [null, undefined, "x", 7, [], [card()], {}, card({ call_id: 5 }), card({ call_id: undefined })]) {
    assert.equal(readApproval(nothing), null, JSON.stringify(nothing));
  }

  // The first row names the tool the entry names.
  assert.equal(readApproval(card({ tool: "grep" })).why, "shape");
  const swapped = card();
  swapped.payload[0] = { name: "tool", value: "list_dir", bytes: 8 };
  assert.equal(readApproval(swapped).why, "shape");

  // The flag is read as a flag; labels make a card tainted by themselves, are strings, at most 8, cleaned and cut at 80.
  assert.equal(readApproval(card({ tainted: "yes" })).approval.tainted, false);
  assert.equal(readApproval(card({ tainted: true })).approval.tainted, true);
  const labelled = readApproval(card({ tainted: false, taint: ["output of list_dir"] })).approval;
  assert.equal(labelled.tainted, true);
  assert.deepEqual(labelled.taint, ["output of list_dir"]);
  const many = readApproval(card({ taint: Array.from({ length: 12 }, (_, i) => `output of t${i}`).concat([7, null]) })).approval;
  assert.equal(many.taint.length, 8);
  const long = readApproval(card({ taint: [`a\u202eb${"x".repeat(200)}`] })).approval;
  assert.equal(Array.from(long.taint[0]).length <= 80, true);
  assert.ok(!long.taint[0].includes("\u202e"));
  assert.equal(readApproval(card({ taint: "output of x" })).why, "shape");
});

test("every card of the recorded tool run reads, keeps its hash and its field order", () => {
  const entries = [];
  for (const state of RUN.states) entries.push(...state.pending);
  const ids = new Set(entries.map((entry) => `${entry.call_id}`));
  assert.ok(ids.size >= 2, "the run holds at least two distinct cards");
  for (const entry of entries) {
    const seen = readApproval(entry);
    assert.equal(seen.why, "ok", entry.call_id);
    assert.equal(seen.approval.hash, entry.hash);
    assert.equal(seen.hash, entry.hash);
    assert.deepEqual(seen.approval.rows.map((r) => r.name), entry.payload.map((p) => p.name));
    assert.deepEqual(seen.approval.rows.map((r) => r.value), entry.payload.map((p) => p.value));
    if (entry.tool === "read_file") {
      assert.deepEqual(seen.approval.rows.map((r) => r.name), ["tool", "pane", "target", "remote", "path", "offset", "limit", "resolved_path"]);
    }
  }
  // The snapshot taken while a card waits holds exactly one, and it reads like the card of the event.
  const waiting = RUN.snapshots.filter((s) => s.state.pending.length > 0);
  assert.ok(waiting.length >= 1);
  for (const snap of waiting) {
    assert.equal(snap.state.pending.length, 1);
    assert.equal(readApproval(snap.state.pending[0]).why, "ok");
  }
  const proposed = RUN.events.filter((e) => e.kind === "tool_proposed");
  assert.ok(proposed.length >= 2);
  for (const event of proposed) assert.equal(readApproval(event.pending).approval.hash, event.pending.hash);
});

test("characters that would hide something are shown as escapes, never dropped", () => {
  const hidden = ["\u0000", "\u001b", "\u007f", "\u0085", "\u009f", "\r", "\n", "\t", "\u202e", "\u2066", "\u200b", "\u200d", "\u2060", "\ufeff", "\u{e0041}"];
  const shown = showText(`a${hidden.join("|")}z`);
  for (const raw of hidden) assert.ok(!shown.includes(raw), `a raw ${JSON.stringify(raw)} is left`);
  assert.ok(shown.startsWith("a") && shown.endsWith("z"));
  for (const escape of ["\\u{0}", "\\u{1b}", "\\u{7f}", "\\u{85}", "\\u{9f}", "\\n", "\\r", "\\t", "\\u{202e}", "\\u{2066}", "\\u{200b}", "\\u{200d}", "\\u{2060}", "\\u{feff}", "\\u{e0041}"]) {
    assert.ok(shown.includes(escape), `${escape} is missing from ${shown}`);
  }
  // Nothing else changes.
  for (const plain of ["plain text", "é", "naïve café", "😀 ok", "a b", "日本語", ""]) assert.equal(showText(plain), plain);
  // Two different hidden characters never look the same.
  assert.notEqual(showText("\u200b"), showText("\u200c"));
  assert.notEqual(showText("\u202d"), showText("\u202e"));
  assert.notEqual(showText("\u0001"), showText("\u0002"));
  // A card whose value holds a direction override keeps the escape.
  const entry = card();
  entry.payload[4] = { name: "path", value: "/tmp/a\u202egnp.txt", bytes: 17 };
  const seen = readApproval(entry);
  assert.equal(seen.approval.rows[4].value, "/tmp/a\\u{202e}gnp.txt");
  assert.ok(!JSON.stringify(seen).includes("\u202e"));
});

test("a request that cannot be shown in full can only be denied", () => {
  const rowsOf = (count) => Array.from({ length: count }, (_, i) => (i === 0 ? { name: "tool", value: "read_file", bytes: 9 } : { name: `f${i}`, value: "x", bytes: 1 }));
  const verdict = (entry, why) => {
    const seen = readApproval(entry);
    assert.equal(seen.why, why, JSON.stringify(entry).slice(0, 80));
    assert.equal(seen.approval, null);
    assert.equal(canDeny(seen), true, "a valid hash can still be denied");
    assert.equal(canAllow(seen), false);
    assert.equal(seen.hash, HASH);
  };

  // Too large: more than 16 rows, a value over 65536 characters, more than 131072 characters in all.
  assert.equal(readApproval(card({ payload: rowsOf(16) })).why, "ok");
  verdict(card({ payload: rowsOf(17) }), "large");
  const big = card();
  big.payload[4] = { name: "path", value: "a".repeat(65537), bytes: 65537 };
  verdict(big, "large");
  const edge = card();
  edge.payload[4] = { name: "path", value: "a".repeat(65536), bytes: 65536 };
  assert.equal(readApproval(edge).why, "ok");
  const total = card();
  total.payload = [{ name: "tool", value: "read_file", bytes: 9 }, ...Array.from({ length: 3 }, (_, i) => ({ name: `f${i}`, value: "b".repeat(50000), bytes: 50000 }))];
  verdict(total, "large");

  // A tool and an effect the page does not know.
  for (const tool of ["write_file", "x"]) {
    const entry = card({ tool });
    entry.payload[0] = { name: "tool", value: tool, bytes: tool.length };
    verdict(entry, "tool");
  }
  for (const effect of ["write", "exec", "x"]) verdict(card({ effect }), "effect");

  // A field of the wrong shape: a bad name, a bad length, a value that is not a string.
  for (const name of ["Path", "", "a".repeat(41), "a b"]) {
    const entry = card();
    entry.payload[4] = { name, value: "v", bytes: 1 };
    verdict(entry, "shape");
  }
  for (const bytes of [-1, 1.5, 4294967296, "3", null]) {
    const entry = card();
    entry.payload[4] = { name: "path", value: "v", bytes };
    verdict(entry, "shape");
  }
  const odd = card();
  odd.payload[4] = { name: "path", value: 12, bytes: 2 };
  verdict(odd, "shape");

  // With no valid hash there is nothing to repeat: not even a denial.
  const none = readApproval(card({ payload: rowsOf(17), hash: "nope" }));
  assert.equal(canDeny(none), false);
  assert.equal(canAllow(none), false);

  // The same entry made valid again is `ok`.
  assert.equal(readApproval(card()).why, "ok");
});
