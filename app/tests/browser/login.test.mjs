// The sign-in screen of the login page (docs/design.md 6.28) against a scripted server: password and code, passkey with and
// without the PRF result, single sign-on, the one sentence of a failed sign-in, throttling, an unreachable server, the
// keyboard and the focus ring, and the rule that the page asks nothing but its own origin.
import assert from "node:assert/strict";
import {
  anonymous,
  errorText,
  focusedName,
  homeStub,
  installAuthenticator,
  installRoutes,
  lineText,
  loginUrl,
  refusal,
} from "./login-lib.mjs";

export const backends = ["ws"];
export const backendNote = "the login page runs in a browser and needs the web bridge: http() throws on the Tauri backend";

const FAILED = "Sign-in failed. Check your details or ask an admin.";
const BUSY = "The server is busy. Try again in a moment.";
const UNREACHABLE = "Could not reach the server. Try again.";
const ENDED = "Your session ended. Sign in again.";

const b64 = (...bytes) => Buffer.from(bytes).toString("base64url");
const b64text = (s) => Buffer.from(s).toString("base64url");

export default async function ({ engine, page, url, errors }) {
  const origin = new URL(url).origin;
  await installAuthenticator(page);
  const open = async (table, suffix = "") => {
    await page.unrouteAll({ behavior: "wait" });
    const server = await installRoutes(page, table, { origin });
    await page.goto(loginUrl(url, suffix));
    return server;
  };
  const clean = (server, what) => {
    assert.deepEqual(server.foreign, [], `${what}: a request left the page's origin`);
    assert.deepEqual(server.unscripted, [], `${what}: a request nobody scripted`);
  };
  /** Wait until the step is idle again and the line has a sentence. */
  const answered = () =>
    page.waitForFunction(
      () => document.querySelector(".lg-box")?.getAttribute("aria-busy") === "false" && (document.querySelector(".lg-line")?.textContent ?? "") !== "",
    );
  const ready = () => page.waitForSelector(".lg-box");
  const password = () => page.getByLabel("Password");
  const username = () => page.getByLabel("Username");

  // ---- tracer: a password and a code sign a person in ---------------------------------------------------------------
  {
    const server = await open([
      { path: "/api/session", body: anonymous() },
      { method: "POST", path: "/auth/password", body: { next: "totp" }, delay: 400 },
      { method: "POST", path: "/auth/totp", body: { next: "done" } },
      homeStub,
    ]);
    await ready();
    assert.equal(await page.title(), "holzWave");
    assert.equal(await page.locator("h1").textContent(), "holzWave");
    assert.equal(await page.locator(".lg-host").textContent(), "holz.example.net");
    assert.equal(await page.locator("main.lg-box").count(), 1, "the box is the main landmark");
    assert.equal(await focusedName(page), "Username", "the first field has the focus");
    assert.equal(await page.getByRole("button", { name: "Sign in with a passkey" }).count(), 0, "no passkey button when the server offers none");
    assert.equal(await page.locator(".lg-or").count(), 0, "no `or` without a button above the form");

    await username().fill("holz");
    await password().fill("correct horse battery staple");
    await page.getByRole("button", { name: "Continue" }).click();
    // While the request is out: the label adds the dots, the box is busy, the password already left the field.
    await page.waitForFunction(() => document.querySelector(".lg-box")?.getAttribute("aria-busy") === "true");
    assert.equal(await page.getByRole("button", { name: "Continue…" }).count(), 1);
    assert.equal(await password().inputValue(), "", "the password is cleared as soon as the request is sent");
    assert.equal(await username().evaluate((el) => el.readOnly), true, "the fields are read-only while working");

    await page.getByLabel("Code").waitFor();
    assert.equal(await page.locator(".lg-lead").textContent(), "Signing in as holz. Enter the six-digit code of your authenticator app.");
    assert.equal(await page.locator(".lg-lead b").textContent(), "holz");
    assert.equal(await focusedName(page), "Code");
    assert.equal(await page.getByLabel("Code").getAttribute("autocomplete"), "one-time-code");
    assert.equal(await page.getByLabel("Code").getAttribute("inputmode"), "numeric");
    await page.getByLabel("Code").fill("482 913");
    await page.getByRole("button", { name: "Sign in" }).click();
    await page.waitForURL(`${origin}/`);

    const first = server.requests("POST", "/auth/password");
    const second = server.requests("POST", "/auth/totp");
    assert.equal(first.length, 1);
    assert.equal(second.length, 1);
    assert.deepEqual(first[0].json, { username: "holz", password: "correct horse battery staple" });
    assert.deepEqual(second[0].json, { code: "482913" });
    assert.equal(first[0].headers["content-type"], "application/json");
    assert.equal(first[0].headers["x-csrf-token"], undefined, "the anonymous answer has no token, so the sign-in steps carry none");
    clean(server, "tracer");
    console.log(
      `login tracer: POST /auth/password ${JSON.stringify({ username: first[0].json.username, password: `<${first[0].json.password.length} characters>` })}; POST /auth/totp ${JSON.stringify(second[0].json)}`,
    );
  }

  // ---- the page loads nothing inline and opens no socket ------------------------------------------------------------
  {
    const server = await open([{ path: "/api/session", body: anonymous() }]);
    await ready();
    assert.equal(await page.locator("script:not([src])").count(), 0, "no inline script");
    assert.equal(await page.locator("style").count(), 0, "no inline style element");
    assert.equal(await page.locator("[style]").count(), 0, "no style attribute");
    assert.equal(await page.evaluate(() => window.__WS_MOCK__.urls.length), 0, "the page opens no socket");
    assert.equal(await page.evaluate(() => window.__WS_MOCK__.sent.length), 0);
    assert.deepEqual(
      server.log.map((l) => `${l.method} ${l.path}`),
      ["GET /api/session"],
      "the page asks GET /api/session once and nothing else",
    );
    clean(server, "static");
  }

  // ---- the same sentence for every refused sign-in, the wait text, an unreachable server ---------------------------------
  {
    const server = await open([
      { path: "/api/session", body: anonymous() },
      {
        method: "POST",
        path: "/auth/password",
        answers: [
          refusal(401, "invalid"),
          refusal(429, "throttled", { retry_seconds: 720 }),
          refusal(503, "busy"),
          { abort: true },
          refusal(500, "internal"),
          refusal(429, "throttled", { retry_seconds: 40 }),
          { body: { next: "totp" } },
        ],
      },
      {
        method: "POST",
        path: "/auth/totp",
        answers: [refusal(401, "invalid"), refusal(429, "throttled", { retry_seconds: 61 }), refusal(401, "invalid")],
      },
    ]);
    await ready();
    await username().fill("nobody");
    const submit = async () => {
      await password().fill("not the password");
      await page.getByRole("button", { name: "Continue" }).click();
      await answered();
    };

    await submit();
    assert.equal(await errorText(page), FAILED);
    assert.equal(await password().inputValue(), "", "the password is emptied after a refusal");
    assert.equal(await username().inputValue(), "nobody", "the username is kept");
    assert.equal(await focusedName(page), "Password", "the focus goes to the first field that was emptied");
    assert.equal(await password().getAttribute("aria-invalid"), "true");
    assert.equal(await page.locator(".lg-line").getAttribute("role"), "alert");
    assert.match(await page.locator(".lg-line").evaluate((el) => getComputedStyle(el, "::before").content), /!/, "the error mark");
    assert.match(await page.locator(".lg-line").evaluate((el) => getComputedStyle(el).textDecorationLine), /underline/, "the underline");
    assert.equal(await page.title(), "holzWave");

    await submit();
    assert.equal(await errorText(page), "Too many attempts. Try again in 12 minutes.");
    await submit();
    assert.equal(await errorText(page), BUSY);
    await submit();
    assert.equal(await errorText(page), UNREACHABLE, "a request that never arrived");
    await submit();
    assert.equal(await errorText(page), UNREACHABLE, "a failing server");
    await submit();
    assert.equal(await errorText(page), "Too many attempts. Try again in 40 seconds.");

    // The next sign-in gets through to the code step; a wrong code says the very same sentence as a wrong password.
    await password().fill("not the password");
    await page.getByRole("button", { name: "Continue" }).click();
    await page.getByLabel("Code").waitFor();
    await page.getByLabel("Code").fill("111111");
    await page.getByRole("button", { name: "Sign in" }).click();
    await answered();
    assert.equal(await errorText(page), FAILED, "the wrong code says what the wrong password said");
    assert.equal(await page.getByLabel("Code").inputValue(), "111111", "the code is kept");
    assert.equal(await focusedName(page), "Code", "the focus is on the first field of the failed step");
    await page.getByRole("button", { name: "Sign in" }).click();
    await answered();
    assert.equal(await errorText(page), "Too many attempts. Try again in 2 minutes.");

    // `Back` returns to the first step with the username kept and the password empty.
    await page.getByRole("button", { name: "Back" }).click();
    await page.getByLabel("Username").waitFor();
    assert.equal(await username().inputValue(), "nobody");
    assert.equal(await password().inputValue(), "");
    assert.equal(await focusedName(page), "Password");
    assert.equal(await lineText(page), "");
    clean(server, "refusals");
  }

  // ---- the keyboard: the order, Enter, the ring, no focus lost on a state change ------------------------------------
  {
    const server = await open(
      [
        { path: "/api/session", body: anonymous({ methods: { passkey: true, oidc: [{ id: "corp", name: "Company SSO" }] } }) },
        { method: "POST", path: "/auth/password", body: refusal(401, "invalid").body, status: 401 },
      ],
      "?authn=plain",
    );
    await ready();
    assert.equal(await focusedName(page), "Username", "the first field has the focus even though the passkey button is above it");
    // A click on the empty ground moves the starting point of the Tab key to the top of the page.
    await page.mouse.click(2, 2);
    const order = [];
    const rings = [];
    // WebKit on macOS leaves buttons and links out of the Tab order unless Alt is held (the system setting for full keyboard
    // access); Alt+Tab is the same walk there.
    const tab = engine === "webkit" && process.platform === "darwin" ? "Alt+Tab" : "Tab";
    for (let i = 0; i < 5; i++) {
      await page.keyboard.press(tab);
      order.push(await focusedName(page));
      rings.push(await page.evaluate(() => getComputedStyle(document.activeElement).outlineWidth));
    }
    assert.deepEqual(order, ["Sign in with a passkey", "Continue with Company SSO", "Username", "Password", "Continue"]);
    assert.deepEqual(rings, ["2px", "2px", "2px", "2px", "2px"], "the focus ring is 2 px");
    assert.equal(await page.locator(".lg-or").count(), 1, "`or` between the buttons and the form");
    assert.equal(await page.locator(".lg-solid").count(), 1, "one solid button");
    assert.equal(await page.getByRole("button", { name: "Sign in with a passkey" }).evaluate((el) => el.classList.contains("lg-solid")), true);

    // Enter in a field presses the button of the form; the state change does not take the focus away from the field.
    await username().fill("holz");
    await password().fill("x");
    await password().press("Enter");
    await page.waitForFunction(() => document.querySelector(".lg-line")?.textContent);
    await answered();
    assert.equal(server.count("POST", "/auth/password"), 1, "Enter submits");
    assert.equal(await focusedName(page), "Password");
    clean(server, "keyboard");
  }

  // ---- the passkey: the options are decoded, the PRF result travels only when there is one ------------------------------
  const options = {
    challenge: b64text("challenge"),
    rpId: "holz.example.net",
    userVerification: "preferred",
    allowCredentials: [{ id: b64text("cred-id"), type: "public-key" }],
    extensions: { prf: { eval: { first: b64text("salt-salt") } } },
  };
  const passkeyRoutes = [
    { path: "/api/session", body: anonymous({ methods: { passkey: true, password: false } }) },
    { method: "POST", path: "/auth/passkey/begin", body: { options: { publicKey: options } } },
    { method: "POST", path: "/auth/passkey/finish", body: { next: "done" } },
    homeStub,
  ];
  for (const withPrf of [true, false]) {
    const server = await open(passkeyRoutes, `?authn=${withPrf ? "prf" : "plain"}`);
    await ready();
    assert.equal(await focusedName(page), "Sign in with a passkey", "with no earlier control the passkey button has the focus");
    await page.getByRole("button", { name: "Sign in with a passkey" }).click();
    await page.waitForURL(`${origin}/`);
    const finish = server.requests("POST", "/auth/passkey/finish");
    assert.equal(finish.length, 1);
    const expected = {
      credential: {
        id: "Y3JlZC1pZA",
        rawId: b64text("cred-id"),
        type: "public-key",
        response: {
          authenticatorData: b64(1, 2, 3),
          clientDataJSON: b64text('{"type":"webauthn.get"}'),
          signature: b64(4, 5, 6),
          userHandle: null,
        },
      },
    };
    if (withPrf) expected.prf = b64(...Array.from({ length: 32 }, (_, i) => i + 1));
    assert.deepEqual(finish[0].json, expected);
    assert.equal("prf" in finish[0].json, withPrf, withPrf ? "the PRF result goes beside the assertion" : "no PRF result, no member");
    assert.deepEqual(server.requests("POST", "/auth/passkey/begin")[0].json, {});
    clean(server, `passkey ${withPrf ? "with" : "without"} PRF`);
  }
  {
    // The options the stand-in received (the page is gone after the redirect, so ask while it waits for the finish step).
    const server = await open(
      [
        { path: "/api/session", body: anonymous({ methods: { passkey: true } }) },
        { method: "POST", path: "/auth/passkey/begin", body: { options: { publicKey: options } } },
        { method: "POST", path: "/auth/passkey/finish", body: { next: "done" }, delay: 600 },
        homeStub,
      ],
      "?authn=prf",
    );
    await ready();
    await page.getByRole("button", { name: "Sign in with a passkey" }).click();
    await page.waitForFunction(() => window.__authn?.length === 1);
    const seen = await page.evaluate(() => window.__authn[0]);
    assert.equal(seen.kind, "get");
    assert.deepEqual(seen.options.challenge, { binary: b64text("challenge") });
    assert.deepEqual(seen.options.allowCredentials, [{ id: { binary: b64text("cred-id") }, type: "public-key" }]);
    assert.deepEqual(seen.options.extensions, { prf: { eval: { first: { binary: b64text("salt-salt") } } } });
    assert.equal(seen.options.rpId, "holz.example.net");
    assert.equal(await page.getByRole("button", { name: "Sign in with a passkey…" }).count(), 1, "the pressed button adds the dots");
    await page.waitForURL(`${origin}/`);
    clean(server, "passkey options");
  }
  {
    // The person closes the prompt: back to idle, no message. Any other failure: the failed sentence.
    const server = await open(passkeyRoutes, "?authn=cancel");
    await ready();
    await page.getByRole("button", { name: "Sign in with a passkey" }).click();
    await page.waitForFunction(() => window.__authn?.length === 1 && document.querySelector(".lg-box").getAttribute("aria-busy") === "false");
    assert.equal(await lineText(page), "", "a cancelled prompt shows nothing");
    assert.equal(await errorText(page), "");
    assert.equal(server.count("POST", "/auth/passkey/finish"), 0);
    assert.equal(await focusedName(page), "Sign in with a passkey");
    assert.equal(await page.getByRole("button", { name: "Sign in with a passkey" }).isEnabled(), true);
    clean(server, "passkey cancel");

    const broken = await open(passkeyRoutes, "?authn=error");
    await ready();
    await page.getByRole("button", { name: "Sign in with a passkey" }).click();
    await answered();
    assert.equal(await errorText(page), FAILED);
    assert.equal(broken.count("POST", "/auth/passkey/finish"), 0);
    clean(broken, "passkey error");

    // The server refuses the assertion: the same sentence again.
    const refused = await open(
      [
        passkeyRoutes[0],
        passkeyRoutes[1],
        { method: "POST", path: "/auth/passkey/finish", ...refusal(401, "invalid") },
      ],
      "?authn=plain",
    );
    await ready();
    await page.getByRole("button", { name: "Sign in with a passkey" }).click();
    await answered();
    assert.equal(await errorText(page), FAILED);
    clean(refused, "passkey refused");
  }
  {
    // The button is drawn only when the server offers passkeys and the browser has them.
    const none = await open([{ path: "/api/session", body: anonymous({ methods: { passkey: false } }) }], "?authn=plain");
    await ready();
    assert.equal(await page.getByRole("button", { name: "Sign in with a passkey" }).count(), 0);
    clean(none, "no passkey offered");
    const noBrowser = await open([{ path: "/api/session", body: anonymous({ methods: { passkey: true } }) }], "?nopk=1");
    await ready();
    assert.equal(await page.getByRole("button", { name: "Sign in with a passkey" }).count(), 0);
    assert.equal(await page.getByRole("button", { name: "Continue" }).evaluate((el) => el.classList.contains("lg-solid")), true, "Continue is the solid button");
    clean(noBrowser, "no browser support");
  }

  // ---- single sign-on: links for the providers in the answer, nothing for a hostile name ---------------------------------
  {
    const hostile = "<img src=x onerror=\"window.__pwned=1\">";
    const server = await open([
      {
        path: "/api/session",
        body: anonymous({
          methods: {
            oidc: [
              { id: "corp", name: "Company SSO" },
              { id: "evil", name: hostile },
              { id: "../x", name: "Bad id" },
            ],
          },
        }),
      },
    ]);
    await ready();
    const links = await page.locator("a.lg-btn").evaluateAll((els) => els.map((el) => [el.textContent, el.getAttribute("href")]));
    assert.deepEqual(links, [
      ["Continue with Company SSO", "/auth/oidc/corp/start"],
      [`Continue with ${hostile}`, "/auth/oidc/evil/start"],
    ]);
    assert.equal(await page.locator("img").count(), 0, "a provider name is text, never markup");
    assert.equal(await page.evaluate(() => window.__pwned), undefined);
    assert.equal(await page.locator(".lg-or").count(), 1);
    clean(server, "providers");
  }
  {
    const server = await open([{ path: "/api/session", body: anonymous({ methods: { password: false, oidc: [{ id: "corp", name: "Company SSO" }] } }) }]);
    await ready();
    assert.equal(await page.locator("form").count(), 0, "no password form when the server has the password method off");
    assert.equal(await page.locator(".lg-or").count(), 0);
    assert.equal(await page.locator("a.lg-btn").count(), 1);
    clean(server, "providers only");
  }

  // ---- what the address can say: a failed provider, an ended session, a signed-in person -----------------------------------
  {
    const server = await open([{ path: "/api/session", body: anonymous({ methods: { oidc: [{ id: "corp", name: "Company SSO" }] } }) }], "?error=oidc");
    await ready();
    assert.equal(await errorText(page), FAILED);
    assert.equal(await page.evaluate(() => location.search), "", "the flag leaves the address at once");
    clean(server, "oidc error");
  }
  {
    const server = await open(
      [
        { path: "/api/session", body: anonymous() },
        { method: "POST", path: "/auth/password", body: { next: "totp" } },
        { method: "POST", path: "/auth/totp", body: { next: "done" } },
        homeStub,
      ],
      "?ended=1",
    );
    await ready();
    assert.equal(await lineText(page), ENDED);
    assert.equal(await errorText(page), "", "nothing failed, so it is not an alert");
    assert.equal(await page.locator(".lg-line").getAttribute("data-tone"), "plain");
    assert.doesNotMatch(await page.locator(".lg-line").evaluate((el) => getComputedStyle(el, "::before").content), /!/, "plain ink and no mark");
    assert.equal(await page.evaluate(() => location.search), "");
    await username().fill("holz");
    await password().fill("pw");
    await page.getByRole("button", { name: "Continue" }).click();
    await page.getByLabel("Code").fill("123456");
    await page.getByRole("button", { name: "Sign in" }).click();
    await page.waitForURL(`${origin}/`);
    clean(server, "ended");
  }
  {
    for (const state of ["signed_in", "locked"]) {
      const server = await open([
        { path: "/api/session", body: { state, host: "holz.example.net", csrf: "tok", account: { id: "a1", username: "holz", display_name: "Holz", role: "admin" }, seconds: 900, step_up: 0 } },
        homeStub,
      ]);
      await page.waitForURL(`${origin}/`);
      assert.equal(server.count("GET", "/"), 1, `${state}: the location is replaced with /`);
      clean(server, state);
    }
  }
  {
    // The server cannot be reached at all: the box still draws, with the sentence.
    const server = await open([{ path: "/api/session", abort: true }]);
    await ready();
    assert.equal(await errorText(page), UNREACHABLE);
    assert.equal(await page.locator("h1").textContent(), "holzWave");
    assert.deepEqual(server.foreign, []);
  }

  assert.deepEqual(errors, [], "no page error and no policy violation");
}
