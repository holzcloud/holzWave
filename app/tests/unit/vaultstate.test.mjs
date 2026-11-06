// The pure model of the secret vault's page (vaultstate.ts, docs/design.md section 6.6): the status answer read defensively, the chip,
// the words, the failure words and the queue that repeats an action once after an unlock. No DOM.
import test from "node:test";
import assert from "node:assert/strict";
import {
  LOCKED_TEXT,
  STRINGS,
  actionOf,
  chipModel,
  createUnlockQueue,
  failureWord,
  freshNotice,
  isLockedFailure,
  lockedAnswer,
  noticeStep,
  parseStatus,
  syncLine,
} from "../../dist/vaultstate.js";

const raw = (over = {}) => ({
  state: "unlocked", protection: "auth", has_screen_lock: true, strongbox: false, can_protect: false, notice: "none", reason: null, ...over,
});

test("parseStatus reads the documented shape and nothing else", () => {
  for (const state of ["none", "new", "locked", "unlocked", "lost"]) assert.equal(parseStatus(raw({ state }))?.state, state);
  for (const protection of ["auth", "plain"]) assert.equal(parseStatus(raw({ protection }))?.protection, protection);
  assert.equal(parseStatus(raw({ notice: "no_lock" }))?.noLock, true);
  assert.equal(parseStatus(raw({ notice: "none" }))?.noLock, false);
  for (const reason of ["key_gone", "corrupt"]) assert.equal(parseStatus(raw({ state: "lost", reason }))?.reason, reason);
  assert.equal(parseStatus(raw({ reason: undefined }))?.reason, null, "an absent reason is none");
  const whole = parseStatus(raw({ can_protect: true, strongbox: true }));
  assert.deepEqual(whole, {
    state: "unlocked", protection: "auth", hasScreenLock: true, strongbox: true, canProtect: true, noLock: false, reason: null,
  });
});

test("parseStatus is null for anything else and never throws", () => {
  const bad = [
    null, undefined, 7, "locked", [], [raw()],
    raw({ state: "open" }), raw({ state: 1 }), raw({ state: "LOCKED" }),
    raw({ protection: "none" }), raw({ protection: null }),
    raw({ notice: "other" }), raw({ notice: undefined }),
    raw({ reason: "gone" }), raw({ reason: 5 }),
    raw({ has_screen_lock: "yes" }), raw({ strongbox: 1 }), raw({ can_protect: null }),
  ];
  for (const value of bad) assert.equal(parseStatus(value), null, JSON.stringify(value));
  for (const key of ["state", "protection", "has_screen_lock", "strongbox", "can_protect", "notice"]) {
    const missing = raw();
    delete missing[key];
    assert.equal(parseStatus(missing), null, `missing ${key}`);
  }
  // A field that only looks like a word through toString is not a word.
  assert.equal(parseStatus({ ...raw(), state: { toString: () => "locked" } }), null);
});

test("chipModel: nothing, or one of three chips that differ by glyph and by word", () => {
  assert.equal(chipModel(null), null);
  assert.equal(chipModel(parseStatus(raw({ state: "none", protection: "plain" }))), null);
  assert.equal(chipModel(parseStatus(raw({ state: "new" }))), null, "a new vault with the screen lock holds nothing yet");
  const locked = chipModel(parseStatus(raw({ state: "locked" })));
  const lost = chipModel(parseStatus(raw({ state: "lost", reason: "key_gone" })));
  const open = chipModel(parseStatus(raw({ state: "unlocked" })));
  const plain = chipModel(parseStatus(raw({ state: "unlocked", protection: "plain", notice: "no_lock" })));
  assert.deepEqual([locked.glyph, locked.label, locked.name, locked.action, locked.dashed], ["lock-closed", "LOCKED", "Locked", "unlock", false]);
  assert.deepEqual(lost, locked, "a lost vault is Locked until the person chooses");
  assert.deepEqual([open.glyph, open.label, open.name, open.action, open.dashed], ["lock-open", "OPEN", "Unlocked", "lock", false]);
  assert.deepEqual([plain.glyph, plain.label, plain.name, plain.action, plain.dashed], ["lock-bar", "NO LOCK", "Not protected", "notice", true]);
  assert.equal(new Set([locked.glyph, open.glyph, plain.glyph]).size, 3);
  assert.equal(new Set([locked.label, open.label, plain.label]).size, 3);
  assert.equal(chipModel(parseStatus(raw({ state: "new", protection: "plain", notice: "no_lock" })))?.kind, "none");
  assert.equal(chipModel(parseStatus(raw({ state: "locked", protection: "plain" })))?.kind, "locked");
});

test("the words: locked answers, the sync line and the constants", () => {
  assert.equal(lockedAnswer("connect"), "Locked: unlock to connect");
  assert.equal(lockedAnswer("ai"), "Locked: unlock to send");
  assert.equal(syncLine("paused"), "Paused: secrets are locked");
  for (const other of ["off", "idle", "syncing", "error", "", "Paused"]) assert.equal(syncLine(other), "");
  assert.equal(STRINGS.sheetTitle, "Secrets are locked");
  assert.equal(STRINGS.sheetLine, "Unlock with your fingerprint, face or screen lock to use saved passwords, keys and the AI key.");
  assert.equal(STRINGS.unlock, "Unlock");
  assert.equal(STRINGS.notNow, "Not now");
  assert.equal(STRINGS.notUnlocked, "Not unlocked");
  assert.equal(STRINGS.syncButton, "☁ sync paused");
  assert.equal(STRINGS.dismiss, "Dismiss");
  assert.equal(STRINGS.useProtected, "Use protected mode");
  assert.equal(STRINGS.group, "Security");
  assert.equal(STRINGS.rowLabel, "Protect secrets with the screen lock");
  assert.equal(STRINGS.turnOff, "Turn off");
  assert.equal(STRINGS.keepOn, "Keep on");
  assert.equal(STRINGS.lostTitle, "Saved secrets can no longer be read");
  assert.equal(STRINGS.startFresh, "Start fresh");
  assert.ok(Object.isFrozen(STRINGS) || Object.values(STRINGS).every((v) => typeof v === "string"));
});

test("a locked failure is the exact text, in a string, an Error or the host's fault object", () => {
  assert.equal(LOCKED_TEXT, "secrets are locked");
  assert.equal(isLockedFailure("secrets are locked"), true);
  assert.equal(isLockedFailure(new Error("secrets are locked")), true);
  assert.equal(isLockedFailure({ code: "key", reason: "locked", text: "secrets are locked" }), true);
  for (const other of ["secrets are locked.", "Secrets are locked", "the secrets are locked", "", null, undefined, 3, { text: "x" }, {}, [], { message: "secrets are locked" }]) {
    assert.equal(isLockedFailure(other), false, JSON.stringify(other));
  }
  assert.equal(actionOf("connect"), "connect");
  assert.equal(actionOf("ai_send"), "ai");
  assert.equal(actionOf("ai_send_with_context"), "ai");
  assert.equal(actionOf("connection_save"), null);
  assert.equal(actionOf("connect_saved"), null);
});

test("the failure words of the host", () => {
  for (const word of ["cancelled", "lost", "no_screen_lock", "failed"]) {
    assert.equal(failureWord(word), word);
    assert.equal(failureWord(new Error(word)), word);
  }
  for (const other of ["no secret vault on this device", "cancelled by the user", "/data/user/0/ch/vault", "", null, 4, { text: "lost" }]) {
    assert.equal(failureWord(other), "other", JSON.stringify(other));
  }
});

test("the retry queue: one action per key, each repeated once after the unlock", async () => {
  let sheets = 0;
  const queue = createUnlockQueue(() => (sheets += 1));
  const runs = { a: 0, b: 0 };
  const first = queue.request("a", async () => (runs.a += 1, "A"), "locked");
  const same = queue.request("a", async () => (runs.a += 100, "A2"), "locked");
  assert.equal(same, first, "a second request with the same key is the same action");
  const other = queue.request("b", async () => (runs.b += 1, "B"), "locked");
  assert.equal(sheets, 1, "a second request while a sheet is open adds no second sheet");
  assert.equal(queue.pending(), 2);
  queue.unlocked();
  assert.deepEqual([await first, await other], ["A", "B"]);
  assert.deepEqual(runs, { a: 1, b: 1 }, "each runs once");
  assert.equal(queue.pending(), 0);
  queue.unlocked();
  assert.deepEqual(runs, { a: 1, b: 1 }, "an unlock with nothing waiting runs nothing");
});

test("a cancelled sheet drops the actions with their original failures, and a later request opens a new sheet", async () => {
  let sheets = 0;
  const queue = createUnlockQueue(() => (sheets += 1));
  let ran = 0;
  const one = queue.request("a", async () => (ran += 1), "first failure");
  const two = queue.request("b", async () => (ran += 1), "second failure");
  queue.cancelled();
  await assert.rejects(one, (e) => e === "first failure");
  await assert.rejects(two, (e) => e === "second failure");
  assert.equal(ran, 0);
  assert.equal(queue.pending(), 0);
  const again = queue.request("a", async () => "ok", "x");
  assert.equal(sheets, 2, "the sheet is closed, so the next request opens one");
  queue.unlocked();
  assert.equal(await again, "ok");
});

test("a retry that fails again fails for its caller and is not repeated (no loop)", async () => {
  let sheets = 0;
  const queue = createUnlockQueue(() => (sheets += 1));
  let tries = 0;
  const result = queue.request("a", async () => { tries += 1; throw "secrets are locked"; }, "secrets are locked");
  queue.unlocked();
  await assert.rejects(result, (e) => e === "secrets are locked");
  assert.equal(tries, 1);
  assert.equal(queue.pending(), 0);
  assert.equal(sheets, 1, "the failed retry did not open a second sheet by itself");
  const sync = queue.request("b", () => { throw new Error("sync throw"); }, "o");
  queue.unlocked();
  await assert.rejects(sync, /sync throw/);
});

const plainNoLock = parseStatus(raw({ protection: "plain", notice: "no_lock", has_screen_lock: false }));

test("the notice: once per start, again on a tap, with the offer only when the system can switch", () => {
  let memory = freshNotice();
  let step = noticeStep(memory, plainNoLock, false);
  assert.deepEqual([step.visible, step.offer], [true, false], "no screen lock: the notice without the offer");
  memory = step.memory;
  memory = { ...memory, dismissed: true };
  step = noticeStep(memory, plainNoLock, false);
  assert.equal(step.visible, false, "dismissed for this start");
  step = noticeStep(memory, plainNoLock, true);
  assert.equal(step.visible, true, "a tap on the chip shows it again");
  const later = parseStatus(raw({ protection: "plain", notice: "none", can_protect: true }));
  step = noticeStep(freshNotice(), later, false);
  assert.equal(step.visible, false, "a vault that never reported no_lock shows nothing by itself");
  step = noticeStep({ seen: true, dismissed: false }, later, false);
  assert.deepEqual([step.visible, step.offer], [true, true], "a screen lock was set meanwhile: the offer appears");
  step = noticeStep({ seen: true, dismissed: false }, parseStatus(raw()), false);
  assert.equal(step.visible, false, "protected mode: nothing to say");
  assert.equal(noticeStep(freshNotice(), null, true).visible, false);
  assert.equal(noticeStep(freshNotice(), parseStatus(raw({ state: "none", protection: "plain", notice: "no_lock" })), true).visible, false);
});
