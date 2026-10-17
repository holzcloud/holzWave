// The pure half of the consent card (consentstate.ts): what the core's context answers are read as, which ids a send
// names, and the words that tell what would go out. No DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import {
  contextArguments, ctxButtonView, decideMode, heldOf, pickedOf, readContextState, readOffer, routeChange,
} from "../../dist/consentstate.js";

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

  assert.deepEqual(ctxButtonView("closed", 0, true), { text: "Context…", expanded: false, disabled: false });
  assert.deepEqual(ctxButtonView("offer", 1, true), { text: "Context · 1 item", expanded: true, disabled: false });
  assert.deepEqual(ctxButtonView("offer", 2, true), { text: "Context · 2 items", expanded: true, disabled: false });
  assert.deepEqual(ctxButtonView("offer", 0, true), { text: "Context · none", expanded: true, disabled: false });
  assert.deepEqual(ctxButtonView("pick", 0, true), { text: "Context…", expanded: true, disabled: false });
  assert.deepEqual(ctxButtonView("ask", 0, true), { text: "Context…", expanded: true, disabled: false });
  assert.deepEqual(ctxButtonView("closed", 0, false), { text: "Context…", expanded: false, disabled: true });
});
