// The page mirror of a chat (chatstate.ts): the reducer is replayed against the stream the core recorded
// (tests/fixtures/ai_stream.json), so a rule that differs from the core's shows here as an unequal state.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { applyChatEvent, isChatId, messageText, newChatState } from "../../dist/chatstate.js";

const golden = JSON.parse(readFileSync(new URL("../fixtures/ai_stream.json", import.meta.url), "utf8"));

/** Folds `events` into a state; the answer of every event is collected next to the state. */
function fold(state, events) {
  const results = [];
  const states = [];
  for (const event of events) {
    const out = applyChatEvent(state, event);
    results.push(out.result);
    states.push(out.state);
    state = out.state;
  }
  return { state, results, states };
}

test("the recorded run folds to the recorded state after every event", () => {
  assert.equal(golden.events.length, 12);
  assert.equal(golden.states.length, golden.events.length);
  let state = newChatState(golden.conversation);
  assert.equal(state.conversation, "chat-golden");
  golden.events.forEach((event, i) => {
    const before = structuredClone(state);
    const eventBefore = structuredClone(event);
    const out = applyChatEvent(state, event);
    assert.equal(out.result, "applied", `event ${i} (${event.kind})`);
    assert.deepEqual(out.state, golden.states[i], `the state after event ${i} (${event.kind})`);
    assert.deepEqual(state, before, `event ${i}: the state that was passed in is not changed`);
    assert.deepEqual(event, eventBefore, `event ${i}: the event is not changed`);
    state = out.state;
  });
  assert.equal(state.running, false);
});

test("the transcript of the recorded run is what the scripts said", () => {
  // The oracle: every message the core announced, in the order it announced it. It does not use the reducer.
  const said = [];
  for (const event of golden.events) {
    if (event.kind === "turn_started" || event.kind === "message") said.push(event.message);
  }
  const { state, results } = fold(newChatState(golden.conversation), golden.events);
  assert.ok(results.every((r) => r === "applied"));
  assert.deepEqual(state.messages.map((m) => m.id), said.map((m) => m.id));
  assert.deepEqual(state.messages.map(messageText), said.map((m) => m.parts.map((p) => p.text).join("")));
  const users = state.messages.filter((m) => m.role === "user");
  assert.equal(users.length, 3);
  assert.deepEqual(users.map((m) => messageText(m)), ["What is a pty?", "And a tty?", "Explain a pipe."]);
  const first = state.messages.find((m) => m.role === "assistant");
  assert.equal(messageText(first), "Hello there.");
  assert.equal(first.status, "complete");
  const last = state.messages.at(-1);
  assert.equal(messageText(last), "partial");
  assert.equal(last.status, "cancelled");
  assert.deepEqual(state.partial, { text: "", reasoning: "" });
  assert.equal(state.ended.end, "cancelled");
  const ends = golden.events.filter((e) => e.kind === "turn_ended").map((e) => e.end);
  assert.deepEqual(ends, ["done", "failed", "cancelled"]);
});

test("every recorded snapshot equals the state after its event", () => {
  assert.equal(golden.snapshots.length, 4);
  for (const snap of golden.snapshots) {
    assert.deepEqual(snap.state, golden.states[snap.after], `the snapshot taken after event ${snap.after}`);
  }
  const middle = golden.snapshots.find((s) => s.state.running);
  assert.ok(middle, "one snapshot was taken while a turn streamed");
  assert.equal(middle.state.partial.text, "partial");
});

test("a chat id is 1 to 64 letters, digits, underscore or hyphen", () => {
  for (const ok of ["a", "chat-golden", "Chat_One-2", "a".repeat(64), "0123456789abcdef0123456789abcdef"]) {
    assert.equal(isChatId(ok), true, ok);
  }
  for (const bad of ["", "a".repeat(65), "../x", "a/b", "a.b", "a b", "é", "a\0b", "a\n", 7, null, undefined, {}]) {
    assert.equal(isChatId(bad), false, String(bad));
  }
});

test("messageText joins the text parts and skips the others", () => {
  const message = (parts) => ({ id: "m", role: "assistant", ts: 1, model: null, status: "complete", usage: null, parts });
  const mixed = message([
    { type: "text", text: "a" },
    { type: "tool_call", id: "c1", name: "run", summary: "ls" },
    { type: "text", text: "b" },
    { type: "tool_result", call_id: "c1", content: "x", is_error: false },
  ]);
  assert.equal(messageText(mixed), "ab");
  assert.equal(messageText(message([{ type: "tool_call", id: "c1", name: "run", summary: "ls" }])), "");
  assert.equal(messageText(message([])), "");
});
