// The words and rules of the AI chat that need no page (chatview.ts): which profile a message is sent with, how a profile
// list and a refusal are read. No DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import { faultText, pickProfile, readProfiles } from "../../dist/chatview.js";

const entry = (id, extra = {}) => ({ id, name: `Name ${id}`, model: "llama3.2", host: "localhost", ...extra });

test("profiles are read member by member and a key or an address is never kept", () => {
  const raw = JSON.parse(
    `{"profiles": [
      {"id": "local-1", "name": "Local", "model": "llama3.2", "host": "localhost", "base_url": "http://localhost:11434",
       "key": "set", "gateway_key": "not_set", "__proto__": {"polluted": true}},
      {"id": "reg", "name": "Apple", "model": "on-device", "host": null}
    ], "notes": ["a note"]}`,
  );
  const list = readProfiles(raw);
  assert.deepEqual(list, [
    { id: "local-1", name: "Local", model: "llama3.2", host: "localhost" },
    { id: "reg", name: "Apple", model: "on-device", host: "" },
  ]);
  for (const p of list) {
    assert.deepEqual(Object.keys(p).sort(), ["host", "id", "model", "name"], "exactly the four members");
    assert.equal(Object.getPrototypeOf(p), Object.prototype);
    assert.equal(Object.hasOwn(p, "__proto__"), false);
    assert.equal(p.polluted, undefined);
  }
  // An entry that cannot be told apart from another is dropped: no id, an empty id, a long id, a bad entry.
  const dropped = readProfiles({
    profiles: [{ name: "x" }, { id: 7 }, { id: "" }, { id: "x".repeat(65) }, null, "text", 3, [], entry("ok")],
  });
  assert.deepEqual(dropped.map((p) => p.id), ["ok"]);
  assert.equal(readProfiles({ profiles: [entry("x".repeat(64))] }).length, 1, "64 characters are still an id");
  // At most 64 entries.
  const many = Array.from({ length: 65 }, (_, i) => entry(`p${i}`));
  assert.equal(readProfiles({ profiles: many }).length, 64);
  // Text members are cut at 120 characters and lose control characters; a member that is no text is empty.
  const long = readProfiles({
    profiles: [{ id: "a", name: "n".repeat(300), model: "m\u0000o\nd", host: `h‮ost`, extra: 1 }, { id: "b", name: 5, model: {}, host: [] }],
  });
  assert.equal(Array.from(long[0].name).length, 120);
  assert.equal(long[0].model, "mod");
  assert.equal(long[0].host, "host");
  assert.deepEqual(long[1], { id: "b", name: "", model: "", host: "" });
  // Anything that is not an object with a list of profiles is an empty list.
  for (const bad of [null, undefined, 5, "profiles", [], { profiles: "x" }, { profiles: {} }, { notes: [] }]) {
    assert.deepEqual(readProfiles(bad), []);
  }
});

test("the profile to send with is the choice, then the pin, then the only profile, and never a guess", () => {
  const one = [entry("a")];
  const two = [entry("a"), entry("b")];
  // The choice wins when it is in the list, or when no list is loaded yet.
  assert.deepEqual(pickProfile(two, "b", "a"), { id: "b", via: "choice" });
  assert.deepEqual(pickProfile(null, "b", null), { id: "b", via: "choice" });
  // A choice that is not in a loaded list is ignored: the pin, then the rest decide.
  assert.deepEqual(pickProfile(two, "gone", "a"), { id: "a", via: "pin" });
  assert.deepEqual(pickProfile(two, "gone", null), { id: "", via: "ask" });
  assert.deepEqual(pickProfile(one, "gone", null), { id: "a", via: "only" });
  // The pin counts also when it is no profile of the list: a registered provider is not a profile.
  assert.deepEqual(pickProfile(two, null, "registered"), { id: "registered", via: "pin" });
  assert.deepEqual(pickProfile(null, null, "registered"), { id: "registered", via: "pin" });
  // An empty choice or pin is no choice and no pin.
  assert.deepEqual(pickProfile(one, "", ""), { id: "a", via: "only" });
  // The one saved profile; two or more mean ask; a loaded empty list means none; no list means read it first.
  assert.deepEqual(pickProfile(one, null, null), { id: "a", via: "only" });
  assert.deepEqual(pickProfile(two, null, null), { id: "", via: "ask" });
  assert.deepEqual(pickProfile([], null, null), { id: "", via: "none" });
  assert.deepEqual(pickProfile(null, null, null), { id: "", via: "unknown" });
  // The names of profiles play no part: a profile named like a cloud service is not preferred.
  const named = [entry("a", { name: "Anthropic", host: "api.example.org" }), entry("b", { name: "Local", host: "localhost" })];
  assert.deepEqual(pickProfile(named, null, null), { id: "", via: "ask" });
});

test("a refusal is read as its text and nothing else", () => {
  assert.equal(faultText({ code: "no_profile", reason: null, text: "no provider is chosen" }), "no provider is chosen");
  // Long text with a newline and a NUL: cut at 300 characters and the control characters are gone.
  const long = faultText({ code: "store", reason: null, text: `${"a".repeat(100)}\n\u0000${"b".repeat(400)}` });
  assert.equal(Array.from(long).length, 300);
  assert.equal(long, `${"a".repeat(100)}${"b".repeat(200)}`);
  // A bare string (the shell's own error) is cut the same way.
  assert.equal(faultText(`x\ny${"z".repeat(500)}`).length, 300);
  assert.equal(faultText("plain error"), "plain error");
  // Anything else, and an empty text, is the fixed sentence.
  for (const bad of [null, undefined, 42, true, {}, { text: 5 }, { text: "" }, { text: "\u0000\n" }, [], ""]) {
    assert.equal(faultText(bad), "the request was refused");
  }
});
