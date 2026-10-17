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
  const { approvals, calls, clock } = make();
  const seenBy = [];
  const stop = approvals.subscribe(() => seenBy.push(approvals.cards.map((c) => c.mode).join()));
  approvals.sync([card("call_1", HASH_A), card("call_2", HASH_B)]);
  assert.deepEqual(calls, [], "nothing is sent by constructing, subscribing or syncing");
  assert.equal(approvals.cards.length, 2);
  assert.deepEqual(approvals.cards.map((c) => c.mode), ["ask", "ask"]);

  clock.now += 1000;
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

test("an allow right after a card appeared is ignored, a deny never is", async () => {
  const { approvals, calls, clock } = make({ start: 1000 });
  approvals.sync([card("call_1", HASH_A)]);
  clock.now = 1200;
  await approvals.decide("call_1", "allow");
  assert.equal(calls.length, 0, "an allow within the window makes no call");
  assert.equal(approvals.cards[0].mode, "ask");
  clock.now = 1499;
  await approvals.decide("call_1", "allow");
  assert.equal(calls.length, 0);
  clock.now = 1500;
  await approvals.decide("call_1", "allow");
  assert.equal(calls.length, 1, "after the window the allow goes");

  // A deny is never held back.
  approvals.sync([card("call_2", HASH_B)]);
  clock.now = 1501;
  await approvals.decide("call_2", "deny");
  assert.equal(calls.length, 2);
  assert.equal(calls[1][1].decision, "deny");

  // A re-sync of the same card keeps its first time; a new hash starts a new record with a new time and no fault.
  const again = make({ start: 5000 });
  again.approvals.sync([card("call_1", HASH_A)]);
  again.clock.now = 5400;
  again.approvals.sync([card("call_1", HASH_A)]);
  again.clock.now = 5500;
  await again.approvals.decide("call_1", "allow");
  assert.equal(again.calls.length, 1, "the window ran from the first sight");
  const fresh = make({ start: 9000, answer: async () => { throw { code: "tool", reason: "denied", text: "nope" }; } });
  fresh.approvals.sync([card("call_1", HASH_A)]);
  fresh.clock.now = 9600;
  await fresh.approvals.decide("call_1", "allow");
  assert.equal(fresh.approvals.cards[0].fault.text, "nope");
  fresh.clock.now = 9700;
  fresh.approvals.sync([card("call_1", HASH_B)]);
  const next = fresh.approvals.cards[0];
  assert.equal(next.mode, "ask");
  assert.equal(next.fault, null);
  assert.equal(next.since, 9700);
  await fresh.approvals.decide("call_1", "allow");
  assert.equal(fresh.calls.length, 1, "a new hash restarts the window");
});

test("a refusal moves the card to the step its reason names and nothing is retried", async () => {
  const refuse = (reason, text = "The core says no.") => async () => {
    throw { code: "tool", reason, text };
  };
  for (const reason of ["unknown_call", "not_pending"]) {
    const t = make({ answer: refuse(reason) });
    t.approvals.sync([card("call_1", HASH_A)]);
    await t.approvals.decide("call_1", "deny");
    assert.equal(t.approvals.cards[0].mode, "ended", reason);
    assert.deepEqual(t.said, [["That call is no longer waiting.", "note"]]);
    assert.equal(t.refreshed.length, 1);
    assert.equal(t.calls.length, 1, "no second invoke");
    await t.approvals.decide("call_1", "deny");
    assert.equal(t.calls.length, 1, "an ended card decides nothing");
  }
  const changed = make({ answer: refuse("hash_mismatch") });
  changed.approvals.sync([card("call_1", HASH_A)]);
  await changed.approvals.decide("call_1", "deny");
  assert.equal(changed.approvals.cards[0].mode, "ask");
  assert.equal(changed.approvals.cards[0].fault.text, "The request changed. Nothing ran. Read the card again.");
  assert.equal(changed.refreshed.length, 1);
  assert.equal(changed.calls.length, 1);
  for (const reason of ["invalid_decision", "denied", "host", "something_new", null]) {
    const t = make({ answer: refuse(reason, "Core text.") });
    t.approvals.sync([card("call_1", HASH_A)]);
    await t.approvals.decide("call_1", "deny");
    assert.equal(t.approvals.cards[0].mode, "ask", String(reason));
    assert.equal(t.approvals.cards[0].fault.text, "Core text.");
    assert.equal(t.refreshed.length, 0, "no refresh");
    assert.deepEqual(t.said, []);
    assert.equal(t.calls.length, 1);
    // A second press is a new, separate user act.
    await t.approvals.decide("call_1", "deny");
    assert.equal(t.calls.length, 2);
  }
  for (const odd of ["boom", null, undefined, 7]) {
    const t = make({ answer: async () => { throw odd; } });
    t.approvals.sync([card("call_1", HASH_A)]);
    await t.approvals.decide("call_1", "deny");
    assert.equal(t.approvals.cards[0].mode, "ask");
    assert.equal(typeof t.approvals.cards[0].fault.text, "string");
    assert.equal(t.calls.length, 1);
  }
  const plain = make({ answer: async () => { throw null; } });
  plain.approvals.sync([card("call_1", HASH_A)]);
  await plain.approvals.decide("call_1", "deny");
  assert.equal(plain.approvals.cards[0].fault.text, "The decision could not be sent.");
});

test("one decision per card at a time, records follow the call id and the hash, and late answers are dropped", async () => {
  // A second decision on a card whose first has not answered makes no call; the other card is untouched.
  const gate = { release: null };
  const slow = make({ answer: () => new Promise((go) => { gate.release = go; }) });
  slow.approvals.sync([card("call_1", HASH_A), card("call_2", HASH_B)]);
  const first = slow.approvals.decide("call_1", "deny");
  assert.equal(slow.approvals.cards[0].mode, "sending");
  await slow.approvals.decide("call_1", "deny");
  await slow.approvals.decide("call_1", "allow");
  assert.equal(slow.calls.length, 1);
  assert.equal(slow.approvals.cards[1].mode, "ask");
  assert.equal(slow.approvals.waiting(), 2, "a card that is travelling still waits");
  gate.release(null);
  await first;
  assert.equal(slow.approvals.cards[0].mode, "sent");
  assert.equal(slow.approvals.waiting(), 1);

  // A sync without the card drops its record; with the card again it is fresh.
  slow.approvals.sync([card("call_2", HASH_B)]);
  assert.deepEqual(slow.approvals.cards.map((c) => c.id), ["call_2"]);
  slow.approvals.sync([card("call_1", HASH_A), card("call_2", HASH_B)]);
  assert.equal(slow.approvals.cards[0].mode, "ask");

  // A controller that is disposed before the answer arrives changes nothing and says nothing.
  const late = make({ answer: () => new Promise((go) => { gate.release = go; }) });
  late.approvals.sync([card("call_1", HASH_A)]);
  const pending = late.approvals.decide("call_1", "deny");
  late.approvals.dispose();
  gate.release(null);
  await pending;
  assert.deepEqual(late.said, []);
  assert.equal(late.refreshed.length, 0);
  const lateFail = make({ answer: () => new Promise((_, no) => { gate.release = () => no({ code: "tool", reason: "unknown_call", text: "x" }); }) });
  lateFail.approvals.sync([card("call_1", HASH_A)]);
  const doomed = lateFail.approvals.decide("call_1", "deny");
  lateFail.approvals.dispose();
  gate.release();
  await doomed;
  assert.deepEqual(lateFail.said, []);
  assert.equal(lateFail.refreshed.length, 0);

  // At most 16 cards are drawn; the rest are counted.
  const many = make();
  many.approvals.sync(Array.from({ length: 18 }, (_, i) => card(`call_${i}`, HASH_A)));
  assert.equal(many.approvals.cards.length, 16);
  assert.equal(many.approvals.hidden, 2);
  assert.equal(many.approvals.waiting(), 18, "every call that waits is counted");

  // A request that cannot be shown has the mode `cannot`; its hash can be denied, and an allow does nothing.
  const cannot = make();
  const huge = card("call_1", HASH_A);
  huge.payload[4] = { name: "path", value: "a".repeat(70000), bytes: 70000 };
  cannot.approvals.sync([huge, card("call_2", "not a hash")]);
  assert.deepEqual(cannot.approvals.cards.map((c) => c.mode), ["cannot", "cannot"]);
  await cannot.approvals.decide("call_1", "allow");
  assert.equal(cannot.calls.length, 0);
  await cannot.approvals.decide("call_2", "deny");
  assert.equal(cannot.calls.length, 0, "with no hash there is nothing to repeat");
  assert.equal(cannot.approvals.waiting(), 1, "only the call that can be denied counts");
  await cannot.approvals.decide("call_1", "deny");
  assert.equal(cannot.calls.length, 1);
  assert.deepEqual(cannot.calls[0][1], { conversation: "chat-1", call_id: "call_1", decision: "deny", hash: HASH_A });
});

test("nothing is sent at construction, on sync or on a notification", async () => {
  const t = make();
  let notified = 0;
  let list = [card("call_1", HASH_A)];
  const stop = t.approvals.subscribe(() => {
    notified += 1;
    t.approvals.sync(list);
  });
  t.approvals.sync(list);
  assert.equal(notified, 1, "a listener that syncs again does not loop");
  t.approvals.sync(list);
  assert.equal(notified, 1, "the same list again is no change");
  list = [card("call_1", HASH_B)];
  t.approvals.sync(list);
  assert.equal(notified, 2, "a changed card is");
  list = [];
  t.approvals.sync(list);
  assert.equal(notified, 3);
  stop();
  t.approvals.sync([card("call_3", HASH_A)]);
  t.approvals.dispose();
  t.approvals.sync([card("call_4", HASH_A)]);
  assert.equal(t.approvals.cards.length, 0);
  assert.deepEqual(t.calls, []);
  assert.deepEqual(t.said, []);
  assert.equal(t.refreshed.length, 0);
});
