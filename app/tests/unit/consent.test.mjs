// The controller of one chat's sharing (consent.ts): which step the card is at and the three commands that move it. A fake
// `invoke` records every call and a fake clock stands in for the time; no DOM and no bridge are needed.
import test from "node:test";
import assert from "node:assert/strict";
import { Consent } from "../../dist/consent.js";

const OFFER_ID = "0123456789abcdef0123456789abcdef";
const STATE = (extra = {}) => ({
  profile: "local-1", model: "llama3.2", host: "localhost:11434", locality: "loopback", required: true, shared: false,
  kinds: ["selection", "screen"], persist: false, ...extra,
});
const OFFER = (extra = {}) => ({
  offer: OFFER_ID, profile: "local-1", model: "llama3.2", host: "localhost:11434", locality: "loopback", ttl_seconds: 300,
  items: [{ id: 1, kind: "screen", label: "Screen of pane 1", bytes: 2457, findings: [] }], empty: ["selection"], bytes: 2457,
  persist: false, ...extra,
});
const ROUTE = { ask: false, pane: 1, profile: "local-1", model: null, entry: "local-1|llama3.2|localhost", settings: "ask|open" };

/** A controller over a fake core: `answers` maps a command to a function of its arguments. */
function make({ route = ROUTE, answers = {} } = {}) {
  const calls = [];
  const said = [];
  const clock = { now: 0 };
  const current = { route: { ...route } };
  const defaults = {
    ai_context_state: () => STATE(),
    ai_context_share: (args) => STATE({ shared: args.shared }),
    ai_context_preview: () => OFFER(),
  };
  const consent = new Consent({
    invoke: async (cmd, args) => {
      calls.push([cmd, structuredClone(args)]);
      return (answers[cmd] ?? defaults[cmd])(args);
    },
    conversation: () => "chat-1",
    route: () => ({ ...current.route }),
    now: () => clock.now,
    say: (text, kind) => said.push([text, kind]),
  });
  return { consent, calls, said, clock, current };
}

test("opening asks the core once with ids only and the answer decides between asking and picking", async () => {
  const quiet = make();
  let changes = 0;
  quiet.consent.subscribe(() => (changes += 1));
  void quiet.consent.model;
  assert.deepEqual(quiet.calls, [], "constructing, subscribing and reading ask nothing");
  assert.equal(changes, 0);
  assert.equal(quiet.consent.model.mode, "closed");

  const ask = make();
  await ask.consent.open();
  assert.equal(ask.calls.length, 1);
  assert.deepEqual(ask.calls[0], ["ai_context_state", { conversation: "chat-1", pane: 1, profile: "local-1", model: null }]);
  assert.deepEqual(Object.keys(ask.calls[0][1]), ["conversation", "pane", "profile", "model"]);
  assert.equal(ask.consent.model.mode, "ask", "required and not shared");
  assert.equal(ask.consent.model.pane, 1);

  const shared = make({ answers: { ai_context_state: () => STATE({ shared: true }) } });
  await shared.consent.open();
  assert.equal(shared.consent.model.mode, "pick", "required and shared");

  const free = make({ answers: { ai_context_state: () => STATE({ required: false }) } });
  await free.consent.open();
  assert.equal(free.consent.model.mode, "pick", "not required");

  const none = make({ answers: { ai_context_state: () => STATE({ kinds: [] }) } });
  await none.consent.open();
  assert.equal(none.consent.model.mode, "blocked", "nothing the pane can offer");

  // A second open while the first is unanswered makes no second call.
  let release;
  const slow = make({ answers: { ai_context_state: () => new Promise((resolve) => (release = () => resolve(STATE()))) } });
  const first = slow.consent.open();
  await slow.consent.open();
  assert.equal(slow.calls.length, 1);
  assert.equal(slow.consent.model.mode, "busy");
  assert.equal(slow.consent.model.busy, "state");
  release();
  await first;
  assert.equal(slow.consent.model.mode, "ask");

  // A route without a pane asks nothing.
  const lost = make({ route: { ...ROUTE, pane: null } });
  await lost.consent.open();
  assert.deepEqual(lost.calls, []);
  assert.equal(lost.consent.model.mode, "closed");

  // An answer the page does not understand blocks with a fixed sentence.
  const odd = make({ answers: { ai_context_state: () => ({ shared: "yes" }) } });
  await odd.consent.open();
  assert.equal(odd.consent.model.mode, "blocked");
  assert.equal(odd.consent.model.fault.text, "The answer was not understood.");
});

test("sharing and previewing ask for ids and hold an offer only with a pick, and a send spends it", async () => {
  const { consent, calls, clock } = make();
  await consent.open();
  await consent.share();
  assert.deepEqual(calls[1], [
    "ai_context_share", { conversation: "chat-1", pane: 1, profile: "local-1", model: null, shared: true },
  ]);
  assert.equal(consent.model.mode, "pick");
  assert.equal(consent.model.state.shared, true);

  // Nothing is previewed until a kind is ticked; a kind the state does not list is not ticked.
  await consent.preview();
  assert.equal(calls.length, 2);
  consent.tick("file", true);
  assert.deepEqual(consent.model.ticked, []);
  consent.tick("screen", true);
  clock.now = 1000;
  await consent.preview();
  assert.deepEqual(calls[2], [
    "ai_context_preview", { conversation: "chat-1", pane: 1, profile: "local-1", model: null, kinds: ["screen"] },
  ]);
  assert.deepEqual(Object.keys(calls[2][1]), ["conversation", "pane", "profile", "model", "kinds"]);
  assert.equal(consent.model.mode, "offer");
  assert.equal(consent.model.offer.offer, OFFER_ID);
  assert.equal(consent.model.at, 1000);
  assert.deepEqual(consent.held(), { offer: OFFER_ID, picked: [1] });

  consent.toggleItem(1, false);
  assert.equal(consent.held(), null, "nothing ticked means a plain message");
  consent.toggleItem(1, true);
  assert.deepEqual(consent.held(), { offer: OFFER_ID, picked: [1] });
  consent.toggleItem(7, false);
  assert.deepEqual(consent.model.off, [], "an item the offer does not hold is ignored");

  consent.discard();
  assert.equal(consent.model.mode, "pick");
  assert.equal(consent.held(), null);
  await consent.preview();
  assert.deepEqual(consent.held(), { offer: OFFER_ID, picked: [1] });

  consent.sent();
  assert.equal(consent.model.mode, "closed");
  assert.equal(consent.model.offer, null);
  assert.equal(consent.held(), null);

  // Every argument object of the whole session names only ids, a flag and kinds.
  const allowed = ["conversation", "pane", "profile", "model", "shared", "kinds"];
  for (const [, args] of calls) for (const key of Object.keys(args)) assert.ok(allowed.includes(key), key);

  // Stopping the share drops an attached offer with it.
  const stop = make();
  await stop.consent.open();
  await stop.consent.share();
  stop.consent.tick("screen", true);
  await stop.consent.preview();
  await stop.consent.unshare();
  assert.deepEqual(stop.calls[stop.calls.length - 1][1].shared, false);
  assert.equal(stop.consent.model.offer, null);
  assert.equal(stop.consent.model.mode, "ask");
});

test("nothing is asked at construction, and no argument of any command is text", async () => {
  const { consent, calls } = make();
  await consent.open();
  await consent.share();
  consent.tick("selection", true);
  consent.tick("screen", true);
  await consent.preview();
  consent.toggleItem(1, false);
  consent.toggleItem(1, true);
  consent.close();
  await consent.open();
  assert.ok(calls.length >= 4);
  for (const [cmd, args] of calls) {
    assert.ok(cmd.startsWith("ai_context_"), cmd);
    for (const key of Object.keys(args)) {
      for (const bad of ["text", "content", "context", "body", "envelope", "offer"]) assert.notEqual(key, bad, `${cmd} has ${key}`);
    }
    assert.equal(args.pane, 1, "the pane of the call is the pane of the route");
  }
  assert.equal(consent.model.pane, 1);
  assert.deepEqual(calls[2][1].kinds, ["selection", "screen"], "the kinds go in the order the state lists them");
});
