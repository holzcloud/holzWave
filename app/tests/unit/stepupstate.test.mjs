// The pure part of `Confirm it is you` (stepupstate.ts, docs/design.md 6.29): when a refused change asks for a recent
// sign-in, which ways an account has to give one, and the reading of the account for the layer.
import test from "node:test";
import assert from "node:assert/strict";
import { NO_METHOD, STEP_UP_TITLE, leadParts, needsStepUp, providerPath, readStepUpMe, stepUpMethods } from "../../dist/stepupstate.js";
import { HttpError } from "../../dist/webhttp.js";

test("needsStepUp_is_true_only_for_a_403_that_says_step_up", () => {
  assert.equal(needsStepUp(new HttpError(403, "forbidden", { code: "forbidden", step_up: true })), true);
  assert.equal(needsStepUp({ status: 403, body: { step_up: true } }), true);
});

test("needsStepUp_is_false_for_a_403_without_the_member_or_with_another_value", () => {
  assert.equal(needsStepUp(new HttpError(403, "forbidden", { code: "forbidden" })), false);
  assert.equal(needsStepUp(new HttpError(403, "forbidden", { code: "forbidden", step_up: false })), false);
  assert.equal(needsStepUp(new HttpError(403, "forbidden", { step_up: "true" })), false);
  assert.equal(needsStepUp(new HttpError(403, "forbidden")), false);
});

test("needsStepUp_is_false_for_another_status_a_string_or_nothing", () => {
  assert.equal(needsStepUp(new HttpError(401, "invalid", { step_up: true })), false);
  assert.equal(needsStepUp(new HttpError(0, "cannot reach the server")), false);
  assert.equal(needsStepUp("403"), false);
  assert.equal(needsStepUp(null), false);
  assert.equal(needsStepUp(undefined), false);
  assert.equal(needsStepUp(new TypeError("x")), false);
  assert.equal(needsStepUp([403]), false);
});

test("stepUpMethods_lists_passkey_then_password_then_provider", () => {
  const me = { password: true, passkeys: 2, oidc: { id: "corp", name: "Company SSO" } };
  assert.deepEqual(stepUpMethods(me).map((m) => m.kind), ["passkey", "password", "provider"]);
  assert.deepEqual(stepUpMethods({ ...me, passkeys: [{ id: "a" }] }).map((m) => m.kind), ["passkey", "password", "provider"]);
  assert.deepEqual(stepUpMethods({ ...me, passkeys: 0 }).map((m) => m.kind), ["password", "provider"]);
  assert.deepEqual(stepUpMethods({ password: false, passkeys: 0, oidc: { id: "corp", name: "Company SSO" } }), [
    { kind: "provider", id: "corp", name: "Company SSO" },
  ]);
});

test("stepUpMethods_drops_a_provider_id_that_is_not_safe_in_a_path", () => {
  for (const id of ["../x", "a/b", "", "UP", "a b", "x".repeat(33), "a?b=c", "a#b", "é", "%2e%2e", "a\\b"]) {
    assert.deepEqual(stepUpMethods({ password: false, passkeys: 0, oidc: { id, name: "Evil" } }), [], JSON.stringify(id));
    assert.equal(providerPath(id), null, JSON.stringify(id));
  }
  assert.equal(providerPath("corp"), "/auth/oidc/corp/start");
  assert.equal(providerPath("a_b-9"), "/auth/oidc/a_b-9/start");
  assert.equal(providerPath("x".repeat(32)), `/auth/oidc/${"x".repeat(32)}/start`);
});

test("stepUpMethods_gives_nothing_for_an_account_with_none_of_the_three", () => {
  assert.deepEqual(stepUpMethods({ password: false, passkeys: 0, oidc: null }), []);
  assert.equal(NO_METHOD, "There is no way to confirm it here. Sign out and sign in again.");
  assert.equal(STEP_UP_TITLE, "Confirm it is you");
});

test("readStepUpMe_reads_the_account_name_and_the_methods", () => {
  const me = readStepUpMe({
    account: { id: "a", username: "holz", display_name: "Holz", role: "admin" },
    oidc: { id: "corp", name: "Company SSO" },
    password: true,
    passkeys: [{ id: "k1" }, { id: "k2" }],
  });
  assert.deepEqual(me, { name: "holz", password: true, passkeys: 2, oidc: { id: "corp", name: "Company SSO" } });
  assert.deepEqual(readStepUpMe({ account: { username: "x" }, oidc: null, password: false, passkeys: [] }), {
    name: "x",
    password: false,
    passkeys: 0,
    oidc: null,
  });
});

test("readStepUpMe_refuses_what_is_not_an_account_and_bounds_the_rest", () => {
  for (const raw of [null, "x", [], {}, { account: null }, { account: { username: 4 } }, { account: { username: "" } }, { account: { username: "x".repeat(129) } }]) {
    assert.equal(readStepUpMe(raw), null, JSON.stringify(raw));
  }
  const many = readStepUpMe({ account: { username: "x" }, passkeys: new Array(5000).fill({}), password: false });
  assert.equal(many.passkeys, 64, "a huge list is capped");
  assert.equal(readStepUpMe({ account: { username: "x" }, oidc: { id: "a", name: "n".repeat(200) } }).oidc, null);
  assert.equal(readStepUpMe({ account: { username: "x" }, oidc: { id: 4, name: "n" } }).oidc, null);
});

test("leadParts_names_the_account_and_the_way_to_confirm", () => {
  const password = leadParts([{ kind: "password" }]);
  assert.equal(`${password.before}holz${password.after}`, "Signed in as holz. Enter your password to make this change.");
  const passkey = leadParts([{ kind: "passkey" }]);
  assert.equal(`${passkey.before}holz${passkey.after}`, "Signed in as holz. Use your passkey to make this change.");
  assert.equal(leadParts([{ kind: "passkey" }, { kind: "password" }]).after, password.after, "a password is the first thing asked");
  assert.equal(leadParts([]).after, ". Sign in again to make this change.");
});
