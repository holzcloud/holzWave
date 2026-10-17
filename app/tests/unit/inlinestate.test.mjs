// The pure half of the palette actions and of the reply action (inlinestate.ts): the words, the choice of the source for an
// explanation, the one slot that keeps an intent until its chat is drawn. No DOM and no bridge.
import test from "node:test";
import assert from "node:assert/strict";
import {
  DESCRIBE_ACTION, DESCRIBE_TEXT, DESCRIBE_TITLE, EXPLAIN_ACTION, EXPLAIN_TEXT, EXPLAIN_TITLE, IntentSlot, MAX_BUTTONS, MAX_COMMAND, WORDS,
  explainPlan, hasSelection,
} from "../../dist/inlinestate.js";

test("an explain plan takes the selection when the pane shows one, else the last command, else the screen", () => {
  const rest = { kinds: ["last_command", "screen"], first: true };
  assert.deepEqual(explainPlan(undefined), rest);
  assert.deepEqual(explainPlan(null), rest);
  assert.deepEqual(explainPlan({ selection: [] }), rest);
  assert.deepEqual(explainPlan({ selection: [[3, 0, 9]] }), { kinds: ["selection"], first: false });

  // Only the presence of a span counts: the spans themselves are never read.
  assert.equal(hasSelection({ selection: [[0, 0, 1]] }), true);
  assert.equal(hasSelection({ selection: [{ nonsense: true }, null, "x"] }), true);
  assert.equal(hasSelection({ selection: [] }), false);
  assert.equal(hasSelection({}), false);
  assert.equal(hasSelection({ selection: "abc" }), false);
  assert.equal(hasSelection({ selection: { length: 3 } }), false);
  assert.equal(hasSelection(null), false);
  assert.equal(hasSelection(undefined), false);

  const mirror = Object.freeze({ selection: Object.freeze([[1, 2, 3]]) });
  const before = JSON.stringify(mirror);
  const a = explainPlan(mirror);
  const b = explainPlan(mirror);
  assert.equal(JSON.stringify(mirror), before, "the input is not changed");
  assert.notEqual(a.kinds, b.kinds, "the arrays are fresh on every call");
  a.kinds.push("file");
  assert.deepEqual(explainPlan(mirror).kinds, ["selection"]);
  assert.notEqual(explainPlan(null).kinds, explainPlan(null).kinds);
});

test("an intent waits in one slot until its chat mounts", () => {
  const i1 = { text: "one", kinds: [], first: false, ready: "r1" };
  const i2 = { text: "two", kinds: ["screen"], first: true, ready: "r2" };
  const slot = new IntentSlot();
  assert.equal(slot.take("chat-a"), null, "an empty slot answers null");

  slot.park("chat-a", i1);
  assert.equal(slot.take("chat-b"), null, "another chat takes nothing");
  assert.equal(slot.take("chat-a"), i1, "and leaves the slot as it was");
  assert.equal(slot.take("chat-a"), null, "an intent is taken once");

  slot.park("chat-a", i1);
  slot.park("chat-b", i2);
  assert.equal(slot.take("chat-a"), null, "the later park replaced the earlier one");
  assert.equal(slot.take("chat-b"), i2);

  slot.park("chat-a", i1);
  slot.drop("chat-b");
  assert.equal(slot.take("chat-a"), i1, "dropping another chat does nothing");
  slot.park("chat-a", i1);
  slot.drop("chat-a");
  assert.equal(slot.take("chat-a"), null, "dropping the chat of the slot empties it");

  slot.park("chat-a", i1);
  slot.clear();
  assert.equal(slot.take("chat-a"), null);
});

test("the words of the actions are fixed sentences", () => {
  assert.equal(EXPLAIN_ACTION, "ai.explain_error");
  assert.equal(DESCRIBE_ACTION, "ai.command_from_description");
  assert.equal(EXPLAIN_TITLE, "Explain error");
  assert.equal(DESCRIBE_TITLE, "Command from description");
  assert.equal(
    EXPLAIN_TEXT,
    "Explain the error in the attached terminal output: what went wrong, the most likely cause, and the command or change that fixes it.",
  );
  assert.equal(
    DESCRIBE_TEXT,
    "Write one shell command for this task. Reply with the command alone in one code block and, after it, one sentence saying what it does. Task: ",
  );
  assert.ok(DESCRIBE_TEXT.endsWith(": "));
  assert.equal(MAX_COMMAND, 1024);
  assert.equal(MAX_BUTTONS, 16);
  assert.equal(WORDS.explainReady, "Nothing is sent until you press Send.");
  assert.equal(WORDS.describeReady, 'Describe the task after "Task:", then press Send.');
  assert.equal(WORDS.notTerminal, "Focus a terminal pane to explain its error.");
  const words = Object.values(WORDS);
  assert.ok(words.length >= 8);
  for (const word of [...words, EXPLAIN_TEXT, DESCRIBE_TEXT]) {
    assert.equal(typeof word, "string");
    assert.ok(word.length > 0);
    assert.ok(!/[\r\n]/.test(word), `a single line: ${word}`);
    assert.ok(!word.includes("://"), word);
    assert.ok(!/[<>]/.test(word), word);
    assert.ok(!/[\u0000-\u001f\u007f-\u009f]/u.test(word), `no control character: ${word}`);
  }
  for (const text of [EXPLAIN_TEXT, DESCRIBE_TEXT]) assert.ok(new TextEncoder().encode(text).length < 131072);
});
