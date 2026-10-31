// Single sign-on in a real browser: the browser leaves for a provider on another origin (https://127.0.0.1:<port> is another
// site than the page's host) and comes back by a top-level GET that carries the 10-minute Lax cookie, and ends on the
// signed-in page with the Strict session cookie. A replayed callback, a callback without the cookie and an identity outside
// the allowed domain and group all end on the failed sentence, and no account is made for them.
import { runTest, openPage, signIn, openPeople, check, equal, poll, answered } from "./lib.mjs";

const FAILED = "Sign-in failed. Check your details or ask an admin.";

await runTest("oidc", { oidc: true }, async ({ harness, open }) => {
  const idpOrigin = harness.idpOrigin;
  check(idpOrigin !== null && idpOrigin.startsWith("https://127.0.0.1:"), "the harness started a provider on 127.0.0.1");
  const admin = await harness.account({ role: "admin" });
  const adminContext = await open({ allow: [idpOrigin] });
  const adminPage = await adminContext.context.newPage();
  await signIn(adminPage, harness, admin);

  /** The number of people the administrator's dialog lists now. */
  const people = async () => {
    const dialog = await openPeople(adminPage);
    const rows = await adminPage.locator(".people tbody tr").count();
    await dialog.getByRole("button", { name: "Close" }).click();
    return rows;
  };
  const strip = (address) => {
    const url = new URL(address);
    return `${url.origin}${url.pathname}`;
  };
  const allowed = { sub: "sub-alice", email: "alice@example.org", name: "Alice Anders", groups: [] };
  const before = await people();
  const hops = [];
  let callbackUrl = "";
  let cookiesBefore = null;
  let sessionCookie = null;

  // ---- the hop: start, provider, callback, signed in ---------------------------------------------------------------------
  await harness.idp(allowed);
  {
    const context = await open({ allow: [idpOrigin] });
    const page = await context.context.newPage();
    // A request event fires for every step of a redirect chain, which a committed navigation does not.
    page.on("request", (request) => {
      if (request.isNavigationRequest() && request.frame() === page.mainFrame()) hops.push(strip(request.url()));
    });
    page.on("request", (request) => {
      if (new URL(request.url()).pathname === "/auth/oidc/callback") callbackUrl = request.url();
    });
    await page.goto(`${harness.origin}/`);
    await page.waitForSelector(".lg-box");
    cookiesBefore = await context.context.cookies(harness.origin);
    await page.getByRole("link", { name: "Continue with Company" }).click();
    await page.waitForSelector("canvas#term", { timeout: 30_000 });
    const after = await context.context.cookies(harness.origin);
    sessionCookie = after.find((c) => c.name === "__Host-hw_session");
    check(sessionCookie !== undefined, "the session cookie is set after the return");
    equal(sessionCookie.sameSite, "Strict", "the session cookie's SameSite");
    equal(sessionCookie.httpOnly, true, "the session cookie is HttpOnly");
    equal(sessionCookie.secure, true, "the session cookie is Secure");
    equal(after.some((c) => c.name === "__Host-hw_oidc"), false, "the transaction cookie is gone after the return");
    equal(cookiesBefore.some((c) => c.name === "__Host-hw_oidc"), false, "no transaction cookie before the hop");
    context.expectClean("the context of the hop");
  }
  check(hops.some((h) => h.startsWith(`${idpOrigin}/authorize`)), "the browser went to the provider");
  check(hops.some((h) => h === `${harness.origin}/auth/oidc/callback`), "the browser came back on the callback");
  const afterFirst = await people();
  equal(afterFirst, before + 1, "one account was made for the identity");

  // ---- the same subject with another address reaches the same account ----------------------------------------------------------
  await harness.idp({ ...allowed, email: "alice.renamed@example.org" });
  {
    const context = await open({ allow: [idpOrigin] });
    const page = await context.context.newPage();
    await page.goto(`${harness.origin}/`);
    await page.waitForSelector(".lg-box");
    await page.getByRole("link", { name: "Continue with Company" }).click();
    await page.waitForSelector("canvas#term", { timeout: 30_000 });
    context.expectClean("the context of the second sign-in");
  }
  equal(await people(), afterFirst, "the same subject with another address made no second account");

  // ---- the callback again, and the callback without the cookie -------------------------------------------------------------------
  {
    const context = await open({ allow: [idpOrigin] });
    const page = await context.context.newPage();
    await page.goto(callbackUrl);
    await page.waitForSelector(".lg-box");
    equal(await page.evaluate(() => location.search), "", "the page took the error mark out of the address after a replayed callback");
    equal(await answered(page), FAILED, "the sentence after a replayed callback");
    equal(await page.locator("canvas#term").count(), 0, "no application after a replayed callback");
    context.expectClean("the context of the replay");
  }

  // A fresh flow whose first two steps are made by hand, so that the callback address is in hand before anything uses it. The
  // owner's context holds the transaction cookie; a stranger's context does not.
  await harness.idp({ ...allowed, email: "alice@example.org" });
  {
    const owner = await open({ allow: [idpOrigin] });
    const started = await owner.context.request.get(`${harness.origin}/auth/oidc/corp/start`, { maxRedirects: 0 });
    equal(started.status(), 302, "the start answers with a redirect");
    // The answer that sends the browser to the provider carries the transaction cookie; its flags are read from the header.
    const lax = (await started.headersArray()).filter((h) => h.name.toLowerCase() === "set-cookie").map((h) => h.value).find((v) => v.startsWith("__Host-hw_oidc="));
    check(lax !== undefined, "the start answer sets the transaction cookie");
    const attributes = lax.split(";").slice(1).map((part) => part.trim().toLowerCase());
    check(attributes.includes("samesite=lax"), `the transaction cookie is SameSite=Lax (${attributes.join(", ")})`);
    check(attributes.includes("httponly"), "the transaction cookie is HttpOnly");
    check(attributes.includes("secure"), "the transaction cookie is Secure");
    check(attributes.includes("path=/"), "the transaction cookie has Path=/");
    check(attributes.includes("max-age=600"), `the transaction cookie lives 10 minutes (${attributes.join(", ")})`);
    check(!attributes.some((a) => a.startsWith("domain=")), "the transaction cookie has no Domain");
    const approved = await owner.context.request.get(started.headers().location, { maxRedirects: 0 });
    equal(approved.status(), 302, "the provider answers with a redirect");
    const caught = approved.headers().location;
    check(caught.startsWith(`${harness.origin}/auth/oidc/callback?`), "the provider returns to the callback");

    const stranger = await open({ allow: [idpOrigin] });
    const strangerPage = await stranger.context.newPage();
    await strangerPage.goto(caught);
    await strangerPage.waitForSelector(".lg-box");
    equal(await strangerPage.evaluate(() => location.search), "", "the page took the error mark out of the address after a callback without the cookie");
    equal(await answered(strangerPage), FAILED, "the sentence after a callback without the cookie");
    equal(await strangerPage.locator("canvas#term").count(), 0, "no application without the cookie");
    stranger.expectClean("the context without the cookie");

    // The stranger's try spent nothing that belongs to the owner: the owner's browser still gets in with the same address.
    const ownerPage = await owner.context.newPage();
    await ownerPage.goto(caught);
    await ownerPage.waitForSelector("canvas#term", { timeout: 30_000 });
    owner.expectClean("the context of the owner");
  }

  // ---- identities the provider entry does not allow ----------------------------------------------------------------------------
  const refusedIdentities = [
    ["another domain and no group", { sub: "sub-eve", email: "eve@evil.test", name: "Eve", groups: ["other"] }],
    ["another domain, an e-mail that is not verified", { sub: "sub-mal", email: "mal@evil.test", name: "Mal", groups: [], email_verified: false }],
  ];
  const countBeforeRefusals = await people();
  for (const [what, identity] of refusedIdentities) {
    await harness.idp(identity);
    const context = await open({ allow: [idpOrigin] });
    const page = await context.context.newPage();
    await page.goto(`${harness.origin}/`);
    await page.waitForSelector(".lg-box");
    await page.getByRole("link", { name: "Continue with Company" }).click();
    await page.getByRole("alert").waitFor();
    equal(await answered(page), FAILED, `the sentence for ${what}`);
    equal(await page.locator("canvas#term").count(), 0, `no application for ${what}`);
    context.expectClean(`the context of ${what}`);
  }
  // The provider says no.
  {
    await harness.idp({ ...allowed, deny: true });
    const context = await open({ allow: [idpOrigin] });
    const page = await context.context.newPage();
    await page.goto(`${harness.origin}/`);
    await page.waitForSelector(".lg-box");
    await page.getByRole("link", { name: "Continue with Company" }).click();
    await page.getByRole("alert").waitFor();
    equal(await answered(page), FAILED, "the sentence for a provider error");
    context.expectClean("the context of the provider's error");
  }
  equal(await people(), countBeforeRefusals, "the refused identities made no account");
  adminContext.expectClean("the administrator's context");

  console.log(
    `oidc: hops: ${[...new Set(hops)].join(" -> ")}; cookies before: no transaction cookie; start answer: transaction cookie __Host-, Lax, HttpOnly, Secure, Path=/, 600 s; ` +
      `after: session cookie Strict, HttpOnly, Secure, transaction cookie gone; replay and missing cookie: the failed sentence and the error mark taken out of the address; ` +
      `refused: ${refusedIdentities.map(([w]) => w).join("; ")}, provider error; people: ${before} -> ${afterFirst} -> ${countBeforeRefusals} (unchanged by the refusals)`,
  );
});
