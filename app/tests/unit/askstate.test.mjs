// The pure half of the card of a `wsh ai` request (askstate.ts): how the core's card and events are read, the words, the presses
// that count and the queue. The readers are held to the golden `ai_ask.json` that the core's tests write from its real types.
// No DOM and no bridge.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  CLOSED, IGNORE_MS, MAX_ASKS, MAX_PROMPT, REASONS, VERDICTS, WORDS, addAsk, answerFault, canRefuse, canSend, closedNotice, findingLines,
  inputWords, placeWords, promptWords, readAskEvent, readAskView, readPending, removeAsk, routeWords,
} from "../../dist/askstate.js";

const GOLDEN = JSON.parse(readFileSync(new URL("../fixtures/ai_ask.json", import.meta.url), "utf8"));
const VIEW_KEYS = ["ask", "seal", "pane", "prompt", "prompt_findings", "input", "profile", "model", "host", "locality"];

/** The golden card with some members replaced. */
const view = (changes = {}) => ({ ...structuredClone(GOLDEN.view), ...changes });
/** A request the page holds, first seen at `since`. */
const item = (changes = {}, since = 1000) => ({ view: readAskView(view(changes)), since, sending: false, fault: null });

test("an ask is read member by member and nothing else is kept", () => {
  const read = readAskView(GOLDEN.view);
  assert.deepEqual(Object.keys(read), VIEW_KEYS, "exactly the ten members, in the core's order");
  assert.deepEqual(Object.keys(read.input), ["lines", "bytes", "sent_bytes", "findings"]);

  // A member that is not named never reaches the page, and an own `__proto__` is never copied.
  const wide = readAskView({ ...GOLDEN.view, extra: 1, text: "piped", input: { ...GOLDEN.view.input, text: "piped" } });
  assert.deepEqual(Object.keys(wide), VIEW_KEYS);
  assert.deepEqual(Object.keys(wide.input), ["lines", "bytes", "sent_bytes", "findings"]);
  const proto = { ...GOLDEN.view };
  Object.defineProperty(proto, "__proto__", { value: { polluted: true }, enumerable: true });
  Object.defineProperty(proto, "constructor", { value: "x", enumerable: true });
  const copied = readAskView(proto);
  assert.equal(Object.hasOwn(copied, "__proto__"), false);
  assert.equal(Object.hasOwn(copied, "constructor"), false);
  assert.equal(copied.polluted, undefined);

  // Each bad member makes the whole card unreadable.
  const bad = [
    ["an id that is not 32 lower-case hex", { ask: "0123456789ABCDEF0123456789ABCDEF" }],
    ["a short id", { ask: "0123456789abcdef" }],
    ["a seal that is not 64", { seal: "ab".repeat(31) }],
    ["a seal of upper case", { seal: "AB".repeat(32) }],
    ["a pane below 1", { pane: 0 }],
    ["a fractional pane", { pane: 1.5 }],
    ["a prompt over 16384 bytes", { prompt: "a".repeat(MAX_PROMPT + 1) }],
    ["a prompt of wide characters over 16384 bytes", { prompt: "é".repeat(MAX_PROMPT / 2 + 1) }],
    ["a prompt that is no string", { prompt: 7 }],
    ["more than 22 findings", { prompt_findings: Array.from({ length: 23 }, () => ({ rule: "jwt", count: 1 })) }],
    ["a negative count", { prompt_findings: [{ rule: "jwt", count: -1 }] }],
    ["a finding without a rule", { prompt_findings: [{ count: 1 }] }],
    ["a host that is no string", { host: 7 }],
    ["a profile that is no string", { profile: null }],
    ["a place that is no string", { locality: 3 }],
    ["an input that is no object", { input: "text" }],
    ["an input with a negative size", { input: { lines: 1, bytes: -1, sent_bytes: 0, findings: [] } }],
    ["an input without findings", { input: { lines: 1, bytes: 1, sent_bytes: 1 } }],
  ];
  for (const [what, changes] of bad) assert.equal(readAskView(view(changes)), null, what);
  for (const raw of [null, undefined, "x", 7, [], [GOLDEN.view]]) assert.equal(readAskView(raw), null);
  const missing = view();
  delete missing.seal;
  assert.equal(readAskView(missing), null, "a missing member");
  // The edge of the limit is accepted, and the profile, model and host are cleaned lines.
  assert.equal(readAskView(view({ prompt: "a".repeat(MAX_PROMPT) })).prompt.length, MAX_PROMPT);
  const marked = readAskView(view({ host: "api.‮example.org\n" }));
  assert.equal(marked.host, "api.example.org", "no direction control and no line break in a host");
});

test("the readers are held to the golden", () => {
  const withInput = readAskView(GOLDEN.view);
  assert.deepEqual(withInput, GOLDEN.view, "the card with piped text reads back as it is");
  const promptOnly = readAskView(GOLDEN.view_prompt_only);
  assert.equal(promptOnly.input, null);
  assert.deepEqual(promptOnly, GOLDEN.view_prompt_only);

  // The events of the golden read back, and the page's names are the golden's.
  assert.deepEqual(readAskEvent(GOLDEN.events.waiting), { kind: "waiting", view: GOLDEN.view });
  assert.deepEqual(readAskEvent(GOLDEN.events.closed), { kind: "closed", ask: GOLDEN.view.ask, why: "timed_out" });
  assert.deepEqual([...CLOSED], GOLDEN.closed, "the six closed names");
  assert.deepEqual([...VERDICTS], GOLDEN.verdicts, "the two verdicts");
  assert.deepEqual([...REASONS].sort(), Object.keys(GOLDEN.faults).sort(), "the refusals of an answer");
  for (const why of GOLDEN.closed) {
    assert.deepEqual(readAskEvent({ kind: "closed", ask: GOLDEN.view.ask, why }), { kind: "closed", ask: GOLDEN.view.ask, why });
  }
  // What the page does not know is not an event.
  assert.equal(readAskEvent({ kind: "closed", ask: GOLDEN.view.ask, why: "vanished" }), null);
  assert.equal(readAskEvent({ kind: "closed", ask: "x", why: "sent" }), null);
  assert.equal(readAskEvent({ kind: "waiting", view: { ...GOLDEN.view, seal: "x" } }), null);
  assert.equal(readAskEvent({ kind: "other" }), null);
  assert.equal(readAskEvent(null), null);
  // The cards a reloaded page asks for: the readable ones only, at most four, without a repeat.
  const many = Array.from({ length: 6 }, (_, n) => ({ ...GOLDEN.view, ask: `${n}`.repeat(32) }));
  assert.equal(readPending([...many, { nonsense: true }]).length, MAX_ASKS);
  assert.deepEqual(readPending([GOLDEN.view, GOLDEN.view]).length, 1);
  assert.deepEqual(readPending("nothing"), []);
  assert.deepEqual(readPending(null), []);
});

test("the prompt is shown in full and hidden characters become escapes", () => {
  const hidden = readAskView(view({ prompt: "a‮b\u{e0041}c\u001bd\ne\tf" }));
  const shown = promptWords(hidden);
  assert.equal(shown, "a\\u{202e}b\\u{e0041}c\\u{1b}d\ne\tf", "escapes for the hidden characters, the line break and the tab stay");
  for (const c of shown) {
    const code = c.codePointAt(0);
    assert.ok(!(code < 0x20 && c !== "\n" && c !== "\t") && code !== 0x202e && !(code >= 0xe0000 && code <= 0xe007f), "a raw hidden character");
  }
  assert.equal(promptWords(readAskView(view({ prompt: "" }))), "none: the piped text is the question");
  const whole = "x".repeat(MAX_PROMPT);
  assert.equal(promptWords(readAskView(view({ prompt: whole }))), whole, "a prompt of 16384 characters comes back whole");
  assert.equal(promptWords(readAskView(view({ prompt: "<img src=x onerror=1>" }))), "<img src=x onerror=1>", "markup is text");
});

test("the route and the place are the core's words", () => {
  const cloud = routeWords(readAskView(view({ profile: "anthropic", model: "example-model", host: "api.example.org", locality: "cloud" })));
  assert.deepEqual(cloud, { route: "anthropic · example-model", host: "api.example.org", place: "cloud", leaves: true });
  assert.equal(placeWords(cloud), "cloud · leaves this computer");
  const places = [
    ["lan", "local network", true],
    ["loopback", "this computer", false],
    ["on_device", "on device", false],
    ["moon", "unknown place", false],
  ];
  for (const [locality, word, leaves] of places) {
    const route = routeWords(readAskView(view({ locality })));
    assert.equal(route.place, word, locality);
    assert.equal(route.leaves, leaves, locality);
    assert.equal(placeWords(route), leaves ? `${word} · leaves this computer` : word);
  }
  // The piped text as counts and findings; the sample of the golden is 6254 bytes, 5939 after cleaning.
  assert.equal(inputWords(GOLDEN.view.input), "142 lines · 6.1 KiB, 5.8 KiB after cleaning");
  assert.equal(inputWords({ lines: 1, bytes: 12, sent_bytes: 10, findings: [] }), "1 line · 12 B, 10 B after cleaning");
  assert.equal(inputWords(null), "none");
  assert.deepEqual(findingLines(GOLDEN.view.input.findings), ["Anthropic key hidden"]);
  assert.deepEqual(findingLines(GOLDEN.view.prompt_findings), ["hidden tag characters removed (2 characters)"]);
  assert.equal(WORDS.title, "Send this to the model?");
  assert.equal(WORDS.lead(3), "A program in pane 3 asked this. Nothing has left this computer yet.");
  assert.equal(WORDS.foot(3), "The answer is printed in pane 3. holzWave does not keep this request. Esc refuses.");
  assert.equal(WORDS.count(1, 2), "1 of 2");
  assert.equal(WORDS.head(3), "wsh ai · pane 3");
});

test("a send is possible only after the ignore window and only for a trusted press", () => {
  assert.equal(IGNORE_MS, 500);
  const one = item({}, 1000);
  assert.equal(canSend(one, 1000, true), false, "at once");
  assert.equal(canSend(one, 1499, true), false, "within the window");
  assert.equal(canSend(one, 1500, true), true, "after the window");
  assert.equal(canSend(one, 9999, false), false, "a press that a script made");
  assert.equal(canSend({ ...one, sending: true }, 9999, true), false, "while an answer is on its way");
  assert.equal(canSend(undefined, 9999, true), false, "a request that is gone");

  // A refusal is never held back by the window, only by a script, a gone request or a press already on its way.
  assert.equal(canRefuse(one, true), true);
  assert.equal(canRefuse(item({}, 0), true), true);
  assert.equal(canRefuse(one, false), false);
  assert.equal(canRefuse(undefined, true), false);
  assert.equal(canRefuse({ ...one, sending: true }, true), false);
});

test("the queue keeps arrival order, drops a repeated ask and holds at most four", () => {
  const ask = (n) => item({ ask: `${n}`.repeat(32) }, n);
  let list = [];
  for (const n of [1, 2, 3]) list = addAsk(list, ask(n));
  assert.deepEqual(list.map((i) => i.view.ask[0]), ["1", "2", "3"], "arrival order");
  const again = addAsk(list, ask(2));
  assert.deepEqual(again.map((i) => i.view.ask[0]), ["1", "2", "3"], "a repeated ask is ignored");
  assert.equal(again[1].since, 2, "and the first record stays");
  list = addAsk(list, ask(4));
  assert.equal(list.length, MAX_ASKS);
  assert.deepEqual(addAsk(list, ask(5)).map((i) => i.view.ask[0]), ["1", "2", "3", "4"], "a fifth is not held");
  assert.deepEqual(removeAsk(list, "2".repeat(32)).map((i) => i.view.ask[0]), ["1", "3", "4"]);
  assert.equal(removeAsk(list, "9".repeat(32)).length, 4, "an unknown ask removes nothing");
  assert.equal(list.length, 4, "the list is not changed in place");
});

test("a closed event removes its ask and only the unplanned ends leave a sentence", () => {
  assert.equal(closedNotice("sent", 3), null);
  assert.equal(closedNotice("refused", 3), null);
  assert.equal(closedNotice("shutting_down", 3), null);
  assert.equal(closedNotice("gone", 3), "The wsh ai request from pane 3 ended before it was answered.");
  assert.equal(closedNotice("timed_out", 3), "The wsh ai request from pane 3 timed out; nothing was sent.");
  assert.equal(closedNotice("pane_closed", 3), "The wsh ai request from pane 3 was withdrawn: the pane closed.");
  for (const why of CLOSED) {
    const notice = closedNotice(why, 12);
    assert.ok(notice === null || notice.includes("pane 12"), why);
  }
  // A refusal of an answer: the core's sentence cleaned and cut, the reason kept, anything else the fixed sentence.
  assert.deepEqual(answerFault({ code: "ask", reason: "seal_mismatch", text: GOLDEN.faults.seal_mismatch }), {
    reason: "seal_mismatch",
    text: GOLDEN.faults.seal_mismatch,
  });
  const long = answerFault({ reason: "other", text: `a\u0007b\n${"x".repeat(400)}` });
  assert.equal(long.reason, "other");
  assert.ok(Array.from(long.text).length <= 300 && !/[\u0000-\u001f]/.test(long.text), long.text);
  for (const raw of ["a string", 7, null, undefined, {}, { text: 3 }, { text: "" }, new Error("x")]) {
    assert.deepEqual(answerFault(raw), { reason: null, text: "The answer could not be sent." });
  }
  assert.equal(answerFault({ text: "x" }).reason, null);
});
