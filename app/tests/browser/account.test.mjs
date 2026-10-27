// The account dialog (docs/design.md 6.29) on the main page against a scripted server: the groups and their state words, the
// keys, the password and the authenticator, each change confirmed by `Confirm it is you` once. The passkeys, the passphrase and
// the recovery code are in account-keys.test.mjs, the sessions and the hostile strings in account-sessions.test.mjs.
import assert from "node:assert/strict";
import { actionTitles, errorText, terminalHasFocus } from "./websession-lib.mjs";
import { STEP, clean, confirmWith, dialog, fixture, focusedName, holdsSecret, itemsOf, layer, legends, openAccountDialog, rowOf, runAction, statusLine, stepUpRoute } from "./account-lib.mjs";

export const backends = ["ws"];
export const backendNote = "the dialog exists on the web backend only: http() throws on the Tauri backend";

const SECRET = "JBSWY3DPEHPK3PXP";
const LINK = `otpauth://totp/holz?secret=${SECRET}&issuer=holzWave`;
const OLD = "old password value";
const NEW = "a brand new password";
const STEP_PW = "step up password";
const RULE = "Use at least 12 characters. Not your username, not one repeated character.";

export default async function ({ page, url, errors }) {
  // ---- the palette, the dialog, the groups and their words -----------------------------------------------------------------
  {
    const state = { me: fixture() };
    const server = await openAccountDialog(page, url, state);
    const titles = await actionTitles(page);
    assert.ok(titles.includes("Account…"), "the palette lists Account…");
    assert.ok(titles.includes("Sign out"), "the palette lists Sign out");
    assert.ok(!titles.includes("People…"), "no People… in this dialog's plan");
    assert.deepEqual(await legends(page), ["Password", "Authenticator", "Passkeys", "Unlock passphrase", "Recovery code", "Sessions"]);
    assert.deepEqual(await rowOf(page, "password"), { name: "Password", state: "set", button: "Change" });
    assert.deepEqual(await rowOf(page, "totp"), { name: "Authenticator app", state: "set", button: "Replace" });
    assert.deepEqual(await rowOf(page, "passkeys"), { name: "2 passkeys", state: "saved", button: "Add passkey" });
    assert.deepEqual(await rowOf(page, "passphrase"), { name: "Passphrase", state: "not set", button: "Set" });
    assert.deepEqual(await rowOf(page, "recovery"), { name: "Recovery code", state: "saved", button: "Make a new code" });
    assert.equal(await page.locator('.account .acc-group[role="group"]').count(), 6, "every group is a fieldset with role group");
    assert.equal(await page.locator('.account [data-group="password"] .acc-state').getAttribute("data-tone"), "ok");
    assert.equal(await page.locator('.account [data-group="passphrase"] .acc-state').getAttribute("data-tone"), "dim");
    assert.equal(await statusLine(page), "", "the status line is empty at first");
    assert.equal(await page.locator(".account .set-status").getAttribute("role"), "status");
    assert.equal(await dialog(page).getAttribute("aria-modal"), "true");
    assert.equal(await focusedName(page), "Change", "the first control has the focus");
    // Tab goes through every control in reading order, and round.
    const order = [];
    for (let i = 0; i < 12; i++) {
      order.push(await focusedName(page));
      await page.keyboard.press("Tab");
    }
    assert.deepEqual(order, ["Change", "Replace", "Add passkey", "Remove", "Remove", "Set", "Make a new code", "Sign out", "Sign out", "Sign out everywhere", "Close", "Change"]);
    await page.keyboard.press("Shift+Tab"); // from `Replace` back to `Change`
    await page.keyboard.press("Shift+Tab"); // and round to the last control
    assert.equal(await focusedName(page), "Close");
    assert.equal(await dialog(page).getByRole("button", { name: "Close", exact: true }).count(), 1);
    // Escape closes, and the keyboard is back with the terminal.
    await page.keyboard.press("Escape");
    await dialog(page).waitFor({ state: "detached" });
    await page.waitForFunction(() => document.activeElement?.id === "ime");
    assert.equal(await terminalHasFocus(page), true);
    // The dialog opens again, once, and `Close` closes it.
    await runAction(page, "account.open");
    await runAction(page, "account.open");
    await dialog(page).waitFor();
    assert.equal(await page.getByRole("dialog", { name: "Account", exact: true }).count(), 1, "one dialog at a time");
    await dialog(page).getByRole("button", { name: "Close", exact: true }).click();
    await dialog(page).waitFor({ state: "detached" });
    assert.equal(server.count("GET", "/api/me"), 2, "the account is read when the dialog opens");
    await clean(page, server, [], "groups");
  }

  // ---- an account that has only a provider: no Password and no Authenticator group ---------------------------------------------
  {
    const state = { me: fixture({ password: false, totp: false, passkeys: [], oidc: { id: "corp", name: "Company SSO" }, secrets: { state: "unlocked", passphrase: true, recovery: false } }) };
    const server = await openAccountDialog(page, url, state);
    assert.deepEqual(await legends(page), ["Passkeys", "Unlock passphrase", "Recovery code", "Sessions"]);
    assert.deepEqual(await rowOf(page, "passkeys"), { name: "No passkeys", state: "none", button: "Add passkey" });
    assert.deepEqual(await rowOf(page, "passphrase"), { name: "Passphrase", state: "set", button: "Change" });
    assert.deepEqual(await rowOf(page, "recovery"), { name: "Recovery code", state: "none", button: "Make a new code" });
    assert.equal(await page.getByRole("button", { name: "Replace" }).count(), 0);
    await clean(page, server, [], "provider only");
  }

  // ---- the password: the refusals at their fields, then a change after `Confirm it is you` ----------------------------------------
  {
    const state = { me: fixture() };
    const server = await openAccountDialog(page, url, state, [
      {
        method: "POST",
        path: "/api/me/password",
        answers: [
          { status: 401, body: { code: "invalid" } },
          { status: 400, body: { code: "bad_request", field: "new" } },
          STEP,
          { status: 204 },
        ],
      },
      stepUpRoute,
    ]);
    await page.getByRole("button", { name: "Change" }).first().click();
    assert.equal(await page.locator('.account [data-group="password"] .acc-row').isHidden(), true, "the form takes the place of the row");
    assert.equal(await focusedName(page), "Current password");
    assert.equal(await page.getByLabel("Current password").getAttribute("autocomplete"), "current-password");
    assert.equal(await page.getByLabel("New password").getAttribute("autocomplete"), "new-password");
    assert.equal(await page.locator('.account [data-group="password"] input').count(), 2, "two fields, as the design asks");
    assert.equal(await page.getByLabel("Current password").getAttribute("type"), "password");
    // A wrong current password: the sentence at its field, both fields empty, nothing put back.
    await page.getByLabel("Current password").fill(OLD);
    await page.getByLabel("New password").fill(NEW);
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.querySelector(".acc-form .web-err")?.textContent !== "");
    assert.equal(await errorText(page), "Sign-in failed. Check your details or ask an admin.");
    assert.equal(await page.getByLabel("Current password").inputValue(), "");
    assert.equal(await page.getByLabel("New password").inputValue(), "");
    assert.equal(await page.getByLabel("Current password").getAttribute("aria-invalid"), "true", "the field that the sentence concerns");
    assert.equal(await focusedName(page), "Current password");
    // A refused new password: the rule, at the new field.
    await page.getByLabel("Current password").fill(OLD);
    await page.getByLabel("New password").fill("short");
    await page.getByRole("button", { name: "Save" }).click();
    await page.waitForFunction(() => document.querySelector(".acc-form .web-err")?.textContent.startsWith("Use at least"));
    assert.equal(await errorText(page), RULE);
    assert.equal(await page.getByLabel("New password").getAttribute("aria-invalid"), "true");
    assert.equal(await page.getByLabel("Current password").getAttribute("aria-invalid"), null, "only the field it concerns");
    assert.equal(await focusedName(page), "New password");
    assert.equal(await page.getByLabel("New password").inputValue(), "");
    // A good change: refused with a step-up request, confirmed in the layer once, then made.
    await page.getByLabel("Current password").fill(OLD);
    await page.getByLabel("New password").fill(NEW);
    await page.keyboard.press("Enter");
    await layer(page).waitFor();
    assert.equal(await page.getByLabel("Current password").inputValue(), "", "the fields were emptied before the layer");
    assert.equal(await page.getByLabel("New password").inputValue(), "");
    assert.deepEqual(await holdsSecret(page, [OLD, NEW]), [], "no password stands in the page while the layer asks");
    await confirmWith(page, STEP_PW);
    await page.waitForFunction(() => document.querySelector(".account .set-status")?.textContent === "Saved.");
    assert.equal(await statusLine(page), "Saved.");
    await page.waitForSelector('.account [data-group="password"] .acc-row:not([hidden])');
    assert.equal(await page.locator('.account [data-group="password"] input').count(), 0, "the form is gone");
    assert.equal(await focusedName(page), "Change", "the focus is back on the button of the group");
    const posts = server.requests("POST", "/api/me/password");
    assert.equal(posts.length, 4, "the change was repeated once after the confirmation");
    assert.deepEqual(posts[2].json, { current: OLD, new: NEW });
    assert.deepEqual(posts[3].json, { current: OLD, new: NEW });
    assert.deepEqual(posts[0].json, { current: OLD, new: NEW });
    assert.equal(server.count("POST", "/auth/step-up"), 1, "the layer was shown once");
    assert.deepEqual(server.requests("POST", "/auth/step-up")[0].json, { password: STEP_PW });
    assert.equal(posts[0].headers["x-csrf-token"], "csrf-token-1");
    await clean(page, server, [OLD, NEW, STEP_PW, "short"], "password");
  }

  // ---- the authenticator: the key through the shared step, the code, a refusal that keeps the step ---------------------------------
  {
    const state = { me: fixture() };
    const server = await openAccountDialog(page, url, state, [
      {
        method: "POST",
        path: "/api/me/totp",
        reply: (request, n) =>
          request.json.step === "begin"
            ? n === 0
              ? STEP
              : { status: 200, body: { secret: SECRET, otpauth: LINK } }
            : { status: 401, body: { code: "invalid" } },
        answers: undefined,
      },
      stepUpRoute,
    ]);
    // `reply` answers begin (after a step-up) and refuses the first finish; a later finish is switched below.
    await page.getByRole("button", { name: "Replace" }).click();
    await confirmWith(page, STEP_PW);
    await page.waitForSelector(".account .lg-secret");
    assert.equal(await page.locator(".account .lg-secret").first().textContent(), "JBSW Y3DP EHPK 3PXP", "the key in groups of four");
    assert.equal(await page.locator(".account .lg-tx").textContent(), LINK);
    assert.equal(await page.getByRole("button", { name: "Copy", exact: true }).count(), 1);
    assert.equal(await page.getByRole("button", { name: "Copy link" }).count(), 1);
    assert.equal(await page.getByLabel("Code").getAttribute("autocomplete"), "one-time-code");
    assert.equal(await page.getByLabel("Code").getAttribute("inputmode"), "numeric");
    assert.equal(await page.locator('.account [data-group="totp"] .acc-row').isHidden(), true);
    // A wrong code: the sentence, the field emptied, the step kept.
    await page.getByLabel("Code").fill("123 456");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.querySelector(".acc-form .web-err")?.textContent !== "");
    assert.equal(await errorText(page), "That code did not match. Try again.");
    assert.equal(await page.getByLabel("Code").inputValue(), "");
    assert.equal(await page.getByLabel("Code").getAttribute("aria-invalid"), "true");
    assert.equal(await page.locator(".account .lg-secret").count(), 1, "a refusal keeps the step");
    assert.deepEqual(server.requests("POST", "/api/me/totp").map((r) => r.json), [{ step: "begin" }, { step: "begin" }, { step: "finish", code: "123456" }]);
    // Cancel ends the step and empties the elements of the secret.
    await page.getByRole("button", { name: "Cancel" }).click();
    assert.equal(await page.locator(".account .lg-secret, .account .lg-tx").count(), 0, "the secret elements are gone");
    assert.deepEqual(await holdsSecret(page, [SECRET, LINK, "JBSW Y3DP"]), [], "the key is nowhere in the page after the step");
    assert.equal(await focusedName(page), "Replace");
    await clean(page, server, [SECRET, LINK, STEP_PW, "123456"], "authenticator cancelled");
  }
  {
    const state = { me: fixture() };
    const server = await openAccountDialog(page, url, state, [
      {
        method: "POST",
        path: "/api/me/totp",
        reply: (request) => (request.json.step === "begin" ? { status: 200, body: { secret: SECRET, otpauth: LINK } } : { status: 204 }),
      },
    ]);
    await page.getByRole("button", { name: "Replace" }).click();
    await page.waitForSelector(".account .lg-secret");
    await page.getByLabel("Code").fill("654321");
    await page.getByRole("button", { name: "Save" }).click();
    await page.waitForFunction(() => document.querySelector(".account .set-status")?.textContent === "Saved.");
    assert.equal(await page.locator(".account .lg-secret, .account .lg-tx").count(), 0, "after the step ends the secret elements are empty");
    assert.deepEqual(server.requests("POST", "/api/me/totp").map((r) => r.json), [{ step: "begin" }, { step: "finish", code: "654321" }]);
    assert.equal(server.count("POST", "/auth/step-up"), 0, "no layer when the sign-in is recent");
    await page.keyboard.press("Escape");
    await dialog(page).waitFor({ state: "detached" });
    await clean(page, server, [SECRET, LINK, "654321"], "authenticator replaced");
  }

  assert.deepEqual(errors, [], "the page reported errors");
}
