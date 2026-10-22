// What the page knows about integration profiles (src/integratestate.ts, built into dist/): the reader of the core's profile list,
// the text of a pinned selection, the fields a tool selects, the profile object a save sends and the words of a secret row. No DOM
// is needed. The fixtures are the golden answers the core writes, so the page and the core cannot drift apart.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  duplicateName, fieldsFor, profileSelectionText, profileToSave, readProfiles, slug, takesOwnFolder, uniqueId,
} from "../../dist/integratestate.js";

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

// ---- Task 2: the form follows the tool, the object a save sends ---------------------------------------------------------

const tool = (id, selects) => ({ id, label: id, state: "found", selects, verbs: [] });

test("the fields come from what the core says the tool selects, in its order, and no tool is named", () => {
  assert.deepEqual(fieldsFor(tool("kube", ["context", "namespace"])), ["context", "namespace"]);
  assert.deepEqual(fieldsFor(tool("talos", ["context", "node"])), ["context", "node"]);
  assert.deepEqual(fieldsFor(tool("azure", ["subscription"])), ["subscription"]);
  assert.deepEqual(fieldsFor(tool("gam", ["section"])), ["section"]);
  // A tool the page has never heard of is drawn from its list alone.
  assert.deepEqual(fieldsFor(tool("later", ["node", "section"])), ["node", "section"]);
  assert.deepEqual(fieldsFor(tool("later", [])), []);
  assert.deepEqual(fieldsFor(null), [], "a tool the core did not list has no fields");
});

test("an own configuration folder is offered where the tool selects a subscription or a section", () => {
  assert.equal(takesOwnFolder(tool("azure", ["subscription"])), true);
  assert.equal(takesOwnFolder(tool("gam", ["section"])), true);
  assert.equal(takesOwnFolder(tool("kube", ["context", "namespace"])), false);
  assert.equal(takesOwnFolder(tool("talos", ["context", "node"])), false);
  assert.equal(takesOwnFolder(null), false);
});

test("the id of a new profile is a lower-case slug of its name, made unique", () => {
  assert.equal(slug("Production EU"), "production-eu");
  assert.equal(slug("  My_work   profile!! "), "my_work-profile");
  assert.equal(slug("--x--"), "x", "an id does not start with a dash");
  assert.equal(slug("Zürich"), "z-rich");
  assert.equal(slug("日本語"), "");
  assert.equal(slug("a".repeat(50)).length, 32);
  assert.equal(slug(`${"a".repeat(31)}-b`), "a".repeat(31), "a cut never ends in a dash");
  assert.equal(uniqueId("staging", ["prod", "staging"]), "staging-2");
  assert.equal(uniqueId("staging", ["staging", "staging-2"]), "staging-3");
  assert.equal(uniqueId("", ["profile"]), "profile-2", "a name with no letter or digit still gets an id");
  assert.equal(uniqueId("prod", []), "prod");
  const long = uniqueId("a".repeat(32), ["a".repeat(32)]);
  assert.ok(long.length <= 32 && long.endsWith("-2") && long !== "a".repeat(32), long);
});

const form = (over = {}) => ({
  id: "prod", tool: "kube", name: "Production", fields: { context: "prod-eu", namespace: "payments" }, production: true, isolated: false,
  env: [], secrets: [], ...over,
});

test("a save sends one profile in the files own shape and nothing else", () => {
  const sent = profileToSave(form({ env: [{ name: "AWS_PROFILE", value: "prod" }], secrets: ["AWS_SECRET_ACCESS_KEY"] }), ["context", "namespace"], false);
  assert.deepEqual(sent, {
    id: "prod", tool: "kube", name: "Production", context: "prod-eu", namespace: "payments", subscription: null, section: null, node: null,
    production: true, isolated: false, env: { AWS_PROFILE: "prod" }, secret_env: ["AWS_SECRET_ACCESS_KEY"],
  });
  assert.deepEqual(Object.keys(sent).sort(), [
    "context", "env", "id", "isolated", "name", "namespace", "node", "production", "secret_env", "section", "subscription", "tool",
  ]);
});

test("an empty field is null and a field the tool does not select is null whatever the form holds", () => {
  const sent = profileToSave(form({ fields: { context: "  ", namespace: "payments", node: "n1", section: "work" } }), ["context"], false);
  assert.equal(sent.context, null, "empty text is null");
  assert.equal(sent.namespace, null, "a member the tool does not select is not sent");
  assert.equal(sent.node, null);
  assert.equal(sent.section, null);
  assert.equal(profileToSave(form({ fields: { context: " prod-eu " } }), ["context"], false).context, "prod-eu", "the ends are trimmed");
});

test("the flags are sent as they are, and an own folder only where the tool takes one", () => {
  assert.equal(profileToSave(form({ production: true }), ["context"], false).production, true);
  assert.equal(profileToSave(form({ production: false }), ["context"], false).production, false);
  assert.equal(profileToSave(form({ isolated: true }), ["subscription"], true).isolated, true);
  assert.equal(profileToSave(form({ isolated: true }), ["context"], false).isolated, false, "a tool without a folder of its own sends false");
});

test("variables: blank rows are left out, a name and a value go as typed, a value is never trimmed", () => {
  const sent = profileToSave(
    form({ env: [{ name: "A", value: "1" }, { name: "", value: "" }, { name: " B ", value: " x " }, { name: "", value: "orphan" }], secrets: ["S1", "", " S2 "] }),
    ["context"],
    false,
  );
  assert.deepEqual(sent.env, { A: "1", B: " x ", "": "orphan" }, "a row with a value and no name goes to the core, which refuses it");
  assert.deepEqual(sent.secret_env, ["S1", "S2"]);
});

test("a name used by two rows is found before anything is sent", () => {
  assert.equal(duplicateName([{ name: "A", value: "1" }, { name: "B", value: "2" }, { name: "A", value: "3" }]), "A");
  assert.equal(duplicateName([{ name: "A", value: "1" }, { name: "B", value: "2" }]), null);
  assert.equal(duplicateName([{ name: "", value: "" }, { name: "", value: "" }]), null, "blank rows are not names");
  assert.equal(duplicateName([{ name: " A", value: "1" }, { name: "A ", value: "2" }]), "A");
});
