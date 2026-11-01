// Passkeys in Chromium with a virtual authenticator (the DevTools WebAuthn domain): an invited person enrols with a passkey
// that has the PRF extension, signs out and in again and is unlocked; a second person with an authenticator that has no PRF
// is signed in and locked and is offered the passphrase; a credential the server never saw is refused with the one sentence.
// WebKit has no virtual authenticator, so this file skips itself there (the file ends with exit code 77 and the runner counts it
// as SKIP, printing the reason); a skip in Chromium is a failure.
import { generateKeyPairSync } from "node:crypto";
import { runTest, skip, openPage, signIn, check, equal, poll, answered, runEntry, inviteLink } from "./lib.mjs";
import { LAST_WAY } from "../../dist/accountstate.js";

if (process.env.HW_ENGINE !== "chromium") skip("no virtual authenticator in this engine");

const FAILED = "Sign-in failed. Check your details or ask an admin.";

/** A virtual authenticator on the page: user verification on, resident keys, and PRF when asked. */
async function authenticator(context, page, { prf }) {
  const cdp = await context.newCDPSession(page);
  await cdp.send("WebAuthn.enable");
  const options = {
    protocol: "ctap2",
    ctap2Version: "ctap2_1",
    transport: "internal",
    hasResidentKey: true,
    hasUserVerification: true,
    isUserVerified: true,
    automaticPresenceSimulation: true,
    hasPrf: prf,
  };
  const { authenticatorId } = await cdp.send("WebAuthn.addVirtualAuthenticator", { options });
  return { cdp, authenticatorId, options };
}

const secretsState = (page) => page.evaluate(async () => (await (await import(new URL("bridge.js", location.href).href)).http("GET", "/api/me")).secrets.state);

await runTest("passkey", {}, async ({ harness, open }) => {
  const admin = await harness.account({ role: "admin" });
  const adminContext = await open();
  const adminPage = await adminContext.context.newPage();
  await signIn(adminPage, harness, admin);
  const first = await inviteLink(adminPage, admin);
  const second = await inviteLink(adminPage, admin);

  const enrolWithPasskey = async (context, page, link, username) => {
    await page.goto(link);
    await page.waitForSelector(".lg-box");
    equal(await page.getByLabel("Passkey", { exact: true }).isChecked(), true, "the passkey is the first choice");
    await page.getByLabel("Username").fill(username);
    await page.getByLabel("Display name").fill(username);
    await page.getByRole("button", { name: "Continue" }).click();
    await page.waitForSelector(".lg-code");
    await page.getByLabel("I have saved this code").check();
    await page.getByRole("button", { name: "Continue", exact: true }).click();
    await page.waitForSelector("canvas#term");
  };
  const signOut = async (page) => {
    await page.locator("canvas#term").click();
    await runEntry(page, "Sign out");
    await page.waitForSelector(".lg-box");
  };
  const passkeySignIn = async (page) => {
    await page.getByRole("button", { name: "Sign in with a passkey" }).click();
  };

  // ---- with PRF: signed in and unlocked ---------------------------------------------------------------------------------------
  const withPrf = { username: "pk-with-prf" };
  {
    const context = await open();
    const page = await context.context.newPage();
    const key = await authenticator(context.context, page, { prf: true });
    await enrolWithPasskey(context, page, first.link, withPrf.username);
    await poll(async () => ((await secretsState(page)) === "unlocked" ? true : undefined), "the store is unlocked after the passkey enrolment");
    await signOut(page);
    await passkeySignIn(page);
    await page.waitForSelector("canvas#term");
    await poll(async () => ((await secretsState(page)) === "unlocked" ? true : undefined), "the store is unlocked after the passkey sign-in with PRF");
    withPrf.state = await secretsState(page);
    // The account dialog lists the passkey: the server sends its id as a number, which is also the path of its removal.
    await page.locator("canvas#term").click();
    await runEntry(page, "Account…");
    const account = page.getByRole("dialog", { name: "Account", exact: true });
    await account.waitFor();
    const keys = account.locator('[data-group="passkeys"] .acc-item');
    await poll(async () => ((await keys.count()) === 1 ? true : undefined), "the account dialog lists the one passkey");
    withPrf.listed = (await keys.locator(".acc-name").innerText()).trim();
    // It is the only way in: removing it is refused with the sentence for that, which means the numeric id reached the route.
    await keys.getByRole("button", { name: "Remove" }).click();
    await keys.getByRole("button", { name: "Click again to remove" }).click();
    const status = account.locator(".set-status");
    await poll(async () => ((await status.innerText()).trim() !== "" ? true : undefined), "the account dialog answers the removal");
    equal((await status.innerText()).trim(), LAST_WAY, "the only passkey cannot be removed");
    equal(await keys.count(), 1, "the passkey is still listed");
    await account.getByRole("button", { name: "Close" }).click();
    await account.waitFor({ state: "detached" });
    context.expectClean("the context with PRF");
    await key.cdp.send("WebAuthn.removeVirtualAuthenticator", { authenticatorId: key.authenticatorId });
  }

  // ---- without PRF: signed in, locked, and the dialog offers the passphrase --------------------------------------------------
  const withoutPrf = { username: "pk-without-prf" };
  {
    const context = await open();
    const page = await context.context.newPage();
    const key = await authenticator(context.context, page, { prf: false });
    await enrolWithPasskey(context, page, second.link, withoutPrf.username);
    await signOut(page);
    await passkeySignIn(page);
    await page.waitForSelector("canvas#term");
    withoutPrf.state = await poll(async () => ((await secretsState(page)) === "locked" ? "locked" : undefined), "the store is locked after a sign-in without PRF");
    // The status line says so and the dialog offers the passphrase.
    const mark = page.locator(".web-mark", { hasText: "secrets locked" });
    await mark.waitFor();
    await mark.getByRole("button", { name: "Unlock" }).click();
    await page.getByRole("dialog", { name: "Your saved passwords are locked" }).waitFor();
    await page.getByLabel("Passphrase").waitFor();
    context.expectClean("the context without PRF");
    await key.cdp.send("WebAuthn.removeVirtualAuthenticator", { authenticatorId: key.authenticatorId });
  }

  // ---- a credential the server never saw -----------------------------------------------------------------------------------
  {
    const context = await open();
    const page = await context.context.newPage();
    const key = await authenticator(context.context, page, { prf: false });
    const { privateKey } = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
    const pkcs8 = privateKey.export({ type: "pkcs8", format: "der" }).toString("base64");
    await key.cdp.send("WebAuthn.addCredential", {
      authenticatorId: key.authenticatorId,
      credential: {
        credentialId: Buffer.from("a credential that no account has").toString("base64"),
        isResidentCredential: true,
        rpId: harness.hostname,
        privateKey: pkcs8,
        userHandle: Buffer.from("0123456789abcdef").toString("base64"),
        signCount: 0,
      },
    });
    await openPage(context, harness, "/").then(async (p) => {
      await p.waitForSelector(".lg-box");
      await p.getByRole("button", { name: "Sign in with a passkey" }).click();
      const sentence = await answered(p, "the unknown credential has been answered");
      equal(sentence, FAILED, "the sentence for a credential the server never saw");
      equal(await p.locator("canvas#term").count(), 0, "no application for an unknown credential");
    });
    await page.close();
    context.expectClean("the context of the unknown credential");
  }

  adminContext.expectClean("the administrator's context");
  console.log(
    `passkey: with PRF (ctap2.1, internal, resident key, user verification, hasPrf true): enrolled, signed out and in, store ${withPrf.state}, the account dialog listed "${withPrf.listed}" and refused to remove the last way in; ` +
      `without PRF (hasPrf false): signed in, store ${withoutPrf.state}, the unlock dialog offers the passphrase; unknown credential: "${FAILED.slice(0, 20)}..."`,
  );
});
