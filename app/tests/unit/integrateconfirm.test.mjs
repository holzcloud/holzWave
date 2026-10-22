// What the page knows about the card of an integration action (src/integratestate.ts, built into dist/): the reader of the
// card the core prepared, the words of every mode and the first characters of the hash. No DOM is needed. The golden card is the
// one the core's own test writes, so the page and the core cannot drift apart.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { CARD_WORDS, hashShown, modeWords, readCard } from "../../dist/integratestate.js";

const golden = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));
const HASH = "9f3a41c2d7be0123456789abcdef0123456789abcdef0123456789abcdef0123";
const TICKET = "0123456789abcdef0123456789abcdef";

/** A card as the core spells it, built by the same rule the core builds its fields by. */
function card(over = {}) {
  const argv = over.argv ?? ["delete", "pod", "--context=prod-eu", "--namespace=payments", "--wait=false", "--request-timeout=10s", "--", "api-0"];
  const target = over.target ?? [["context", "prod-eu"], ["namespace", "payments"], ["pod", "api-0"]];
  const production = "production" in over ? over.production : null;
  const effect = over.effect ?? "write";
  const fields = [
    ["tool", over.tool ?? "kube"], ["verb", over.verb ?? "delete_pod"], ["effect", effect], ["program", over.program ?? "kubectl"],
    ...argv.map((a, i) => [`arg ${i + 1}`, a]),
    ...target,
    ["guard", production === null ? "none" : "production"],
    ...(production === null ? [] : [["production_name", production]]),
  ];
  return {
    ticket: TICKET, tool: over.tool ?? "kube", verb: over.verb ?? "delete_pod", effect, program: over.program ?? "kubectl", argv, target,
    guard: { production }, fields: over.fields ?? fields, hash: HASH, expires_in_secs: 120, ...("raw" in over ? over.raw : {}),
  };
}

test("the reader keeps the golden card whole, in the core's order", () => {
  const read = readCard(golden.answers.integration.card);
  assert.ok(read);
  assert.equal(read.ticket, golden.answers.integration.card.ticket);
  assert.equal(read.hash, golden.answers.integration.card.hash);
  assert.deepEqual([read.tool, read.verb, read.effect, read.program], ["kube", "delete_pod", "write", "kubectl"]);
  assert.deepEqual(read.argv, ["delete", "pod", "--wait=false", "--request-timeout=10s", "--", "api-0"]);
  assert.deepEqual(read.target, [["context", "prod"], ["namespace", "payments"], ["pod", "api-0"]]);
  assert.equal(read.production, "Production");
  assert.equal(read.expires, 120);
});

test("the reader keeps a card without a production guard and a Local card with its changes pair", () => {
  const plain = readCard(card());
  assert.ok(plain);
  assert.equal(plain.production, null);
  const local = readCard(
    card({
      tool: "kube", verb: "use_context", effect: "local", argv: ["config", "use-context", "--", "dev"],
      target: [["context", "dev"], ["changes", "the kubeconfig files named by KUBECONFIG, by default ~/.kube/config"]],
    }),
  );
  assert.ok(local);
  assert.deepEqual(local.target.map(([name]) => name), ["context", "changes"]);
});

test("the reader gives null for anything that is not the contract", () => {
  const good = card();
  const without = (key) => ({ ...good, [key]: undefined });
  for (const bad of [null, undefined, "card", 4, [], {}, without("ticket"), without("hash"), without("fields"), without("argv"), without("target")]) {
    assert.equal(readCard(bad), null, JSON.stringify(bad)?.slice(0, 60));
  }
  assert.equal(readCard({ ...good, ticket: "" }), null, "an empty ticket");
  assert.equal(readCard({ ...good, ticket: 7 }), null, "a ticket that is no text");
  assert.equal(readCard({ ...good, hash: HASH.slice(1) }), null, "a hash of 63 characters");
  assert.equal(readCard({ ...good, hash: HASH.toUpperCase() }), null, "a hash that is not lower case hex");
  assert.equal(readCard({ ...good, hash: `${HASH.slice(0, 63)}g` }), null, "a hash that is not hex");
  assert.equal(readCard({ ...good, effect: "read" }), null, "a read has no card");
  assert.equal(readCard({ ...good, effect: "nuke" }), null, "an effect the page does not know");
  assert.equal(readCard({ ...good, fields: good.fields.map(([a]) => a) }), null, "fields that are no pairs");
  assert.equal(readCard({ ...good, fields: good.fields.map(([a, b]) => [a, b, "x"]) }), null, "fields of the wrong shape");
  assert.equal(readCard({ ...good, fields: good.fields.map(([a, b]) => [a, typeof b === "string" ? 4 : b]) }), null, "values that are no text");
  assert.equal(readCard({ ...good, argv: ["a", 4] }), null, "an argument that is no text");
  assert.equal(readCard({ ...good, target: [["context"]] }), null, "a target pair of the wrong shape");
  assert.equal(readCard({ ...good, guard: null }), null, "no guard object");
  assert.equal(readCard({ ...good, guard: { production: "" } }), null, "a production guard without a name");
  assert.equal(readCard({ ...good, guard: { production: 4 } }), null, "a production name that is no text");
});

test("the reader refuses more than 64 fields and a value over 8192 characters", () => {
  const argv = Array.from({ length: 70 }, (_, i) => `a${i}`);
  assert.equal(readCard(card({ argv })), null, "70 arguments make more than 64 fields");
  const sixty = Array.from({ length: 52 }, (_, i) => `a${i}`);
  assert.ok(readCard(card({ argv: sixty })), "52 arguments and the rest are 64 fields or fewer");
  assert.equal(readCard(card({ argv: ["delete", "x".repeat(8193)] })), null, "a value over 8192 characters");
  assert.ok(readCard(card({ argv: ["delete", "x".repeat(8192)] })), "a value of exactly 8192 characters");
});

test("the members must agree with the fields the hash covers", () => {
  const good = card();
  // The program or an argument that differs from its field, a target that is not in the fields, a missing guard field.
  assert.equal(readCard({ ...good, program: "other" }), null);
  assert.equal(readCard({ ...good, argv: [...good.argv.slice(0, -1), "api-1"] }), null);
  assert.equal(readCard({ ...good, target: [["context", "other"]] }), null);
  assert.equal(readCard({ ...good, fields: good.fields.filter(([name]) => name !== "guard") }), null);
  assert.equal(readCard({ ...good, tool: "talos" }), null);
  assert.equal(readCard({ ...good, guard: { production: "Production" } }), null, "a guard the fields do not carry");
});

test("the hash is shown by its first twelve characters", () => {
  assert.equal(hashShown(HASH), "9f3a41c2d7be");
  assert.equal(hashShown(HASH).length, 12);
  assert.equal(hashShown(golden.answers.integration.card.hash), "0123456789ab");
});

test("every mode has words, and the words of an outcome come from the core", () => {
  assert.equal(CARD_WORDS.head, "Confirm needed");
  assert.equal(modeWords("ask"), "");
  assert.equal(modeWords("armed"), "Armed. The next press allows this on production. Nothing has run yet.");
  assert.equal(modeWords("sending"), "Allowed · running…");
  assert.equal(modeWords("done", { kind: "ran", note: 'pod "api-0" deleted', pane: null }), 'Done: pod "api-0" deleted');
  assert.equal(modeWords("done", { kind: "launched", note: "Exec in pod api-0", pane: 5 }), "Opened pane 5");
  assert.equal(modeWords("done", { kind: "launched", note: "x", pane: null }), "Opened a pane");
  assert.equal(modeWords("done", { kind: "ran", note: "", pane: null }), "Done.", "a verb that said nothing");
  assert.equal(modeWords("ended"), "That action is no longer waiting.");
  assert.equal(modeWords("changed"), "The request changed. Nothing ran. Read the card again.");
  assert.equal(modeWords("cannot"), "This request could not be read in full. It can only be denied.");
});
