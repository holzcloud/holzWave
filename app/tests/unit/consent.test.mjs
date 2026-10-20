// The controller of one chat's sharing (consent.ts): which step the card is at and the three commands that move it. A fake
// `invoke` records every call and a fake clock stands in for the time; no DOM and no bridge are needed.
import test from "node:test";
import assert from "node:assert/strict";
import { Consent } from "../../dist/consent.js";
import { REASON_WORDS } from "../../dist/consentstate.js";

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

const NOTE = {
  provider: "Sharing ended because the provider changed. Share again to attach context.",
  pane: "The chat now acts for another pane. The attached context was dropped.",
  settings: "A privacy setting changed. Add the context again.",
};

/** A controller with an offer attached for the screen of pane 1. */
async function withOffer(options) {
  const made = make(options);
  await made.consent.open();
  await made.consent.share();
  made.consent.tick("screen", true);
  await made.consent.preview();
  assert.equal(made.consent.model.mode, "offer");
  return made;
}

test("a change of provider, model, pane or address ends the share and the offer and says why", async () => {
  const cases = [
    [{ profile: "other-1" }, NOTE.provider],
    [{ model: "big-model" }, NOTE.provider],
    [{ entry: "local-1|llama3.2|elsewhere" }, NOTE.provider],
    [{ pane: 2 }, NOTE.pane],
    [{ pane: null }, NOTE.pane],
    [{ settings: "auto|open" }, NOTE.settings],
  ];
  for (const [change, note] of cases) {
    const { consent, calls, said, current } = await withOffer();
    const before = calls.length;
    Object.assign(current.route, change);
    consent.check();
    assert.equal(consent.model.mode, "closed", JSON.stringify(change));
    assert.equal(consent.model.offer, null);
    assert.equal(consent.held(), null);
    assert.deepEqual(said, [[note, "note"]], JSON.stringify(change));
    assert.equal(calls.length, before, "check() asks the core nothing");
    // The next open asks the core again with the new arguments.
    if (current.route.pane !== null) {
      await consent.open();
      assert.equal(calls.length, before + 1);
      const args = calls[calls.length - 1][1];
      assert.equal(args.pane, current.route.pane);
      assert.equal(args.profile, current.route.profile);
      assert.equal(args.model, current.route.model);
    }
  }

  // The list was read: the entry is adopted, nothing is dropped and nothing is said.
  const adopt = await withOffer({ route: { ...ROUTE, entry: null } });
  adopt.current.route.entry = "local-1|llama3.2|localhost";
  adopt.consent.check();
  assert.equal(adopt.consent.model.mode, "offer");
  assert.deepEqual(adopt.said, []);
  adopt.current.route.entry = "local-1|llama3.2|elsewhere";
  adopt.consent.check();
  assert.equal(adopt.consent.model.mode, "closed", "the adopted entry is the one later compared");

  // Nothing open, nothing shared: nothing is said. A share the page knows of is told when the provider changes.
  const idle = make();
  idle.current.route.profile = "other-1";
  idle.consent.check();
  assert.deepEqual(idle.said, []);
  const quiet = make();
  await quiet.consent.open();
  quiet.consent.close();
  quiet.current.route.profile = "other-1";
  quiet.consent.check();
  assert.deepEqual(quiet.said, [], "asked but never shared");
  const shared = make({ answers: { ai_context_state: () => STATE({ shared: true }) } });
  await shared.consent.open();
  shared.consent.close();
  shared.current.route.profile = "other-1";
  shared.consent.check();
  assert.deepEqual(shared.said, [[NOTE.provider, "note"]], "a share was known");
  // A pane change with a share but no offer says nothing: the first pane stays shared in the core.
  const pane = make({ answers: { ai_context_state: () => STATE({ shared: true }) } });
  await pane.consent.open();
  pane.current.route.pane = 2;
  pane.consent.check();
  assert.deepEqual(pane.said, []);
  assert.equal(pane.consent.model.mode, "closed");
});

test("a refusal moves the card to the step it names", async () => {
  const refuse = (reason, text = "x", code = "context") => () => {
    throw { code, reason, text };
  };
  for (const reason of ["local_only", "destination", "nothing_to_share", "pane_gone"]) {
    for (const cmd of ["ai_context_state", "ai_context_share"]) {
      const made = make({ answers: { [cmd]: refuse(reason, "the core's words") } });
      await made.consent.open();
      if (cmd === "ai_context_share") await made.consent.share();
      assert.equal(made.consent.model.mode, "blocked", `${cmd} ${reason}`);
      assert.deepEqual(made.consent.model.fault, { reason, text: REASON_WORDS[reason] }, "the page's words for the reason");
    }
  }
  // A code other than `context` is blocked with the core's text and no reason.
  for (const code of ["no_such_pane", "no_profile"]) {
    const made = make({ answers: { ai_context_state: refuse(null, "no such thing", code) } });
    await made.consent.open();
    assert.equal(made.consent.model.mode, "blocked");
    assert.deepEqual(made.consent.model.fault, { reason: null, text: "no such thing" });
  }
  // From a preview: a refused read leaves the pick list with the words; the state is asked once more for `not_shared`.
  for (const reason of ["bad_kinds", "unsupported_kind", "busy", "denied", "not_text", "unreadable"]) {
    const made = make({ answers: { ai_context_preview: refuse(reason, "words") } });
    await made.consent.open();
    await made.consent.share();
    made.consent.tick("screen", true);
    await made.consent.preview();
    assert.equal(made.consent.model.mode, "pick", reason);
    assert.deepEqual(made.consent.model.fault, { reason, text: REASON_WORDS[reason] });
    assert.deepEqual(made.consent.model.ticked, ["screen"], "the ticks stay");
    assert.equal(made.calls.filter(([cmd]) => cmd === "ai_context_state").length, 1);
  }
  for (const reason of ["local_only", "nothing_to_share", "pane_gone"]) {
    const made = make({ answers: { ai_context_preview: refuse(reason) } });
    await made.consent.open();
    await made.consent.share();
    made.consent.tick("screen", true);
    await made.consent.preview();
    assert.equal(made.consent.model.mode, "blocked", reason);
  }
  const lost = make({ answers: { ai_context_preview: refuse("not_shared") } });
  await lost.consent.open();
  await lost.consent.share();
  lost.consent.tick("screen", true);
  await lost.consent.preview();
  assert.equal(lost.consent.model.mode, "ask", "the share ended: ask again");
  assert.equal(lost.calls.filter(([cmd]) => cmd === "ai_context_state").length, 2, "one second state call");
  assert.equal(lost.consent.model.fault, null);

  // A refusal of a send: an offer that is no longer good is dropped and told; any other refusal keeps it.
  for (const raw of [
    { code: "context", reason: "stale_offer", text: "old" },
    { code: "context", reason: "not_shared", text: "unshared" },
    { code: "context", reason: "bad_pick", text: "wrong pick" },
    { code: "context_duplicate", reason: null, text: "twice" },
  ]) {
    const { consent, said } = await withOffer();
    assert.equal(consent.refused(raw), true, JSON.stringify(raw));
    assert.equal(consent.model.mode, "closed");
    assert.equal(consent.model.offer, null);
    assert.equal(said.length, 1);
    assert.equal(said[0][1], "error");
    assert.ok(said[0][0].length >= 3);
  }
  for (const raw of [
    { code: "busy", reason: null, text: "a reply is running" },
    { code: "context", reason: "local_only", text: "no" },
    { code: "no_profile", reason: null, text: "none" },
    "a string",
    null,
  ]) {
    const { consent, said } = await withOffer();
    assert.equal(consent.refused(raw), false, JSON.stringify(raw));
    assert.equal(consent.model.mode, "offer", "the offer is kept");
    assert.deepEqual(said, []);
  }
});

test("an answer that arrives after the card closed or the route changed is dropped, and one command runs at a time", async () => {
  /** A fake core whose answers are released by the test. */
  const held = [];
  const gate = (value) => () => new Promise((resolve) => held.push(() => resolve(value())));
  const slow = {
    ai_context_state: gate(() => STATE({ shared: true })),
    ai_context_share: gate(() => STATE({ shared: true })),
    ai_context_preview: gate(() => OFFER()),
  };
  const release = async () => {
    for (const go of held.splice(0)) go();
    await new Promise((done) => setImmediate(done));
  };

  // One at a time: a second open, share or preview while one is in flight makes no call.
  const one = make({ answers: slow });
  const opening = one.consent.open();
  await one.consent.open();
  await one.consent.share();
  await one.consent.preview();
  assert.equal(one.calls.length, 1);
  await release();
  await opening;
  one.consent.tick("screen", true);
  const sharing = one.consent.share();
  await one.consent.share();
  await one.consent.preview();
  assert.equal(one.calls.length, 2);
  await release();
  await sharing;

  // A close while the state is in flight: the answer arrives and the card stays closed.
  const closing = make({ answers: slow });
  const first = closing.consent.open();
  closing.consent.close();
  assert.equal(closing.consent.model.mode, "closed");
  await release();
  await first;
  assert.equal(closing.consent.model.mode, "closed");
  assert.equal(closing.consent.model.state, null, "the late answer was not taken");

  // A provider change while a preview is in flight: the offer that arrives later is dropped.
  const moving = make({ answers: { ...slow, ai_context_state: () => STATE({ shared: true }) } });
  await moving.consent.open();
  moving.consent.tick("screen", true);
  const reading = moving.consent.preview();
  assert.equal(moving.consent.model.mode, "busy");
  assert.equal(moving.consent.model.busy, "preview");
  moving.current.route.profile = "other-1";
  moving.consent.check();
  assert.deepEqual(moving.said, [[NOTE.provider, "note"]]);
  await release();
  await reading;
  assert.equal(moving.consent.model.offer, null);
  assert.equal(moving.consent.model.mode, "closed");
  assert.equal(moving.consent.held(), null);

  // A disposed controller ignores late answers and every call.
  const gone = make({ answers: slow });
  const last = gone.consent.open();
  gone.consent.dispose();
  await release();
  await last;
  assert.equal(gone.consent.model.state, null);
  await gone.consent.open();
  await gone.consent.share();
  assert.equal(gone.calls.length, 1);
});

test("an offer older than its time is not sent and a pick that was never previewed blocks the send", async () => {
  const { consent, clock } = await withOffer();
  assert.equal(consent.model.at, 0);
  clock.now = 299_000;
  assert.equal(consent.expired(), false);
  assert.deepEqual(consent.held(), { offer: OFFER_ID, picked: [1] }, "still good at 299 seconds");
  clock.now = 301_000;
  assert.equal(consent.expired(), true);
  assert.equal(consent.held(), null, "an expired offer is never held");
  consent.close();
  assert.equal(consent.expired(), false);

  // The sentences of a send that has to wait.
  const idle = make();
  assert.equal(idle.consent.blocksSend(), null, "closed");
  await idle.consent.open();
  assert.equal(idle.consent.model.mode, "ask");
  assert.equal(idle.consent.blocksSend(), null, "asking");
  await idle.consent.share();
  assert.equal(idle.consent.model.mode, "pick");
  assert.equal(idle.consent.blocksSend(), null, "nothing ticked");
  idle.consent.tick("screen", true);
  assert.equal(idle.consent.blocksSend(), "Preview what you ticked, or untick it, before sending.");
  idle.consent.tick("screen", false);
  assert.equal(idle.consent.blocksSend(), null);
  idle.consent.tick("screen", true);
  await idle.consent.preview();
  assert.equal(idle.consent.blocksSend(), null, "an offer is attached");

  let release;
  const busy = make({ answers: { ai_context_state: () => new Promise((resolve) => (release = () => resolve(STATE()))) } });
  const opening = busy.consent.open();
  assert.equal(busy.consent.blocksSend(), "Wait for the context card to finish.");
  release();
  await opening;
  assert.equal(busy.consent.blocksSend(), null);
});

test("a prefill ticks the kinds and previews at once when allowed, and waits for the share when not", async () => {
  // Shared: ticks `screen` only (the state lists selection and screen) and previews once.
  const ready = make({ answers: { ai_context_state: () => STATE({ shared: true }) } });
  await ready.consent.open(["screen", "file"]);
  assert.deepEqual(ready.calls.map(([cmd]) => cmd), ["ai_context_state", "ai_context_preview"]);
  assert.deepEqual(ready.calls[1][1].kinds, ["screen"]);
  assert.equal(ready.consent.model.mode, "offer");
  assert.deepEqual(ready.consent.model.ticked, ["screen"]);

  // Not required: the same without a share.
  const free = make({ answers: { ai_context_state: () => STATE({ required: false }) } });
  await free.consent.open(["screen"]);
  assert.deepEqual(free.calls.map(([cmd]) => cmd), ["ai_context_state", "ai_context_preview"]);

  // Asking: nothing is read, and the share is never made by the prefill. After the user shares, the kinds are read.
  const ask = make();
  await ask.consent.open(["screen", "selection"]);
  assert.deepEqual(ask.calls.map(([cmd]) => cmd), ["ai_context_state"]);
  assert.equal(ask.consent.model.mode, "ask");
  await ask.consent.share();
  assert.deepEqual(ask.calls.map(([cmd]) => cmd), ["ai_context_state", "ai_context_share", "ai_context_preview"]);
  assert.deepEqual(ask.calls[2][1].kinds, ["selection", "screen"], "in the order the state lists them");
  assert.equal(ask.consent.model.mode, "offer");

  // A prefill with no kind the state lists reads nothing.
  const none = make({ answers: { ai_context_state: () => STATE({ shared: true }) } });
  await none.consent.open(["file", "preview"]);
  assert.deepEqual(none.calls.map(([cmd]) => cmd), ["ai_context_state"]);
  assert.equal(none.consent.model.mode, "pick");
  assert.deepEqual(none.consent.model.ticked, []);

  // Closing the card before the share forgets the prefill.
  const closed = make();
  await closed.consent.open(["screen"]);
  closed.consent.close();
  await closed.consent.open();
  await closed.consent.share();
  assert.deepEqual(closed.calls.map(([cmd]) => cmd), ["ai_context_state", "ai_context_state", "ai_context_share"]);
});

test("a prefill in order of preference ticks only the first kind the state lists", async () => {
  const pick = (kinds, extra = {}) => make({ answers: { ai_context_state: () => STATE({ shared: true, kinds, ...extra }) } });

  const first = pick(["selection", "screen", "last_command"]);
  await first.consent.open(["last_command", "screen"], { first: true });
  assert.deepEqual(first.calls.map(([cmd]) => cmd), ["ai_context_state", "ai_context_preview"]);
  assert.deepEqual(first.calls[1][1].kinds, ["last_command"], "the first of the preference that is listed");
  assert.deepEqual(first.consent.model.ticked, ["last_command"]);

  const second = pick(["selection", "screen"]);
  await second.consent.open(["last_command", "screen"], { first: true });
  assert.deepEqual(second.calls[1][1].kinds, ["screen"]);
  assert.deepEqual(second.consent.model.ticked, ["screen"]);

  const none = pick(["selection"]);
  await none.consent.open(["last_command", "screen"], { first: true });
  assert.deepEqual(none.calls.map(([cmd]) => cmd), ["ai_context_state"], "nothing listed, nothing previewed");
  assert.deepEqual(none.consent.model.ticked, []);

  // Asking: nothing is previewed before the share; after it the first listed kind of the preference is read, not both.
  const all = ["selection", "screen", "last_command"];
  const ask = make({
    answers: { ai_context_state: () => STATE({ kinds: all }), ai_context_share: (args) => STATE({ kinds: all, shared: args.shared }) },
  });
  await ask.consent.open(["last_command", "screen"], { first: true });
  assert.deepEqual(ask.calls.map(([cmd]) => cmd), ["ai_context_state"]);
  assert.deepEqual(ask.consent.model.prefill, ["last_command", "screen"], "the order the caller gave is kept");
  await ask.consent.share();
  assert.deepEqual(ask.calls.map(([cmd]) => cmd), ["ai_context_state", "ai_context_share", "ai_context_preview"]);
  assert.deepEqual(ask.calls[2][1].kinds, ["last_command"]);

  // Without the option the old rule holds: every listed kind of the prefill is ticked, in the order of the state.
  const both = pick(["selection", "screen", "last_command"]);
  await both.consent.open(["last_command", "screen"]);
  assert.deepEqual(both.calls[1][1].kinds, ["screen", "last_command"]);
});

// ---- Auto: a send attaches the pane without a card (attachAll) ---------------------------------------------------------------

const KINDS3 = ["selection", "screen", "last_command"];
const SHARED_STATE = () => STATE({ shared: true, kinds: KINDS3 });

test("attachAll asks for the share once and makes no preview while consent is missing", async () => {
  const { consent, calls, said } = make({ answers: { ai_context_state: () => STATE({ kinds: KINDS3 }) } });
  const out = await consent.attachAll(1);
  assert.deepEqual(out, { kind: "ask" });
  assert.deepEqual(calls.map((c) => c[0]), ["ai_context_state"], "a state call only");
  assert.deepEqual(calls[0], ["ai_context_state", { conversation: "chat-1", pane: 1, profile: "local-1", model: null }]);
  assert.equal(consent.model.mode, "ask");
  assert.equal(consent.model.viaAuto, true);
  assert.equal(consent.model.pane, 1);
  assert.equal(consent.held(), null);
  assert.deepEqual(said, []);

  // A second send with the card still open asks the state again and keeps one card.
  assert.deepEqual(await consent.attachAll(1), { kind: "ask" });
  assert.equal(consent.model.mode, "ask");
  assert.deepEqual(calls.map((c) => c[0]), ["ai_context_state", "ai_context_state"]);
});

test("Share on a card that Auto opened closes it and says to press Send again; a card opened by hand is untouched", async () => {
  const auto = make({ answers: { ai_context_state: () => STATE({ kinds: KINDS3 }), ai_context_share: () => SHARED_STATE() } });
  await auto.consent.attachAll(1);
  await auto.consent.share();
  assert.equal(auto.consent.model.mode, "closed");
  assert.equal(auto.consent.model.viaAuto, false);
  assert.deepEqual(auto.said, [["Pane shared. Press Send again to send it along.", "note"]]);
  assert.equal(auto.calls.filter((c) => c[0] === "ai_context_share").length, 1);
  assert.equal(auto.calls.filter((c) => c[0] === "ai_context_preview").length, 0, "nothing is read by the share");

  const hand = make({ answers: { ai_context_share: () => SHARED_STATE() } });
  await hand.consent.open();
  assert.equal(hand.consent.model.viaAuto, false);
  await hand.consent.share();
  assert.equal(hand.consent.model.mode, "pick", "the card of open() stays");
  assert.deepEqual(hand.said, []);
});

test("attachAll on a shared pane reads every kind the state lists and answers what the send names", async () => {
  const { consent, calls } = make({
    answers: {
      ai_context_state: () => SHARED_STATE(),
      ai_context_preview: () =>
        OFFER({ items: [1, 2, 3].map((id) => ({ id, kind: KINDS3[id - 1], label: `Item ${id}`, bytes: 10, findings: [] })) }),
    },
  });
  const out = await consent.attachAll(1);
  assert.deepEqual(out, { kind: "attach", held: { offer: OFFER_ID, picked: [1, 2, 3] } });
  assert.deepEqual(calls.map((c) => c[0]), ["ai_context_state", "ai_context_preview"], "no share call");
  assert.deepEqual(calls[1], [
    "ai_context_preview", { conversation: "chat-1", pane: 1, profile: "local-1", model: null, kinds: KINDS3 },
  ]);
  assert.deepEqual(Object.keys(calls[1][1]), ["conversation", "pane", "profile", "model", "kinds"]);
  assert.equal(consent.model.mode, "closed", "no card");
  assert.equal(consent.held(), null, "held() answers only for a card opened by hand");
});

test("attachAll names the pane of the send in every call", async () => {
  const { consent, calls } = make({ answers: { ai_context_state: () => SHARED_STATE() } });
  await consent.attachAll(3);
  assert.deepEqual(calls.map((c) => c[1].pane), [3, 3]);
});

test("attachAll asks nothing about sharing where the core says it is not required", async () => {
  const { consent, calls } = make({ answers: { ai_context_state: () => STATE({ required: false, shared: false, kinds: KINDS3 }) } });
  const out = await consent.attachAll(1);
  assert.equal(out.kind, "attach");
  assert.deepEqual(calls.map((c) => c[0]), ["ai_context_state", "ai_context_preview"]);
  assert.equal(consent.model.mode, "closed");
});

test("attachAll answers none for nothing to share and ask for a share that ended", async () => {
  const empty = make({ answers: { ai_context_state: () => STATE({ kinds: [], shared: true }) } });
  assert.deepEqual(await empty.consent.attachAll(1), { kind: "none" });
  assert.deepEqual(empty.calls.map((c) => c[0]), ["ai_context_state"], "no preview without a kind");

  const nothing = make({
    answers: {
      ai_context_state: () => SHARED_STATE(),
      ai_context_preview: () => Promise.reject({ code: "context", reason: "nothing_to_share", text: "nothing" }),
    },
  });
  assert.deepEqual(await nothing.consent.attachAll(1), { kind: "none" });

  const ended = make({
    answers: {
      ai_context_state: () => SHARED_STATE(),
      ai_context_preview: () => Promise.reject({ code: "context", reason: "not_shared", text: "not shared" }),
    },
  });
  assert.deepEqual(await ended.consent.attachAll(1), { kind: "ask" });
  assert.equal(ended.consent.model.mode, "ask");
  assert.equal(ended.consent.model.viaAuto, true);
  assert.equal(ended.consent.model.state.shared, false);
});

test("attachAll turns any other refusal and an unreadable answer into a sentence and sends nothing", async () => {
  const busy = make({
    answers: {
      ai_context_state: () => SHARED_STATE(),
      ai_context_preview: () => Promise.reject({ code: "context", reason: "busy", text: "x" }),
    },
  });
  assert.deepEqual(await busy.consent.attachAll(1), { kind: "fault", text: REASON_WORDS.busy });

  const failed = make({ answers: { ai_context_state: () => Promise.reject({ code: "context", reason: "local_only", text: "x" }) } });
  assert.deepEqual(await failed.consent.attachAll(1), { kind: "fault", text: REASON_WORDS.local_only });

  const oddState = make({ answers: { ai_context_state: () => ({ shared: "yes" }) } });
  assert.deepEqual(await oddState.consent.attachAll(1), { kind: "fault", text: "The answer was not understood." });

  const oddOffer = make({ answers: { ai_context_state: () => SHARED_STATE(), ai_context_preview: () => ({ offer: "short" }) } });
  assert.deepEqual(await oddOffer.consent.attachAll(1), { kind: "fault", text: "The answer was not understood." });
});

test("attachAll leaves a card the person opened by hand alone and waits for a command in flight", async () => {
  const hand = make({ answers: { ai_context_state: () => STATE({ shared: true }) } });
  await hand.consent.open();
  assert.equal(hand.consent.model.mode, "pick");
  const before = hand.calls.length;
  assert.deepEqual(await hand.consent.attachAll(1), { kind: "none" });
  assert.equal(hand.calls.length, before, "no call");
  hand.consent.tick("screen", true);
  await hand.consent.preview();
  assert.equal(hand.consent.model.mode, "offer");
  const again = hand.calls.length;
  assert.deepEqual(await hand.consent.attachAll(1), { kind: "none" });
  assert.equal(hand.calls.length, again, "no call");

  let release;
  const slow = make({ answers: { ai_context_state: () => new Promise((resolve) => (release = () => resolve(STATE()))) } });
  const opening = slow.consent.open();
  assert.deepEqual(await slow.consent.attachAll(1), { kind: "fault", text: "Wait for the context card to finish." });
  assert.equal(slow.calls.length, 1);
  release();
  await opening;
});

test("closeAsk closes a card only when Auto opened it, and a state that needs no card drops the card of Auto", async () => {
  const auto = make({ answers: { ai_context_state: () => STATE({ kinds: KINDS3 }) } });
  await auto.consent.attachAll(1);
  assert.equal(auto.consent.model.mode, "ask");
  auto.consent.closeAsk();
  assert.equal(auto.consent.model.mode, "closed");
  assert.equal(auto.consent.model.viaAuto, false);

  const hand = make();
  await hand.consent.open();
  assert.equal(hand.consent.model.mode, "ask");
  hand.consent.closeAsk();
  assert.equal(hand.consent.model.mode, "ask", "a card opened by hand stays");

  // The pane was shared elsewhere: the next send finds the state shared and the card of Auto goes away.
  let shared = false;
  const moved = make({ answers: { ai_context_state: () => STATE({ kinds: KINDS3, shared }) } });
  await moved.consent.attachAll(1);
  assert.equal(moved.consent.model.mode, "ask");
  shared = true;
  assert.equal((await moved.consent.attachAll(1)).kind, "attach");
  assert.equal(moved.consent.model.mode, "closed");
});

test("attachAll after the route changed drops what an earlier share knew", async () => {
  const { consent, current, calls } = make({ answers: { ai_context_state: () => STATE({ kinds: KINDS3 }) } });
  await consent.attachAll(1);
  assert.equal(consent.model.viaAuto, true);
  current.route = { ...current.route, profile: "other-1" };
  await consent.attachAll(1);
  assert.equal(consent.model.mode, "ask");
  assert.equal(calls[1][1].profile, "other-1");
});
