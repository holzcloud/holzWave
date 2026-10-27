// The pure part of the people dialog (peoplestate.ts, docs/design.md 6.29): the reading of `GET /api/admin/accounts`, the state
// words with their glyphs and the time words of the last sign-in.
import test from "node:test";
import assert from "node:assert/strict";
import { MAX_ACCOUNTS, lastSigninText, readAccounts, stateWord } from "../../dist/peoplestate.js";

const NOW = Date.parse("2026-10-27T12:00:00Z");
const iso = (secondsAgo) => new Date(NOW - secondsAgo * 1000).toISOString();

const row = (over = {}) => ({
  id: "a1",
  username: "anna",
  display_name: "Anna Keller",
  role: "admin",
  state: "active",
  last_signin: iso(300),
  created_at: iso(900_000),
  ...over,
});
const answer = (over = {}) => ({ admins: 1, accounts: [row(), row({ id: "a2", username: "ben", display_name: "Ben Ito", role: "member", last_signin: null })], ...over });

test("readAccounts reads the documented shape and keeps only name, role, state and last sign-in", () => {
  const read = readAccounts(answer());
  assert.equal(read.admins, 1);
  assert.equal(read.rows.length, 2);
  assert.deepEqual(read.rows[0], {
    id: "a1",
    username: "anna",
    displayName: "Anna Keller",
    role: "admin",
    state: "active",
    lastSigninMs: NOW - 300_000,
  });
  assert.equal(read.rows[1].lastSigninMs, null);
  // A member of the answer that the page does not use is not carried along.
  const extra = readAccounts(answer({ accounts: [row({ secret: "x", scrollback: "y", layout: {} })] }));
  assert.deepEqual(Object.keys(extra.rows[0]).sort(), ["displayName", "id", "lastSigninMs", "role", "state", "username"]);
});

test("readAccounts gives null for more than 500 accounts and takes exactly 500", () => {
  const many = (n) => Array.from({ length: n }, (_, i) => row({ id: `a${i}` }));
  assert.equal(readAccounts(answer({ accounts: many(MAX_ACCOUNTS + 1) })), null);
  assert.equal(readAccounts(answer({ accounts: many(MAX_ACCOUNTS) })).rows.length, 500);
  assert.equal(MAX_ACCOUNTS, 500);
});

test("readAccounts gives null for a name over 128 characters and takes exactly 128", () => {
  assert.equal(readAccounts(answer({ accounts: [row({ display_name: "x".repeat(129) })] })), null);
  assert.equal(readAccounts(answer({ accounts: [row({ username: "x".repeat(129) })] })), null);
  assert.equal(readAccounts(answer({ accounts: [row({ display_name: "x".repeat(128), username: "y".repeat(128) })] })).rows.length, 1);
});

test("readAccounts gives null for a role, a state, a time or an id of the wrong kind", () => {
  assert.notEqual(readAccounts(answer()), null, "the same answer with good rows is read");
  for (const bad of [
    row({ role: "owner" }),
    row({ role: "ADMIN" }),
    row({ role: null }),
    row({ state: "deleted" }),
    row({ state: "locked out" }),
    row({ state: 1 }),
    row({ last_signin: 5 }),
    row({ last_signin: "not a time" }),
    row({ last_signin: undefined }),
    row({ id: "" }),
    row({ id: "i".repeat(129) }),
    row({ display_name: 7 }),
    "anna",
    null,
    [],
  ]) {
    assert.equal(readAccounts(answer({ accounts: [bad] })), null, JSON.stringify(bad));
  }
});

test("readAccounts gives null when the answer is not of the shape: no admins, no list, a list that is not a list", () => {
  const { admins, ...without } = answer();
  assert.ok(admins === 1);
  assert.equal(readAccounts(without), null, "a missing admins");
  for (const bad of [-1, 1.5, "1", null, Number.NaN, 10 ** 9]) assert.equal(readAccounts(answer({ admins: bad })), null, String(bad));
  assert.equal(readAccounts({ admins: 1 }), null);
  assert.equal(readAccounts({ admins: 1, accounts: {} }), null);
  for (const bad of [null, undefined, "x", 3, [], true]) assert.equal(readAccounts(bad), null);
  assert.equal(readAccounts(answer({ accounts: [] })).rows.length, 0, "an empty list is a list");
});

test("stateWord gives each state its own word and its own glyph, and shows no unknown value", () => {
  assert.deepEqual(stateWord("active"), { word: "active", glyph: "●", tone: "ok" });
  assert.deepEqual(stateWord("disabled"), { word: "disabled", glyph: "○", tone: "dim" });
  assert.deepEqual(stateWord("locked_out"), { word: "locked out", glyph: "!", tone: "warn" });
  const glyphs = new Set(["active", "disabled", "locked_out"].map((s) => stateWord(s).glyph));
  assert.equal(glyphs.size, 3, "no two states share a glyph");
  for (const unknown of ["", "deleted", "locked out", "ACTIVE", "__proto__", "constructor", "toString"]) assert.equal(stateWord(unknown), null, unknown);
});

test("lastSigninText: never, just now, minutes, hours, days and weeks", () => {
  assert.equal(lastSigninText(null, NOW), "never");
  assert.equal(lastSigninText(NOW - 300_000, NOW), "5 min ago");
  assert.equal(lastSigninText(NOW - 10_000, NOW), "just now");
  assert.equal(lastSigninText(NOW + 60_000, NOW), "just now", "a time in the future");
  assert.equal(lastSigninText(NOW - 5 * 3_600_000, NOW), "5 h ago");
  assert.equal(lastSigninText(NOW - 30 * 3_600_000, NOW), "yesterday");
  assert.equal(lastSigninText(NOW - 4 * 86_400_000, NOW), "4 days ago");
  assert.equal(lastSigninText(NOW - 21 * 86_400_000, NOW), "3 weeks ago");
  assert.equal(lastSigninText(Number.NaN, NOW), "never");
});
