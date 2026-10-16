// The page mirror of a chat (chatstate.ts): the reducer is replayed against the stream the core recorded
// (tests/fixtures/ai_stream.json), so a rule that differs from the core's shows here as an unequal state.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  ChatFeed, MAX_HELD, MAX_ITEMS, MAX_MESSAGES, MAX_PARTS, MAX_PENDING, applyChatEvent, isChatId, messageText,
  newChatState, readChatEvent, readChatMessage, readChatState,
} from "../../dist/chatstate.js";

// A text that must never reach a page state: it is a member name the core never sends, so it can only get in by a copy.
const CANARY = "zq-chat-native-7c2e91";

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

// ---- small factories for what the recorded run does not hold (tool events, caps, hostile values) -----------------------

const CHAT = "chat-t";
const route = { profile: "p", model: "m", host: null, locality: "cloud" };
const msg = (over = {}) => ({
  id: "m1", role: "user", ts: 1, model: null, status: "complete", usage: null, parts: [{ type: "text", text: "hi" }], ...over,
});
const pendingCall = (over = {}) => ({
  conversation: CHAT, call_id: "c1", tool: "run_command", effect: "write", pane: 1, target: "This computer", remote: false,
  payload: [{ label: "command", value: "ls" }], hash: "ab".repeat(32), tainted: false, taint: [], ...over,
});
const ev = (turn, seq, kind, members = {}, conversation = CHAT) => ({ conversation, turn, seq, kind, ...members });
const ended = { end: "done", error: null, rounds: 1, usage: { input_tokens: 2, output_tokens: 3 }, saved: true };
const nine = () => [
  ev(5, 1, "turn_started", { message: msg(), route }),
  ev(5, 2, "delta", { text: "a" }),
  ev(5, 3, "reasoning", { text: "r" }),
  ev(5, 4, "usage", { input_tokens: 1, output_tokens: 2 }),
  ev(5, 5, "trimmed", { results: 1, exchanges: 2 }),
  ev(5, 6, "message", { message: msg({ id: "m2", role: "assistant", model: "m", usage: { input_tokens: 1, output_tokens: 2 } }) }),
  ev(5, 7, "tool_proposed", { pending: pendingCall() }),
  ev(5, 8, "tool_result", { call_id: "c1", tool: "run_command", content: "ok", is_error: false, redactions: 0 }),
  ev(5, 9, "turn_ended", ended),
];

/** Applies one event and checks that neither argument was changed. */
function step(state, event) {
  const before = structuredClone(state);
  const eventBefore = structuredClone(event);
  const out = applyChatEvent(state, event);
  assert.deepEqual(state, before, `${event.kind}: the state that was passed in is not changed`);
  assert.deepEqual(event, eventBefore, `${event.kind}: the event is not changed`);
  return out;
}

test("the readers accept every recorded event, state and snapshot unchanged", () => {
  golden.events.forEach((event, i) => {
    const read = readChatEvent(event);
    assert.deepEqual(read, event, `event ${i}`);
    assert.notEqual(read, event, `event ${i}: the result is a new object`);
  });
  const states = [...golden.states, ...golden.snapshots.map((s) => s.state)];
  for (const [i, state] of states.entries()) {
    const copy = structuredClone(state);
    const read = readChatState(state);
    assert.deepEqual(read, copy, `state ${i}`);
    for (const message of state.messages) assert.deepEqual(readChatMessage(message), message);
    if (read.messages.length > 0) {
      read.messages[0].parts[0].text = "changed";
      read.partial.text = "changed";
    }
    assert.deepEqual(state, copy, `state ${i}: a change to the result leaves the recorded object as it was`);
  }
});

test("the readers refuse what is not a chat event or state and keep only the members the page uses", () => {
  const delta = ev(5, 2, "delta", { text: "x" });
  const refused = [
    null, "text", [], 7, undefined,
    { ...delta, conversation: "../x" }, { ...delta, turn: -1 }, { ...delta, turn: 1.5 }, { ...delta, turn: "1" },
    { ...delta, seq: 0 }, { ...delta, seq: Number.MAX_SAFE_INTEGER + 1 }, { ...delta, kind: "tool_state" },
    { conversation: CHAT, turn: 5, seq: 2, text: "x" },
    ev(5, 2, "delta"), ev(5, 2, "delta", { text: 7 }),
    ev(5, 1, "turn_started", { message: msg() }), ev(5, 1, "turn_started", { route }),
    ev(5, 1, "turn_started", { message: msg(), route: { ...route, locality: "moon" } }),
    ev(5, 2, "message", { message: msg({ role: "system" }) }),
    ev(5, 2, "message", { message: msg({ status: "done" }) }),
    ev(5, 2, "message", { message: msg({ parts: [{ type: "image", text: "x" }] }) }),
    ev(5, 2, "usage", { input_tokens: -1, output_tokens: 0 }),
    ev(5, 2, "tool_proposed", { pending: pendingCall({ conversation: "other-chat" }) }),
    ev(5, 2, "tool_proposed", { pending: pendingCall({ call_id: 3 }) }),
    ev(5, 9, "turn_ended", { ...ended, end: 3 }),
  ];
  for (const [i, raw] of refused.entries()) assert.equal(readChatEvent(raw), null, `refused value ${i}: ${JSON.stringify(raw)}`);
  for (const event of nine()) assert.deepEqual(readChatEvent(event), event, event.kind);

  // A member the core never sends is not copied, wherever it sits.
  const extras = { native: CANARY, stored: CANARY, api_key: CANARY };
  const hostile = ev(5, 2, "message", {
    ...extras,
    message: msg({ ...extras, parts: [{ type: "text", text: "hi", ...extras }] }),
  });
  const read = readChatEvent(hostile);
  assert.deepEqual(read, ev(5, 2, "message", { message: msg() }));
  assert.ok(!JSON.stringify(read).includes(CANARY), "the canary is nowhere in the result");
  assert.equal(readChatMessage({ ...msg(), ...extras }).native, undefined);
  const state = { ...golden.states[0], ...extras };
  assert.ok(!JSON.stringify(readChatState(state)).includes(CANARY));

  // An own __proto__ member (what JSON.parse makes of the text) never reaches a result.
  const parsed = JSON.parse(`{"conversation":"${CHAT}","turn":5,"seq":2,"kind":"delta","text":"x","__proto__":{"polluted":true}}`);
  assert.ok(Object.hasOwn(parsed, "__proto__"));
  const clean = readChatEvent(parsed);
  assert.equal(Object.getPrototypeOf(clean), Object.prototype);
  assert.ok(!Object.hasOwn(clean, "__proto__"));
  assert.equal(clean.polluted, undefined);
  const withProto = JSON.parse(
    `{"conversation":"${CHAT}","turn":5,"seq":7,"kind":"tool_proposed","pending":{"conversation":"${CHAT}","call_id":"c1","tool":"t",` +
      `"payload":[{"label":"a","__proto__":{"x":1}}],"hash":"h","__proto__":{"polluted":true}}}`,
  );
  const proposed = readChatEvent(withProto).pending;
  assert.deepEqual(proposed.payload, [{ label: "a" }]);
  assert.equal(Object.getPrototypeOf(proposed.payload[0]), Object.prototype, "a nested __proto__ member is not applied either");
  assert.equal(proposed.payload[0].x, undefined);
  assert.equal(proposed.hash, "h");
  assert.ok(!Object.hasOwn(proposed, "__proto__"));
  assert.equal(Object.getPrototypeOf(proposed), Object.prototype);
  assert.equal(readChatEvent(nine()[6]).pending.hash, "ab".repeat(32), "the other members of a pending entry pass through");

  assert.deepEqual([MAX_MESSAGES, MAX_PARTS, MAX_PENDING, MAX_ITEMS, MAX_HELD], [4096, 1024, 64, 64, 1024]);

  // A state with a missing or wrong member is refused whole; a list over its cap too; unknown members are dropped.
  const base = structuredClone(golden.states[5]);
  for (const key of Object.keys(base)) {
    const without = { ...base };
    delete without[key];
    assert.equal(readChatState(without), null, `without ${key}`);
  }
  assert.equal(readChatState({ ...base, running: "yes" }), null);
  assert.equal(readChatState({ ...base, conversation: "../x" }), null);
  assert.equal(readChatState({ ...base, ended: { end: "done" } }), null);
  assert.equal(readChatState({ ...base, extra: 1 }).extra, undefined);
  const many = (n, make) => Array.from({ length: n }, make);
  const withMessages = (n) => ({ ...base, messages: many(n, (_, i) => msg({ id: `m${i}` })) });
  assert.ok(readChatState(withMessages(MAX_MESSAGES)));
  assert.equal(readChatState(withMessages(MAX_MESSAGES + 1)), null);
  const withParts = (n) => ({ ...base, messages: [msg({ parts: many(n, () => ({ type: "text", text: "p" })) })] });
  assert.ok(readChatState(withParts(MAX_PARTS)));
  assert.equal(readChatState(withParts(MAX_PARTS + 1)), null);
  const withPending = (n) => ({ ...base, conversation: CHAT, pending: many(n, (_, i) => pendingCall({ call_id: `c${i}` })) });
  assert.ok(readChatState(withPending(MAX_PENDING)));
  assert.equal(readChatState(withPending(MAX_PENDING + 1)), null);
  const context = (n) => ({
    type: "context", hash: "h", provider: "p", locality: "lan", items: many(n, () => ({ kind: "file", label: "a.txt", bytes: 3 })),
  });
  const withItems = (n) => ({ ...base, messages: [msg({ parts: [context(n)] })] });
  assert.ok(readChatState(withItems(MAX_ITEMS)));
  assert.equal(readChatState(withItems(MAX_ITEMS + 1)), null);
});

test("the rules of apply: applied, stale and gap, and what each kind changes", () => {
  const start = ev(5, 1, "turn_started", { message: msg({ id: "m5-user", parts: [{ type: "text", text: "hello" }] }), route });
  let out = step(newChatState(CHAT), start);
  assert.equal(out.result, "applied");
  let state = out.state;
  assert.deepEqual([state.running, state.profile, state.model, state.turn, state.seq], [true, "p", "m", 5, 1]);
  assert.deepEqual(state.messages.map((m) => m.id), ["m5-user"]);
  assert.equal(state.title, "hello");

  out = step(state, start);
  assert.equal(out.result, "stale");
  assert.equal(out.state, state, "a stale answer holds the same state object");
  out = step(state, ev(5, 3, "delta", { text: "x" }));
  assert.equal(out.result, "gap");
  assert.equal(out.state, state, "a gap answer holds the same state object");
  assert.equal(step(state, ev(5, 2, "delta", { text: "x" }, "other-chat")).result, "stale");

  const apply = (event) => {
    const r = step(state, event);
    assert.equal(r.result, "applied", `${event.kind} at ${event.seq}`);
    state = r.state;
  };
  apply(ev(5, 2, "delta", { text: "a" }));
  apply(ev(5, 3, "delta", { text: "b" }));
  apply(ev(5, 4, "reasoning", { text: "r1" }));
  apply(ev(5, 5, "reasoning", { text: "r2" }));
  assert.deepEqual(state.partial, { text: "ab", reasoning: "r1r2" });
  apply(ev(5, 6, "usage", { input_tokens: 3, output_tokens: 4 }));
  apply(ev(5, 7, "trimmed", { results: 1, exchanges: 2 }));
  assert.deepEqual([state.usage, state.trimmed], [{ input_tokens: 3, output_tokens: 4 }, { results: 1, exchanges: 2 }]);
  apply(ev(5, 8, "tool_proposed", { pending: pendingCall({ call_id: "c1" }) }));
  apply(ev(5, 9, "tool_proposed", { pending: pendingCall({ call_id: "c2" }) }));
  assert.deepEqual(state.pending.map((p) => p.call_id), ["c1", "c2"]);
  const midTurn = state;
  apply(ev(5, 10, "tool_result", { call_id: "c1", tool: "t", content: "x", is_error: false, redactions: 0 }));
  assert.deepEqual(state.pending.map((p) => p.call_id), ["c2"], "only the entry with that call id goes");
  apply(ev(5, 11, "message", { message: msg({ id: "m5-1", role: "assistant" }) }));
  assert.deepEqual(state.partial, { text: "", reasoning: "" });
  assert.equal(state.pending.length, 1, "a message keeps the pending calls");
  assert.deepEqual(state.messages.map((m) => m.id), ["m5-user", "m5-1"]);
  const finish = ev(5, 12, "turn_ended", { ...ended, end: "failed", error: { class: "overload", text: "busy" }, rounds: 3 });
  apply(finish);
  assert.deepEqual([state.running, state.pending, state.partial], [false, [], { text: "", reasoning: "" }]);
  assert.deepEqual(state.ended, {
    end: "failed", error: { class: "overload", text: "busy" }, rounds: 3, usage: ended.usage, saved: true,
  });

  // Another turn applies only at seq 1, resets the members of a turn and keeps the title.
  out = step(state, ev(6, 4, "delta", { text: "z" }));
  assert.equal(out.result, "gap");
  assert.equal(out.state, state);
  apply(ev(6, 1, "turn_started", { message: msg({ id: "m6-user", parts: [{ type: "text", text: "second" }] }), route: { ...route, model: "m2" } }));
  assert.deepEqual([state.turn, state.seq, state.running, state.model, state.title], [6, 1, true, "m2", "hello"]);
  assert.deepEqual([state.pending, state.partial, state.usage, state.trimmed, state.ended],
    [[], { text: "", reasoning: "" }, { input_tokens: 0, output_tokens: 0 }, { results: 0, exchanges: 0 }, null]);
  assert.deepEqual(state.messages.map((m) => m.id), ["m5-user", "m5-1", "m6-user"]);

  // The members of a turn are reset by the first event of another turn, whatever its kind (the end of the old turn was lost).
  const reset = step(midTurn, ev(6, 1, "delta", { text: "z" })).state;
  assert.deepEqual(
    [reset.turn, reset.pending, reset.partial, reset.usage, reset.trimmed, reset.ended, reset.running],
    [6, [], { text: "z", reasoning: "" }, { input_tokens: 0, output_tokens: 0 }, { results: 0, exchanges: 0 }, null, false],
  );

  // A kind this module does not know is a gap and changes nothing.
  const unknown = step(state, ev(6, 2, "tool_state", { text: "?" }));
  assert.equal(unknown.result, "gap");
  assert.equal(unknown.state, state);

  // The title is the first line with text, made safe and cut at 60 characters; an existing title is kept.
  const titled = (text) => step(newChatState(CHAT), ev(1, 1, "turn_started", { message: msg({ parts: [{ type: "text", text }] }), route })).state.title;
  assert.equal(titled("x".repeat(70)), `${"x".repeat(60)}\u2026`);
  assert.equal(titled("\n\n  a\u0007b \u202e c \n second line"), "a b c");
  assert.equal(titled("   \n "), "");
});

test("a gap asks for one snapshot, holds the events that follow and continues from the snapshot", () => {
  const events = golden.events;
  const states = golden.states;
  const empty = () => newChatState(golden.conversation);
  const fresh = new ChatFeed(golden.conversation);
  assert.equal(fresh.waiting, true);
  assert.equal(fresh.conversation, golden.conversation);
  assert.equal(fresh.receive(events[0]), "held");
  assert.equal(fresh.install(empty()), "installed", "the held event is replayed after the snapshot");
  assert.equal(fresh.waiting, false);
  assert.deepEqual(fresh.state, states[0]);

  const feed = new ChatFeed(golden.conversation);
  assert.equal(feed.install(empty()), "installed");
  assert.equal(feed.receive(events[0]), "applied");
  assert.equal(feed.receive(events[1]), "applied");
  assert.equal(feed.receive(events[3]), "snapshot", "event 2 was lost");
  assert.equal(feed.waiting, true);
  assert.equal(feed.receive(events[4]), "held");
  assert.equal(feed.receive(events[5]), "held");
  assert.equal(feed.receive(events[3]), "held", "a second event never asks again");
  assert.equal(feed.install(structuredClone(golden.snapshots[0].state)), "installed", "the snapshot holds events 3 to 5");
  assert.equal(feed.waiting, false);
  assert.deepEqual(feed.state, states[5]);
  for (let i = 6; i < events.length; i++) assert.equal(feed.receive(events[i]), "applied", `event ${i}`);
  assert.deepEqual(feed.state, states.at(-1));

  // A snapshot older than the held events: the ones it does not hold are replayed.
  const part = new ChatFeed(golden.conversation);
  part.install(empty());
  part.receive(events[0]);
  part.receive(events[1]);
  assert.equal(part.receive(events[3]), "snapshot");
  part.receive(events[4]);
  part.receive(events[5]);
  assert.equal(part.install(structuredClone(states[3])), "installed");
  assert.deepEqual(part.state, states[5]);
});

test("no single lost event leaves a wrong state, whatever the age of the snapshot", () => {
  const events = golden.events;
  const states = golden.states;
  let cases = 0;
  for (let k = 0; k <= events.length - 2; k++) {
    const lastOfTurn = events[k + 1].turn !== events[k].turn;
    const plans = lastOfTurn ? [{ m: 0, model: "none" }] : [0, 1, 2, 3].flatMap((m) => ["request", "answer"].map((model) => ({ m, model })));
    for (const { m, model } of plans) {
      const where = `lost event ${k}, ${m} event(s) before the answer, snapshot taken at the ${model}`;
      const feed = new ChatFeed(golden.conversation);
      assert.equal(feed.install(newChatState(golden.conversation)), "installed", where);
      let requests = 0;
      const deliver = (i) => {
        const answer = feed.receive(events[i]);
        if (answer === "snapshot") requests += 1;
        return answer;
      };
      for (let i = 0; i < k; i++) assert.equal(deliver(i), "applied", `${where}: event ${i}`);
      const first = k + 1;
      let last = first;
      if (lastOfTurn) {
        assert.equal(deliver(first), "applied", `${where}: the next turn needs nothing`);
      } else {
        assert.equal(deliver(first), "snapshot", where);
        for (let j = 1; j <= m && first + j < events.length; j++) {
          assert.equal(deliver(first + j), "held", `${where}: event ${first + j}`);
          last = first + j;
        }
        const at = model === "request" ? first : last;
        assert.equal(feed.install(structuredClone(states[at])), "installed", where);
      }
      for (let i = last + 1; i < events.length; i++) {
        const answer = deliver(i);
        assert.ok(answer === "applied" || answer === "stale", `${where}: event ${i} answered ${answer}`);
      }
      assert.deepEqual(feed.state, states.at(-1), where);
      assert.equal(requests, lastOfTurn ? 0 : 1, `${where}: requests`);
      cases += 1;
    }
  }
  assert.ok(cases > 40, `only ${cases} cases ran`);
});

test("a snapshot that is older than the hole asks again and a failed request lets the next gap ask again", () => {
  const events = golden.events;
  const states = golden.states;
  const ready = (upTo) => {
    const feed = new ChatFeed(golden.conversation);
    feed.install(newChatState(golden.conversation));
    for (let i = 0; i < upTo; i++) feed.receive(events[i]);
    return feed;
  };
  const feed = ready(2);
  assert.equal(feed.receive(events[3]), "snapshot", "event 2 was lost");
  assert.equal(feed.install(structuredClone(states[1])), "again", "the snapshot is older than the hole");
  assert.equal(feed.waiting, true);
  assert.equal(feed.install(structuredClone(states[3])), "installed", "the held event is gone, the snapshot stands");
  assert.deepEqual(feed.state, states[3]);
  assert.equal(feed.waiting, false);

  const rejected = ready(2);
  assert.equal(rejected.receive(events[3]), "snapshot");
  const before = structuredClone(rejected.state);
  rejected.failed();
  assert.equal(rejected.waiting, false);
  assert.deepEqual(rejected.state, before);
  assert.equal(rejected.receive(events[4]), "snapshot", "the next event that does not fit asks again");
  assert.equal(rejected.install(structuredClone(states[4])), "installed");
  assert.deepEqual(rejected.state, states[4], "the event that started the second wait is covered by the snapshot");

  // The list of held events is bounded; an overrun drops it and asks again once.
  const waiting = new ChatFeed(golden.conversation);
  for (let i = 0; i < MAX_HELD; i++) assert.equal(waiting.receive(events[0]), "held", `held ${i}`);
  assert.equal(waiting.receive(events[0]), "snapshot");
  assert.equal(waiting.waiting, true);
  assert.equal(waiting.install(newChatState(golden.conversation)), "installed");
  assert.deepEqual(waiting.state, newChatState(golden.conversation), "the dropped events are not replayed");
});

test("a feed refuses a snapshot it did not ask for and ignores the events of another chat", () => {
  const feed = new ChatFeed(golden.conversation);
  assert.equal(feed.install(newChatState(golden.conversation)), "installed");
  const state = feed.state;
  assert.equal(feed.install(structuredClone(golden.states[5])), "refused", "it did not ask");
  assert.equal(feed.state, state);

  const waiting = new ChatFeed(golden.conversation);
  assert.equal(waiting.install(newChatState("other-chat")), "refused", "another chat");
  assert.equal(waiting.waiting, true);
  const foreign = { ...golden.events[0], conversation: "other-chat" };
  assert.equal(waiting.receive(foreign), "stale", "an event of another chat is neither held nor applied");
  assert.equal(waiting.waiting, true);
  assert.equal(waiting.install(newChatState(golden.conversation)), "installed");
  assert.deepEqual(waiting.state, newChatState(golden.conversation));

  const idle = new ChatFeed(golden.conversation);
  idle.install(newChatState(golden.conversation));
  assert.equal(idle.receive(foreign), "stale");
  assert.equal(idle.waiting, false);
});

test("a lost last event is repaired by an explicit snapshot", () => {
  const events = golden.events;
  const feed = new ChatFeed(golden.conversation);
  feed.install(newChatState(golden.conversation));
  for (let i = 0; i < events.length - 1; i++) assert.equal(feed.receive(events[i]), "applied");
  assert.equal(feed.state.running, true, "no event can show that the last one is missing");
  assert.equal(feed.waiting, false);
  feed.awaitSnapshot();
  assert.equal(feed.waiting, true);
  assert.equal(feed.receive(events[events.length - 2]), "held");
  assert.equal(feed.install(structuredClone(golden.states.at(-1))), "installed");
  assert.deepEqual(feed.state, golden.states.at(-1));
  assert.equal(feed.state.running, false);
});

test("the module touches no DOM, no network and no command", () => {
  const text = readFileSync(new URL("../../src/chatstate.ts", import.meta.url), "utf8");
  assert.ok(!/^import\b/m.test(text), "the module imports nothing");
  const banned = new RegExp(
    "document\\.[a-zA-Z]|window\\.[a-zA-Z]|innerHTML|insertAdjacentHTML|Object\\.assign|eval\\(|new Function|fetch\\(|" +
      "\\bcall\\(|invoke\\(|localStorage|XMLHttp|setTimeout|setInterval|Promise|async ",
  );
  const hit = text.split("\n").findIndex((line) => banned.test(line));
  assert.equal(hit, -1, `line ${hit + 1} uses something the module must not use`);
});
