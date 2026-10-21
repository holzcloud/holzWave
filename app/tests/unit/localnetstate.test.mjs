// The pure half of the Local Network notice and row (localnetstate.ts): how the answer of the commands is read. No DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { GROUP, NOTICE, STATE_WORDS, describeLocalNetwork, readLocalNetwork } from "../../dist/localnetstate.js";

const golden = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));

/** The hostile word of these tests; it appears in test code only. */
const CANARY = "zq-local-network-7d2a";

test("every sample of the golden is read as the shell wrote it", () => {
  const samples = golden.answers.local_network;
  assert.equal(samples.length, 5, "four states of a supported system and one unsupported");
  for (const sample of samples) {
    assert.deepEqual(Object.keys(sample).sort(), ["notice_due", "state", "supported"]);
    const view = readLocalNetwork(sample);
    assert.deepEqual(view, { supported: sample.supported, noticeDue: sample.notice_due, state: sample.state }, JSON.stringify(sample));
    assert.deepEqual(Object.keys(view).sort(), ["noticeDue", "state", "supported"], "exactly those three own members");
    assert.equal(Object.getPrototypeOf(view), Object.prototype);
  }
  const states = samples.filter((s) => s.supported).map((s) => s.state).sort();
  assert.deepEqual(states, ["allowed", "denied", "not_asked", "unknown"]);
  assert.equal(readLocalNetwork(samples[0]).noticeDue, true, "the first start owes the notice");
});

test("hostile input is never kept", () => {
  // Not an object, or flags that are not booleans: null.
  for (const bad of [null, undefined, [], [{ supported: true }], "supported", 7, true, {}, { supported: true }, { notice_due: false },
    { supported: 1, notice_due: false, state: "allowed" }, { supported: true, notice_due: "false", state: "allowed" },
    { supported: null, notice_due: null, state: "allowed" }]) {
    assert.equal(readLocalNetwork(bad), null, JSON.stringify(bad));
  }
  // Inherited members do not count.
  assert.equal(readLocalNetwork(Object.create({ supported: true, notice_due: false, state: "allowed" })), null);

  // A state word that is not exactly one of the four reads as unknown, whatever it looks like.
  for (const state of ["Allowed", "ALLOWED", "allowed ", " allowed", "allowed\n", "not-asked", "notasked", "", 7, null, undefined,
    ["allowed"], { toString: () => "allowed" }, `<img src=x ${CANARY}>`, "a".repeat(5000), "__proto__", "constructor"]) {
    const view = readLocalNetwork({ supported: true, notice_due: false, state });
    assert.equal(view.state, "unknown", JSON.stringify(state));
    assert.ok(!JSON.stringify(view).includes(CANARY), "markup never survives");
  }

  // Extra members and an own __proto__ member are never copied.
  const extra = readLocalNetwork(JSON.parse(`{"supported": true, "notice_due": false, "state": "denied", "api_key": "k",
    "__proto__": {"polluted": true}}`));
  assert.deepEqual(extra, { supported: true, noticeDue: false, state: "denied" });
  assert.equal(Object.hasOwn(extra, "__proto__"), false);
  assert.equal(extra.polluted, undefined);
  assert.equal(Object.keys(extra).length, 3);
});

// The sentences below are the design record's (docs/design.md 6.24); the tables of the page are held to them here and in the browser test.
test("the notice and the group say what the design record says", () => {
  assert.equal(NOTICE.title, "Allow local network access?");
  assert.equal(NOTICE.lead, "holzWave reaches computers on your local network:");
  assert.deepEqual([...NOTICE.items], [
    "SSH to machines on your network.",
    "Local AI servers on other computers, such as Ollama or LM Studio.",
    "Sync with a Nextcloud on your network.",
  ]);
  assert.equal(NOTICE.note, "macOS asks for your permission next. Programs you run in a pane, such as ssh, count as holzWave. You can change this any time in Settings.");
  assert.equal(NOTICE.notNow, "Not now");
  assert.equal(NOTICE.allow, "Allow");
  assert.deepEqual({ ...GROUP }, {
    legend: "Local network", name: "Access", meta: "Privacy & Security", open: "Open System Settings", allow: "Allow",
    check: "Check again", checking: "Checking\u2026", checked: "Checked.",
    asked: "Asked. Answer the macOS question, then choose Check again.",
  });
});

test("every state has a word, a tone, a second cue, an advice sentence and its buttons", () => {
  assert.deepEqual(Object.keys(STATE_WORDS).sort(), ["allowed", "denied", "not_asked", "unknown"]);
  const expected = {
    allowed: ["allowed", "ok", false, "", "holzWave can reach computers on your local network.", false, false],
    denied: ["denied", "warn", true, "!", "macOS blocks holzWave from your local network. Turn holzWave on in System Settings, then choose Check again.", false, true],
    not_asked: ["not asked yet", "plain", false, "", "macOS has not been asked. Choose Allow and macOS shows its own question.", true, false],
    unknown: ["not known", "plain", false, "?", "holzWave could not tell. macOS may still be waiting for your answer. Choose Check again.", false, true],
  };
  for (const [state, [word, tone, underline, mark, advice, allow, check]] of Object.entries(expected)) {
    assert.deepEqual({ ...STATE_WORDS[state] }, { word, tone, underline, mark, advice, allow, check }, state);
    // The state is never colour alone: the word differs for each state, and a state that needs attention has a mark or a line too.
    assert.ok(STATE_WORDS[state].word.length > 0);
  }
  assert.equal(new Set(Object.values(STATE_WORDS).map((w) => w.word)).size, 4, "four different words");
});

test("describeLocalNetwork gives the entry of the state and the unknown entry for anything else", () => {
  for (const sample of golden.answers.local_network.filter((x) => x.supported)) {
    const view = readLocalNetwork(sample);
    assert.deepEqual({ ...describeLocalNetwork(view) }, { state: sample.state, ...STATE_WORDS[sample.state] }, sample.state);
  }
  assert.equal(describeLocalNetwork(null).state, "unknown");
  assert.equal(describeLocalNetwork(null).word, "not known");
  const hostile = readLocalNetwork({ supported: true, notice_due: false, state: "Allowed" });
  assert.equal(describeLocalNetwork(hostile).state, "unknown");
  assert.equal(describeLocalNetwork({ supported: true, noticeDue: false, state: "constructor" }).state, "unknown", "a table key is never inherited");
});
