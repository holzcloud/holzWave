// The words and rules of the AI chat that need no page (chatview.ts): which profile a message is sent with, how a profile
// list and a refusal are read. No DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import {
  BYTES_PER_TOKEN, MAX_TEXT_BYTES, chipView, endNote, estimateTokens, faultText, leavesComputer, localityWord, outcomeWord, pickProfile,
  linkKind, readProfiles, roleWord, setupLabel, statusLabel, statusWord, titleWords, tokenWords, usageWords, utf8Bytes,
} from "../../dist/chatview.js";

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

test("the token estimate is the core rule and the limit is in bytes", () => {
  assert.equal(BYTES_PER_TOKEN, 3);
  assert.equal(MAX_TEXT_BYTES, 131072);
  assert.equal(estimateTokens(""), 0);
  assert.equal(estimateTokens("a"), 1);
  assert.equal(estimateTokens("abc"), 1);
  assert.equal(estimateTokens("abcd"), 2);
  assert.equal(estimateTokens("é".repeat(300)), 200, "600 bytes");
  assert.equal(utf8Bytes("é"), 2);
  assert.equal(utf8Bytes("\u{1F600}"), 4, "an emoji is a surrogate pair of four bytes");
  assert.equal(utf8Bytes("€"), 3);
  assert.equal(utf8Bytes("\ud800"), 3, "a lone surrogate is written as U+FFFD");
  assert.equal(utf8Bytes("a\ud800b"), 5);
  for (const text of ["", "abc", "é€\u{1F600}x", "line\nbreak", "\udc00\ud800", "x".repeat(1000)]) {
    assert.equal(utf8Bytes(text), new TextEncoder().encode(text).length, `the same length as TextEncoder for ${JSON.stringify(text)}`);
  }
  assert.equal(tokenWords(""), "");
  assert.equal(tokenWords("abcd"), "≈ 2 tokens");
  assert.equal(tokenWords("a"), "≈ 1 token");
  assert.equal(tokenWords("a".repeat(3000)), "≈ 1k tokens");
  assert.equal(tokenWords("a".repeat(4500)), "≈ 1.5k tokens");
  assert.equal(tokenWords("a".repeat(36)), "≈ 12 tokens");
});

test("places are words from the core's route and unknown stays unknown", () => {
  assert.equal(localityWord("on_device"), "on device");
  assert.equal(localityWord("loopback"), "this computer");
  assert.equal(localityWord("lan"), "local network");
  assert.equal(localityWord("cloud"), "cloud");
  for (const other of ["Cloud", "", "localhost", "wan", "__proto__", "constructor", null, undefined, 3]) {
    assert.equal(localityWord(other), "unknown place", String(other));
  }
  assert.equal(leavesComputer("lan"), true);
  assert.equal(leavesComputer("cloud"), true);
  for (const stays of ["on_device", "loopback", "other", "", null]) assert.equal(leavesComputer(stays), false);
});

const route = (over = {}) => ({ profile: "local-1", model: "llama3.2", host: "localhost", locality: "loopback", ...over });
const local = { id: "local-1", name: "Local", model: "llama3.2", host: "localhost" };
const cloud = { id: "other-1", name: "Cloud", model: "big-model", host: "api.example.org" };
const nopin = { profile: null, model: null };

test("the chip says what it sends to and the picker appears only with a choice to make", () => {
  const one = chipView(route(), [local], { profile: "local-1", model: "llama3.2" }, null);
  assert.equal(one.kind, "text");
  assert.deepEqual([one.name, one.model, one.host, one.where, one.leaves], ["Local", "llama3.2", "localhost", "this computer", false]);
  assert.equal(one.text, "Local · llama3.2 → localhost");
  for (const piece of ["Local", "llama3.2", "localhost", "this computer"]) assert.ok(one.aria.includes(piece), `the label says ${piece}`);
  // Two profiles and no route: a button, and nothing is named before the user chooses.
  const two = chipView(null, [local, cloud], nopin, null);
  assert.equal(two.kind, "button");
  assert.equal(two.where, "");
  assert.equal(two.name, "");
  assert.equal(chipView(null, [local, cloud], nopin, "other-1").name, "Cloud", "the choice names the profile");
  assert.equal(chipView(null, [local, cloud], nopin, "other-1").kind, "button");
  // No profile at all.
  const none = chipView(null, [], nopin, null);
  assert.equal(none.kind, "none");
  assert.equal(none.text, "no provider set up");
  // A route for a provider that has no address: the place word and no arrow.
  const registered = chipView(route({ profile: "registered", model: "on-device", host: null, locality: "on_device" }), [local], { profile: "registered", model: "on-device" }, null);
  assert.equal(registered.where, "on device");
  assert.equal(registered.text, "registered · on-device");
  assert.ok(!registered.text.includes("→"));
  // A pin without a route: its id and model, no place word.
  const pinned = chipView(null, null, { profile: "local-1", model: "llama3.2" }, null);
  assert.deepEqual([pinned.name, pinned.model, pinned.where, pinned.kind], ["local-1", "llama3.2", "", "text"]);
  // The name falls back to the id when the list has no entry.
  assert.equal(chipView(null, [cloud], { profile: "local-1", model: "m" }, null).name, "local-1");
  assert.equal(chipView(null, null, nopin, null).where, "", "no route, no place word");
  // The place comes from a route for the profile that is named, never from another profile's route.
  const other = chipView(route({ profile: "other-1", locality: "cloud", host: "api.example.org" }), [local, cloud], { profile: "other-1", model: "big-model" }, "local-1");
  assert.equal(other.name, "Local");
  assert.equal(other.where, "", "the route was for another profile");
  // A place that leaves the computer says so.
  const away = chipView(route({ profile: "other-1", locality: "cloud", host: "api.example.org", model: "big-model" }), [cloud], { profile: "other-1", model: "big-model" }, null);
  assert.deepEqual([away.where, away.leaves], ["cloud", true]);
  // The place is never guessed from a name, a preset or an address.
  const guess = chipView(null, [{ id: "x", name: "Cloud provider", model: "m", host: "api.example.org" }], nopin, null);
  assert.equal(guess.where, "");
});

test("the words of a turn: working, thinking, writing, the status of a message and the end row", () => {
  const state = (over = {}) => ({
    running: false, partial: { text: "", reasoning: "" }, messages: [], pending: [], ended: null, ...over,
  });
  assert.equal(statusWord(state({ running: true })), "working");
  assert.equal(statusWord(state({ running: true, partial: { text: "", reasoning: "hm" } })), "thinking");
  assert.equal(statusWord(state({ running: true, partial: { text: "Hi", reasoning: "hm" } })), "writing");
  assert.equal(statusWord(state({ partial: { text: "Hi", reasoning: "" } })), "");
  assert.deepEqual(["complete", "cancelled", "failed", "interrupted", "weird"].map(statusLabel), ["", "stopped", "failed", "interrupted", "unfinished"]);
  assert.deepEqual([roleWord("user"), roleWord("assistant")], ["you", "assistant"]);
  assert.deepEqual(["done", "failed", "cancelled", "limit_steps", "x"].map(outcomeWord), ["reply finished", "reply failed", "reply stopped", "reply stopped", "reply stopped"]);

  const ended = (end, error = null) => ({ end, error, rounds: 1, usage: { input_tokens: 0, output_tokens: 0 }, saved: true });
  const message = (role, status) => ({ id: "m", role, ts: 0, model: null, status, usage: null, parts: [] });
  assert.equal(endNote(state()), null, "no end");
  assert.equal(endNote(state({ ended: ended("done") })), null, "finished");
  // Stopped: the message says so when there is one, else the end row does.
  assert.equal(endNote(state({ ended: ended("cancelled"), messages: [message("assistant", "cancelled")] })), null);
  assert.deepEqual(endNote(state({ ended: ended("cancelled"), messages: [message("user", "complete")] })), { kind: "note", label: "stopped", text: "" });
  assert.deepEqual(endNote(state({ ended: ended("cancelled") })), { kind: "note", label: "stopped", text: "" });
  // Failed: the class in words and the core's text.
  assert.deepEqual(endNote(state({ ended: ended("failed", { class: "overload", text: "The provider is overloaded." }) })), {
    kind: "error", label: "failed · overload", text: "The provider is overloaded.",
  });
  assert.equal(endNote(state({ ended: ended("failed", { class: "rate_limit", text: "slow down" }) })).label, "failed · rate limit");
  assert.deepEqual(endNote(state({ ended: ended("failed") })), { kind: "error", label: "failed", text: "" });
  // Limits and unknown codes.
  const limits = ["limit_steps", "limit_tokens", "limit_tool_output", "limit_reply_size", "limit_repeated_call", "limit_tool_errors"];
  assert.deepEqual(limits.map((l) => endNote(state({ ended: ended(l) })).label), [
    "stopped · step limit", "stopped · token limit", "stopped · tool output limit", "stopped · reply size limit", "stopped · repeated call", "stopped · tool errors",
  ]);
  assert.equal(endNote(state({ ended: ended("brand_new_end") })).label, "stopped · brand new end");
  assert.equal(endNote(state({ ended: ended("x".repeat(100)) })).label, `stopped · ${"x".repeat(40)}`);
  assert.equal(endNote(state({ ended: ended("limit_steps"), messages: [message("assistant", "complete")] })).kind, "note", "a limit shows also after a message");
});

test("usage and counts are written compactly", () => {
  assert.equal(usageWords({ input_tokens: 11, output_tokens: 5 }), "in 11 · out 5");
  assert.equal(usageWords({ input_tokens: 1234, output_tokens: 340 }), "in 1.2k · out 340");
  assert.equal(usageWords({ input_tokens: 12345, output_tokens: 0 }), "in 12k · out 0");
  assert.equal(usageWords({ input_tokens: 1500000, output_tokens: 999 }), "in 1.5M · out 999");
  assert.equal(usageWords({ input_tokens: 999999, output_tokens: 1 }), "in 1M · out 1");
  assert.equal(usageWords(null), "");
  assert.equal(usageWords({ input_tokens: 0, output_tokens: 0 }), "", "zero means not reported");
  assert.equal(titleWords("  a\u0000b\u202e  c\n\td "), "a b c d");
  assert.equal(titleWords("x".repeat(200)).length, 80);
  assert.equal(titleWords(""), "");
});

test("a link is opened only when its address carries no data", () => {
  const web = (rest) => `${"https"}://${rest}`;
  // A plain address, with a path or a fragment, is a link.
  for (const ok of [web("example.org/docs"), web("example.org"), web("example.org/a/b.html#section"), web("example.org/#top"), `${"http"}://example.org/x`, "mailto:a@example.org"]) {
    assert.equal(linkKind(ok), "open", ok);
  }
  // A query string, a user name, a password: not a link.
  for (const data of [
    web("example.org/?d=SECRET"), web("example.org/a?x"), web("user@example.org/"), web("user:pass@example.org/"), web(":pass@example.org/"),
    "mailto:a@example.org?subject=x", "mailto:a@example.org?body=SECRET",
  ]) {
    assert.equal(linkKind(data), "inert", data);
  }
  // Another scheme, nothing, text that is no address, and too long an address.
  for (const other of ["javascript:alert(1)", "data:text/html;base64,AAAA", "file:///etc/passwd", "ftp://example.org/x", "tel:+41000", "", "not an address", "//example.org/x", "/relative/path", web(`example.org/${"a".repeat(3000)}`)]) {
    assert.equal(linkKind(other), "inert", other.slice(0, 40));
  }
  assert.equal(linkKind(web(`example.org/${"a".repeat(1900)}`)), "open", "a long address under the limit is still a link");
  assert.equal(linkKind(web("EXAMPLE.org/Docs")), "open");
});

test("the providers button says set up before there is a provider", () => {
  assert.equal(setupLabel(null, 0), "Providers…", "the list is not known yet");
  assert.equal(setupLabel([], 0), "Set up…", "a loaded empty list");
  assert.equal(setupLabel([entry("a")], 0), "Providers…");
  assert.equal(setupLabel([entry("a"), entry("b")], 1), "Providers… (1 to review)");
  assert.equal(setupLabel([entry("a"), entry("b"), entry("c")], 3), "Providers… (3 to review)");
  assert.equal(setupLabel([], 2), "Set up…", "a review count without a profile is an inconsistent call");
});

test("a turn that waits for a decision says so", () => {
  const state = (over = {}) => ({
    running: false, partial: { text: "", reasoning: "" }, messages: [], pending: [], ended: null, ...over,
  });
  const waiting = [{ conversation: "chat-1", call_id: "call_1", tool: "read_file" }];
  assert.equal(statusWord(state({ running: true, pending: waiting })), "waiting for your decision");
  assert.equal(statusWord(state({ running: true, pending: waiting, partial: { text: "Hi", reasoning: "hm" } })), "waiting for your decision");
  assert.equal(statusWord(state({ running: true })), "working");
  assert.equal(statusWord(state({ running: true, partial: { text: "Hi", reasoning: "" } })), "writing");
  assert.equal(statusWord(state({ running: false, pending: waiting })), "", "a turn that is not running says nothing");
});
