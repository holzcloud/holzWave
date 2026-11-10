// The pure part of signing in to a server (serverstate.ts, docs/design.md 6.35 and 6.9): the strict readers of what the host answers,
// the check of the address, the closed set of the states, the pacing of the polls and every word of the dialog held to the record.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import * as state from "../../dist/serverstate.js";
import {
  ADDRESS_HELP,
  ADDRESS_LABEL,
  ADDRESS_REFUSED,
  CANCEL,
  CLOSE,
  COMPARE,
  CONTINUE,
  DEVICE_LABEL,
  LOCKED_LINE,
  OPENING,
  OPEN_AGAIN,
  Pacer,
  SIGN_IN_LINK,
  SIGN_IN_TITLE,
  SIGN_OUT,
  SIGN_OUT_ARMED,
  START_AGAIN,
  WAIT_WORDS,
  checkAddress,
  isFailure,
  plain,
  readPoll,
  readServerView,
  readSignIn,
  refusalOf,
  waitWords,
} from "../../dist/serverstate.js";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const record = readFileSync(join(ROOT, "docs", "design.md"), "utf8");
const section = record.slice(record.indexOf("### 6.35 Devices and signing in to a server"));
const row = record.slice(record.indexOf("### 6.34 Server connection and sessions"), record.indexOf("### 6.35 Devices and signing in to a server"));

/** Control, separator and direction characters; built from a string because a literal cannot hold the separators. */
const UNSEEN = new RegExp("[" + String.fromCharCode(92) + "u0000-" + String.fromCharCode(92) + "u001f" + String.fromCharCode(92) + "u2028" + String.fromCharCode(92) + "u202e]", "u");

/** A token-shaped canary that must never come out of a reader. It appears in test code only. */
const CANARY = "hwd_" + "A".repeat(43);

// ---- the words are the words of the record -----------------------------------------------------------------------------------------

test("every word of the dialog is a word of the record", () => {
  const words = [
    SIGN_IN_TITLE, ADDRESS_LABEL, ADDRESS_HELP, ADDRESS_REFUSED, DEVICE_LABEL, CONTINUE, CANCEL, OPENING, COMPARE, OPEN_AGAIN, START_AGAIN,
    CLOSE, ...Object.values(WAIT_WORDS).map((w) => w.word),
  ];
  for (const word of words) assert.ok(section.includes("`" + word + "`"), `the record has no word ${word}`);
  assert.ok(row.includes("`" + SIGN_IN_LINK + "`") && row.includes("`" + SIGN_OUT + "`") && row.includes("`" + SIGN_OUT_ARMED + "`"));
  assert.ok(row.includes("`" + LOCKED_LINE + "`"), "the locked line is the record's");
});

test("the five sentences of the plan are exactly the record's", () => {
  assert.equal(SIGN_IN_TITLE, "Sign in to the server");
  assert.equal(ADDRESS_LABEL, "Server address");
  assert.equal(ADDRESS_REFUSED, "Use an address that starts with https://.");
  assert.equal(OPENING, "Opening your browser…");
  assert.equal(COMPARE, "Enter this code only if you started this here.");
  assert.equal(WAIT_WORDS.approved.word, "Signed in");
  assert.equal(WAIT_WORDS.denied.word, "The request was denied.");
  assert.equal(WAIT_WORDS.expired.word, "The code expired. Start again.");
  assert.equal(WAIT_WORDS.unreachable.word, "The server cannot be reached.");
  assert.equal(WAIT_WORDS.off.word, "Device sign-in is off on this server.");
  assert.equal(LOCKED_LINE, "Locked: unlock to connect");
});

test("a state is a shape and a word; a failure also has the mark and the underline", () => {
  for (const [key, words] of Object.entries(WAIT_WORDS)) {
    assert.ok(words.word.length > 0, key);
    assert.equal(words.bad, key !== "waiting" && key !== "approved", key);
    assert.equal(words.dot, words.bad ? "mark" : key === "waiting" ? "ring" : "filled", key);
  }
  assert.equal(isFailure("waiting"), false);
  assert.equal(isFailure("approved"), false);
  for (const s of ["denied", "expired", "too_many", "unreachable", "off", "failed"]) assert.equal(isFailure(s), true, s);
  // A refusal the record has no word for shows the app's own sentence, bounded, as a failure.
  assert.deepEqual(waitWords("failed", "The sign-in could not be saved in the keychain."), {
    dot: "mark", bad: true, word: "The sign-in could not be saved in the keychain.",
  });
  assert.equal(waitWords("failed", "").word, "The sign-in failed.");
  assert.equal(waitWords("failed", "x".repeat(900)).word.length, 200);
});

// ---- the readers -------------------------------------------------------------------------------------------------------------------

const VIEW = { state: "signed_in", url: "https://holz.example.net", name: "Holz MacBook", locked: false, sync_on: true, remote_on: false, error: null };

test("the view is read member by member", () => {
  assert.deepEqual(readServerView(VIEW), {
    state: "signed_in", url: "https://holz.example.net", name: "Holz MacBook", locked: false, syncOn: true, remoteOn: false, error: null,
  });
  assert.deepEqual(readServerView({ ...VIEW, state: "unreachable", locked: true, error: "The server cannot be reached." }), {
    state: "unreachable", url: "https://holz.example.net", name: "Holz MacBook", locked: true, syncOn: true, remoteOn: false,
    error: "The server cannot be reached.",
  });
  for (const word of ["signed_out", "signing_in", "signed_in", "unreachable"]) assert.equal(readServerView({ ...VIEW, state: word }).state, word);
});

test("anything that is not the documented shape reads as signed out with no code", () => {
  const safe = { state: "signed_out", url: "", name: "", locked: false, syncOn: false, remoteOn: false, error: null };
  for (const bad of [null, undefined, [], "signed_in", 7, true]) assert.deepEqual(readServerView(bad), safe, String(bad));
  for (const word of ["", "SIGNED_IN", "signed_in ", "on", 3, null, "__proto__", "toString"]) {
    assert.equal(readServerView({ ...VIEW, state: word }).state, "signed_out", String(word));
  }
  // An address that is not an https origin is not kept; the switches are booleans or false.
  for (const url of ["http://holz.example.net", "https://holz.example.net/path", "https://u:p@holz.example.net", "javascript:alert(1)", "", 5, null]) {
    assert.equal(readServerView({ ...VIEW, url }).url, "", String(url));
  }
  const odd = readServerView({ ...VIEW, locked: "yes", sync_on: 1, remote_on: "true" });
  assert.deepEqual([odd.locked, odd.syncOn, odd.remoteOn], [false, false, false]);
  // Inherited members never count.
  const inherited = Object.create({ state: "signed_in", url: "https://holz.example.net" });
  assert.deepEqual(readServerView(inherited), safe);
});

test("no member for a secret is read, even when present", () => {
  const read = readServerView({ ...VIEW, token: CANARY, device_code: CANARY, hwd: CANARY });
  assert.equal(JSON.stringify(read).includes(CANARY), false);
  assert.deepEqual(Object.keys(read).sort(), ["error", "locked", "name", "remoteOn", "state", "syncOn", "url"]);
  const start = readSignIn({ code: "BCDF-GHJK", url: "https://holz.example.net/#device=hwg_x", expires_in: 600, interval: 5, token: CANARY, device_code: CANARY });
  assert.equal(JSON.stringify(start).includes(CANARY), false);
  assert.deepEqual(Object.keys(start).sort(), ["code", "expiresIn", "interval", "openUrl"]);
  const poll = readPoll({ state: "approved", interval: 5, token: CANARY });
  assert.deepEqual(Object.keys(poll).sort(), ["interval", "state"]);
  assert.equal(JSON.stringify(poll).includes(CANARY), false);
});

test("a hostile name, error or address is bounded text", () => {
  const read = readServerView({
    ...VIEW, name: `<img src=x onerror=alert(1)>\u0000\u202e${"n".repeat(500)}`, error: `line\nbreak\u2028${"e".repeat(900)}`,
  });
  assert.equal(read.name.length, 64);
  assert.equal(UNSEEN.test(read.name), false);
  assert.ok(read.error.length <= 200 && !UNSEEN.test(read.error));
  assert.equal(plain(17, 10), "");
  assert.equal(plain("  a\tb  ", 10), "a b");
});

test("the start of a sign-in needs a code of the right shape and a web address", () => {
  const good = { code: "BCDF-GHJK", url: "https://holz.example.net/#device=hwg_x", expires_in: 600, interval: 5 };
  assert.deepEqual(readSignIn(good), { code: "BCDF-GHJK", openUrl: good.url, expiresIn: 600, interval: 5 });
  for (const code of ["bcdf-ghjk", "BCDFGHJK", "BCDF-GHJA", "BCDF-GHJ", "", 5, null, "<b>x</b>"]) assert.equal(readSignIn({ ...good, code }), null, String(code));
  for (const url of ["javascript:alert(1)", "file:///etc/passwd", "http://holz.example.net/", "https://a b", "", null, "https://x/" + "a".repeat(2100)]) {
    assert.equal(readSignIn({ ...good, url }), null, String(url).slice(0, 30));
  }
  assert.equal(readSignIn(null), null);
  assert.equal(readSignIn([]), null);
});

test("the interval is bounded to 1 to 60 seconds", () => {
  const good = { code: "BCDF-GHJK", url: "https://holz.example.net/", expires_in: 600 };
  assert.equal(readSignIn({ ...good, interval: 1 }).interval, 1);
  assert.equal(readSignIn({ ...good, interval: 60 }).interval, 60);
  for (const bad of [0, -3, 61, 1e9, NaN, Infinity, "5", null, undefined]) assert.equal(readSignIn({ ...good, interval: bad }).interval, 5, String(bad));
  assert.equal(readSignIn({ ...good, interval: 2.9 }).interval, 2);
  assert.equal(readPoll({ state: "pending", interval: 0 }).interval, 5);
  assert.equal(readPoll({ state: "pending", interval: 999 }).interval, 5);
  assert.equal(readSignIn({ ...good, interval: 5, expires_in: -1 }).expiresIn, 600);
});

test("a poll reads its word from a closed set", () => {
  assert.deepEqual(readPoll({ state: "pending", interval: 7 }), { state: "waiting", interval: 7 });
  for (const word of ["approved", "denied", "expired", "too_many", "off", "unreachable"]) assert.equal(readPoll({ state: word, interval: 5 }).state, word);
  // An unknown word, a different case and a non-object stop the wait with a state of the record.
  for (const bad of ["Approved", "slow_down", "", "approved ", 3, null, "__proto__"]) assert.equal(readPoll({ state: bad, interval: 5 }).state, "unreachable", String(bad));
  for (const bad of [null, undefined, [], "approved", 1]) assert.equal(readPoll(bad).state, "unreachable", String(bad));
});

// ---- the address -------------------------------------------------------------------------------------------------------------------

test("the page's check says the refusal before any command", () => {
  assert.deepEqual(checkAddress("https://holz.example.net"), { ok: true, address: "https://holz.example.net" });
  assert.deepEqual(checkAddress("  https://Holz.Example.net:8443/ "), { ok: true, address: "https://holz.example.net:8443" });
  for (const bad of [
    "", "   ", "holz.example.net", "http://holz.example.net", "ftp://holz.example.net", "https://", "https:///", "https://holz.example.net/app",
    "https://holz.example.net/?q=1", "https://holz.example.net?q=1", "https://holz.example.net/#x", "https://u@holz.example.net", "https://u:p@holz.example.net",
    "https://holz example.net", "https://holz.exam\nple.net", "https://holz.example.net\\x", "javascript:alert(1)", "https://" + "a".repeat(260) + ".net", 5, null, undefined,
  ]) {
    assert.deepEqual(checkAddress(bad), { ok: false }, String(bad));
  }
});

test("http is accepted for a loopback host only", () => {
  for (const ok of ["http://localhost:8443", "http://127.0.0.1:8443", "http://[::1]:8443"]) assert.equal(checkAddress(ok).ok, true, ok);
  for (const bad of ["http://192.168.1.2", "http://holz.local", "http://localhost.example.net", "http://127.0.0.1.example.net"]) assert.equal(checkAddress(bad).ok, false, bad);
});

// ---- what a refused command comes to -----------------------------------------------------------------------------------------------

test("a refusal of the host that names a state of the record is read as that state", () => {
  assert.deepEqual(refusalOf("The server cannot be reached."), { kind: "state", state: "unreachable" });
  assert.deepEqual(refusalOf("This feature is off on the server."), { kind: "state", state: "off" });
  assert.deepEqual(refusalOf("secrets are locked"), { kind: "locked" });
  assert.deepEqual(refusalOf("Locked: unlock to connect"), { kind: "locked" });
  assert.deepEqual(refusalOf("The sign-in could not be saved in the keychain."), { kind: "sentence", text: "The sign-in could not be saved in the keychain." });
  assert.deepEqual(refusalOf(new Error("The server cannot be reached.")), { kind: "state", state: "unreachable" });
  for (const odd of [null, undefined, 3, {}, ""]) assert.deepEqual(refusalOf(odd), { kind: "sentence", text: "The sign-in failed." });
  assert.equal(refusalOf("x".repeat(900)).text.length, 200);
});

// ---- the pace ----------------------------------------------------------------------------------------------------------------------

test("the pacer never sends a poll earlier than the host asked", () => {
  const pacer = new Pacer(1000, 5);
  assert.equal(pacer.seconds, 5);
  assert.equal(pacer.wait(1000), 5000, "the first poll is one interval after the start");
  assert.equal(pacer.due(5999), false);
  assert.equal(pacer.wait(5999), 1);
  assert.equal(pacer.due(6000), true);
  assert.equal(pacer.wait(9000), 0);
  pacer.polled(6000, 5);
  assert.equal(pacer.due(10999), false);
  assert.equal(pacer.due(11000), true);
  // The host asked for slower polls: the new interval holds from the poll it answered.
  pacer.polled(11000, 10);
  assert.equal(pacer.seconds, 10);
  assert.equal(pacer.due(20999), false);
  assert.equal(pacer.due(21000), true);
  // An interval outside 1 to 60 keeps the last good one.
  pacer.polled(21000, 9999);
  assert.equal(pacer.seconds, 10);
  pacer.polled(21000, 0);
  assert.equal(pacer.seconds, 10);
  pacer.polled(21000, 60);
  assert.equal(pacer.wait(21000), 60000);
  assert.equal(new Pacer(0, 1e9).seconds, 5);
  assert.equal(new Pacer(0, 1).wait(0), 1000);
});

test("nothing exported carries a secret or a planning label", () => {
  for (const [name, value] of Object.entries(state)) {
    if (typeof value === "string") assert.equal(/hw[dg]_|\b12-\d\d\b|\bT-\d\d/u.test(value), false, name);
  }
});
