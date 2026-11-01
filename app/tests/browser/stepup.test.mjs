// `Confirm it is you` (docs/design.md 6.29) through `withStepUp`: a change is sent, a 403 that says `step_up` shows one layer,
// the proof goes out and the change is repeated exactly once. Driven by a dynamic import of the page's own module, the
// interaction running while the evaluated promise is pending.
import assert from "node:assert/strict";
import { focusedName, holdsSecret, installAuthenticator, leftover, openMain, sessionRoute } from "./websession-lib.mjs";

export const backends = ["ws"];
export const backendNote = "the layer exists on the web backend only: http() throws on the Tauri backend";

const PASSWORD = "hunter2 hunter2";
const ME = {
  account: { id: "acc1", username: "holz", display_name: "Holz", role: "admin" },
  oidc: null,
  password: true,
  totp: true,
  passkeys: [],
  secrets: { state: "unlocked", passphrase: true, recovery: true },
  sessions: [],
};
const STEP = { status: 403, body: { code: "forbidden", step_up: true } };
// The shape of the server: the library's own text, `{"publicKey": {...}}`, inside `options`.
const OPTIONS = { options: { publicKey: { challenge: "AQID", rpId: "holz.example.net", allowCredentials: [{ id: "Y3JlZA", type: "public-key" }], userVerification: "preferred", timeout: 60000 } } };

export default async function ({ page, url, errors }) {
  await installAuthenticator(page);
  const layer = () => page.getByRole("dialog", { name: "Confirm it is you" });
  const pw = () => page.locator("#web-step-field");
  /** Start a change through `withStepUp`; the promise stays pending in the page under `window[slot]`. */
  const start = (path, slot = "__done", method = "PUT") =>
    page.evaluate(
      async ([p, name, verb]) => {
        const bridge = await import(new URL("bridge.js", location.href).href);
        const { withStepUp } = await import(new URL("stepup.js", location.href).href);
        window[name] = withStepUp(() => bridge.http(verb, p, { passphrase: "new" })).then(
          (v) => ({ ok: true, v: v ?? null }),
          (e) => ({ ok: false, name: e.name, status: e.status ?? null }),
        );
      },
      [path, slot, method],
    );
  const outcome = (slot = "__done") => page.evaluate((name) => window[name], slot);
  const opener = () =>
    page.evaluate(() => {
      const b = document.createElement("button");
      b.id = "opener";
      b.textContent = "Change";
      document.body.append(b);
      b.focus();
    });
  const open = (table, suffix = "?renderer=canvas") => openMain(page, url, [sessionRoute(), ...table], suffix);
  const clean = (server, what) => {
    const l = leftover(server);
    assert.deepEqual(l.foreign, [], `${what}: a request left the page's origin`);
    assert.deepEqual(l.unscripted, [], `${what}: a request nobody scripted`);
  };
  const me = (over = {}) => ({ path: "/api/me", body: { ...ME, ...over } });
  const put = (path = "/api/me/passphrase") => ({ method: "PUT", path, answers: [STEP, { status: 204 }] });

  // ---- the happy path: 403, the layer, the password, a second PUT, the second answer ------------------------------------
  {
    const server = await open([me(), put(), { method: "POST", path: "/auth/step-up", body: { step_up: 300 }, delay: 300 }]);
    await opener();
    await start("/api/me/passphrase");
    await layer().waitFor();
    assert.equal(await focusedName(page), "Password", "the password field has the focus");
    assert.equal(await page.locator(".web-step .set-title").textContent(), "Confirm it is you");
    assert.equal(await page.locator(".web-step .web-lead").textContent(), "Signed in as holz. Enter your password to make this change.");
    assert.equal(await page.locator(".web-step .web-lead b").textContent(), "holz", "the account name stands out in its own element");
    assert.equal(await pw().getAttribute("type"), "password");
    assert.equal(await pw().getAttribute("autocomplete"), "current-password");
    assert.equal(await page.getByRole("button", { name: "Use a passkey" }).count(), 0, "no passkey button for an account without one");
    assert.equal(await page.getByRole("button", { name: "Cancel" }).count(), 1);
    assert.equal(server.count("PUT", "/api/me/passphrase"), 1, "one PUT before the proof");
    await page.keyboard.type(PASSWORD);
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.querySelector(".web-step")?.getAttribute("aria-busy") === "true");
    assert.equal(await pw().inputValue(), "", "the field is empty after the submit");
    assert.equal(await page.getByRole("button", { name: "Confirm…" }).count(), 1);
    assert.deepEqual(await holdsSecret(page, [PASSWORD]), []);
    const result = await outcome();
    assert.deepEqual(result, { ok: true, v: null }, "the promise resolves with the answer of the second PUT");
    await layer().waitFor({ state: "detached" });
    assert.equal(server.count("PUT", "/api/me/passphrase"), 2, "exactly two PUTs");
    assert.equal(server.count("POST", "/auth/step-up"), 1);
    assert.deepEqual(server.requests("POST", "/auth/step-up")[0].json, { password: PASSWORD });
    assert.equal(await page.evaluate(() => document.activeElement?.id), "opener", "the focus returns to the button that opened it");
    assert.deepEqual(await holdsSecret(page, [PASSWORD]), []);
    clean(server, "happy path");
    const log = server.log.filter((l) => l.path !== "/api/session" || l.method !== "GET");
    console.log(`Step-up: ${log.map((l) => `${l.method} ${l.path}${l.method === "POST" && l.path === "/auth/step-up" ? ` {"password":"<${l.json.password.length} characters>"}` : ""}`).join(" -> ")}`);
  }

  // ---- Escape cancels: StepUpCancelled and nothing more is sent -----------------------------------------------------------
  {
    const server = await open([me(), { method: "PUT", path: "/api/me/passphrase", answers: [STEP] }]);
    await opener();
    await start("/api/me/passphrase");
    await layer().waitFor();
    await page.keyboard.type("not sent");
    await page.keyboard.press("Escape");
    await layer().waitFor({ state: "detached" });
    assert.deepEqual(await outcome(), { ok: false, name: "StepUpCancelled", status: null });
    assert.equal(server.count("PUT", "/api/me/passphrase"), 1, "no second PUT after a cancel");
    assert.equal(server.count("POST", "/auth/step-up"), 0);
    assert.equal(await page.evaluate(() => document.activeElement?.id), "opener");
    // The button does the same.
    await start("/api/me/passphrase", "__done2");
    await layer().waitFor();
    await page.getByRole("button", { name: "Cancel" }).click();
    assert.equal((await outcome("__done2")).name, "StepUpCancelled");
    assert.equal(server.count("POST", "/auth/step-up"), 0);
    assert.equal(server.count("PUT", "/api/me/passphrase"), 2, "each start sent one PUT and no more");
    assert.deepEqual(await holdsSecret(page, ["not sent"]), []);
    clean(server, "cancel");
  }

  // ---- a wrong password keeps the layer with the sentence; a right one finishes it ---------------------------------------
  {
    const server = await open([
      me(),
      put(),
      {
        method: "POST",
        path: "/auth/step-up",
        answers: [{ status: 401, body: { code: "invalid" } }, { status: 429, body: { code: "throttled", retry_seconds: 40 } }, { body: { step_up: 300 } }],
      },
    ]);
    await start("/api/me/passphrase");
    await layer().waitFor();
    await pw().fill("wrong");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.querySelector("#web-step-error")?.textContent !== "");
    assert.equal(await page.locator("#web-step-error").textContent(), "Sign-in failed. Check your details or ask an admin.");
    assert.equal(await pw().inputValue(), "", "nothing is put back");
    assert.equal(await pw().getAttribute("aria-invalid"), "true");
    assert.equal(await focusedName(page), "Password");
    assert.equal(await layer().count(), 1);
    await pw().fill("wrong again");
    await page.getByRole("button", { name: "Confirm" }).click();
    await page.waitForFunction(() => document.querySelector("#web-step-error")?.textContent.startsWith("Too many"));
    assert.equal(await page.locator("#web-step-error").textContent(), "Too many attempts. Try again in 40 seconds.");
    await pw().fill(PASSWORD);
    await page.keyboard.press("Enter");
    assert.equal((await outcome()).ok, true);
    assert.equal(server.count("PUT", "/api/me/passphrase"), 2, "the change was repeated once, after the proof");
    assert.equal(server.count("POST", "/auth/step-up"), 3);
    assert.deepEqual(await holdsSecret(page, ["wrong", PASSWORD]), []);
    clean(server, "wrong password");
  }

  // ---- a second 403 after a successful step-up rejects and shows nothing more -----------------------------------------------
  {
    const server = await open([me(), { method: "PUT", path: "/api/me/passphrase", answers: [STEP, STEP, STEP] }, { method: "POST", path: "/auth/step-up", body: { step_up: 300 } }]);
    await start("/api/me/passphrase");
    await layer().waitFor();
    await pw().fill(PASSWORD);
    await page.keyboard.press("Enter");
    assert.deepEqual(await outcome(), { ok: false, name: "HttpError", status: 403 });
    assert.equal(server.count("PUT", "/api/me/passphrase"), 2, "the retry is made exactly once");
    assert.equal(server.count("POST", "/auth/step-up"), 1);
    await page.waitForTimeout(200);
    assert.equal(await layer().count(), 0, "no second layer");
    clean(server, "second 403");
  }

  // ---- a 403 without the member, a 401 and a plain failure never show the layer ------------------------------------------
  {
    const server = await open([
      me(),
      { method: "PUT", path: "/api/me/passphrase", answers: [{ status: 403, body: { code: "forbidden" } }] },
      { method: "PUT", path: "/api/me/other", answers: [{ status: 401, body: { code: "invalid", step_up: true } }] },
      { method: "PUT", path: "/api/me/gone", answers: [{ abort: true }] },
    ]);
    await start("/api/me/passphrase", "__a");
    await start("/api/me/other", "__b");
    await start("/api/me/gone", "__c");
    assert.deepEqual(await outcome("__a"), { ok: false, name: "HttpError", status: 403 });
    assert.deepEqual(await outcome("__b"), { ok: false, name: "HttpError", status: 401 });
    assert.deepEqual(await outcome("__c"), { ok: false, name: "HttpError", status: 0 });
    assert.equal(await layer().count(), 0);
    assert.equal(server.count("GET", "/api/me"), 0, "the account was not even asked");
    assert.equal(server.count("PUT", "/api/me/passphrase"), 1);
    clean(server, "plain refusals");
  }

  // ---- two callers at once share one layer, and both retry ----------------------------------------------------------------
  {
    const answers = { answers: [STEP, { status: 204 }] };
    const server = await open([me(), { method: "PUT", path: "/api/me/one", ...answers }, { method: "PUT", path: "/api/me/two", ...answers }, { method: "POST", path: "/auth/step-up", body: { step_up: 300 } }]);
    await start("/api/me/one", "__one");
    await start("/api/me/two", "__two");
    await layer().waitFor();
    await page.waitForTimeout(300);
    assert.equal(await layer().count(), 1, "one layer");
    assert.equal(server.count("GET", "/api/me"), 1, "the account was asked once");
    await pw().fill(PASSWORD);
    await page.keyboard.press("Enter");
    assert.equal((await outcome("__one")).ok, true);
    assert.equal((await outcome("__two")).ok, true);
    assert.equal(server.count("PUT", "/api/me/one"), 2);
    assert.equal(server.count("PUT", "/api/me/two"), 2);
    assert.equal(server.count("POST", "/auth/step-up"), 1, "one proof for both");
    clean(server, "two callers");
  }

  // ---- a passkey: the second way, sent as the sign-in sends it ---------------------------------------------------------------
  {
    const server = await open(
      [me({ passkeys: [{ id: "k1", name: "MacBook", created_at: "2026-01-01T00:00:00Z", last_used: null }] }), put(), { method: "POST", path: "/auth/step-up/begin", body: OPTIONS }, { method: "POST", path: "/auth/step-up", body: { step_up: 300 } }],
      "?renderer=canvas&authn=plain",
    );
    await start("/api/me/passphrase");
    await layer().waitFor();
    assert.equal(await focusedName(page), "Password", "the password is first");
    const key = page.getByRole("button", { name: "Use a passkey" });
    assert.equal(await key.count(), 1);
    // Tab order: Password, Cancel, Confirm, Use a passkey.
    const order = [];
    for (let i = 0; i < 4; i++) {
      order.push(await focusedName(page));
      await page.keyboard.press("Tab");
    }
    assert.deepEqual(order, ["Password", "Cancel", "Confirm", "Use a passkey"]);
    await key.click();
    assert.equal((await outcome()).ok, true);
    assert.deepEqual(server.requests("POST", "/auth/step-up/begin")[0].json, {});
    const body = server.requests("POST", "/auth/step-up")[0].json;
    assert.equal(body.credential.id, "Y3JlZC1pZA");
    assert.equal(body.credential.response.authenticatorData, "AQID");
    assert.equal(body.password, undefined, "a passkey proof carries no password");
    const seen = await page.evaluate(() => window.__authn);
    assert.equal(seen.length, 1);
    assert.equal(seen[0].kind, "get");
    assert.deepEqual(seen[0].options.challenge, { binary: "AQID" }, "the challenge reached the authenticator as bytes");
    assert.equal(server.count("PUT", "/api/me/passphrase"), 2);
    clean(server, "passkey");
  }
  {
    // An account with a passkey only: the passkey button is the solid one, and a prompt the person closed is not a failure.
    const server = await open(
      [me({ password: false, passkeys: [{ id: "k1", name: "Phone", created_at: "2026-01-01T00:00:00Z", last_used: null }] }), put(), { method: "POST", path: "/auth/step-up/begin", body: OPTIONS }],
      "?renderer=canvas&authn=cancel",
    );
    await start("/api/me/passphrase");
    await layer().waitFor();
    assert.equal(await pw().count(), 0, "no password field without a password");
    assert.equal(await page.locator(".web-step .web-lead").textContent(), "Signed in as holz. Use your passkey to make this change.");
    assert.equal(await focusedName(page), "Use a passkey");
    await page.getByRole("button", { name: "Use a passkey" }).click();
    await page.waitForFunction(() => window.__authn.length === 1);
    await page.waitForTimeout(200);
    assert.equal(await page.locator("#web-step-error").textContent(), "", "a cancelled prompt says nothing");
    assert.equal(await page.locator(".web-step").getAttribute("aria-busy"), null);
    assert.equal(await layer().count(), 1, "the layer stays for another try");
    await page.keyboard.press("Escape");
    assert.equal((await outcome()).name, "StepUpCancelled");
    clean(server, "passkey cancelled");
  }
  {
    // A browser that fails: the one sentence of a failed sign-in.
    const server = await open(
      [me({ password: false, passkeys: [{ id: "k1", name: "Phone", created_at: "2026-01-01T00:00:00Z", last_used: null }] }), put(), { method: "POST", path: "/auth/step-up/begin", body: OPTIONS }],
      "?renderer=canvas&authn=error",
    );
    await start("/api/me/passphrase");
    await layer().waitFor();
    await page.getByRole("button", { name: "Use a passkey" }).click();
    await page.waitForFunction(() => document.querySelector("#web-step-error")?.textContent !== "");
    assert.equal(await page.locator("#web-step-error").textContent(), "Sign-in failed. Check your details or ask an admin.");
    await page.keyboard.press("Escape");
    await outcome();
    clean(server, "passkey failed");
  }

  // ---- an account with only a provider: a link, built from a safe id only --------------------------------------------------
  {
    const server = await open([me({ password: false, passkeys: [], oidc: { id: "corp", name: "Company SSO" } }), put()]);
    await start("/api/me/passphrase");
    await layer().waitFor();
    const link = page.getByRole("link", { name: "Sign in again with Company SSO" });
    assert.equal(await link.count(), 1);
    assert.equal(await link.getAttribute("href"), "/auth/oidc/corp/start");
    assert.equal(await page.locator(".web-step .web-lead").textContent(), "Signed in as holz. Sign in again to make this change.");
    assert.equal(await pw().count(), 0);
    assert.equal(await page.getByRole("button", { name: "Confirm" }).count(), 0);
    await page.keyboard.press("Escape");
    assert.equal((await outcome()).name, "StepUpCancelled");
    clean(server, "provider");
  }
  {
    const hostile = '<img src=x onerror="window.__pwned=1">';
    const server = await open([me({ password: false, passkeys: [], oidc: { id: "corp", name: hostile } }), put()]);
    await start("/api/me/passphrase");
    await layer().waitFor();
    assert.equal(await page.getByRole("link").first().textContent(), `Sign in again with ${hostile}`, "a server string is text");
    assert.equal(await page.locator(".web-step img").count(), 0, "no element is made from it");
    assert.equal(await page.evaluate(() => window.__pwned), undefined);
    await page.keyboard.press("Escape");
    await outcome();
    clean(server, "hostile name");
  }
  for (const id of ["../x", "a/b", "UP", "a?b"]) {
    const server = await open([me({ password: false, passkeys: [], oidc: { id, name: "Evil" } }), put()]);
    await start("/api/me/passphrase");
    await layer().waitFor();
    assert.equal(await page.locator(".web-step a").count(), 0, `a hostile id (${id}) gives no link`);
    assert.equal(await page.locator(".web-step .web-lead").textContent(), "There is no way to confirm it here. Sign out and sign in again.");
    assert.equal(await page.getByRole("button", { name: "Cancel" }).count(), 1);
    await page.keyboard.press("Escape");
    await outcome();
    clean(server, `hostile id ${id}`);
  }
  {
    // An account with none of the three ways has a plain-text message.
    const server = await open([me({ password: false, passkeys: [], oidc: null }), put()]);
    await start("/api/me/passphrase");
    await layer().waitFor();
    assert.equal(await page.locator(".web-step .web-lead").textContent(), "There is no way to confirm it here. Sign out and sign in again.");
    assert.equal(await page.locator(".web-step input, .web-step a").count(), 0);
    await page.keyboard.press("Escape");
    await outcome();
  }

  // ---- the account cannot be read: the layer says so, and only `Cancel` is left -------------------------------------------
  {
    const server = await open([{ path: "/api/me", abort: true }, put()]);
    await start("/api/me/passphrase");
    await layer().waitFor();
    assert.equal(await page.locator("#web-step-error").textContent(), "Could not reach the server. Try again.");
    assert.equal(await page.locator(".web-step input").count(), 0);
    assert.equal(await focusedName(page), "Cancel");
    await page.keyboard.press("Escape");
    assert.equal((await outcome()).name, "StepUpCancelled");
    clean(server, "me unreachable");
  }

  assert.deepEqual(errors, [], "the page reported errors");
}
