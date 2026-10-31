// The pure part of the people dialog (peoplestate.ts, docs/design.md 6.29): the reading of `GET /api/admin/accounts`, the state
// words with their glyphs and the time words of the last sign-in.
import test from "node:test";
import assert from "node:assert/strict";
import {
  ADMIN_ONLY,
  DEFAULT_DAYS,
  INVITE_DAYS,
  LIST_NOTE,
  MAX_ACCOUNTS,
  ONE_ADMIN,
  ONLY_ADMIN,
  confirmFor,
  inviteBody,
  inviteNote,
  lastSigninText,
  menuFor,
  outcomeOf,
  readAccounts,
  readInvitation,
  requestFor,
  stateWord,
} from "../../dist/peoplestate.js";

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

test("readAccounts takes the whole-number id the server sends and keeps its digits", () => {
  const read = readAccounts(answer({ accounts: [row({ id: 7 }), row({ id: 0 })] }));
  assert.deepEqual(read.rows.map((r) => r.id), ["7", "0"]);
  for (const bad of [-1, 1.5, Number.NaN, 2 ** 53, true, null]) {
    assert.equal(readAccounts(answer({ accounts: [row({ id: bad })] })), null, String(bad));
  }
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

test("the invitation: five expiry choices with 7 at first, a body of role and days only, and the sentence over the link", () => {
  assert.deepEqual([...INVITE_DAYS], [1, 3, 7, 14, 30]);
  assert.equal(DEFAULT_DAYS, 7);
  assert.deepEqual(inviteBody("admin", 3), { role: "admin", days: 3 });
  assert.deepEqual(inviteBody("member", 7), { role: "member", days: 7 });
  assert.deepEqual(inviteBody("member", 99), { role: "member", days: 7 }, "a number that is no choice is the first choice");
  assert.equal(inviteNote(7, "member"), "Shown once. Copy it now and send it to the person. It works for 7 days, as a member.");
  assert.equal(inviteNote(1, "admin"), "Shown once. Copy it now and send it to the person. It works for 1 day, as an administrator.");
});

test("readInvitation takes a link of one piece and bounded length, and nothing else", () => {
  assert.equal(readInvitation({ link: "x/#invite=hwi_abc", expires_at: "2026-11-01T00:00:00Z" }), "x/#invite=hwi_abc");
  for (const bad of [{}, { link: "" }, { link: 5 }, { link: "a b" }, { link: "a\nb" }, { link: "x".repeat(2049) }, null, "link", []]) assert.equal(readInvitation(bad), null, JSON.stringify(bad));
  assert.equal(readInvitation({ link: "x".repeat(2048) }).length, 2048);
});

test("the sentence under the table says what the list never shows", () => {
  assert.equal(LIST_NOTE, "This list shows names, roles and states only. It never shows anyone's secrets, terminals, commands or layout.");
});

// ---- the menu of a row, the questions and the refusals ------------------------------------------------------------------------

const rowOf = (over = {}) => ({ id: "a1", username: "ben", displayName: "Ben Ito", role: "member", state: "active", lastSigninMs: null, ...over });
const labels = (entries) => entries.map((e) => e.label);
const reasons = (entries) => Object.fromEntries(entries.filter((e) => e.reason !== null).map((e) => [e.label, e.reason]));

test("menuFor: an active member offers Make admin, Disable, Reset secrets and Delete, and none of them is held back", () => {
  const entries = menuFor(rowOf(), 1);
  assert.deepEqual(labels(entries), ["Make admin", "Disable", "Reset secrets", "Delete"]);
  assert.deepEqual(entries.map((e) => e.id), ["role", "disable", "reset", "delete"]);
  assert.deepEqual(reasons(entries), {});
});

test("menuFor: a disabled member offers Enable instead of Disable", () => {
  assert.deepEqual(labels(menuFor(rowOf({ state: "disabled" }), 1)), ["Make admin", "Enable", "Reset secrets", "Delete"]);
  assert.deepEqual(menuFor(rowOf({ state: "disabled" }), 1).map((e) => e.id), ["role", "enable", "reset", "delete"]);
});

test("menuFor: Unlock sign-in is there for a locked out person and for nobody else", () => {
  const locked = menuFor(rowOf({ state: "locked_out" }), 1);
  assert.deepEqual(labels(locked), ["Make admin", "Disable", "Unlock sign-in", "Reset secrets", "Delete"]);
  for (const state of ["active", "disabled"]) assert.ok(!labels(menuFor(rowOf({ state }), 1)).includes("Unlock sign-in"), state);
  for (const role of ["admin", "member"]) assert.equal(labels(menuFor(rowOf({ role, state: "locked_out" }), 2)).filter((l) => l === "Unlock sign-in").length, 1, role);
});

test("menuFor: an administrator offers Make member in the place of Make admin", () => {
  const entries = menuFor(rowOf({ role: "admin" }), 2);
  assert.deepEqual(labels(entries), ["Make member", "Disable", "Reset secrets", "Delete"]);
  assert.deepEqual(reasons(entries), {}, "with another active administrator nothing is held back");
});

test("menuFor: the only active administrator keeps Make member, Disable and Delete in the list, dim, with the reason", () => {
  const entries = menuFor(rowOf({ role: "admin" }), 1);
  assert.deepEqual(labels(entries), ["Make member", "Disable", "Reset secrets", "Delete"]);
  assert.deepEqual(reasons(entries), { "Make member": ONLY_ADMIN, Disable: ONLY_ADMIN, Delete: ONLY_ADMIN });
  assert.equal(ONLY_ADMIN, "The only active administrator.");
  assert.equal(entries.find((e) => e.id === "reset").reason, null, "Reset secrets of the only administrator is allowed");
});

test("menuFor: a disabled administrator is never the active one, and a member never is", () => {
  assert.deepEqual(reasons(menuFor(rowOf({ role: "admin", state: "disabled" }), 1)), {});
  assert.deepEqual(labels(menuFor(rowOf({ role: "admin", state: "disabled" }), 1)), ["Make member", "Enable", "Reset secrets", "Delete"]);
  assert.deepEqual(reasons(menuFor(rowOf({ role: "member" }), 1)), {});
  assert.deepEqual(reasons(menuFor(rowOf({ role: "admin" }), 0)), { "Make member": ONLY_ADMIN, Disable: ONLY_ADMIN, Delete: ONLY_ADMIN }, "a count of none is no cover");
  assert.deepEqual(reasons(menuFor(rowOf({ role: "admin", state: "locked_out" }), 1)), { "Make member": ONLY_ADMIN, Disable: ONLY_ADMIN, Delete: ONLY_ADMIN }, "a locked out administrator is still the one the server counts");
});

test("menuFor: only Reset secrets and Delete are drawn in the danger style, and they come last", () => {
  const entries = menuFor(rowOf({ state: "locked_out" }), 1);
  assert.deepEqual(entries.filter((e) => e.danger).map((e) => e.id), ["reset", "delete"]);
  assert.deepEqual(entries.slice(-2).map((e) => e.id), ["reset", "delete"]);
});

test("confirmFor: Delete and Reset secrets name the person and say what is lost; every other entry gives null", () => {
  const del = confirmFor("delete", "Ben Ito");
  assert.equal(del.title, "Delete Ben Ito?");
  assert.equal(del.button, "Delete");
  assert.equal(del.text, "They can no longer sign in and their terminals stop. Their files stay on the server, and nobody can open their saved passwords and keys again.");
  const reset = confirmFor("reset", "Ben Ito");
  assert.equal(reset.title, "Reset secrets of Ben Ito?");
  assert.equal(reset.button, "Reset secrets");
  assert.equal(reset.text, "Their saved passwords and keys are deleted. Their account, layout and history stay.");
  for (const entry of ["role", "disable", "enable", "unlock", "", "constructor"]) assert.equal(confirmFor(entry, "Ben"), null, entry);
  assert.equal(confirmFor("delete", "<img src=x>").title, "Delete <img src=x>?", "a name is text, whatever it holds");
});

test("requestFor: the path and the body of each entry, with an id that is a path piece", () => {
  const row = rowOf({ id: "a/b c" });
  assert.deepEqual(requestFor("role", row), { method: "POST", path: "/api/admin/accounts/a%2Fb%20c/role", body: { role: "admin" } });
  assert.deepEqual(requestFor("role", rowOf({ role: "admin" })), { method: "POST", path: "/api/admin/accounts/a1/role", body: { role: "member" } });
  for (const [entry, word] of [["disable", "disable"], ["enable", "enable"], ["unlock", "unlock-signin"], ["reset", "reset-secrets"]]) {
    assert.deepEqual(requestFor(entry, rowOf()), { method: "POST", path: `/api/admin/accounts/a1/${word}`, body: {} }, entry);
  }
  assert.deepEqual(requestFor("delete", rowOf()), { method: "DELETE", path: "/api/admin/accounts/a1" });
});

test("outcomeOf: a 403 that is no step-up locks the dialog, a 409 on the role says why, a 404 reloads, the rest is a plain sentence", () => {
  assert.deepEqual(outcomeOf(403, { code: "forbidden" }), { kind: "locked" });
  assert.deepEqual(outcomeOf(403), { kind: "locked" });
  assert.equal(ADMIN_ONLY, "Only administrators can open this.");
  assert.deepEqual(outcomeOf(403, { code: "forbidden", step_up: true }), { kind: "say", text: "That change was not made. Try again." }, "a second refusal for a recent sign-in is the change's own");
  assert.deepEqual(outcomeOf(409, { code: "conflict", field: "role" }), { kind: "say", text: ONE_ADMIN });
  assert.equal(ONE_ADMIN, "There must be one active administrator.");
  assert.deepEqual(outcomeOf(409, { code: "conflict" }), { kind: "say", text: "That change was not made. Try again." });
  assert.deepEqual(outcomeOf(404, { code: "not_found" }), { kind: "gone" });
  assert.deepEqual(outcomeOf(0), { kind: "say", text: "Could not reach the server. Try again." });
  assert.deepEqual(outcomeOf(502), { kind: "say", text: "Could not reach the server. Try again." });
  assert.equal(outcomeOf(429, { retry_seconds: 60 }).text.startsWith("Too many attempts."), true);
  assert.deepEqual(outcomeOf(400, { code: "bad_request" }), { kind: "say", text: "That change was not made. Try again." });
});
