// The pure half of the AI providers dialog (aistate.ts): the page's copy of the provider table, held to the core by the golden
// that a core test writes, the reader of the provider list, and the input of a save. No DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  PRESETS, SLOTS, WIRE_LABELS, inputOf, newDraft, draftOf, presetOf, readProfileViews, sameDraft,
} from "../../dist/aistate.js";

const golden = JSON.parse(readFileSync(new URL("../fixtures/ai_profiles.json", import.meta.url), "utf8"));
const protocol = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));

/** A canary that must never be kept from a profile view. */
const CANARY = "zq-view-canary-4d83a1";

test("the preset table is the core's table and holds no address", () => {
  const asGolden = PRESETS.map((p) => ({
    tag: p.tag, label: p.label, wires: p.wires, address: p.address, key: p.key, gateway_token: p.gateway_token,
    disclosure: p.disclosure,
  }));
  assert.deepEqual(asGolden, golden.presets, "the page's table differs from the golden the core writes");
  assert.deepEqual(PRESETS.map((p) => Object.keys(p).sort()), PRESETS.map(() => [
    "address", "disclosure", "gateway_token", "key", "label", "tag", "wires",
  ]));
  assert.equal(JSON.stringify(PRESETS).includes("://"), false, "the page holds no address");
  assert.equal(JSON.stringify(WIRE_LABELS).includes("://"), false);
  assert.equal(presetOf("custom").wires.length, 5);
  assert.equal(presetOf("cloudflare_gateway").gateway_token, true);
  assert.equal(presetOf("nope"), null);
  assert.equal(presetOf("__proto__"), null);
  assert.equal(presetOf("constructor"), null);
  for (const p of PRESETS) {
    for (const wire of p.wires) assert.equal(typeof WIRE_LABELS[wire], "string", `${p.tag}: no label for ${wire}`);
    assert.deepEqual(Object.keys(p.address).sort(), [...p.wires].sort(), `${p.tag}: an address rule for every wire`);
  }
  assert.deepEqual(Object.keys(WIRE_LABELS).sort(), ["anthropic", "gemini", "ollama", "openai_chat", "openai_responses"]);
});

/** A profile view as the core writes it, with the members of the golden's first view. */
const view = (id, extra = {}) => ({ ...structuredClone(golden.views[0]), id, ...extra });

test("profile views are read member by member and no key is ever read", () => {
  const rows = readProfileViews({ profiles: golden.views, notes: [] });
  assert.equal(rows.profiles.length, 5);
  for (const row of rows.profiles) {
    assert.deepEqual(Object.keys(row).sort(), [
      "allow_lan_http", "base_url", "disclosure", "gateway_key", "host", "id", "key", "model", "name", "preset", "wire",
    ]);
    assert.equal(Object.getPrototypeOf(row), Object.prototype);
  }
  assert.deepEqual(rows.profiles.map((r) => r.key), ["not_set", "present", "present", "present", "wrong_address"]);
  assert.deepEqual(rows.profiles.map((r) => r.gateway_key), [null, null, null, "present", null]);
  assert.deepEqual(rows.profiles.map((r) => r.disclosure), golden.views.map((v) => v.disclosure));
  assert.deepEqual(rows.profiles.map((r) => r.id), golden.views.map((v) => v.id));
  assert.deepEqual(rows.notes, []);

  // A member that is a key, a secret or anything unknown is not kept, and an own __proto__ member never arrives.
  const hostile = JSON.parse(
    JSON.stringify({ profiles: [view("a", { api_key: CANARY, key_text: CANARY, extra: { deep: CANARY } })] }).replace(
      "{\"allow", "{\"__proto__\":{\"polluted\":true},\"allow",
    ),
  );
  const kept = readProfileViews(hostile).profiles;
  assert.equal(kept.length, 1);
  assert.equal(JSON.stringify(kept).includes(CANARY), false);
  assert.equal(Object.hasOwn(kept[0], "__proto__"), false);
  assert.equal(kept[0].polluted, undefined);

  // An entry without a usable id, or of an unknown kind or wire, or that is no object, is dropped.
  const dropped = readProfileViews({
    profiles: [
      view("x", { id: undefined }), view("x", { id: 7 }), view(""), view("y".repeat(65)), view("p", { preset: "nope" }),
      view("w", { wire: "carrier_pigeon" }), view("c", { preset: 3 }), null, "text", 3, [], view("ok"),
    ],
  });
  assert.deepEqual(dropped.profiles.map((r) => r.id), ["ok"]);
  assert.equal(readProfileViews({ profiles: [view("y".repeat(64))] }).profiles.length, 1, "64 characters are still an id");
  // The same id twice is one row: the first.
  assert.deepEqual(readProfileViews({ profiles: [view("a", { name: "one" }), view("a", { name: "two" })] }).profiles.map((r) => r.name), ["one"]);

  // A key state that is not one of the seven names reads as damaged: it is never taken for a good one.
  const states = readProfileViews({
    profiles: [view("a", { key: "set" }), view("b", { key: 7 }), view("c", { key: "present", gateway_key: "yes" }), view("d", { gateway_key: "missing" })],
  }).profiles;
  assert.deepEqual(states.map((r) => r.key), ["damaged", "damaged", "present", "not_set"]);
  assert.deepEqual(states.map((r) => r.gateway_key), [null, null, "damaged", "missing"]);

  // A disclosure that is an object but malformed reads as the stricter kind with the profile's host.
  const disclosures = readProfileViews({
    profiles: [
      view("a", { host: "h.example", disclosure: {} }),
      view("b", { host: "h.example", disclosure: { host: 3, kind: "other" } }),
      view("c", { host: "h.example", disclosure: { host: "gw.example", kind: "gateway" } }),
      view("d", { disclosure: null }),
      view("e", { host: "h.example", disclosure: "yes" }),
    ],
  }).profiles.map((r) => r.disclosure);
  assert.deepEqual(disclosures, [
    { host: "h.example", kind: "third_party" },
    { host: "h.example", kind: "third_party" },
    { host: "gw.example", kind: "gateway" },
    null,
    { host: "h.example", kind: "third_party" },
  ]);

  // Text members are cut (the address at 2048) and lose control characters.
  const long = readProfileViews({
    profiles: [view("a", { name: `a\u0000b‮${"n".repeat(400)}`, model: "m".repeat(400), host: "h".repeat(400), base_url: `https://x/${"p".repeat(3000)}` })],
  }).profiles[0];
  assert.equal(long.name.length, 300);
  assert.equal(long.name.startsWith("ab"), true);
  assert.equal(long.model.length, 300);
  assert.equal(long.host.length, 300);
  assert.equal(long.base_url.length, 2048);
  assert.equal(readProfileViews({ profiles: [view("a", { allow_lan_http: "yes" })] }).profiles[0].allow_lan_http, false);

  // At most 64 rows and 16 notes, each note cut at 200 characters; a note that is no text is dropped.
  const many = Array.from({ length: 65 }, (_, i) => view(`p${i}`));
  assert.equal(readProfileViews({ profiles: many }).profiles.length, 64);
  const notes = Array.from({ length: 17 }, (_, i) => `note ${i} ${"n".repeat(300)}`);
  const noted = readProfileViews({ profiles: [], notes: [...notes, 7, null] }).notes;
  assert.equal(noted.length, 16);
  assert.ok(noted.every((n) => n.length === 200));
  assert.deepEqual(readProfileViews({ profiles: [], notes: ["a\u0001b"] }).notes, ["ab"]);

  // Anything that is not an object with a list gives nothing.
  for (const raw of [null, undefined, 0, "x", [], {}, { profiles: "x" }, { profiles: {} }]) {
    assert.deepEqual(readProfileViews(raw), { profiles: [], notes: [] });
  }
});

test("the input of a save has exactly eight members and never a key", () => {
  const eight = ["acknowledge_gateway", "allow_lan_http", "base_url", "id", "model", "name", "preset", "wire"];
  const fresh = newDraft();
  const input = inputOf(fresh, false);
  assert.deepEqual(Object.keys(input).sort(), eight);
  assert.equal(input.id, null);
  assert.equal(input.acknowledge_gateway, false);
  assert.equal(fresh.preset, PRESETS[0].tag, "a new provider starts on the first type of the table");
  assert.equal(fresh.wire, PRESETS[0].wires[0]);
  assert.equal(inputOf(fresh, true).acknowledge_gateway, true);
  assert.equal(inputOf(fresh, false).acknowledge_gateway, false);

  // Blanks around the name, the address and the model are trimmed; nothing else is changed.
  const typed = { ...fresh, name: "  Local \n", base_url: "  http://Example.org/A  ", model: " llama3.2 ", allow_lan_http: true, preset: "custom", wire: "ollama" };
  assert.deepEqual(inputOf(typed, false), {
    id: null, name: "Local", preset: "custom", wire: "ollama", base_url: "http://Example.org/A", model: "llama3.2",
    allow_lan_http: true, acknowledge_gateway: false,
  });

  // The draft of a saved row carries its id, and equals itself until a field changes.
  const row = readProfileViews({ profiles: [golden.views[3]] }).profiles[0];
  const draft = draftOf(row);
  assert.equal(inputOf(draft, false).id, row.id);
  assert.equal(inputOf(draft, false).wire, row.wire);
  assert.equal(sameDraft(draft, draftOf(row)), true);
  assert.equal(sameDraft(draft, { ...draft, model: "other" }), false);
  assert.equal(sameDraft(draft, { ...draft, allow_lan_http: true }), false);
  assert.equal(sameDraft(draft, { ...draft, wire: "gemini" }), false);
  assert.equal(sameDraft(draft, { ...draft, id: null }), false);

  // The protocol golden's inputs have members from the same eight, and the slots are the shell's.
  for (const sample of protocol.ops.ai_profile_input) {
    for (const name of Object.keys(sample)) assert.ok(eight.includes(name), `${name} is not a member of a save`);
  }
  assert.deepEqual(SLOTS, protocol.ops.key_slot);
});
