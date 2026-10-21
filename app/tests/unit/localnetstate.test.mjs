// The pure half of the Local Network notice and row (localnetstate.ts): how the answer of the commands is read. No DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { readLocalNetwork } from "../../dist/localnetstate.js";

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
