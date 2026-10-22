// What the page knows about integration profiles (src/integratestate.ts, built into dist/): the reader of the core's profile list,
// the text of a pinned selection, the fields a tool selects, the profile object a save sends and the words of a secret row. No DOM
// is needed. The fixtures are the golden answers the core writes, so the page and the core cannot drift apart.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { profileSelectionText, readProfiles } from "../../dist/integratestate.js";

const golden = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));

const profile = (over = {}) => ({
  id: "prod", tool: "kube", name: "Production", context: "prod-cluster", namespace: null, subscription: null, section: null, node: null,
  production: true, isolated: false, env: {}, secret_env: [], ...over,
});

test("the reader keeps the profiles and notes of the golden answer", () => {
  const read = readProfiles(golden.answers.integration.profiles);
  assert.deepEqual(read.notes, ["integrations.json was not used: it has a member or a value that is not allowed (line 3, column 9)"]);
  assert.deepEqual(read.profiles.map((p) => [p.id, p.tool, p.name, p.context, p.subscription, p.production, p.isolated]), [
    ["prod", "kube", "Production", "prod-cluster", null, true, false],
    ["work", "azure", "Work", null, "00000000-0000-0000-0000-000000000000", false, true],
  ]);
  assert.deepEqual(read.profiles[0].env, [{ name: "AWS_PROFILE", value: "prod" }]);
  assert.deepEqual(read.profiles[0].secrets, [
    { name: "AWS_SECRET_ACCESS_KEY", has_value: true },
    { name: "AWS_SESSION_TOKEN", has_value: false },
  ]);
  assert.deepEqual(read.profiles[1].secrets, []);
});

test("the reader drops what is not the contract", () => {
  assert.deepEqual(readProfiles(null), { profiles: [], notes: [] });
  assert.deepEqual(readProfiles("profiles"), { profiles: [], notes: [] });
  assert.deepEqual(readProfiles({ profiles: "x", notes: 4 }), { profiles: [], notes: [] });
  const read = readProfiles({
    profiles: [
      profile(),
      profile({ id: 7 }),
      profile({ id: "" }),
      "prod",
      null,
      profile({ id: "dup", name: "first" }),
      profile({ id: "dup", name: "second" }),
      profile({ id: "odd", tool: "<b>nope</b>", name: 4, production: "yes", isolated: 1, context: 4, namespace: ["a"] }),
      profile({ id: "env", env: { OK: "1", BAD: 5, "": "x" }, secret_env: [{ name: "A", has_value: "yes" }, { name: 9 }, "B", { name: "C", has_value: true }] }),
    ],
    notes: ["one", 4, null, "two"],
  });
  assert.deepEqual(read.profiles.map((p) => p.id), ["prod", "dup", "odd", "env"], "a profile needs a text id and each id counts once");
  assert.equal(read.profiles[1].name, "first");
  const odd = read.profiles[2];
  assert.equal(odd.tool, "<b>nope</b>", "an unknown tool word is kept as text");
  assert.equal(odd.name, "odd", "a name that is no text is the id");
  assert.equal(odd.production, false, "a flag is true or it is false");
  assert.equal(odd.isolated, false);
  assert.equal(odd.context, null, "a member that is no text is empty");
  assert.equal(odd.namespace, null);
  const env = read.profiles[3];
  assert.deepEqual(env.env, [{ name: "OK", value: "1" }], "a variable needs a name and a text value");
  assert.deepEqual(env.secrets, [{ name: "A", has_value: false }, { name: "C", has_value: true }]);
  assert.deepEqual(read.notes, ["one", "two"]);
});

test("more than 64 profiles are cut and long text is cut", () => {
  const many = Array.from({ length: 80 }, (_, i) => profile({ id: `p${i}` }));
  assert.equal(readProfiles({ profiles: many, notes: [] }).profiles.length, 64);
  const long = "x".repeat(500);
  const read = readProfiles({ profiles: [profile({ name: long, context: long })], notes: [long] });
  assert.ok(read.profiles[0].name.length <= 128);
  assert.ok((read.profiles[0].context ?? "").length <= 253);
  assert.ok(read.notes[0].length <= 400);
  const notes = readProfiles({ profiles: [], notes: Array.from({ length: 40 }, (_, i) => `n${i}`) }).notes;
  assert.ok(notes.length <= 16, "a list of notes is bounded");
});

test("the pinned selection is text and a dash when it pins nothing", () => {
  const p = (over) => readProfiles({ profiles: [profile(over)], notes: [] }).profiles[0];
  assert.equal(profileSelectionText(p({ context: "prod-eu", namespace: "payments" })), "prod-eu · payments");
  assert.equal(profileSelectionText(p({ context: "talos-prod", node: "node-1", tool: "talos" })), "talos-prod · node-1");
  assert.equal(profileSelectionText(p({ context: null, subscription: "holz-prod", tool: "azure" })), "holz-prod");
  assert.equal(profileSelectionText(p({ context: null, section: "work", tool: "gam" })), "work");
  assert.equal(profileSelectionText(p({ context: null })), "—");
  const hostile = "<img src=x onerror=alert(1)>";
  assert.equal(profileSelectionText(p({ context: hostile, namespace: "a" })), `${hostile} · a`, "markup stays those characters");
  assert.ok(Array.from(profileSelectionText(p({ context: "c".repeat(200) }))).length <= 60, "cut for the list");
});
