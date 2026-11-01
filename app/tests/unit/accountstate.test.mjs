// The pure part of the account dialog (accountstate.ts, docs/design.md 6.29): the reading of `GET /api/me`, the state words of
// the groups, the rows of the sessions, the time words and the sentence for each refusal of a change.
import test from "node:test";
import assert from "node:assert/strict";
import {
  CHANGE_FAILED,
  LAST_WAY,
  LOCKED_FIRST,
  SAVED,
  agoText,
  changeMessage,
  groupStates,
  readMe,
  sessionRows,
  usedText,
} from "../../dist/accountstate.js";

const NOW = Date.parse("2026-10-27T12:00:00Z");
const iso = (secondsAgo) => new Date(NOW - secondsAgo * 1000).toISOString();

const ME = {
  account: { id: 7, username: "holz", display_name: "Holz", role: "admin" },
  oidc: null,
  password: true,
  totp: true,
  passkeys: [
    { id: 1, name: "MacBook", created_at: iso(900_000), last_used: iso(100) },
    { id: 2, name: "Phone", created_at: iso(900_000), last_used: null },
  ],
  secrets: { state: "unlocked", passphrase: false, recovery: true },
  sessions: [
    { id: "s1", device: "Chrome on Linux", addr: "198.51.100.24", created_at: iso(20_000), last_seen: iso(10_800), current: false },
    { id: "s2", device: "Firefox on macOS", addr: "203.0.113.7", created_at: iso(30_000), last_seen: iso(5), current: true },
    { id: "s3", device: "Safari on iOS", addr: "192.0.2.9", created_at: iso(40_000), last_seen: iso(600), current: false },
  ],
};
const me = (over = {}) => readMe({ ...ME, ...over });

test("readMe_reads_the_documented_shape", () => {
  const read = readMe(ME);
  assert.deepEqual(read.account, { id: "7", username: "holz", displayName: "Holz", role: "admin" });
  assert.equal(read.password, true);
  assert.equal(read.totp, true);
  assert.equal(read.oidc, null);
  assert.equal(read.passkeys.length, 2);
  assert.equal(read.passkeys[0].lastUsedMs, NOW - 100_000);
  assert.equal(read.passkeys[1].lastUsedMs, null);
  assert.deepEqual(read.secrets, { state: "unlocked", passphrase: false, recovery: true });
  assert.equal(read.sessions.length, 3);
  assert.equal(readMe({ ...ME, oidc: { id: "corp", name: "Company SSO" } }).oidc.name, "Company SSO");
});

test("readMe_reads_the_ids_the_way_the_server_sends_them", () => {
  // The server numbers its accounts and passkeys (JSON numbers); the page keeps them as digits, the form they have in a path.
  const read = readMe(ME);
  assert.equal(read.account.id, "7");
  assert.deepEqual(read.passkeys.map((k) => k.id), ["1", "2"]);
  // A bounded string is still an id; a fraction, a negative number or a number past the safe range is not.
  assert.equal(me({ account: { ...ME.account, id: "a1" } }).account.id, "a1");
  assert.equal(me({ passkeys: [{ ...ME.passkeys[0], id: "k1" }] }).passkeys[0].id, "k1");
  for (const bad of [1.5, -1, 2 ** 60, Number.NaN, null, true, {}, [], ""]) {
    assert.equal(me({ account: { ...ME.account, id: bad } }), null, `account id ${String(bad)}`);
    assert.equal(me({ passkeys: [{ ...ME.passkeys[0], id: bad }] }), null, `passkey id ${String(bad)}`);
  }
});

test("readMe_refuses_lists_that_are_too_long", () => {
  const key = (i) => ({ id: `k${i}`, name: "x", created_at: iso(1), last_used: null });
  assert.equal(me({ passkeys: Array.from({ length: 64 }, (_, i) => key(i)) }).passkeys.length, 64);
  assert.equal(me({ passkeys: Array.from({ length: 65 }, (_, i) => key(i)) }), null);
  const row = (i) => ({ id: `s${i}`, device: "d", addr: "a", created_at: iso(1), last_seen: iso(1), current: false });
  assert.equal(me({ sessions: Array.from({ length: 64 }, (_, i) => row(i)) }).sessions.length, 64);
  assert.equal(me({ sessions: Array.from({ length: 65 }, (_, i) => row(i)) }), null);
  assert.equal(me({ passkeys: "none" }), null);
  assert.equal(me({ sessions: null }), null);
});

test("readMe_refuses_names_over_the_limit_and_a_wrong_type", () => {
  const key = (name) => ({ id: "k", name, created_at: iso(1), last_used: null });
  assert.equal(me({ passkeys: [key("x".repeat(128))] }).passkeys[0].name.length, 128);
  assert.equal(me({ passkeys: [key("x".repeat(129))] }), null);
  assert.equal(me({ passkeys: [key(7)] }), null);
  assert.equal(me({ account: { ...ME.account, display_name: "x".repeat(129) } }), null);
  assert.equal(me({ account: { ...ME.account, username: "x".repeat(1024 * 1024) } }), null);
  assert.equal(me({ sessions: [{ ...ME.sessions[0], device: "x".repeat(257) }] }), null);
  assert.equal(me({ sessions: [{ ...ME.sessions[0], addr: "x".repeat(65) }] }), null);
  assert.equal(me({ sessions: [{ ...ME.sessions[0], id: "" }] }), null);
});

test("readMe_refuses_a_role_that_is_not_admin_or_member_and_a_missing_account", () => {
  for (const role of ["root", "", null, 1, "ADMIN"]) assert.equal(me({ account: { ...ME.account, role } }), null, String(role));
  assert.equal(me({ account: { ...ME.account, role: "member" } }).account.role, "member");
  assert.equal(readMe({ ...ME, account: undefined }), null);
  assert.equal(readMe({ ...ME, account: null }), null);
  assert.equal(readMe(null), null);
  assert.equal(readMe("me"), null);
  assert.equal(readMe([]), null);
});

test("readMe_refuses_a_current_that_is_not_boolean_and_times_that_are_not_times", () => {
  for (const current of ["true", 1, null, undefined]) assert.equal(me({ sessions: [{ ...ME.sessions[0], current }] }), null, String(current));
  assert.equal(me({ sessions: [{ ...ME.sessions[0], last_seen: "yesterday" }] }), null);
  assert.equal(me({ sessions: [{ ...ME.sessions[0], created_at: 5 }] }), null);
  assert.equal(me({ passkeys: [{ id: "k", name: "n", created_at: "x", last_used: null }] }), null);
  assert.equal(me({ passkeys: [{ id: "k", name: "n", created_at: iso(1), last_used: "x" }] }), null);
  assert.equal(me({ password: "yes" }), null);
  assert.equal(me({ secrets: { state: "open", passphrase: true, recovery: true } }), null);
  assert.equal(me({ secrets: { state: "locked", passphrase: 1, recovery: true } }), null);
  assert.equal(me({ oidc: "corp" }), null);
});

test("agoText_follows_the_table", () => {
  const table = [
    [0, "just now"],
    [44, "just now"],
    [45, "1 min ago"],
    [60, "1 min ago"],
    [119, "1 min ago"],
    [120, "2 min ago"],
    [3599, "59 min ago"],
    [3600, "1 h ago"],
    [10_800, "3 h ago"],
    [86_399, "23 h ago"],
    [86_400, "1 day ago"],
    [172_800, "2 days ago"],
    [3 * 86_400 + 5, "3 days ago"],
    [-30, "just now"],
    [Number.NaN, "just now"],
  ];
  for (const [seconds, text] of table) assert.equal(agoText(seconds), text, `${seconds} s`);
});

test("usedText_says_never_today_or_how_long_ago", () => {
  assert.equal(usedText(null, NOW), "not used yet");
  assert.equal(usedText(NOW - 100_000, NOW), "used today");
  assert.equal(usedText(NOW - 86_399_000, NOW), "used today");
  assert.equal(usedText(NOW - 3 * 86_400_000, NOW), "used 3 days ago");
  assert.equal(usedText(NOW + 5000, NOW), "used today", "a time in the future is today");
});

test("groupStates_gives_the_words_of_a_full_account", () => {
  const groups = groupStates(readMe(ME));
  assert.deepEqual(groups.map((g) => g.id), ["password", "totp", "passkeys", "passphrase", "recovery"]);
  const by = Object.fromEntries(groups.map((g) => [g.id, g]));
  assert.equal(by.password.state, "set");
  assert.equal(by.password.button.label, "Change");
  assert.equal(by.totp.state, "set");
  assert.equal(by.totp.button.label, "Replace");
  assert.equal(by.passkeys.name, "2 passkeys");
  assert.equal(by.passkeys.state, "saved");
  assert.equal(by.passkeys.button.label, "Add passkey");
  assert.equal(by.passphrase.state, "not set");
  assert.equal(by.passphrase.tone, "dim");
  assert.equal(by.passphrase.button.label, "Set");
  assert.equal(by.recovery.state, "saved");
  assert.equal(by.recovery.tone, "ok");
  assert.equal(by.recovery.button.label, "Make a new code");
  assert.equal(by.recovery.legend, "Recovery code");
  assert.equal(by.passphrase.legend, "Unlock passphrase");
});

test("groupStates_names_the_passkeys_by_count", () => {
  const name = (n) => groupStates(me({ passkeys: Array.from({ length: n }, (_, i) => ({ id: `k${i}`, name: "n", created_at: iso(1), last_used: null })) })).find((g) => g.id === "passkeys");
  assert.deepEqual([name(0).name, name(0).state, name(0).tone], ["No passkeys", "none", "dim"]);
  assert.deepEqual([name(1).name, name(1).state], ["1 passkey", "saved"]);
  assert.equal(name(3).name, "3 passkeys");
});

test("groupStates_draws_only_the_groups_that_apply", () => {
  const providerOnly = groupStates(me({ password: false, totp: false, passkeys: [], oidc: { id: "corp", name: "Company SSO" } }));
  assert.deepEqual(providerOnly.map((g) => g.id), ["passkeys", "passphrase", "recovery"]);
  const passwordOnly = groupStates(me({ totp: false }));
  assert.ok(passwordOnly.some((g) => g.id === "password") && !passwordOnly.some((g) => g.id === "totp"));
});

test("groupStates_offers_unlock_instead_of_a_change_while_the_store_is_locked", () => {
  const locked = groupStates(me({ secrets: { state: "locked", passphrase: true, recovery: true } }));
  const by = Object.fromEntries(locked.map((g) => [g.id, g]));
  assert.deepEqual(by.passphrase.button, { label: "Unlock", action: "unlock" });
  assert.deepEqual(by.recovery.button, { label: "Unlock", action: "unlock" });
  assert.equal(by.passphrase.state, "set", "the state word still says what is kept");
  assert.equal(by.password.button.action, "change", "a password change does not need the store");
  const open = Object.fromEntries(groupStates(me({ secrets: { state: "unlocked", passphrase: true, recovery: false } })).map((g) => [g.id, g]));
  assert.deepEqual(open.passphrase.button, { label: "Change", action: "change" });
  assert.equal(open.recovery.state, "none");
});

test("sessionRows_puts_this_session_first_and_the_rest_by_last_seen", () => {
  const rows = sessionRows(readMe(ME));
  assert.deepEqual(rows.map((r) => r.id), ["s2", "s3", "s1"]);
  assert.deepEqual(rows.map((r) => r.current), [true, false, false], "`this session` is only on the current one");
  assert.equal(readMe(ME).sessions[0].id, "s1", "the list of the answer is not reordered in place");
  assert.deepEqual(sessionRows(me({ sessions: [] })), []);
});

test("changeMessage_chooses_the_words_for_each_refusal", () => {
  assert.deepEqual(changeMessage("password", 401), { text: "Sign-in failed. Check your details or ask an admin.", field: "current" });
  assert.deepEqual(changeMessage("totp", 401), { text: "That code did not match. Try again.", field: "code" });
  assert.deepEqual(changeMessage("password", 400, { code: "bad_request", field: "new" }), {
    text: "Use at least 12 characters. Not your username, not one repeated character.",
    field: "new",
  });
  assert.equal(changeMessage("passphrase", 400, { field: "passphrase" }).text.startsWith("Use at least 12"), true);
  assert.equal(changeMessage("passkey-remove", 409, { code: "conflict", field: "method" }).text, LAST_WAY);
  assert.equal(LAST_WAY, "This is your last way to sign in. Add another first.");
  assert.equal(changeMessage("recovery", 423).text, LOCKED_FIRST);
  assert.equal(changeMessage("password", 429, { retry_seconds: 720 }).text, "Too many attempts. Try again in 12 minutes.");
  assert.equal(changeMessage("session", 0).text, "Could not reach the server. Try again.");
  assert.equal(changeMessage("session", 503).text, "The server is busy. Try again in a moment.");
  assert.equal(changeMessage("session", 404).text, CHANGE_FAILED);
  assert.equal(changeMessage("passkey-add", 409, { field: "other" }).text, CHANGE_FAILED);
  assert.equal(changeMessage("passphrase", 400, { field: "other" }).text, CHANGE_FAILED);
  assert.equal(SAVED, "Saved.");
});
