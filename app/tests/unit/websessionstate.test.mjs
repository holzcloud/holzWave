// The pure part of the session marks (websessionstate.ts, docs/design.md 6.30): the reading of the server's answers, the
// model of the session, the connection and the secret store, the marks as a function of the model and a clock, and the
// time of the next change. Every table injects `now`, so no test waits.
import test from "node:test";
import assert from "node:assert/strict";
import {
  UNLOCK_FAILED,
  connection,
  failureOf,
  fromInfo,
  marks,
  minutesLeft,
  newModel,
  nextChange,
  readSessionInfo,
  reduce,
  setOffline,
  unlockMessage,
} from "../../dist/websessionstate.js";

const ACCOUNT = { id: "a1", username: "holz", display_name: "Holz", role: "admin" };
const SIGNED_IN = { state: "signed_in", csrf: "tok", account: ACCOUNT, seconds: 1500, step_up: 0 };
const T0 = 1_000_000;
const words = (model, now) => marks(model, now).map((m) => m.word);
const withEvent = (state, seconds, now = T0, model = newModel()) => reduce(model, seconds === undefined ? { state } : { state, seconds }, now);

test("readSessionInfo_reads_the_three_documented_shapes", () => {
  assert.deepEqual(readSessionInfo({ state: "anonymous", host: "h", bootstrap: false, methods: {} }), {
    state: "anonymous",
    csrf: "",
    account: null,
    seconds: null,
    stepUp: 0,
  });
  const signedIn = readSessionInfo(SIGNED_IN);
  assert.equal(signedIn.state, "signed_in");
  assert.deepEqual(signedIn.account, { id: "a1", username: "holz", displayName: "Holz", role: "admin" });
  assert.equal(signedIn.seconds, 1500);
  assert.equal(readSessionInfo({ ...SIGNED_IN, state: "locked", step_up: 240 }).stepUp, 240);
  assert.equal(readSessionInfo({ ...SIGNED_IN, state: "locked" }).state, "locked");
});

test("readSessionInfo_refuses_a_missing_or_unknown_state", () => {
  for (const raw of [null, undefined, "locked", 3, [], {}, { state: "" }, { state: "ok" }, { state: "LOCKED", csrf: "x", account: ACCOUNT }]) {
    assert.equal(readSessionInfo(raw), null, JSON.stringify(raw));
  }
});

test("readSessionInfo_refuses_seconds_that_are_not_whole_and_not_negative", () => {
  for (const seconds of [-1, 1.5, "60", null, Number.NaN, Infinity]) {
    assert.equal(readSessionInfo({ ...SIGNED_IN, seconds }), null, String(seconds));
  }
  assert.equal(readSessionInfo({ ...SIGNED_IN, seconds: 0 }).seconds, 0);
  assert.equal(readSessionInfo({ ...SIGNED_IN, step_up: -5 }), null);
});

test("readSessionInfo_bounds_every_string", () => {
  assert.equal(readSessionInfo({ ...SIGNED_IN, csrf: "x".repeat(256) }).csrf.length, 256);
  assert.equal(readSessionInfo({ ...SIGNED_IN, csrf: "x".repeat(257) }), null);
  assert.equal(readSessionInfo({ ...SIGNED_IN, account: { ...ACCOUNT, username: 7 } }), null);
  assert.equal(readSessionInfo({ ...SIGNED_IN, account: { ...ACCOUNT, display_name: null } }), null);
  assert.equal(readSessionInfo({ ...SIGNED_IN, account: { ...ACCOUNT, role: "root" } }), null);
  assert.equal(readSessionInfo({ ...SIGNED_IN, account: { ...ACCOUNT, username: "x".repeat(1024 * 1024) } }), null);
  assert.equal(readSessionInfo({ ...SIGNED_IN, csrf: "x".repeat(1024 * 1024) }), null);
  assert.equal(readSessionInfo({ ...SIGNED_IN, account: null }), null);
  assert.equal(readSessionInfo({ ...SIGNED_IN, csrf: undefined }), null);
});

test("fromInfo_takes_the_store_and_the_expiry_and_reads_anonymous_as_ended", () => {
  const locked = fromInfo(newModel(), readSessionInfo({ ...SIGNED_IN, state: "locked", seconds: 120 }), T0);
  assert.equal(locked.store, "locked");
  assert.equal(locked.expiresAt, T0 + 120_000);
  assert.equal(fromInfo(newModel(), readSessionInfo(SIGNED_IN), T0).store, "unlocked");
  assert.equal(fromInfo(newModel(), readSessionInfo(SIGNED_IN), T0).expiresAt, null, "1500 s is outside the window");
  assert.equal(fromInfo(newModel(), readSessionInfo({ state: "anonymous" }), T0).ended, true);
});

test("reduce_an_ok_above_the_window_clears_the_expiry", () => {
  const expiring = withEvent("expiring", 240);
  assert.equal(expiring.expiresAt, T0 + 240_000);
  assert.equal(reduce(expiring, { state: "ok", seconds: 301 }, T0 + 5).expiresAt, null);
  assert.equal(reduce(expiring, { state: "ok", seconds: 1500 }, T0 + 5).expiresAt, null);
  assert.equal(reduce(expiring, { state: "ok" }, T0 + 5).expiresAt, null, "an ok without a time clears it too");
  assert.equal(reduce(newModel(), { state: "ok", seconds: 300 }, T0).expiresAt, T0 + 300_000);
});

test("reduce_expiring_sets_the_expiry_and_defaults_to_the_window", () => {
  assert.equal(withEvent("expiring", 240).expiresAt, T0 + 240_000);
  assert.equal(withEvent("expiring").expiresAt, T0 + 300_000);
  assert.equal(withEvent("expiring", 0).expiresAt, T0);
  assert.equal(withEvent("expiring", -4).expiresAt, T0 + 300_000, "a negative time is not a time");
  assert.equal(withEvent("expiring", 1.5).expiresAt, T0 + 300_000);
});

test("reduce_locked_and_unlocked_flip_the_store_and_keep_the_rest", () => {
  const locked = withEvent("locked");
  assert.equal(locked.store, "locked");
  const unlocked = reduce(locked, { state: "unlocked" }, T0 + 1);
  assert.equal(unlocked.store, "unlocked");
  assert.equal(reduce(locked, { state: "ok", seconds: 900 }, T0).store, "locked", "an ok does not say what the store is");
  const expiring = withEvent("expiring", 200);
  assert.equal(reduce(expiring, { state: "locked" }, T0).expiresAt, expiring.expiresAt, "no time given, the expiry stays");
});

test("reduce_ended_ends_the_session", () => {
  assert.equal(withEvent("ended", 0).ended, true);
  assert.equal(withEvent("ended").ended, true);
});

test("reduce_ignores_an_unknown_state_and_a_payload_that_is_not_an_object", () => {
  const model = withEvent("expiring", 120);
  for (const event of [null, undefined, "locked", 4, [], [{ state: "locked" }], { state: "rebooting" }, { state: 4 }, {}, { seconds: 4 }]) {
    assert.equal(reduce(model, event, T0 + 1), model, JSON.stringify(event));
  }
});

test("marks_reconnecting_is_absent_at_999_ms_and_present_at_1000_ms_and_gone_after_open", () => {
  const lost = connection(newModel(), { state: "reconnecting", attempt: 1 }, T0);
  assert.deepEqual(words(lost, T0), []);
  assert.deepEqual(words(lost, T0 + 999), []);
  assert.deepEqual(words(lost, T0 + 1000), ["reconnecting"]);
  assert.deepEqual(words(lost, T0 + 60_000), ["reconnecting"]);
  const again = connection(lost, { state: "reconnecting", attempt: 2 }, T0 + 1500);
  assert.equal(again.lostAt, T0, "the second attempt does not restart the clock of the loss");
  const back = connection(lost, { state: "open", resumed: true }, T0 + 1200);
  assert.deepEqual(words(back, T0 + 1200), []);
  assert.equal(back.lostAt, null);
  assert.equal(connection(newModel(), { state: "open" }, T0).lostAt, null);
});

test("marks_offline_replaces_reconnecting_and_needs_no_wait", () => {
  const lost = connection(newModel(), { state: "reconnecting" }, T0);
  const offline = setOffline(lost, true);
  assert.deepEqual(words(offline, T0), ["offline"], "offline shows at once");
  assert.deepEqual(words(offline, T0 + 5000), ["offline"], "offline outranks reconnecting");
  assert.deepEqual(words(setOffline(offline, false), T0 + 5000), ["reconnecting"]);
  assert.equal(setOffline(offline, true), offline);
  const m = marks(offline, T0)[0];
  assert.equal(m.tone, "danger");
  assert.equal(m.glyph, "!");
});

test("marks_expiry_text_follows_the_table", () => {
  const table = [
    [300, "session ends in 5 min"],
    [240, "session ends in 4 min"],
    [61, "session ends in 2 min"],
    [60, "session ends in 1 min"],
    [59, "session ends in 1 min"],
    [1, "session ends in 1 min"],
  ];
  for (const [seconds, text] of table) assert.deepEqual(words(withEvent("expiring", seconds), T0), [text], `${seconds} s`);
  assert.deepEqual(words(withEvent("ok", 301), T0), [], "301 s is not shown");
  assert.deepEqual(words(withEvent("expiring", 0), T0), [], "nothing is left to count");
});

test("marks_expiry_count_follows_the_clock", () => {
  const model = withEvent("expiring", 240);
  assert.deepEqual(words(model, T0), ["session ends in 4 min"]);
  assert.deepEqual(words(model, T0 + 61_000), ["session ends in 3 min"]);
  assert.deepEqual(words(model, T0 + 180_000), ["session ends in 1 min"]);
  assert.deepEqual(words(model, T0 + 239_999), ["session ends in 1 min"]);
  assert.deepEqual(words(model, T0 + 240_000), [], "at the expiry the mark goes; the server says `ended`");
  assert.equal(marks(model, T0)[0].glyph, "◔");
  assert.equal(marks(model, T0)[0].tone, "warn");
});

test("marks_come_in_the_order_connection_then_expiry_then_lock", () => {
  let model = withEvent("locked", 200);
  model = connection(model, { state: "reconnecting" }, T0);
  assert.deepEqual(words(model, T0 + 2000), ["reconnecting", "session ends in 4 min", "secrets locked"]);
  const m = marks(setOffline(model, true), T0 + 2000);
  assert.deepEqual(m.map((x) => x.id), ["offline", "expiring", "locked"]);
  assert.equal(m.at(-1).action, "unlock");
  assert.equal(m[0].action, undefined);
});

test("marks_with_the_session_ended_leave_only_ended", () => {
  let model = withEvent("locked", 100);
  model = setOffline(connection(model, { state: "reconnecting" }, T0), true);
  model = reduce(model, { state: "ended" }, T0 + 5);
  assert.deepEqual(marks(model, T0 + 5000), [{ id: "ended", glyph: "!", word: "session ended", tone: "danger" }]);
});

test("connection_4401_ends_the_session_and_4403_leaves_offline_for_good", () => {
  const lost = connection(newModel(), { state: "reconnecting" }, T0);
  const ended = connection(lost, { state: "ended", code: 4401 }, T0 + 10);
  assert.equal(ended.ended, true);
  assert.equal(ended.lostAt, null);
  assert.deepEqual(words(ended, T0 + 99_999), ["session ended"]);
  const refused = connection(lost, { state: "ended", code: 4403 }, T0 + 10);
  assert.equal(refused.ended, false, "a refused page has no session layer");
  assert.deepEqual(words(refused, T0 + 10), ["offline"]);
  assert.deepEqual(words(refused, T0 + 3_600_000), ["offline"], "for good");
  assert.equal(nextChange(refused, T0 + 10), null, "nothing changes by itself");
  assert.equal(connection(newModel(), { state: "ended", code: 1006 }, T0).ended, false);
  assert.equal(connection(newModel(), null, T0).ended, false);
  assert.equal(connection(newModel(), { state: "connecting" }, T0).lostAt, null);
});

test("nextChange_gives_the_rest_of_the_second_and_the_rest_of_the_minute", () => {
  const lost = connection(newModel(), { state: "reconnecting" }, T0);
  assert.equal(nextChange(lost, T0), 1000);
  assert.equal(nextChange(lost, T0 + 400), 600);
  assert.equal(nextChange(lost, T0 + 999), 1);
  assert.equal(nextChange(lost, T0 + 1000), null, "the mark is there, nothing changes until the connection does");
  const model = withEvent("expiring", 240);
  assert.equal(nextChange(model, T0), 60_000, "the minute of 4 min ends in a minute");
  assert.equal(nextChange(model, T0 + 59_000), 1000);
  assert.equal(nextChange(model, T0 + 60_000), 60_000);
  assert.equal(nextChange(model, T0 + 180_000), 60_000, "the last minute ends at the expiry");
  assert.equal(nextChange(model, T0 + 240_000), null);
  assert.equal(nextChange(newModel(), T0), null);
  const both = connection(model, { state: "reconnecting" }, T0 + 30_000);
  assert.equal(nextChange(both, T0 + 30_000), 1000, "the nearer of the two");
  assert.equal(nextChange(setOffline(lost, true), T0), null, "offline shows at once and has no wait");
  assert.equal(nextChange(reduce(model, { state: "ended" }, T0), T0), null);
});

test("minutesLeft_rounds_up_and_never_says_zero", () => {
  const table = [[300, 5], [299.9, 5], [240, 4], [61, 2], [60, 1], [1, 1], [0.2, 1], [0, 1]];
  for (const [seconds, minutes] of table) assert.equal(minutesLeft(seconds), minutes, String(seconds));
});

test("unlockMessage_says_the_one_sentence_for_a_refusal_and_the_wait_for_a_throttle", () => {
  assert.equal(unlockMessage(401), UNLOCK_FAILED);
  assert.equal(UNLOCK_FAILED, "That did not unlock your secrets.");
  assert.equal(unlockMessage(400), UNLOCK_FAILED);
  assert.equal(unlockMessage(423), UNLOCK_FAILED);
  assert.equal(unlockMessage(429, { retry_seconds: 720 }), "Too many attempts. Try again in 12 minutes.");
  assert.equal(unlockMessage(429), "Too many attempts. Try again in a moment.");
  assert.equal(unlockMessage(503), "The server is busy. Try again in a moment.");
  assert.equal(unlockMessage(0), "Could not reach the server. Try again.");
  assert.equal(unlockMessage(502), "Could not reach the server. Try again.");
});

test("failureOf_reads_the_status_and_a_body_that_is_an_object", () => {
  assert.deepEqual(failureOf({ status: 429, body: { retry_seconds: 5 } }), { status: 429, body: { retry_seconds: 5 } });
  assert.deepEqual(failureOf({ status: 401 }), { status: 401 });
  assert.deepEqual(failureOf(new TypeError("x")), { status: 0 });
  assert.deepEqual(failureOf(null), { status: 0 });
  assert.deepEqual(failureOf({ status: 403, body: ["x"] }), { status: 403 });
  assert.deepEqual(failureOf({ status: "403" }), { status: 0 });
});
