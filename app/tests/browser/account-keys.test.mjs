// The account dialog (docs/design.md 6.29), the keys of an account: passkeys (added through the browser's prompt with the PRF
// result only when it returned one, removed after a second press), the unlock passphrase, the unlock of a locked store, and the new
// recovery code shown once. Each change goes through `Confirm it is you`.
import assert from "node:assert/strict";
import { errorText, focusedName } from "./websession-lib.mjs";
import { STEP, clean, confirmWith, dialog, fixture, holdsSecret, installAuthenticator, itemsOf, legends, openAccountDialog, rowOf, statusLine, stepUpRoute } from "./account-lib.mjs";

export const backends = ["ws"];
export const backendNote = "the dialog exists on the web backend only: http() throws on the Tauri backend";

// The shape of the server: the library's own text, `{"publicKey": {...}}`, inside `options`.
const CREATION = {
  options: {
    publicKey: {
      challenge: "AQID",
      rp: { id: "holz.example.net", name: "holzWave" },
      user: { id: "BAUG", name: "holz", displayName: "Holz" },
      pubKeyCredParams: [{ type: "public-key", alg: -7 }],
      timeout: 60000,
      extensions: { prf: { eval: { first: "BwgJ" } } },
    },
  },
};
const RULE = "Use at least 12 characters. Not your username, not one repeated character.";
const RECOVERY = "ABCD-EFGH-IJKL-MNOP-QRST-UVWX";
const UNLOCK_PASS = "my old passphrase";
const NEW_PASS = "a long new passphrase";
const STEP_PW = "step up password";

export default async function ({ page, url, errors }) {
  await installAuthenticator(page);
  const added = (state) => (request) => {
    if (request.json.step === "begin") return { status: 200, body: CREATION };
    state.me.passkeys.push({ id: "k3", name: state.name, created_at: new Date().toISOString(), last_used: null });
    return { status: 200, body: { id: "k3" } };
  };

  // ---- add a passkey with the PRF result, and without it -----------------------------------------------------------------------
  for (const mode of ["prf", "plain"]) {
    const state = { me: fixture(), name: "Laptop" };
    const server = await openAccountDialog(page, url, state, [{ method: "POST", path: "/api/me/passkeys", reply: added(state) }], `?renderer=canvas&authn=${mode}`);
    await page.getByRole("button", { name: "Add passkey" }).click();
    assert.equal(await focusedName(page), "Name");
    assert.equal(await page.locator('.account [data-group="passkeys"] .acc-row').isHidden(), true);
    await page.keyboard.type("Laptop");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.querySelector('.account [data-group="passkeys"] .acc-name')?.textContent === "3 passkeys");
    assert.deepEqual(await rowOf(page, "passkeys"), { name: "3 passkeys", state: "saved", button: "Add passkey" });
    assert.ok((await itemsOf(page, "passkeys")).some((row) => row.startsWith("Laptop | not used yet")), "the new row is there");
    assert.equal(await statusLine(page), "Saved.");
    const sent = server.requests("POST", "/api/me/passkeys").map((r) => r.json);
    assert.deepEqual(sent[0], { step: "begin", name: "Laptop" });
    assert.equal(sent[1].step, "finish");
    assert.equal(sent[1].credential.id, "Y3JlZC1pZA");
    assert.equal(sent[1].credential.response.attestationObject, "BwgJ");
    assert.deepEqual(sent[1].credential.response.transports, ["internal"]);
    if (mode === "prf") assert.equal(sent[1].prf, "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA", "the PRF result goes beside the attestation");
    else assert.equal("prf" in sent[1], false, "no PRF member when the browser returned none");
    const seen = await page.evaluate(() => window.__authn);
    assert.equal(seen.length, 1);
    assert.equal(seen[0].kind, "create");
    assert.deepEqual(seen[0].options.challenge, { binary: "AQID" });
    assert.deepEqual(seen[0].options.user.id, { binary: "BAUG" });
    await clean(page, server, [], `passkey ${mode}`);
  }
  {
    // A prompt the person closed says nothing and leaves the form; a browser that fails says the sentence for a passkey not saved.
    for (const [mode, sentence] of [["cancel", ""], ["error", "That passkey was not saved. Try again."]]) {
      const state = { me: fixture(), name: "Laptop" };
      const server = await openAccountDialog(page, url, state, [{ method: "POST", path: "/api/me/passkeys", reply: added(state) }], `?renderer=canvas&authn=${mode}`);
      await page.getByRole("button", { name: "Add passkey" }).click();
      await page.keyboard.type("Laptop");
      await page.getByRole("button", { name: "Save" }).click();
      await page.waitForFunction(() => window.__authn.length === 1);
      await page.waitForTimeout(150);
      assert.equal(await page.locator(".acc-form .web-err").textContent(), sentence, mode);
      assert.equal(server.requests("POST", "/api/me/passkeys").length, 1, "no finish without a credential");
      assert.equal(await page.locator(".account").getAttribute("aria-busy"), null);
      assert.equal(await page.getByRole("button", { name: "Save", exact: true }).count(), 1, "the form stays for another try");
      await page.getByRole("button", { name: "Cancel" }).click();
      assert.equal(await focusedName(page), "Add passkey");
      assert.equal((await rowOf(page, "passkeys")).name, "2 passkeys");
    }
  }

  // ---- remove a passkey after a second press; the last way to sign in is refused ---------------------------------------------------
  {
    const state = { me: fixture() };
    const server = await openAccountDialog(page, url, state, [
      {
        method: "DELETE",
        path: "/api/me/passkeys/k1",
        reply: (request, n) => {
          if (n === 0) return { status: 409, body: { code: "conflict", field: "method" } };
          state.me.passkeys = state.me.passkeys.filter((k) => k.id !== "k1");
          return { status: 204 };
        },
      },
      { method: "DELETE", path: "/api/me/passkeys/k2", answers: [STEP, { status: 204 }] },
      stepUpRoute,
    ]);
    const remove = page.locator('.account [data-group="passkeys"] .acc-danger').first();
    await remove.click();
    assert.equal(await remove.textContent(), "Click again to remove");
    assert.equal(await remove.getAttribute("data-armed"), "true");
    assert.equal(server.count("DELETE", "/api/me/passkeys/k1"), 0, "the first press only arms the button");
    await page.keyboard.press("Tab");
    assert.equal(await remove.textContent(), "Remove", "leaving the button disarms it");
    await remove.click();
    await remove.click();
    await page.waitForFunction(() => document.querySelector(".account .set-status")?.textContent !== "");
    assert.equal(await statusLine(page), "This is your last way to sign in. Add another first.");
    assert.equal((await itemsOf(page, "passkeys")).length, 2, "the row stays");
    await remove.click();
    await remove.click();
    await page.waitForFunction(() => document.querySelector('.account [data-group="passkeys"] .acc-name')?.textContent === "1 passkey");
    assert.equal(await statusLine(page), "Saved.");
    assert.deepEqual(await itemsOf(page, "passkeys"), ["Phone | used 3 days ago | Remove"]);
    // Another passkey, with a recent sign-in needed.
    const other = page.locator('.account [data-group="passkeys"] .acc-danger').first();
    await other.click();
    await other.click();
    await confirmWith(page, STEP_PW);
    assert.equal(server.count("DELETE", "/api/me/passkeys/k2"), 2, "the removal was repeated once after the confirmation");
    await clean(page, server, [STEP_PW], "remove passkey");
  }

  // ---- the unlock passphrase: set, refused by the rule, and a locked store ----------------------------------------------------------
  {
    const state = { me: fixture() };
    const server = await openAccountDialog(page, url, state, [
      {
        method: "PUT",
        path: "/api/me/passphrase",
        reply: (request, n) => {
          if (n === 0) return { status: 400, body: { code: "bad_request", field: "passphrase" } };
          if (n === 1) return STEP;
          state.me.secrets.passphrase = true;
          return { status: 204 };
        },
      },
      stepUpRoute,
    ]);
    await page.getByRole("button", { name: "Set" }).click();
    assert.equal(await page.getByLabel("New passphrase").getAttribute("autocomplete"), "new-password");
    assert.equal(await focusedName(page), "New passphrase");
    await page.keyboard.type("short");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.querySelector(".acc-form .web-err")?.textContent !== "");
    assert.equal(await errorText(page), RULE);
    assert.equal(await page.getByLabel("New passphrase").inputValue(), "");
    assert.equal(await page.getByLabel("New passphrase").getAttribute("aria-invalid"), "true");
    await page.keyboard.type(NEW_PASS);
    await page.keyboard.press("Enter");
    await confirmWith(page, STEP_PW);
    await page.waitForFunction(() => document.querySelector('.account [data-group="passphrase"] .acc-state')?.textContent === "set");
    assert.deepEqual(await rowOf(page, "passphrase"), { name: "Passphrase", state: "set", button: "Change" });
    assert.deepEqual(server.requests("PUT", "/api/me/passphrase").map((r) => r.json), [{ passphrase: "short" }, { passphrase: NEW_PASS }, { passphrase: NEW_PASS }]);
    assert.equal(await statusLine(page), "Saved.");
    await clean(page, server, [NEW_PASS, "short", STEP_PW], "passphrase");
  }
  {
    // A locked store: neither the passphrase nor the recovery code can be changed, `Unlock` is offered, and the dialog follows the unlock.
    const state = { me: fixture({ secrets: { state: "locked", passphrase: true, recovery: true } }) };
    const server = await openAccountDialog(page, url, state, [
      {
        method: "POST",
        path: "/auth/unlock",
        reply: () => {
          state.me.secrets.state = "unlocked";
          return { status: 200, body: { state: "unlocked" } };
        },
      },
    ]);
    assert.deepEqual(await rowOf(page, "passphrase"), { name: "Passphrase", state: "set", button: "Unlock" });
    assert.deepEqual(await rowOf(page, "recovery"), { name: "Recovery code", state: "saved", button: "Unlock" });
    assert.equal(await page.getByRole("button", { name: "Make a new code" }).count(), 0, "no change while locked");
    assert.equal(await page.getByRole("button", { name: "Change" }).count(), 1, "only the password has `Change`");
    await page.locator('.account [data-group="passphrase"] .acc-main').click();
    const unlock = page.getByRole("dialog", { name: "Your saved passwords are locked" });
    await unlock.waitFor();
    assert.equal(await focusedName(page), "Passphrase");
    await page.keyboard.type(UNLOCK_PASS);
    await page.keyboard.press("Enter");
    await unlock.waitFor({ state: "detached" });
    await page.waitForFunction(() => document.querySelector('.account [data-group="recovery"] .acc-main')?.textContent === "Make a new code");
    assert.deepEqual(await rowOf(page, "passphrase"), { name: "Passphrase", state: "set", button: "Change" });
    assert.equal(await dialog(page).count(), 1, "the account dialog is still there under the unlock dialog");
    assert.deepEqual(server.requests("POST", "/auth/unlock").map((r) => r.json), [{ passphrase: UNLOCK_PASS }]);
    await clean(page, server, [UNLOCK_PASS], "unlock from the account");
  }

  // ---- a new recovery code: said before, shown once, and gone after `Continue` ------------------------------------------------------
  {
    const state = { me: fixture() };
    const server = await openAccountDialog(page, url, state, [
      { method: "POST", path: "/api/me/recovery", answers: [STEP, { status: 200, body: { recovery_code: RECOVERY } }] },
      stepUpRoute,
    ]);
    await page.getByRole("button", { name: "Make a new code" }).click();
    assert.equal(await page.locator(".acc-note").textContent(), "The new code replaces the old one.", "said before it is made");
    assert.equal(server.count("POST", "/api/me/recovery"), 0);
    await page.getByRole("button", { name: "Make code" }).click();
    await confirmWith(page, STEP_PW);
    await page.waitForSelector(".account .lg-code");
    assert.equal(await page.locator(".account .lg-code").textContent(), RECOVERY, "the code in groups of four");
    assert.equal(await page.locator(".account .set-status").textContent(), "");
    assert.equal(await dialog(page).getByRole("button", { name: "Close", exact: true }).isDisabled(), true, "the dialog cannot be left");
    await page.keyboard.press("Escape");
    assert.equal(await dialog(page).count(), 1, "Escape does nothing while the code is shown");
    const go = page.getByRole("button", { name: "Continue" });
    assert.equal(await go.isDisabled(), true);
    assert.equal(await page.locator(".account .lg-note").textContent(), "Tick the box to continue.");
    await page.getByLabel("I have saved this code").check();
    assert.equal(await go.isDisabled(), false);
    await go.click();
    await page.waitForFunction(() => document.querySelector(".account .set-status")?.textContent === "Saved.");
    assert.equal(await page.locator(".account .lg-code").count(), 0, "the code is not hidden but gone");
    assert.deepEqual(await holdsSecret(page, [RECOVERY, "ABCDEFGH", "ABCD-EFGH"]), [], "the code is nowhere in the page, storage, title or address");
    assert.equal(await dialog(page).getByRole("button", { name: "Close", exact: true }).isDisabled(), false);
    assert.deepEqual(server.requests("POST", "/api/me/recovery").map((r) => r.json), [{}, {}]);
    await page.keyboard.press("Escape");
    await dialog(page).waitFor({ state: "detached" });
    assert.deepEqual((await legends(page)).length, 0);
    await clean(page, server, [RECOVERY, STEP_PW], "recovery code");
  }

  assert.deepEqual(errors, [], "the page reported errors");
}
