// Password and authenticator code in a real browser against the real server: a person signs in; an unknown name, a wrong
// password and a wrong code all end on the one sentence; repeated failures reach the wait sentence and then the right
// password is refused too; the password field is empty after each submit and no address holds a secret.
import { codeForStep, stepOf } from "./totp.mjs";
import { runTest, openPage, signIn, check, equal, answered, addressesHold } from "./lib.mjs";

const FAILED = "Sign-in failed. Check your details or ask an admin.";
const WAIT = "Too many attempts. Try again in";
// The sign-in failures the server counts for one name before it locks the password path (five), plus two attempts of slack.
const ATTEMPT_LIMIT = 5 + 2;

await runTest("signin", {}, async ({ harness, open }) => {
  const alice = await harness.account({ role: "member" });
  const bob = await harness.account({ role: "member" });
  const secrets = [alice.password, bob.password];

  // ---- a person signs in with the password and the current code -------------------------------------------------------
  {
    const context = await open();
    const page = await context.context.newPage();
    await signIn(page, harness, alice);
    equal(new URL(page.url()).pathname, "/", "the address after the sign-in");
    check(!page.url().includes(alice.password), "the address holds the password");
    check((await page.locator("canvas#term").count()) === 1, "the application is shown");
    equal((await context.context.cookies(harness.origin)).some((c) => c.name === "__Host-hw_session"), true, "a session cookie is set");
    check(!addressesHold(context, secrets), "an address a request used holds a password");
    context.expectClean("the signed-in context");
  }

  // ---- the three refusals share one sentence ------------------------------------------------------------------------------
  const refusals = [];
  {
    const context = await open();
    const page = await openPage(context, harness, "/");
    await page.waitForSelector(".lg-box");
    const attempt = async (name, password) => {
      await page.getByLabel("Username").fill(name);
      await page.getByLabel("Password", { exact: true }).fill(password);
      await page.getByRole("button", { name: "Continue" }).click();
      return answered(page);
    };

    // An unknown name.
    refusals.push(["unknown name", await attempt("nobody-by-that-name", "a password that is not known")]);
    equal(await page.getByLabel("Password", { exact: true }).inputValue(), "", "the password field after an unknown name");

    // The right name, a wrong password.
    refusals.push(["wrong password", await attempt(alice.username, "this is not the password")]);
    equal(await page.getByLabel("Password", { exact: true }).inputValue(), "", "the password field after a wrong password");

    // The right password, a code that cannot be right: a step far outside the window.
    await page.getByLabel("Username").fill(alice.username);
    await page.getByLabel("Password", { exact: true }).fill(alice.password);
    await page.getByRole("button", { name: "Continue" }).click();
    await page.getByLabel("Code").waitFor();
    const far = codeForStep(alice.secret, stepOf(Date.now() / 1000) + 40);
    await page.getByLabel("Code").fill(far);
    await page.getByRole("button", { name: "Sign in" }).click();
    refusals.push(["wrong code", await answered(page, "the wrong code has been answered")]);

    for (const [what, sentence] of refusals) equal(sentence, FAILED, `the sentence for ${what}`);
    check(!page.url().includes(alice.password) && !page.url().includes(far), "the address holds a secret");
    check(!addressesHold(context, [...secrets, far]), "an address a request used holds a secret");
    equal(await page.locator("canvas#term").count(), 0, "no application after three refusals");
    context.expectClean("the context of the refusals");
  }

  // ---- the wait sentence, and then the right password is refused too ---------------------------------------------------------
  let attempts = 0;
  let waitSentence = "";
  {
    const context = await open();
    const page = await openPage(context, harness, "/");
    await page.waitForSelector(".lg-box");
    while (attempts < ATTEMPT_LIMIT && !waitSentence.startsWith(WAIT)) {
      attempts++;
      await page.getByLabel("Username").fill(bob.username);
      await page.getByLabel("Password", { exact: true }).fill(`${bob.password}-not`);
      await page.getByRole("button", { name: "Continue" }).click();
      const sentence = await answered(page);
      if (sentence.startsWith(WAIT)) waitSentence = sentence;
      else equal(sentence, FAILED, `the sentence of failed attempt ${attempts}`);
    }
    check(waitSentence.startsWith(WAIT), `the wait sentence was not reached within ${ATTEMPT_LIMIT} attempts`);

    // The right password now: no code step, the wait sentence again.
    await page.getByLabel("Username").fill(bob.username);
    await page.getByLabel("Password", { exact: true }).fill(bob.password);
    await page.getByRole("button", { name: "Continue" }).click();
    const refused = await answered(page, "the right password has been answered during the lock");
    check(refused.startsWith(WAIT), `the right password during the lock gave another sentence`);
    equal(await page.getByLabel("Code").count(), 0, "no code step while the lock holds");
    equal(await page.locator("canvas#term").count(), 0, "no application while the lock holds");
    context.expectClean("the context of the lock");
  }

  console.log(`signin: sentences: ${[...new Set(refusals.map(([, s]) => s))].join(" | ")}; wait sentence: ${waitSentence.replace(/\d+/, "N")}; attempts until the wait sentence: ${attempts}; the right password during the lock: refused with the wait sentence`);
});
