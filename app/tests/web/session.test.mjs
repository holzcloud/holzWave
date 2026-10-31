// The end of a session in a real browser: an administrator disables a person and that person's open page shows the layer
// within 10 seconds; the layer's button leads to the sign-in page with the same sentence; the disabled person is refused with
// the ordinary sentence; a sign-out in one tab ends the other tab of the same person; and a socket opened after the end is
// closed with 4401.
import { runTest, signIn, openPeople, openMenu, confirmItsYou, runEntry, socketOutcome, lineText, check, equal, poll } from "./lib.mjs";

const ENDED = "Your session ended. Sign in again.";
const FAILED = "Sign-in failed. Check your details or ask an admin.";
const LAYER_LIMIT_S = 10;

await runTest("session", {}, async ({ harness, open }) => {
  const admin = await harness.account({ role: "admin", name: "ps-admin" });
  const dana = await harness.account({ role: "member", name: "ps-dana" });
  const erin = await harness.account({ role: "member", name: "ps-erin" });

  const adminContext = await open();
  const adminPage = await adminContext.context.newPage();
  await signIn(adminPage, harness, admin);
  await harness.backdate(admin.username);

  // ---- an administrator disables a person whose page is open -------------------------------------------------------------------
  const danaContext = await open();
  const danaPage = await danaContext.context.newPage();
  await signIn(danaPage, harness, dana);
  const marker = danaPage.getByText(ENDED, { exact: true });
  equal(await marker.count(), 0, "no sentence of an ended session while the session lives");

  await openPeople(adminPage);
  await openMenu(adminPage, dana.username);
  await adminPage.getByRole("menuitem", { name: "Disable" }).click();
  const started = Date.now();
  await confirmItsYou(adminPage, admin.password);
  await marker.waitFor({ timeout: (LAYER_LIMIT_S + 5) * 1000 });
  const seconds = (Date.now() - started) / 1000;
  check(seconds <= LAYER_LIMIT_S, `the layer came ${seconds.toFixed(1)} s after the disable, the limit is ${LAYER_LIMIT_S} s`);
  const afterEnd = await socketOutcome(danaPage);
  equal(afterEnd.code, 4401, "the close code of a socket opened after the end");
  const asleep = await danaPage.getByRole("button", { name: "Sign in" }).count();
  equal(asleep, 1, "the layer has one Sign in button");

  // ---- the button leads to the sign-in page, which says the same ---------------------------------------------------------------
  await danaPage.getByRole("button", { name: "Sign in" }).click();
  await danaPage.waitForSelector(".lg-box");
  equal(new URL(danaPage.url()).pathname, "/", "the sign-in page is at the root");
  check(!danaPage.url().includes("ended"), "the page took the mark out of the address");
  equal(await danaPage.getByText(ENDED, { exact: true }).count(), 1, "the sign-in page says the session ended");
  equal(await danaPage.locator("canvas#term").count(), 0, "no application behind the sign-in page");

  // ---- the disabled person is refused with the ordinary sentence ---------------------------------------------------------------
  await danaPage.getByLabel("Username").fill(dana.username);
  await danaPage.getByLabel("Password", { exact: true }).fill(dana.password);
  await danaPage.getByRole("button", { name: "Continue" }).click();
  // The password step may go on to the code step or end at once; both ends are the one sentence.
  let steps = "password";
  await poll(async () => {
    if ((await danaPage.getByLabel("Code").count()) > 0) return "code";
    const idle = await danaPage.evaluate(() => document.querySelector(".lg-box")?.getAttribute("aria-busy") === "false");
    return idle && (await lineText(danaPage)) === FAILED ? "failed" : undefined;
  }, "the answer to the disabled person's password").then(async (where) => {
    if (where === "code") {
      steps = "password and code";
      await danaPage.getByLabel("Code").fill(await dana.authenticator.next());
      await danaPage.getByRole("button", { name: "Sign in" }).click();
      await poll(async () => ((await lineText(danaPage)) === FAILED ? true : undefined), "the sentence after the disabled person's code");
    }
  });
  equal(await lineText(danaPage), FAILED, "the sentence for a disabled person");
  equal(await danaPage.locator("canvas#term").count(), 0, "no application for a disabled person");

  // ---- a sign-out in one tab ends the other tab -----------------------------------------------------------------------------------
  const erinContext = await open();
  const first = await erinContext.context.newPage();
  await signIn(first, harness, erin);
  const second = await erinContext.context.newPage();
  await second.goto(`${harness.origin}/`);
  await second.waitForSelector("canvas#term");
  const secondMarker = second.getByText(ENDED, { exact: true });
  equal(await secondMarker.count(), 0, "the second tab works before the sign-out");
  await first.locator("canvas#term").click();
  await runEntry(first, "Sign out");
  const signedOutAt = Date.now();
  await first.waitForSelector(".lg-box");
  await secondMarker.waitFor({ timeout: (LAYER_LIMIT_S + 5) * 1000 });
  const otherTab = (Date.now() - signedOutAt) / 1000;
  check(otherTab <= LAYER_LIMIT_S, `the other tab ended ${otherTab.toFixed(1)} s after the sign-out, the limit is ${LAYER_LIMIT_S} s`);
  equal((await socketOutcome(second)).code, 4401, "the close code of a socket the second tab opens after the sign-out");
  equal((await erinContext.context.cookies(harness.origin)).some((c) => c.name === "__Host-hw_session"), false, "the session cookie is gone after the sign-out");

  adminContext.expectClean("the administrator's context");
  danaContext.expectClean("the disabled person's context");
  erinContext.expectClean("the context of the two tabs");
  console.log(
    `session: disable to layer ${seconds.toFixed(1)} s (limit ${LAYER_LIMIT_S}); socket opened after the end closed with ${afterEnd.code}; the Sign in button led to the sign-in page, which said "${ENDED}"; ` +
      `the disabled person was refused with the ordinary sentence after ${steps}; sign-out in one tab to layer in the other ${otherTab.toFixed(1)} s, socket code ${4401}`,
  );
});
