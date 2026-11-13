// The pure part of the device features of the web page (devicestate.ts, docs/design.md 6.35): the words held to the record, the
// strict readers of the lookup, the two lists and the state of the two switches, the time words and the sentences for a refusal.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import * as words from "../../dist/devicestate.js";
import {
  ACCESS_HELP,
  ACCESS_LEGEND,
  APPROVED,
  APPROVE,
  APPROVE_SENTENCE,
  AUTHORIZE_ENTRY,
  AUTHORIZE_TITLE,
  CLOSE,
  CODE_LABEL,
  DENIED,
  DENY,
  DEVICES_LEGEND,
  LIST_HEADS,
  LOCKED_LINE,
  LOOK_UP,
  MAX_ALL_DEVICES,
  MAX_NAME,
  MAX_PERSON_DEVICES,
  NEVER,
  NOT_VALID,
  NO_DEVICES,
  REVOKE,
  REVOKE_ARMED,
  ROW_ADDRESS,
  ROW_DEVICE,
  ROW_PLATFORM,
  ROW_REQUESTED,
  SWITCHES,
  SWITCH_OFF,
  SWITCH_ON,
  accessBody,
  codeMessage,
  deviceChangeMessage,
  lastSeenText,
  metaText,
  normaliseDeviceCode,
  orderDevices,
  platformWord,
  readAccess,
  readDevices,
  readLookup,
  requestedText,
  shown,
} from "../../dist/devicestate.js";
import { CHANGE_FAILED } from "../../dist/accountstate.js";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const record = readFileSync(join(ROOT, "docs", "design.md"), "utf8");
// The section of the record that holds the words of this work.
const section = record.slice(record.indexOf("### 6.35 Devices and signing in to a server"));
const NOW = Date.parse("2026-11-10T12:00:00Z");
const iso = (secondsAgo) => new Date(NOW - secondsAgo * 1000).toISOString();

// ---- the words are the words of the record -----------------------------------------------------------------------------------

test("every word of the page is a word of the record's tables", () => {
  const everyWord = [
    AUTHORIZE_TITLE, AUTHORIZE_ENTRY, ROW_DEVICE, ROW_PLATFORM, ROW_ADDRESS, ROW_REQUESTED, CODE_LABEL, APPROVE_SENTENCE, APPROVE, DENY,
    LOOK_UP, APPROVED, DENIED, NOT_VALID, DEVICES_LEGEND, NO_DEVICES, REVOKE, REVOKE_ARMED, ACCESS_LEGEND, SWITCHES[0].label,
    SWITCHES[1].label, SWITCH_ON, SWITCH_OFF, LOCKED_LINE, ACCESS_HELP, CLOSE, "Cancel", "Owner",
  ];
  for (const word of everyWord) assert.ok(section.includes(`\`${word}\``), `the record has no \`${word}\``);
  // The heads of the administrator's list are drawn in the record's picture and named in its prose; `Owner` is a word of the table.
  assert.deepEqual([...LIST_HEADS], ["Name", "Owner", "Platform", "Last seen"]);
});

test("the page keeps no word of its own: every exported sentence is in the record", () => {
  // A constant that is a sentence (it has a space) must be in the record; the short ones are checked above.
  const sentences = Object.entries(words).filter(([name, v]) => typeof v === "string" && /[a-z] [a-z]/i.test(v) && /^[A-Z_]+$/.test(name));
  assert.ok(sentences.length >= 8, "the sentences were found");
  for (const [name, text] of sentences) assert.ok(section.includes(text), `${name}: "${text}" is not in the record`);
});

test("the record places the controls where the page draws them", () => {
  assert.match(section, /\*\*The page `Authorize device`\.\*\*/);
  assert.match(section, /after Sessions/);
  assert.match(section, /In the People dialog, below the Devices group/);
  assert.match(section, /`Close` \(nothing is left to decide\)/);
  assert.match(section, /Both are `OFF` by default/);
});

// ---- the lookup --------------------------------------------------------------------------------------------------------------

const lookup = (over = {}) => ({ name: "Holz MacBook", platform: "macos", addr: "203.0.113.7", created_at: iso(120), ...over });

test("readLookup reads the documented shape, with the platform from the closed list, and carries nothing else", () => {
  const read = readLookup({ ...lookup(), user_code: "BCDF-GHJK", device_code: "hwg_x", token: "hwd_y" });
  assert.deepEqual(read, { name: "Holz MacBook", platform: "macOS", addr: "203.0.113.7", requestedMs: NOW - 120_000 });
  assert.deepEqual(Object.keys(read).sort(), ["addr", "name", "platform", "requestedMs"]);
  for (const [id, word] of [["macos", "macOS"], ["windows", "Windows"], ["linux", "Linux"], ["android", "Android"], ["other", "Other"]]) {
    assert.equal(readLookup(lookup({ platform: id })).platform, word);
  }
});

test("readLookup refuses anything that is not the documented shape", () => {
  const bad = [
    null, undefined, "text", 7, [], [lookup()],
    lookup({ name: undefined }), lookup({ name: 5 }), lookup({ name: "" }), lookup({ name: "   " }), lookup({ name: "\u0000\u0007" }),
    lookup({ platform: "ios" }), lookup({ platform: "MacOS" }), lookup({ platform: "__proto__" }), lookup({ platform: "constructor" }),
    lookup({ platform: undefined }), lookup({ platform: ["macos"] }),
    lookup({ addr: 4 }), lookup({ addr: "" }), lookup({ addr: undefined }),
    lookup({ created_at: "yesterday" }), lookup({ created_at: 5 }), lookup({ created_at: undefined }), lookup({ created_at: "9".repeat(80) }),
  ];
  for (const raw of bad) assert.equal(readLookup(raw), null, JSON.stringify(raw));
});

test("a name from the server is text of a bounded length with no control or direction character", () => {
  assert.equal(shown("Holz\u0000\u001b[31m MacBook", 64), "Holz[31m MacBook");
  assert.equal(shown("a‮b⁦c‏d", 64), "abcd");
  assert.equal(shown("<img src=x onerror=alert(1)>", 64), "<img src=x onerror=alert(1)>", "markup is kept as text, never dropped or decoded");
  assert.equal(shown("x".repeat(500), 64).length, MAX_NAME);
  assert.equal(shown("x".repeat(100_000), 64), null, "an absurd string is refused, not cut");
  assert.equal(shown("é".repeat(70), 64), "é".repeat(64));
  assert.equal(Array.from(shown("😀".repeat(70), 64)).length, 64, "a cut never splits a character");
  assert.equal(readLookup(lookup({ name: "n".repeat(200) })).name.length, MAX_NAME);
});

test("a code from a link or a field is normalised by the one function the fragment uses", () => {
  assert.equal(normaliseDeviceCode("bcdfghjk"), "BCDF-GHJK");
  assert.equal(normaliseDeviceCode("bcdf-ghjk"), "BCDF-GHJK");
  assert.equal(normaliseDeviceCode("bcdf ghjk"), null);
  assert.equal(normaliseDeviceCode(""), null);
});

// ---- the lists -----------------------------------------------------------------------------------------------------------------

const device = (over = {}) => ({
  id: 7, name: "Holz MacBook", platform: "macos", created_at: iso(86_400 * 5), last_seen: iso(30), expires_at: iso(-86_400 * 80), ...over,
});

test("readDevices reads the person's rows, keeps only the members it uses, and has no owner", () => {
  const rows = readDevices({ devices: [device({ token_hash: "x", secret: "y" }), device({ id: 8, name: "Pixel 8", platform: "android", last_seen: iso(3 * 3600) })] }, false);
  assert.equal(rows.length, 2);
  assert.deepEqual(rows[0], { id: "7", name: "Holz MacBook", platform: "macOS", lastSeenMs: NOW - 30_000, owner: null });
  assert.deepEqual(Object.keys(rows[0]).sort(), ["id", "lastSeenMs", "name", "owner", "platform"]);
  assert.equal(rows[1].platform, "Android");
  assert.deepEqual(readDevices({ devices: [] }, false), []);
});

test("readDevices for an administrator needs and keeps the owner", () => {
  const rows = readDevices({ devices: [device({ username: "holz", display_name: "Holz" }), device({ id: 9, username: "", display_name: "Anna" })] }, true);
  assert.equal(rows[0].owner, "holz");
  assert.equal(rows[1].owner, "Anna", "the display name stands in when there is no user name");
  assert.equal(readDevices({ devices: [device()] }, true), null, "an administrator's row without an owner is not the documented shape");
});

test("readDevices refuses a list that is not the documented shape or holds more than the page takes", () => {
  const bad = [
    null, [], "x", {}, { devices: null }, { devices: {} }, { devices: "x" },
    { devices: [device({ id: -1 })] }, { devices: [device({ id: 1.5 })] }, { devices: [device({ id: "" })] }, { devices: [device({ id: null })] },
    { devices: [device({ name: "" })] }, { devices: [device({ name: 3 })] }, { devices: [device({ platform: "ios" })] },
    { devices: [device({ created_at: "never" })] }, { devices: [device({ last_seen: "soon" })] }, { devices: [device({ last_seen: 5 })] },
    { devices: [null] }, { devices: ["x"] }, { devices: [device(), 7] },
    { devices: Array.from({ length: MAX_PERSON_DEVICES + 1 }, (_, i) => device({ id: i })) },
  ];
  for (const raw of bad) assert.equal(readDevices(raw, false), null, JSON.stringify(raw)?.slice(0, 80));
  assert.equal(readDevices({ devices: Array.from({ length: MAX_PERSON_DEVICES }, (_, i) => device({ id: i })) }, false).length, MAX_PERSON_DEVICES);
  const many = Array.from({ length: MAX_ALL_DEVICES + 1 }, (_, i) => device({ id: i, username: "u" }));
  assert.equal(readDevices({ devices: many }, true), null, "an administrator's list has a bound too");
  assert.equal(readDevices({ devices: many.slice(0, MAX_ALL_DEVICES) }, true).length, MAX_ALL_DEVICES);
});

test("a device that was never seen has no time, and the time words say never", () => {
  for (const last of [null, undefined, "1970-01-01T00:00:00Z"]) {
    const rows = readDevices({ devices: [device({ last_seen: last })] }, false);
    assert.equal(rows[0].lastSeenMs, null, String(last));
    assert.equal(metaText(rows[0], NOW), "macOS · never");
  }
  assert.equal(NEVER, "never");
  assert.equal(lastSeenText(null, NOW), "never");
});

test("the time words reuse the account dialog's words", () => {
  assert.equal(lastSeenText(NOW - 5_000, NOW), "just now");
  assert.equal(lastSeenText(NOW - 3 * 60_000, NOW), "3 min ago");
  assert.equal(lastSeenText(NOW - 3 * 3_600_000, NOW), "3 h ago");
  assert.equal(lastSeenText(NOW - 86_400_000, NOW), "1 day ago");
  assert.equal(lastSeenText(NOW - 2 * 86_400_000, NOW), "2 days ago");
  assert.equal(lastSeenText(NOW + 60_000, NOW), "just now", "a time in the future is now");
  assert.equal(requestedText(NOW - 20_000, NOW), "just now");
  assert.equal(requestedText(NOW - 3 * 60_000, NOW), "3 min ago");
  const row = { id: "1", name: "n", platform: "macOS", lastSeenMs: NOW - 3 * 60_000, owner: null };
  assert.equal(metaText(row, NOW), "macOS · 3 min ago");
  assert.equal(NO_DEVICES, "No devices.");
});

test("the devices are ordered by last seen, newest first, and a device never seen comes last", () => {
  const row = (id, ms) => ({ id, name: id, platform: "macOS", lastSeenMs: ms, owner: null });
  const out = orderDevices([row("a", null), row("b", 100), row("c", 300), row("d", 300), row("e", null)]);
  assert.deepEqual(out.map((r) => r.id), ["c", "d", "b", "a", "e"]);
});

test("platformWord knows the closed list and nothing on the prototype", () => {
  assert.equal(platformWord("macos"), "macOS");
  for (const v of ["toString", "__proto__", "hasOwnProperty", "", "MACOS", 3, null]) assert.equal(platformWord(v), null, String(v));
});

// ---- the two switches ------------------------------------------------------------------------------------------------------------

test("readAccess reads the documented shape and a switch that is not allowed is off and locked whatever it says", () => {
  assert.deepEqual(readAccess({ sync: { on: true, allowed: true }, remote: { on: false, allowed: true } }), {
    sync: { on: true, locked: false },
    remote: { on: false, locked: false },
  });
  assert.deepEqual(readAccess({ sync: { on: true, allowed: false }, remote: { on: false, allowed: false } }), {
    sync: { on: false, locked: true },
    remote: { on: false, locked: true },
  });
  const extra = readAccess({ sync: { on: false, allowed: true, x: 1 }, remote: { on: false, allowed: true }, other: {} });
  assert.deepEqual(Object.keys(extra).sort(), ["remote", "sync"]);
  assert.deepEqual(Object.keys(extra.sync).sort(), ["locked", "on"]);
});

test("readAccess refuses everything that is not the documented shape", () => {
  const ok = { on: false, allowed: true };
  const bad = [
    null, undefined, [], "on", 1, {}, { sync: ok }, { remote: ok }, { sync: ok, remote: null }, { sync: ok, remote: "on" },
    { sync: ok, remote: { on: "yes", allowed: true } }, { sync: ok, remote: { on: false } }, { sync: ok, remote: { allowed: true } },
    { sync: ok, remote: { on: 1, allowed: 1 } }, { sync: [], remote: ok }, { sync: ok, remote: [ok] },
  ];
  for (const raw of bad) assert.equal(readAccess(raw), null, JSON.stringify(raw));
});

test("a request to change a switch carries that switch and nothing else", () => {
  assert.deepEqual(accessBody("sync", true), { sync: true });
  assert.deepEqual(accessBody("remote", false), { remote: false });
  assert.deepEqual(SWITCHES.map((s) => s.key), ["sync", "remote"], "the order of the group");
});

// ---- what a refusal says ------------------------------------------------------------------------------------------------------------

test("an unknown, used, expired, malformed or hidden code gives the one sentence; busy and slow are said as they are", () => {
  for (const status of [400, 401, 404, 409, 410, 403]) assert.equal(codeMessage(status), NOT_VALID, String(status));
  assert.equal(codeMessage(404, { code: "not_found" }), NOT_VALID);
  assert.match(codeMessage(429, { retry_seconds: 600 }), /^Too many attempts\. Try again in 10 minutes\.$/);
  assert.equal(codeMessage(0), "Could not reach the server. Try again.");
  assert.equal(codeMessage(503), "The server is busy. Try again in a moment.");
  assert.equal(codeMessage(500), "Could not reach the server. Try again.");
  // Nothing from the body reaches the sentence but the wait.
  assert.equal(codeMessage(404, { message: "<b>feature remote is off</b>" }), NOT_VALID);
});

test("a failed change says the generic sentence, or the busy and slow ones", () => {
  assert.equal(deviceChangeMessage(400), CHANGE_FAILED);
  assert.equal(deviceChangeMessage(404), CHANGE_FAILED);
  assert.equal(deviceChangeMessage(409, { field: "remote" }), CHANGE_FAILED);
  assert.equal(deviceChangeMessage(0), "Could not reach the server. Try again.");
  assert.match(deviceChangeMessage(429, { retry_seconds: 30 }), /30 seconds/);
});

test("the Devices group and the Account dialog do not import each other", () => {
  const src = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "src");
  const imports = (file) => [...readFileSync(join(src, file), "utf8").matchAll(/from "\.\/([a-z]+)\.js"/g)].map((m) => m[1]);
  assert.ok(imports("account.ts").includes("devices"), "the Account dialog builds the Devices group");
  assert.ok(!imports("devices.ts").includes("account"), "so the Devices group takes the button that asks twice from dom.ts");
});
