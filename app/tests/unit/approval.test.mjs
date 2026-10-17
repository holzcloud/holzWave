// The controller of one chat's waiting calls (approval.ts): which cards exist, and the one decision command that a press
// makes. A fake `invoke` records every call and a fake clock stands in for the time; no DOM and no bridge are needed.
import test from "node:test";
import assert from "node:assert/strict";
import { Approvals } from "../../dist/approval.js";

const HASH_A = "a".repeat(63) + "1";
const HASH_B = "b".repeat(63) + "2";

/** The core's card of a `read_file` call. */
function card(id, hash, extra = {}) {
  const rows = [
    ["tool", "read_file"], ["pane", "1"], ["target", "This computer"], ["remote", "false"], ["path", `/work/${id}.txt`],
    ["offset", "0"], ["limit", "16384"], ["resolved_path", `/work/${id}.txt`],
  ];
  return {
    conversation: "chat-1", call_id: id, tool: "read_file", effect: "read", pane: 1, target: "This computer", remote: false,
    payload: rows.map(([name, value]) => ({ name, value, bytes: value.length })), hash, tainted: false, taint: [], ...extra,
  };
}

/** A controller over a fake core. `answer` is what `invoke` does for the decision command. */
function make({ answer = async () => null, start = 1_000_000 } = {}) {
  const calls = [];
  const said = [];
  const refreshed = [];
  const clock = { now: start };
  const approvals = new Approvals({
    invoke: async (cmd, args) => {
      calls.push([cmd, structuredClone(args)]);
      return answer(cmd, args);
    },
    conversation: () => "chat-1",
    now: () => clock.now,
    say: (text, kind) => said.push([text, kind]),
    refresh: () => refreshed.push(1),
  });
  return { approvals, calls, said, refreshed, clock };
}

test("a decision sends the four ids and the card's own hash, once", async () => {
  const { approvals, calls } = make();
  const seenBy = [];
  const stop = approvals.subscribe(() => seenBy.push(approvals.cards.map((c) => c.mode).join()));
  approvals.sync([card("call_1", HASH_A), card("call_2", HASH_B)]);
  assert.deepEqual(calls, [], "nothing is sent by constructing, subscribing or syncing");
  assert.equal(approvals.cards.length, 2);
  assert.deepEqual(approvals.cards.map((c) => c.mode), ["ask", "ask"]);

  await approvals.decide("call_1", "allow");
  assert.equal(calls.length, 1);
  assert.equal(calls[0][0], "ai_decide");
  assert.deepEqual(Object.keys(calls[0][1]), ["conversation", "call_id", "decision", "hash"]);
  assert.deepEqual(calls[0][1], { conversation: "chat-1", call_id: "call_1", decision: "allow", hash: HASH_A });
  assert.equal(approvals.cards[0].mode, "sent");
  assert.equal(approvals.cards[0].verdict, "allow");
  assert.equal(approvals.cards[1].mode, "ask", "the other card is untouched");

  await approvals.decide("call_2", "deny");
  assert.equal(calls.length, 2);
  assert.deepEqual(calls[1][1], { conversation: "chat-1", call_id: "call_2", decision: "deny", hash: HASH_B });
  assert.equal(approvals.cards[1].mode, "sent");
  assert.equal(approvals.cards[1].verdict, "deny");

  // No argument holds a row, a path or a result.
  const text = JSON.stringify(calls);
  for (const word of ["/work/", "read_file", "This computer", "16384"]) assert.ok(!text.includes(word), word);
  assert.ok(seenBy.length >= 2, "the listener was told about the changes");
  stop();
});
