// Enrolment in a real browser: the setup token, an invitation made in the people dialog (after the step-up layer), the link
// opened in a second context, the person enrolled with a password and an authenticator, signed in again in a third context,
// the link spent, and a member with no `People…` entry.
import { secretOfLink, Authenticator } from "./totp.mjs";
import { runTest, openPage, signIn, poll, check, equal, runEntry, paletteTitles, addressesHold, inviteLink, openPeople } from "./lib.mjs";

const SETUP_GONE = "This token has expired or was used. Ask the person who runs this server for a new one.";
const INVITE_GONE = "This link has expired or was used. Ask an admin for a new one.";

await runTest("enrol", {}, async ({ harness, open }) => {
  // ---- a setup token that was never issued -----------------------------------------------------------------------------
  {
    const context = await open();
    const page = await openPage(context, harness, "/");
    await page.waitForSelector(".lg-box");
    equal(await page.locator("h2").first().textContent(), "Set up this server", "the first screen with no administrator");
    await page.getByLabel("Setup token").fill(`hwb_${"x".repeat(43)}`);
    await page.getByLabel("Username").fill("nobody");
    await page.getByLabel("Display name").fill("Nobody");
    await page.getByLabel("Password and authenticator app").check();
    await page.getByLabel("Password", { exact: true }).fill("pw-aaaaaaaaaaaa-bbbbbbbbbbbb");
    await page.getByRole("button", { name: "Continue" }).click();
    await poll(async () => ((await page.locator("body").innerText()).includes(SETUP_GONE) ? true : undefined), "the setup box says the token is gone");
    equal(await page.locator("form").count(), 0, "no form after a refused token");
    context.expectClean("the context of the refused token");
  }

  // ---- an administrator invites through the people dialog -----------------------------------------------------------------
  const admin = await harness.account({ role: "admin" });
  const adminContext = await open();
  const adminPage = await adminContext.context.newPage();
  await signIn(adminPage, harness, admin);
  await harness.backdate(admin.username);
  // The sign-in is an hour old: the step-up layer comes before the link.
  const { link, note, stepUp } = await inviteLink(adminPage, admin);
  check(note.includes("It works for 7 days, as a member."), "the link layer says how long and as whom");
  check(stepUp, "the step-up layer did not come before the invitation");
  check(link.startsWith(`${harness.origin}/#invite=hwi_`), "the link has the invitation in its fragment");

  // ---- the link, in a second context ------------------------------------------------------------------------------------------
  const guest = { username: `guest-${Math.random().toString(16).slice(2, 8)}`, password: `pw-${Math.random().toString(16).slice(2, 14)}-${Math.random().toString(16).slice(2, 14)}` };
  let authenticator;
  {
    const context = await open();
    const page = await context.context.newPage();
    await page.goto(link);
    await page.waitForSelector(".lg-box");
    // The fragment is gone from the address at once, and no request carried the token in its address.
    equal(await page.evaluate(() => location.hash), "", "the fragment after the page was drawn");
    check(!page.url().includes("invite="), "the address still holds the invitation");
    equal(await page.locator("h2").first().textContent(), "You were invited", "the invitation screen");
    const roleWord = (await page.locator(".lg-box").innerText()).toLowerCase();
    check(roleWord.includes("member"), "the invitation names the role");
    await page.getByLabel("Username").fill(guest.username);
    await page.getByLabel("Display name").fill("Guest Person");
    await page.getByLabel("Password and authenticator app").check();
    await page.getByLabel("Password", { exact: true }).fill(guest.password);
    await page.getByRole("button", { name: "Continue" }).click();
    await page.waitForSelector(".lg-secret");
    const secret = secretOfLink((await page.locator(".lg-tx").textContent()).trim());
    authenticator = new Authenticator(secret);
    await page.getByLabel("Code").fill(await authenticator.next());
    await page.getByRole("button", { name: "Continue" }).click();
    await page.waitForSelector(".lg-code");
    await page.getByLabel("I have saved this code").check();
    await page.getByRole("button", { name: "Continue", exact: true }).click();
    await page.waitForSelector("canvas#term");
    // A member has no People entry.
    await page.locator("canvas#term").click();
    const titles = await paletteTitles(page, "Peo");
    check(!titles.some((t) => t.startsWith("People")), `a member's palette lists People… (${titles.join(" | ")})`);
    check(!addressesHold(context, [link.split("#")[1], guest.password]), "an address a request used holds the invitation or the password");
    context.expectClean("the context of the invited person");
  }

  // ---- signing in afterwards, in a third context, with a new code -----------------------------------------------------------
  {
    const context = await open();
    const page = await context.context.newPage();
    await signIn(page, harness, { username: guest.username, password: guest.password, authenticator });
    check((await page.locator("canvas#term").count()) === 1, "the invited person is signed in");
    context.expectClean("the context of the second sign-in");
  }

  // ---- the link is spent -------------------------------------------------------------------------------------------------------
  {
    const context = await open();
    const page = await context.context.newPage();
    await page.goto(link);
    await page.waitForSelector(".lg-box");
    await poll(async () => ((await page.locator("body").innerText()).includes(INVITE_GONE) ? true : undefined), "the spent link says it has expired");
    equal(await page.locator("form").count(), 0, "no form for a spent link");
    context.expectClean("the context of the spent link");
  }

  // ---- the administrator's list has the new person as a member -------------------------------------------------------------
  await openPeople(adminPage);
  const row = adminPage.locator(".people tbody tr", { hasText: guest.username });
  await row.waitFor();
  const cells = (await row.innerText()).replace(/\s+/g, " ");
  check(cells.includes("member"), "the people list shows the new person as a member");
  adminContext.expectClean("the administrator's context");

  console.log(
    `enrol: wrong setup token: "${SETUP_GONE.slice(0, 24)}..."; step-up layer shown before the link; link layer: 7 days, as a member;  ` +
      `address after the fragment was read: no fragment; spent link: "${INVITE_GONE.slice(0, 24)}..."; role in the list: member`,
  );
});
