// The controller of the cards of `wsh ai` requests (ask.ts) against a fake `invoke` that records every call: nothing is asked
// at construction or on an event, the answer sends exactly `ask`, `verdict` and `seal`, a press that does not count makes no call,
// and the refusals of the core are told in its words and act on two names only.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { AskQueue } from "../../dist/ask.js";

const GOLDEN = JSON.parse(readFileSync(new URL("../fixtures/ai_ask.json", import.meta.url), "utf8"));
const waiting = (changes = {}) => ({ kind: "waiting", view: { ...structuredClone(GOLDEN.view), ...changes } });
const closed = (ask, why) => ({ kind: "closed", ask, why });
const ID = GOLDEN.view.ask;
const OTHER = "f".repeat(32);

/** A controller over a fake `invoke`: the calls it got, the answers to give and a clock the test moves. */
function rig({ pending = [] } = {}) {
  const log = { calls: [], said: [], answers: [], now: 10_000 };
  const queue = new AskQueue({
    invoke: async (cmd, args) => {
      log.calls.push([cmd, args]);
      if (cmd === "ai_ask_pending") return structuredClone(pending);
      const next = log.answers.shift();
      if (next === undefined) return null;
      if (next.reject !== undefined) throw next.reject;
      return null;
    },
    now: () => log.now,
    say: (text) => log.said.push(text),
  });
  return { queue, log };
}

const answerCalls = (log) => log.calls.filter((c) => c[0] === "ai_ask_answer");

test("building the queue and receiving an event make no call", () => {
  const { queue, log } = rig();
  assert.deepEqual(log.calls, [], "nothing at construction");
  queue.receive(waiting());
  queue.receive(closed(ID, "gone"));
  queue.receive({ nonsense: true });
  queue.receive(null);
  assert.deepEqual(log.calls, [], "nothing on an event");
  assert.equal(queue.items.length, 0);
  queue.receive(waiting());
  assert.equal(queue.items.length, 1);
  assert.equal(queue.items[0].view.ask, ID);
  assert.deepEqual(log.calls, []);
  queue.dispose();
});

test("loading asks the core once and merges what it answers", async () => {
  const fresh = { ...GOLDEN.view, seal: "9".repeat(64) };
  const { queue, log } = rig({ pending: [fresh, { ...GOLDEN.view, ask: OTHER }] });
  queue.receive(waiting());
  const first = queue.items[0].since;
  log.now += 3000;
  await queue.load();
  assert.deepEqual(log.calls, [["ai_ask_pending", undefined]], "one call, no argument");
  assert.equal(queue.items.length, 2, "what the core lists is merged in");
  assert.equal(queue.items[0].view.ask, ID);
  assert.equal(queue.items[0].view.seal, "9".repeat(64), "a request that is held takes the new seal");
  assert.equal(queue.items[0].since, first, "and keeps the time it was first shown");
  // A listing that holds less removes nothing, and a refused one changes nothing.
  const empty = rig({ pending: [] });
  empty.queue.receive(waiting());
  await empty.queue.load();
  assert.equal(empty.queue.items.length, 1, "load never removes");
  const failing = new AskQueue({ invoke: async () => Promise.reject(new Error("no")), now: () => 0, say: () => {} });
  await failing.load();
  assert.equal(failing.items.length, 0);
});

test("an answer sends the three values once and nothing else", async () => {
  const { queue, log } = rig();
  queue.receive(waiting());
  log.now += 600;
  await queue.answer(ID, "send", true);
  const made = answerCalls(log);
  assert.equal(made.length, 1);
  assert.equal(made[0][0], "ai_ask_answer");
  assert.deepEqual(Object.keys(made[0][1]), ["ask", "verdict", "seal"]);
  assert.deepEqual(made[0][1], { ask: ID, verdict: "send", seal: GOLDEN.view.seal });
  assert.equal(queue.items.length, 0, "the core took the answer: the request is gone");
  assert.deepEqual(log.said, [], "an answered request leaves nothing");

  // A refusal is the same call with the other word, at once.
  queue.receive(waiting());
  await queue.answer(ID, "refuse", true);
  assert.deepEqual(answerCalls(log)[1][1], { ask: ID, verdict: "refuse", seal: GOLDEN.view.seal });
  // The answer to a request that is not held is no call.
  await queue.answer(OTHER, "refuse", true);
  assert.equal(answerCalls(log).length, 2);
  // A verdict that is neither word is no call.
  queue.receive(waiting());
  await queue.answer(ID, "allow", true);
  assert.equal(answerCalls(log).length, 2);
});

test("an untrusted, early or repeated press makes no call", async () => {
  const { queue, log } = rig();
  queue.receive(waiting());
  await queue.answer(ID, "send", true);
  assert.equal(answerCalls(log).length, 0, "Send once within the window");
  log.now += 499;
  await queue.answer(ID, "send", true);
  assert.equal(answerCalls(log).length, 0, "Send once at 499 ms");
  log.now += 10;
  await queue.answer(ID, "send", false);
  await queue.answer(ID, "refuse", false);
  assert.equal(answerCalls(log).length, 0, "a press that a script made");

  // While a press is on its way a second one makes no call: the first travels, held by a promise.
  let release;
  const held = new AskQueue({
    invoke: (cmd) => (cmd === "ai_ask_answer" ? new Promise((go) => (release = go)) : Promise.resolve(null)),
    now: () => log.now,
    say: () => {},
  });
  held.receive(waiting());
  log.now += 600;
  const first = held.answer(ID, "send", true);
  assert.equal(held.items[0].sending, true);
  await held.answer(ID, "send", true);
  await held.answer(ID, "refuse", true);
  release(null);
  await first;
  assert.equal(held.items.length, 0);

  // A request that comes to the front has its own window: the second card of two is held back for half a second too.
  const two = rig();
  two.queue.receive(waiting());
  two.queue.receive(waiting({ ask: OTHER }));
  two.log.now += 2000;
  await two.queue.answer(ID, "refuse", true);
  assert.equal(two.queue.items.length, 1);
  await two.queue.answer(OTHER, "send", true);
  assert.equal(answerCalls(two.log).length, 1, "only the first answer was made: the second card was shown just now");
  two.log.now += 500;
  await two.queue.answer(OTHER, "send", true);
  assert.equal(answerCalls(two.log).length, 2);
});

test("the refusals of the core are told as the core's words and act on two names only", async () => {
  // unknown_ask: the card is removed and the status line says the fixed sentence.
  const unknown = rig();
  unknown.queue.receive(waiting());
  unknown.log.now += 600;
  unknown.log.answers.push({ reject: { code: "ask", reason: "unknown_ask", text: GOLDEN.faults.unknown_ask } });
  await unknown.queue.answer(ID, "send", true);
  assert.equal(unknown.queue.items.length, 0);
  assert.deepEqual(unknown.log.said, ["That request is not waiting any more."]);

  // seal_mismatch: the card stays with the core's text, and the core is asked once more for the cards.
  const changed = rig({ pending: [{ ...GOLDEN.view, seal: "7".repeat(64) }] });
  changed.queue.receive(waiting());
  changed.log.now += 600;
  changed.log.answers.push({ reject: { code: "ask", reason: "seal_mismatch", text: GOLDEN.faults.seal_mismatch } });
  await changed.queue.answer(ID, "send", true);
  await new Promise((go) => setTimeout(go, 0));
  assert.equal(changed.queue.items.length, 1);
  assert.equal(changed.queue.items[0].fault.text, GOLDEN.faults.seal_mismatch);
  assert.equal(changed.queue.items[0].sending, false, "the buttons are free again");
  assert.equal(changed.log.calls.filter((c) => c[0] === "ai_ask_pending").length, 1, "read again once");
  assert.equal(changed.queue.items[0].view.seal, "7".repeat(64), "the card has the new seal");
  changed.log.answers.push({});
  await changed.queue.answer(ID, "send", true);
  assert.deepEqual(answerCalls(changed.log)[1][1].seal, "7".repeat(64), "the next answer carries the new seal");

  // Any other object keeps the card with its text cut at 300 characters and without control characters.
  const other = rig();
  other.queue.receive(waiting());
  other.log.now += 600;
  other.log.answers.push({ reject: { code: "ask", reason: "bad_verdict", text: `bad\u0007 ${"y".repeat(400)}` } });
  await other.queue.answer(ID, "send", true);
  assert.equal(other.queue.items.length, 1);
  const text = other.queue.items[0].fault.text;
  assert.ok(Array.from(text).length <= 300 && !/[\u0000-\u001f]/.test(text), text);
  assert.deepEqual(other.log.said, []);

  // A rejection that is not an object says the fixed sentence.
  const bare = rig();
  bare.queue.receive(waiting());
  bare.log.now += 600;
  bare.log.answers.push({ reject: "the shell failed" });
  await bare.queue.answer(ID, "send", true);
  assert.equal(bare.queue.items[0].fault.text, "The answer could not be sent.");
  assert.equal(bare.queue.items[0].fault.reason, null);

  // The closed events: an answered request leaves nothing, the others one sentence each, an unknown ask nothing.
  const ends = rig();
  ends.queue.receive(waiting());
  ends.queue.receive(closed(ID, "timed_out"));
  assert.equal(ends.queue.items.length, 0);
  assert.deepEqual(ends.log.said, ["The wsh ai request from pane 3 timed out; nothing was sent."]);
  ends.queue.receive(waiting());
  ends.queue.receive(closed(OTHER, "gone"));
  assert.equal(ends.queue.items.length, 1, "a close of another request changes nothing");
  ends.queue.receive(closed(ID, "sent"));
  assert.equal(ends.log.said.length, 1, "a sent request says nothing");
});
