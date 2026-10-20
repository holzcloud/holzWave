// The pure half of the consent card (consentstate.ts): what the core's context answers are read as, which ids a send
// names, and the words that tell what would go out. No DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { localityWord } from "../../dist/chatview.js";
import {
  BEST_EFFORT, KIND_WORDS, NOTES, REASON_WORDS, RULE_WORDS, cardTitle, contextArguments, contextFault, autoButtonView, decideMode,
  destinationWords, findingWords, heldOf, paneWords, persistWords, pickedOf, readContextState, readOffer, routeChange, sentLine,
  ruleTag, sizeWords, tokenWordsOfBytes, totalWords, validForWords,
} from "../../dist/consentstate.js";

const GOLDEN = JSON.parse(readFileSync(new URL("../fixtures/ai_consent.json", import.meta.url), "utf8"));
const PROTOCOL = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));

const STATE = {
  profile: "local-1", model: "llama3.2", host: "localhost:11434", locality: "loopback", required: true, shared: false,
  kinds: ["selection", "screen"], persist: false,
};
const ITEM = { id: 1, kind: "screen", label: "Screen of pane 1", bytes: 2457, findings: [{ rule: "anthropic_key", count: 1 }] };
const OFFER = {
  offer: "0123456789abcdef0123456789abcdef", profile: "local-1", model: "llama3.2", host: "localhost:11434", locality: "loopback",
  ttl_seconds: 300, items: [ITEM], empty: ["selection"], bytes: 2457, persist: false,
};
const item = (id, extra = {}) => ({ ...ITEM, id, label: `Item ${id}`, ...extra });

test("the context state and the offer are read member by member and no text enters", () => {
  const read = readContextState(STATE);
  assert.deepEqual(Object.keys(read), ["profile", "model", "host", "locality", "required", "shared", "kinds", "persist"]);
  assert.deepEqual(read, STATE);

  // A member that is not named never reaches the page, and an own `__proto__` is never copied.
  const hostile = JSON.parse(
    '{"profile":"p","model":"m","host":"h","locality":"cloud","required":true,"shared":true,"kinds":["screen"],"persist":false,' +
      '"text":"zq-consent-text-71b9e4","content":"c","envelope":"e","api_key":"k","__proto__":{"text":"t"}}',
  );
  const copy = readContextState(hostile);
  assert.deepEqual(Object.keys(copy), ["profile", "model", "host", "locality", "required", "shared", "kinds", "persist"]);
  const text = JSON.stringify(copy);
  for (const name of ["text", "content", "envelope", "api_key", "zq-consent-text-71b9e4", "__proto__"]) assert.ok(!text.includes(name), name);
  assert.equal(Object.getPrototypeOf(copy), Object.prototype);

  // Kinds that are unknown or repeated are dropped, and more than five are cut.
  assert.deepEqual(readContextState({ ...STATE, kinds: ["screen", "bogus", "screen", "file", 4, null] }).kinds, ["screen", "file"]);
  assert.deepEqual(
    readContextState({ ...STATE, kinds: ["selection", "screen", "last_command", "file", "preview", "selection", "screen"] }).kinds,
    ["selection", "screen", "last_command", "file", "preview"],
  );
  // Fail closed: a flag that is no boolean, no object, an array, kinds that are no list.
  for (const bad of [null, undefined, 4, "x", [], [STATE]]) assert.equal(readContextState(bad), null);
  for (const name of ["required", "shared", "persist"]) {
    for (const value of [1, "true", null, undefined]) assert.equal(readContextState({ ...STATE, [name]: value }), null, `${name} ${value}`);
  }
  assert.equal(readContextState({ ...STATE, kinds: "screen" }), null);
  assert.equal(readContextState({ ...STATE, host: 3 }), null);
  // The host loses control, bidirectional and zero-width characters and is cut at 253 characters.
  assert.equal(readContextState({ ...STATE, host: "lo\u0000cal‮host​:1" }).host, "localhost:1");
  assert.equal(Array.from(readContextState({ ...STATE, host: "h".repeat(400) }).host).length, 253);

  // The offer: exactly ten members.
  const offer = readOffer(OFFER);
  assert.deepEqual(Object.keys(offer), [
    "offer", "profile", "model", "host", "locality", "ttl_seconds", "items", "empty", "bytes", "persist",
  ]);
  assert.deepEqual(offer, OFFER);
  assert.deepEqual(Object.keys(offer.items[0]), ["id", "kind", "label", "bytes", "findings"]);
  const withText = readOffer({ ...OFFER, text: "t", excerpt: "e", items: [{ ...ITEM, text: "zq-consent-text-71b9e4" }] });
  assert.ok(!JSON.stringify(withText).includes("zq-consent-text-71b9e4"));
  assert.ok(!("text" in withText) && !("excerpt" in withText));

  // The offer id is 32 lower-case hex characters.
  for (const id of ["", "0123456789abcdef0123456789abcde", "0123456789abcdef0123456789abcdef0", "0123456789ABCDEF0123456789ABCDEF", "0123456789abcdef0123456789abcdeg", " 0123456789abcdef0123456789abcde", "0123456789abcdef0123456789abcdef\n", 7, null]) {
    assert.equal(readOffer({ ...OFFER, offer: id }), null, JSON.stringify(id));
  }
  // The time must be an integer from 1 to 3600.
  for (const ttl of [0, -1, 3601, 1.5, "300", null]) assert.equal(readOffer({ ...OFFER, ttl_seconds: ttl }), null, String(ttl));
  assert.equal(readOffer({ ...OFFER, ttl_seconds: 3600 }).ttl_seconds, 3600);

  // At most 8 items; each with exactly its members; ids unique integers from 1 to 8.
  const eight = Array.from({ length: 8 }, (_, i) => item(i + 1));
  assert.equal(readOffer({ ...OFFER, items: eight }).items.length, 8);
  assert.equal(readOffer({ ...OFFER, items: [...eight, item(9)] }), null, "nine items");
  assert.equal(readOffer({ ...OFFER, items: [] }), null, "no item");
  assert.equal(readOffer({ ...OFFER, items: [item(1), item(1)] }), null, "a repeated id");
  for (const bad of [0, 9, 1.5, "1", null]) assert.equal(readOffer({ ...OFFER, items: [item(1, { id: bad })] }), null, `id ${bad}`);
  assert.equal(readOffer({ ...OFFER, items: [item(1, { kind: "bogus" })] }), null);
  for (const bad of [-1, 1.5, 4294967296, "7", null]) assert.equal(readOffer({ ...OFFER, items: [item(1, { bytes: bad })] }), null, `bytes ${bad}`);
  assert.equal(readOffer({ ...OFFER, items: [item(1, { bytes: 4294967295 })] }).items[0].bytes, 4294967295);
  assert.equal(readOffer({ ...OFFER, items: [item(1, { label: 4 })] }), null);
  assert.equal(readOffer({ ...OFFER, items: [item(1, { findings: "x" })] }), null);
  assert.equal(readOffer({ ...OFFER, items: [item(1, { findings: [{ rule: "jwt" }] })] }), null);
  assert.equal(readOffer({ ...OFFER, items: [item(1, { findings: [{ rule: "jwt", count: 1.5 }] })] }), null);
  // Any malformed item gives null for the whole offer, even beside a good one.
  assert.equal(readOffer({ ...OFFER, items: [item(1), item(2, { bytes: "x" })] }), null);

  // At most 22 findings of a rule (cut at 40 characters) and an integer count.
  const rules = Array.from({ length: 22 }, (_, i) => ({ rule: `rule_${i}`, count: i }));
  assert.equal(readOffer({ ...OFFER, items: [item(1, { findings: rules })] }).items[0].findings.length, 22);
  assert.equal(readOffer({ ...OFFER, items: [item(1, { findings: [...rules, { rule: "x", count: 1 }] })] }), null);
  assert.equal(readOffer({ ...OFFER, items: [item(1, { findings: [{ rule: "r".repeat(90), count: 2 }] })] }).items[0].findings[0].rule.length, 40);

  // A label is cleaned, collapsed and cut at 200 characters with an ellipsis; markup stays a plain string.
  assert.equal(readOffer({ ...OFFER, items: [item(1, { label: "  Screen‮ of​   pane\n1\t" })] }).items[0].label, "Screen of pane 1");
  const long = readOffer({ ...OFFER, items: [item(1, { label: "x".repeat(400) })] }).items[0].label;
  assert.equal(Array.from(long).length, 200);
  assert.ok(long.endsWith("…"));
  const markup = "zq-label-<img src=x onerror=alert(1)>-8c3f";
  assert.equal(readOffer({ ...OFFER, items: [item(1, { label: markup })] }).items[0].label, markup);

  // `empty` keeps the known kinds only.
  assert.deepEqual(readOffer({ ...OFFER, empty: ["selection", "bogus", "selection", 3] }).empty, ["selection"]);
  assert.equal(readOffer({ ...OFFER, empty: "selection" }), null);
  for (const bad of [null, [], "x", 3]) assert.equal(readOffer(bad), null);
});

test("the arguments of a send are the five, and the offer and the picks only together", () => {
  const offer = readOffer({ ...OFFER, items: [item(1), item(2), item(3)] });
  assert.deepEqual(pickedOf(offer, new Set([2])), [1, 3]);
  assert.deepEqual(pickedOf(offer, [3, 1]), [2]);
  assert.deepEqual(pickedOf(offer, new Set()), [1, 2, 3]);
  assert.deepEqual(pickedOf(offer, new Set([1, 2, 3])), []);
  // The numbers come in ascending order whatever the order of the items.
  const shuffled = readOffer({ ...OFFER, items: [item(3), item(1), item(2)] });
  assert.deepEqual(pickedOf(shuffled, new Set()), [1, 2, 3]);

  assert.equal(heldOf(offer, new Set([1, 2, 3])), null, "nothing picked");
  assert.deepEqual(heldOf(offer, new Set([2])), { offer: OFFER.offer, picked: [1, 3] });

  assert.deepEqual(contextArguments(null), {});
  const args = contextArguments({ offer: OFFER.offer, picked: [1, 3] });
  assert.deepEqual(Object.keys(args), ["offer", "picked"]);
  assert.deepEqual(args, { offer: OFFER.offer, picked: [1, 3] });
  // Both or neither: a pick list that is empty, too long, no integers or not ascending gives none.
  for (const picked of [[], [1, 2, 3, 4, 5, 6, 7, 8, 9], [1.5], [0], [-1], [2, 1], [1, 1], ["1"], [1, null]]) {
    assert.deepEqual(contextArguments({ offer: OFFER.offer, picked }), {}, JSON.stringify(picked));
  }
  assert.deepEqual(contextArguments({ offer: "not an offer id", picked: [1] }), {});
  assert.deepEqual(contextArguments({ offer: OFFER.offer, picked: [1, 2, 3, 4, 5, 6, 7, 8] }).picked.length, 8);
  // The result is a copy: changing it changes nothing held.
  const held = { offer: OFFER.offer, picked: [1] };
  contextArguments(held).picked.push(2);
  assert.deepEqual(held.picked, [1]);
});

const ROUTE = { ask: false, pane: 1, profile: "local-1", model: null, entry: "local-1|llama3.2|localhost", settings: "ask|open" };

test("a route change is the pane, the provider, the saved address or a privacy setting, and nothing else", () => {
  const same = (extra) => routeChange(ROUTE, { ...ROUTE, ...extra });
  assert.equal(same({}), "same");
  assert.equal(same({ ask: true }), "same", "`ask` is not part of the comparison");
  assert.equal(same({ pane: 2 }), "pane");
  assert.equal(same({ pane: null }), "pane", "a pane that became none counts");
  assert.equal(routeChange({ ...ROUTE, pane: null }, ROUTE), "pane");
  assert.equal(same({ profile: "other-1" }), "provider");
  assert.equal(same({ profile: null }), "provider");
  assert.equal(same({ model: "big-model" }), "provider");
  assert.equal(same({ entry: "local-1|llama3.2|elsewhere" }), "provider", "the saved address or model changed");
  assert.equal(same({ settings: "auto|open" }), "settings");
  assert.equal(same({ settings: "" }), "settings");
  // The entry was not known before and is known now: the list was read, nothing changed.
  assert.equal(routeChange({ ...ROUTE, entry: null }, ROUTE), "adopt");
  assert.equal(routeChange({ ...ROUTE, entry: null }, { ...ROUTE, entry: null }), "same");
  assert.equal(routeChange(ROUTE, { ...ROUTE, entry: null }), "same", "an entry that went unknown is no change by itself");
  // Precedence: the pane, then the provider, then the settings, then adopting.
  assert.equal(same({ pane: 2, profile: "other-1", settings: "x" }), "pane");
  assert.equal(same({ profile: "other-1", settings: "x" }), "provider");
  assert.equal(routeChange({ ...ROUTE, entry: null }, { ...ROUTE, settings: "x" }), "settings");
  assert.equal(routeChange({ ...ROUTE, entry: null }, { ...ROUTE, profile: "other-1" }), "provider");
});

test("the mode follows the core's answer", () => {
  const state = (extra) => ({ ...readContextState({ ...STATE }), ...extra });
  assert.equal(decideMode(state({ kinds: [] })), "blocked");
  assert.equal(decideMode(state({ required: true, shared: false })), "ask");
  assert.equal(decideMode(state({ required: true, shared: true })), "pick");
  assert.equal(decideMode(state({ required: false, shared: false })), "pick");
  assert.equal(decideMode(state({ required: false, shared: true })), "pick");
  assert.equal(decideMode(state({ kinds: [], required: false })), "blocked");
});

test("the Auto toggle reads its state in words and a dot", () => {
  assert.deepEqual(autoButtonView(true, 2), {
    text: "● Auto · pane 2", label: "Auto · pane 2", pressed: true,
    title: "Every message carries what pane 2 shows. Press to switch off.",
  });
  assert.deepEqual(autoButtonView(true, null), {
    text: "● Auto · no pane", label: "Auto · no pane", pressed: true,
    title: "Focus a terminal or preview pane to send it with your messages. Press to switch off.",
  });
  assert.deepEqual(autoButtonView(false, 2), {
    text: "○ Auto", label: "Auto", pressed: false, title: "Messages go without a pane. Press to switch on.",
  });
  assert.deepEqual(autoButtonView(false, null), autoButtonView(false, 2));
  for (const bad of [0, -1, 1.5, Number.NaN, "2", undefined]) {
    assert.equal(autoButtonView(true, bad).text, "● Auto · no pane", `pane ${String(bad)}`);
  }
  for (const view of [autoButtonView(true, 1), autoButtonView(true, null), autoButtonView(false, 1)]) {
    assert.ok(view.text.endsWith(view.label), "the accessible name is the visible words");
    assert.ok(!/[●○]/u.test(view.label), "the name has no symbol");
    assert.ok(view.title.endsWith("."), view.title);
  }
});

test("the word tables are the core's tables", () => {
  assert.deepEqual(Object.keys(RULE_WORDS), GOLDEN.rules, "the rule names, in the core's order");
  assert.deepEqual(Object.keys(REASON_WORDS), GOLDEN.reasons, "the reason names, in the core's order");
  assert.deepEqual(Object.keys(KIND_WORDS), PROTOCOL.ops.context_kind, "the five kinds");
  assert.equal(GOLDEN.rules.length, 22);
  assert.equal(GOLDEN.reasons.length, 13);
  for (const place of GOLDEN.localities) assert.notEqual(localityWord(place), "unknown place", place);
  assert.deepEqual(GOLDEN.localities.map(localityWord), ["on device", "this computer", "local network", "cloud"]);
  for (const other of ["mars", "", "Cloud", null, undefined, 4]) assert.equal(localityWord(other), "unknown place");

  for (const [rule, row] of Object.entries(RULE_WORDS)) {
    assert.ok(["chars", "times", "none"].includes(row.unit), `${rule} unit`);
    assert.ok(["warn", "plain"].includes(row.tone), `${rule} tone`);
    assert.ok(row.words.length >= 3 && !row.words.endsWith("."), `${rule}: ${row.words}`);
    assert.ok(!/[<>&"`]|:\/\//.test(row.words), `${rule}: no markup and no address`);
  }
  for (const rule of ["hidden_text", "bidi_control", "zero_width", "tag_chars"]) assert.equal(RULE_WORDS[rule].tone, "warn", rule);
  assert.equal(RULE_WORDS.truncated.unit, "none");
  for (const [reason, sentence] of Object.entries(REASON_WORDS)) {
    assert.ok(sentence.length >= 10 && sentence.endsWith("."), `${reason}: ${sentence}`);
    assert.ok(!/[<>&`]|:\/\//.test(sentence), `${reason}: no markup and no address`);
  }
  for (const word of Object.values(KIND_WORDS)) assert.ok(word.length >= 3);
  assert.equal(Object.keys(NOTES).length, 10);
});

test("a finding, a size, a token count and a destination are told in words", () => {
  assert.equal(findingWords({ rule: "hidden_text", count: 15 }), "hidden text removed (15 characters)");
  assert.equal(findingWords({ rule: "anthropic_key", count: 1 }), "Anthropic key hidden");
  assert.equal(findingWords({ rule: "anthropic_key", count: 3 }), "Anthropic key hidden ×3");
  assert.equal(findingWords({ rule: "truncated", count: 7 }), "cut to fit the size limit");
  assert.equal(findingWords({ rule: "control_chars", count: 1 }), "control characters removed (1 character)");
  assert.equal(findingWords({ rule: "from_a_newer_core", count: 1 }), "other change", "an unknown rule is never dropped");
  assert.equal(findingWords({ rule: "from_a_newer_core", count: 7 }), "other change ×7");
  assert.equal(findingWords({ rule: "constructor", count: 2 }), "other change ×2", "a name of the prototype is no rule");
  for (const rule of GOLDEN.rules) assert.ok(findingWords({ rule, count: 2 }).length >= 3, rule);

  assert.deepEqual([0, 812, 1024, 2457, 131072, 1048576].map(sizeWords), ["0 B", "812 B", "1.0 KiB", "2.4 KiB", "128 KiB", "1.0 MiB"]);
  assert.deepEqual([-1, 1.5, "7", null, Number.NaN].map(sizeWords), ["0 B", "0 B", "0 B", "0 B", "0 B"]);
  assert.deepEqual([3, 6, 3000, 4500, 0].map(tokenWordsOfBytes), ["≈ 1 token", "≈ 2 tokens", "≈ 1k tokens", "≈ 1.5k tokens", ""]);
  assert.equal(tokenWordsOfBytes(2457), "≈ 819 tokens");
  assert.equal(totalWords(1, 2457), "1 item · 2.4 KiB · ≈ 819 tokens");
  assert.equal(totalWords(0, 0), "0 items · 0 B");
  assert.equal(totalWords(2, 3400), "2 items · 3.3 KiB · ≈ 1.1k tokens");

  assert.deepEqual(destinationWords({ host: "localhost:11434", locality: "loopback" }), { host: "localhost:11434", place: "this computer", leaves: false });
  assert.deepEqual(destinationWords({ host: "api.example.org", locality: "cloud" }), { host: "api.example.org", place: "cloud", leaves: true });
  assert.deepEqual(destinationWords({ host: "nas.local", locality: "lan" }), { host: "nas.local", place: "local network", leaves: true });
  assert.deepEqual(destinationWords({ host: "h", locality: "on_device" }), { host: "h", place: "on device", leaves: false });
  assert.deepEqual(destinationWords({ host: "h", locality: "mars" }), { host: "h", place: "unknown place", leaves: false });
  assert.equal(persistWords(false), "This chat keeps a note of what was sent (labels and sizes), not the text.");
  assert.equal(persistWords(true), "This chat keeps the text that was sent, on this computer.");
  assert.equal(validForWords(300), "Valid for 5 minutes.");
  assert.equal(validForWords(60), "Valid for 1 minute.");
  assert.equal(validForWords(90), "Valid for 90 seconds.");
  assert.equal(validForWords(1), "Valid for 1 second.");

  assert.equal(paneWords(1, "shell"), "pane 1 · shell");
  assert.equal(paneWords(2, ""), "pane 2");
  assert.equal(paneWords(2, "  a\u202Eb\n c "), "pane 2 · ab c");
  assert.equal(Array.from(paneWords(3, "x".repeat(90))).length, "pane 3 · ".length + 40);
  assert.equal(cardTitle("ask", null, "pane 1"), "Consent needed");
  assert.equal(cardTitle("offer", null, "pane 1"), "Goes out with your next message");
  assert.equal(cardTitle("blocked", null, "pane 1"), "Cannot share");
  assert.equal(cardTitle("busy", null, "pane 1", "preview"), "Reading…");
  assert.equal(cardTitle("busy", null, "pane 1", "state"), "Asking…");
  assert.equal(cardTitle("pick", readContextState({ ...STATE, shared: true }), "pane 1 · shell"), "Shared · pane 1 · shell");
  assert.equal(cardTitle("pick", readContextState({ ...STATE, required: false }), "pane 1 · shell"), "Pane 1 · shell");

  assert.equal(
    sentLine({ items: [{ label: "Screen of pane 1", bytes: 2457 }], locality: "loopback" }),
    "Context sent: Screen of pane 1 (2.4 KiB) → this computer",
  );
  assert.equal(
    sentLine({ items: [{ label: "A", bytes: 812 }, { label: "B", bytes: 1048576 }], locality: "cloud" }),
    "Context sent: A (812 B); B (1.0 MiB) → cloud",
  );
  assert.ok(sentLine({ items: [{ label: "A", bytes: 1 }], locality: "mars" }).endsWith("→ unknown place"));
});

test("a refusal is told by its reason, falls back to the core's own text, and the notes never promise", () => {
  assert.deepEqual(
    contextFault({ code: "context", reason: "local_only", text: "x" }),
    { code: "context", reason: "local_only", text: "Local only is on, and this provider is not on this computer." },
  );
  for (const reason of GOLDEN.reasons) {
    const fault = contextFault({ code: "context", reason, text: "the core's words" });
    assert.equal(fault.text, REASON_WORDS[reason], reason);
    assert.equal(fault.reason, reason);
  }
  // A reason the table does not know shows the core's text, cleaned and cut at 300 characters.
  const unknown = contextFault({ code: "context", reason: "from_a_newer_core", text: `a\u0000b${"x".repeat(400)}` });
  assert.equal(unknown.reason, "from_a_newer_core");
  assert.ok(unknown.text.startsWith("ab") && Array.from(unknown.text).length === 300);
  assert.equal(contextFault({ code: "context", reason: "constructor", text: "own words" }).text, "own words");
  // A code other than `context` is the core's text, whatever the reason.
  assert.deepEqual(contextFault({ code: "no_such_pane", reason: "denied", text: "no such pane" }), { code: "no_such_pane", reason: null, text: "no such pane" });
  assert.deepEqual(contextFault("a bare string"), { code: null, reason: null, text: "a bare string" });
  assert.equal(contextFault(null).text, "the request was refused");
  assert.match(contextFault({ code: "context_duplicate", reason: null, text: "twice" }).text, /already sent in this chat/);

  assert.ok(BEST_EFFORT.includes("Best effort, not a guarantee"));
  for (const text of [BEST_EFFORT, ...Object.values(NOTES), ...Object.values(REASON_WORDS)]) {
    assert.ok(!/never|always safe|guaranteed|no secrets/i.test(text.replace("not a guarantee", "")), text);
  }
  for (const text of [...Object.values(NOTES), ...Object.values(REASON_WORDS)]) assert.ok(!/never sent/i.test(text), text);
});

test("a rule name reaches an attribute only when the page knows it", () => {
  // The card writes the rule of a finding into an attribute for the stylesheet and the tests; a name the page does not know
  // is shown as `other change` and is never copied into an attribute.
  for (const rule of Object.keys(RULE_WORDS)) assert.equal(ruleTag(rule), rule);
  for (const rule of ['<img src=x onerror="x">', "__proto__", "constructor", "", "a\u0000b", "javascript:x", "anthropic_key "]) {
    assert.equal(ruleTag(rule), "other", JSON.stringify(rule));
  }
});
