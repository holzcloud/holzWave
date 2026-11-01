// The first administrator and the invitation on the login page (docs/design.md 6.28) against a scripted server: the token
// that arrives in the address fragment, the password path with the authenticator step, the passkey path, the recovery step
// that cannot be left by accident, the refusals, and the places a secret must never be found.
import assert from "node:assert/strict";
import {
  INVITE_TOKEN,
  SETUP_TOKEN,
  anonymous,
  errorText,
  focusedName,
  homeStub,
  installAddressTrace,
  installAuthenticator,
  installClipboard,
  installRoutes,
  lineText,
  loginUrl,
  refusal,
} from "./login-lib.mjs";

export const backends = ["ws"];
export const backendNote = "the login page runs in a browser and needs the web bridge: http() throws on the Tauri backend";

const INVITE_GONE = "This link has expired or was used. Ask an admin for a new one.";
const SETUP_GONE = "This token has expired or was used. Ask the person who runs this server for a new one.";
const TAKEN = "That username is taken. Choose another.";
const RULE = "Use at least 12 characters. Not your username, not one repeated character.";
const MISMATCH = "That code did not match. Try again.";
const NOT_SAVED = "That passkey was not saved. Try again.";
const TICK = "Tick the box to continue.";

const SECRET = "JBSWY3DPEHPK3PXP5NMFUWYZ2LQA7TBC";
const GROUPED = "JBSW Y3DP EHPK 3PXP 5NMF UWYZ 2LQA 7TBC";
const OTPAUTH = `otpauth://totp/holzWave:anna?secret=${SECRET}&issuer=holzWave`;
const RECOVERY = "Q7K2-M9XD-T4WP-H8NB-C3FZ-6VRA";
const PASSWORD = "twelve chars ok!";

const text = (s) => Buffer.from(s).toString("base64url");
const ok = (body) => ({ status: 200, body });

const creation = {
  challenge: text("challenge"),
  rp: { name: "holzWave", id: "holz.example.net" },
  user: { id: text("user-1"), name: "anna", displayName: "Anna Keller" },
  pubKeyCredParams: [{ type: "public-key", alg: -7 }],
  excludeCredentials: [],
  extensions: { prf: { eval: { first: text("salt-salt") } } },
};

/** The route of one flow: `peek`, `begin` and `finish` answer from the step in the body. */
const enrolment = (path, handlers = {}) => ({
  method: "POST",
  path,
  reply: (req) => {
    const step = req.json?.step;
    const h = handlers[step];
    if (h) return h(req);
    if (step === "peek") return ok({ role: "member", existing: false });
    if (step === "begin") {
      return req.json.method === "passkey"
        ? ok({ next: "passkey_register", options: { publicKey: creation } })
        : ok({ next: "totp_setup", secret: SECRET, otpauth: OTPAUTH });
    }
    if (step === "finish") {
      if (req.json.credential) return ok({ next: "recovery", recovery_code: RECOVERY });
      return req.json.code === "123456" ? ok({ next: "recovery", recovery_code: RECOVERY }) : refusal(401, "invalid");
    }
    return refusal(400, "bad_request");
  },
});

export default async function ({ page, url, errors }) {
  const origin = new URL(url).origin;
  await installAuthenticator(page);
  await installClipboard(page);
  await installAddressTrace(page);
  const requested = [];
  page.on("request", (r) => requested.push({ url: r.url(), referer: r.headers().referer ?? "" }));

  const open = async (table, suffix = "") => {
    // A navigation that only changes the fragment would not load the page again.
    await page.goto("about:blank");
    await page.unrouteAll({ behavior: "wait" });
    requested.length = 0;
    const server = await installRoutes(page, table, { origin });
    await page.goto(loginUrl(url, suffix));
    await page.waitForSelector(".lg-box");
    return server;
  };
  const clean = (server, what) => {
    assert.deepEqual(server.foreign, [], `${what}: a request left the page's origin`);
    assert.deepEqual(server.unscripted, [], `${what}: a request nobody scripted`);
  };

  /** Everywhere a secret must not be: the address, the title, storage, a cookie, an attribute, a text node outside its area. */
  const scan = (needles) =>
    page.evaluate((list) => {
      const hits = [];
      const check = (where, value) => {
        for (const n of list) if (value.includes(n)) hits.push(`${where}: ${n.slice(0, 6)}...`);
      };
      check("address", location.href);
      check("title", document.title);
      check("referrer", document.referrer);
      check("localStorage", JSON.stringify({ ...localStorage }));
      check("sessionStorage", JSON.stringify({ ...sessionStorage }));
      check("cookie", document.cookie);
      for (const el of document.querySelectorAll("*")) for (const a of el.attributes) check(`attribute ${el.tagName} ${a.name}`, a.value);
      const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
      for (let node = walker.nextNode(); node; node = walker.nextNode()) {
        if (node.parentElement?.closest(".lg-secret, .lg-tx")) continue;
        check("text", node.textContent);
      }
      return hits;
    }, needles);
  const quiet = async (what) => {
    assert.equal(await page.evaluate(() => localStorage.length + sessionStorage.length), 0, `${what}: Web Storage is empty`);
    assert.equal(await page.evaluate(() => document.cookie), "", `${what}: the page sets no cookie`);
    assert.equal(await page.title(), "holzWave", `${what}: the title is the original`);
  };
  const leaveWarning = () =>
    page.evaluate(() => {
      const e = new Event("beforeunload", { cancelable: true });
      window.dispatchEvent(e);
      return e.defaultPrevented;
    });
  const copied = () => page.evaluate(() => window.__copied);
  const field = (label) => page.getByLabel(label, { exact: true });
  const press = (name) => page.getByRole("button", { name, exact: true }).click();
  const answered = () =>
    page.waitForFunction(() => document.querySelector(".lg-box")?.getAttribute("aria-busy") === "false");

  const bootstrapRoutes = (handlers) => [
    { path: "/api/session", body: anonymous({ bootstrap: true, methods: { passkey: true } }) },
    enrolment("/auth/bootstrap", handlers),
    homeStub,
  ];

  // ---- first administrator with a password: the fragment, the token, the authenticator step, the recovery step ------------
  {
    const server = await open(bootstrapRoutes(), `?authn=plain#setup=${SETUP_TOKEN}`);
    // The token left the address before the first element was drawn and is in no later address.
    assert.equal(await page.evaluate(() => location.hash), "", "the fragment is gone");
    assert.equal((await page.evaluate(() => location.href)).includes(SETUP_TOKEN), false);
    assert.equal((await page.evaluate(() => window.__atFirstRender)).includes(SETUP_TOKEN), false, "gone before the first render");
    assert.deepEqual(
      (await page.evaluate(() => window.__addresses)).filter((a) => a.includes(SETUP_TOKEN)),
      [],
      "no address given to the history holds the token",
    );
    assert.equal((await page.content()).includes(SETUP_TOKEN), false, "the token is in no attribute of the page");
    assert.equal(await page.title(), "holzWave");
    assert.equal(await page.locator("h2").first().textContent(), "Set up this server");
    assert.equal(await page.locator(".lg-lead").textContent(), "There is no administrator yet. The account you make here becomes the first one.");
    const token = field("Setup token");
    assert.equal(await token.inputValue(), SETUP_TOKEN, "the field is pre-filled from the fragment");
    assert.equal(await token.getAttribute("type"), "password", "the token is masked");
    assert.equal(await token.getAttribute("autocomplete"), "off");
    assert.equal(await page.locator(".lg-hint").textContent(), "The token the server printed when you started it.");
    assert.equal(await focusedName(page), "Username", "the first field to fill has the focus");
    assert.equal(await field("Username").getAttribute("autocomplete"), "username");
    assert.equal(await field("Password").isVisible(), false, "no password field while the passkey is chosen");
    assert.deepEqual(
      await page.locator(".lg-option").allTextContents(),
      ["Passkey", "Password and authenticator app"],
      "the choice, in the words and the order of the design",
    );
    assert.equal(await field("Passkey").isChecked(), true, "the passkey is the first choice");

    await field("Username").fill("anna");
    await field("Display name").fill("Anna Keller");
    await field("Password and authenticator app").check();
    assert.equal(await field("Password").isVisible(), true);
    assert.equal(await field("Password").getAttribute("autocomplete"), "new-password");
    await field("Password").fill(PASSWORD);
    await press("Continue");
    await page.waitForSelector(".lg-secret");

    // The authenticator step: the key in groups of four, the link, `Code`.
    assert.deepEqual(server.requests("POST", "/auth/bootstrap")[0].json, {
      step: "begin",
      token: SETUP_TOKEN,
      username: "anna",
      display_name: "Anna Keller",
      method: "password",
      password: PASSWORD,
    });
    assert.equal(await page.locator(".lg-lead").textContent(), "Add this key to your authenticator app, then enter the code it shows.");
    assert.equal(await page.locator(".lg-secret").textContent(), GROUPED);
    assert.equal(await page.locator(".lg-tx").textContent(), OTPAUTH);
    assert.deepEqual(await page.locator(".lg-step .lg-label").allTextContents(), ["Key", "Link"]);
    assert.equal(await focusedName(page), "Copy", "the person copies first");
    assert.equal(await field("Code").getAttribute("autocomplete"), "one-time-code");
    assert.equal(await field("Code").getAttribute("inputmode"), "numeric");
    assert.deepEqual(await scan([SECRET, GROUPED, OTPAUTH, SETUP_TOKEN]), [], "the secret is only in its own areas");
    await press("Copy");
    await page.waitForFunction(() => window.__copied.length === 1);
    assert.deepEqual(await copied(), [SECRET], "Copy puts the key without its spaces on the clipboard");
    assert.equal((await page.locator(".lg-below").textContent()).trim(), "Copied.");
    await press("Copy link");
    await page.waitForFunction(() => window.__copied.length === 2);
    assert.deepEqual(await copied(), [SECRET, OTPAUTH], "Copy link copies the whole link");

    // A wrong code keeps the step and says so at the field, emptied.
    await field("Code").fill("654321");
    await press("Continue");
    await page.waitForSelector('.lg-err[role="alert"]');
    assert.equal(await errorText(page), MISMATCH);
    assert.equal(await field("Code").inputValue(), "", "the code is emptied");
    assert.equal(await field("Code").getAttribute("aria-invalid"), "true");
    assert.equal(await focusedName(page), "Code");
    assert.equal(await page.locator(".lg-secret").count(), 1, "the step is still there");

    await field("Code").fill("123 456");
    await press("Continue");
    await page.waitForSelector(".lg-code");
    const finishes = server.requests("POST", "/auth/bootstrap").filter((r) => r.json.step === "finish");
    assert.deepEqual(finishes.map((r) => r.json), [{ step: "finish", code: "654321" }, { step: "finish", code: "123456" }]);
    assert.equal(finishes.some((r) => r.text.includes(SETUP_TOKEN)), false, "the token travels in the first step only");

    // The recovery step: the secret of the last step is gone, the code is shown once.
    assert.equal(await page.locator("h2").textContent(), "Save your recovery code");
    assert.equal(await page.locator(".lg-lead").textContent(), "If you forget how you unlock your saved passwords, this code unlocks them. It is shown once and cannot be shown again.");
    assert.equal(await page.locator(".lg-code").textContent(), RECOVERY);
    assert.equal(await page.locator(".lg-tx").count(), 0, "the link is removed from the page");
    assert.deepEqual(await scan([SECRET, GROUPED, OTPAUTH, SETUP_TOKEN, RECOVERY]), [], "no secret outside its own areas");
    assert.equal(await page.locator("body").evaluate((b, s) => b.textContent.includes(s), SECRET), false, "the authenticator secret is cleared");
    assert.equal(await focusedName(page), "Copy");
    assert.equal(await page.getByRole("button", { name: "Continue", exact: true }).isDisabled(), true, "Continue waits for the box");
    assert.equal(await page.locator(".lg-note").textContent(), TICK);
    assert.equal(await leaveWarning(), true, "a reload or a close asks the browser to confirm");

    // The step cannot be left: Escape, a click outside and the browser's back action do nothing.
    const here = await page.url();
    await page.keyboard.press("Escape");
    await page.mouse.click(2, 2);
    await page.goBack();
    assert.equal(await page.url(), here, "Back keeps the step");
    assert.equal(await page.locator(".lg-code").textContent(), RECOVERY);
    await page.getByLabel("I have saved this code").focus();
    await page.keyboard.press("Escape");
    assert.equal(await page.locator(".lg-code").count(), 1);

    await page.getByLabel("I have saved this code").check();
    assert.equal(await page.getByRole("button", { name: "Continue", exact: true }).isEnabled(), true);
    assert.equal(await page.locator(".lg-note").textContent(), "", "the line goes when the box is ticked");
    assert.equal(await leaveWarning(), false, "no warning once the code is saved");
    await page.getByLabel("I have saved this code").uncheck();
    assert.equal(await page.getByRole("button", { name: "Continue", exact: true }).isDisabled(), true);
    assert.equal(await leaveWarning(), true);
    await page.getByLabel("I have saved this code").check();

    // Continue removes the code from the page, not hides it, and then goes to the app.
    await page.exposeFunction("__after", (report) => (afterContinue = report));
    let afterContinue;
    await page.evaluate(() => {
      document.querySelector(".lg-step button.lg-full").addEventListener("click", () => {
        window.__after({
          secrets: document.querySelectorAll(".lg-secret, .lg-code").length,
          text: document.body.textContent,
          warned: (() => {
            const e = new Event("beforeunload", { cancelable: true });
            window.dispatchEvent(e);
            return e.defaultPrevented;
          })(),
        });
      });
    });
    await press("Continue");
    await page.waitForURL(`${origin}/`);
    assert.equal(afterContinue.secrets, 0, "the code element is removed");
    assert.equal(afterContinue.text.includes(RECOVERY), false, "the code is in no text of the page after Continue");
    assert.equal(afterContinue.warned, false, "no leave warning after Continue");

    // Nothing the person typed or was shown went anywhere but the body of a request to the page's own origin.
    for (const r of requested) {
      assert.equal(r.url.includes(SETUP_TOKEN) || r.url.includes(SECRET) || r.url.includes(RECOVERY) || r.url.includes(PASSWORD), false, `address ${r.url}`);
      assert.equal(r.referer.includes(SETUP_TOKEN), false, "no referrer holds the token");
    }
    clean(server, "first administrator with a password");
  }

  // The same page again, to read the storage and the title of a finished flow (the stub page of the app has none of it).
  {
    const server = await open(bootstrapRoutes(), `#setup=${SETUP_TOKEN}`);
    await field("Username").fill("anna");
    await field("Display name").fill("Anna");
    await field("Password and authenticator app").check();
    await field("Password").fill(PASSWORD);
    await press("Continue");
    await page.waitForSelector(".lg-secret");
    await field("Code").fill("123456");
    await press("Continue");
    await page.waitForSelector(".lg-code");
    await quiet("recovery step");
    assert.deepEqual(await scan([SECRET, GROUPED, SETUP_TOKEN]), []);
    clean(server, "storage and title");
  }

  // ---- a refused password, a token typed by hand, a token that is no longer good -----------------------------------------
  {
    let refusals = 0;
    const server = await open(
      bootstrapRoutes({
        begin: (req) => {
          if (refusals++ === 0) return refusal(400, "bad_request", { field: "password" });
          return ok({ next: "totp_setup", secret: SECRET, otpauth: OTPAUTH });
        },
      }),
    );
    assert.equal(await focusedName(page), "Setup token", "with no token in the address the token field has the focus");
    assert.equal(await field("Setup token").inputValue(), "");
    await field("Setup token").fill(SETUP_TOKEN);
    await field("Username").fill("anna");
    await field("Display name").fill("Anna Keller");
    await field("Password and authenticator app").check();
    await field("Password").fill("short");
    await press("Continue");
    await page.waitForSelector('.lg-err[role="alert"]');
    assert.equal(await errorText(page), RULE);
    assert.equal(await page.locator(".lg-f", { has: page.getByLabel("Password", { exact: true }) }).locator(".lg-err").textContent(), RULE, "under the password field");
    assert.equal(await field("Password").inputValue(), "", "the password is emptied");
    assert.equal(await field("Username").inputValue(), "anna", "everything else is kept");
    assert.equal(await field("Display name").inputValue(), "Anna Keller");
    assert.equal(await field("Setup token").inputValue(), SETUP_TOKEN);
    assert.equal(await field("Password").getAttribute("aria-invalid"), "true");
    assert.equal(await focusedName(page), "Password");
    assert.equal(server.requests("POST", "/auth/bootstrap")[0].json.token, SETUP_TOKEN, "a typed token is sent in the body");
    await field("Password").fill(PASSWORD);
    await press("Continue");
    await page.waitForSelector(".lg-secret");
    clean(server, "refused password");
  }
  {
    const server = await open(bootstrapRoutes({ begin: () => refusal(410, "gone") }), `#setup=${SETUP_TOKEN}`);
    await field("Username").fill("anna");
    await field("Password and authenticator app").check();
    await field("Password").fill(PASSWORD);
    await press("Continue");
    await page.waitForSelector('[role="alert"]');
    assert.equal(await errorText(page), SETUP_GONE);
    assert.equal(await page.locator("form").count(), 0, "no form for a token that is no longer good");
    assert.equal(await page.locator("h2").textContent(), "Set up this server");
    clean(server, "setup token gone");
  }

  // ---- first administrator with a passkey: the options, the PRF result, the cancel and the failure ------------------------
  for (const withPrf of [true, false]) {
    const server = await open(bootstrapRoutes(), `?authn=${withPrf ? "prf" : "plain"}#setup=${SETUP_TOKEN}`);
    await field("Username").fill("anna");
    await field("Display name").fill("Anna Keller");
    await press("Continue");
    await page.waitForSelector(".lg-code");
    const [begin, finish] = server.requests("POST", "/auth/bootstrap");
    assert.deepEqual(begin.json, { step: "begin", token: SETUP_TOKEN, username: "anna", display_name: "Anna Keller", method: "passkey" });
    assert.equal("password" in begin.json, false, "no password member for a passkey");
    const asked = await page.evaluate(() => window.__authn);
    assert.equal(asked.length, 1);
    assert.equal(asked[0].kind, "create");
    assert.deepEqual(asked[0].options.challenge, { binary: text("challenge") });
    assert.deepEqual(asked[0].options.user.id, { binary: text("user-1") });
    assert.deepEqual(asked[0].options.extensions, { prf: { eval: { first: { binary: text("salt-salt") } } } });
    assert.equal(finish.json.step, "finish");
    assert.equal(finish.json.credential.id, "Y3JlZC1pZA");
    assert.equal(finish.json.credential.response.attestationObject, Buffer.from([7, 8, 9]).toString("base64url"));
    assert.deepEqual(finish.json.credential.response.transports, ["internal"]);
    assert.equal("prf" in finish.json, withPrf, withPrf ? "the PRF result goes with the credential" : "no result, no member");
    if (withPrf) assert.equal(finish.json.prf, Buffer.from(Array.from({ length: 32 }, (_, i) => i + 1)).toString("base64url"));
    assert.equal(finish.text.includes(SETUP_TOKEN), false);
    assert.equal(await page.locator(".lg-code").textContent(), RECOVERY, "the recovery step follows");
    clean(server, `passkey ${withPrf ? "with" : "without"} PRF`);
  }
  {
    const server = await open(bootstrapRoutes(), `?authn=cancel#setup=${SETUP_TOKEN}`);
    await field("Username").fill("anna");
    await press("Continue");
    await page.waitForFunction(() => window.__authn?.length === 1);
    await answered();
    assert.equal(await lineText(page), "", "a closed prompt shows nothing");
    assert.equal(await page.getByRole("button", { name: "Continue", exact: true }).isEnabled(), true);
    assert.equal(await field("Username").inputValue(), "anna", "what was typed is kept");
    assert.equal(server.requests("POST", "/auth/bootstrap").filter((r) => r.json.step === "finish").length, 0);
    clean(server, "passkey cancelled");

    const broken = await open(bootstrapRoutes(), `?authn=error#setup=${SETUP_TOKEN}`);
    await field("Username").fill("anna");
    await press("Continue");
    await page.waitForSelector('[role="alert"]');
    assert.equal(await errorText(page), NOT_SAVED);
    clean(broken, "passkey failed");

    const refused = await open(bootstrapRoutes({ finish: () => refusal(401, "invalid") }), `?authn=plain#setup=${SETUP_TOKEN}`);
    await field("Username").fill("anna");
    await press("Continue");
    await page.waitForSelector('[role="alert"]');
    assert.equal(await errorText(page), NOT_SAVED, "the server refused the credential");
    clean(refused, "passkey refused");
  }

  // ---- an invitation: the peek, the role, the preset name, the taken name that keeps the token usable --------------------
  {
    let taken = 0;
    const server = await open(
      [
        { path: "/api/session", body: anonymous() },
        enrolment("/auth/invite/redeem", {
          peek: () => ok({ role: "admin", existing: false }),
          begin: (req) => {
            if (taken++ === 0) return refusal(409, "conflict", { field: "username" });
            return ok({ next: "totp_setup", secret: SECRET, otpauth: OTPAUTH });
          },
        }),
        homeStub,
      ],
      `#invite=${INVITE_TOKEN}`,
    );
    assert.equal(await page.evaluate(() => location.hash), "");
    assert.equal((await page.evaluate(() => window.__atFirstRender)).includes(INVITE_TOKEN), false);
    assert.equal((await page.content()).includes(INVITE_TOKEN), false);
    assert.deepEqual(server.requests("POST", "/auth/invite/redeem")[0].json, { step: "peek", token: INVITE_TOKEN }, "the peek sends the token in the body");
    assert.equal(await page.locator("h2").textContent(), "You were invited");
    assert.equal(await page.locator(".lg-chip").textContent(), "administrator", "the role as a label");
    assert.equal(await page.locator(".lg-title .lg-chip").count(), 1, "in the title row");
    assert.equal(await page.getByLabel("Setup token").count(), 0, "no token field for an invitation");
    assert.equal(await focusedName(page), "Username");
    await field("Username").fill("anna");
    await field("Display name").fill("Anna Keller");
    await field("Password and authenticator app").check();
    await field("Password").fill(PASSWORD);
    await press("Continue");
    await page.waitForSelector('.lg-err[role="alert"]');
    assert.equal(await errorText(page), TAKEN);
    assert.equal(await field("Username").getAttribute("aria-invalid"), "true");
    assert.equal(await focusedName(page), "Username", "the focus goes to the taken name");
    assert.match(await page.locator(".lg-err").first().evaluate((el) => getComputedStyle(el, "::before").content), /!/);
    assert.match(await page.locator(".lg-err").first().evaluate((el) => getComputedStyle(el).textDecorationLine), /underline/);
    assert.equal(await field("Password").inputValue(), "", "the password is emptied");

    // The token is still good: a second begin with another name carries it again.
    await field("Username").fill("anna2");
    await field("Password").fill(PASSWORD);
    await press("Continue");
    await page.waitForSelector(".lg-secret");
    const begins = server.requests("POST", "/auth/invite/redeem").filter((r) => r.json.step === "begin");
    assert.deepEqual(begins.map((r) => [r.json.token, r.json.username]), [[INVITE_TOKEN, "anna"], [INVITE_TOKEN, "anna2"]]);
    assert.equal(await page.locator(".lg-chip").count(), 0, "the authenticator step has the title without the role");
    await field("Code").fill("123456");
    await press("Continue");
    await page.waitForSelector(".lg-code");
    assert.equal(await page.locator(".lg-code").textContent(), RECOVERY);
    await page.getByLabel("I have saved this code").check();
    await press("Continue");
    await page.waitForURL(`${origin}/`);
    clean(server, "invitation");
  }
  {
    const hostile = "<img src=x onerror=\"window.__pwned=1\">";
    const server = await open(
      [
        { path: "/api/session", body: anonymous() },
        enrolment("/auth/invite/redeem", { peek: () => ok({ role: "member", existing: true, username: hostile }) }),
      ],
      `#invite=${INVITE_TOKEN}`,
    );
    assert.equal(await page.locator(".lg-chip").textContent(), "member");
    assert.equal(await field("Username").inputValue(), hostile, "a preset name is shown as text");
    assert.equal(await field("Username").evaluate((el) => el.readOnly), true, "a preset name is read-only");
    assert.equal(await focusedName(page), "Display name", "the first field to fill has the focus");
    assert.equal(await page.locator("img").count(), 0);
    assert.equal(await page.evaluate(() => window.__pwned), undefined);
    clean(server, "preset name");
  }
  {
    const server = await open(
      [{ path: "/api/session", body: anonymous() }, enrolment("/auth/invite/redeem", { peek: () => refusal(410, "gone") })],
      `#invite=${INVITE_TOKEN}`,
    );
    assert.equal(await errorText(page), INVITE_GONE);
    assert.equal(await page.locator("form").count(), 0, "a link that is no longer good never shows a form");
    assert.equal(await page.locator("h2").textContent(), "You were invited");
    assert.equal(await page.evaluate(() => location.hash), "", "the fragment is gone either way");
    clean(server, "invitation gone");
  }

  // ---- fragments the page does not take ----------------------------------------------------------------------------------
  for (const hash of ["#invite=hwi_short", `#setup=${SETUP_TOKEN}`, `#invite=${SETUP_TOKEN}`, `#invite=${INVITE_TOKEN}&setup=${SETUP_TOKEN}`]) {
    const server = await open([{ path: "/api/session", body: anonymous() }], hash);
    assert.equal(await page.locator("h1").textContent(), "holzWave");
    assert.equal(await page.getByRole("button", { name: "Continue", exact: true }).count(), 1, `${hash.slice(0, 12)}: the sign-in screen`);
    assert.equal(await page.getByLabel("Setup token").count(), 0);
    assert.equal(await page.evaluate(() => location.hash), "", "the fragment is removed whatever it held");
    assert.deepEqual(server.log.map((l) => `${l.method} ${l.path}`), ["GET /api/session"], "no token is sent to a route that does not expect it");
    clean(server, hash.slice(0, 12));
  }

  assert.deepEqual(errors, [], "no page error and no policy violation");
}
