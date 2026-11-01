// The account dialog against the real server. The server sends the account id and the id of each passkey as JSON numbers, so the
// page must read them as numbers: a reader that wanted strings made the dialog answer "not an account" to every real
// `GET /api/me` while the unit fixtures (with string ids) stayed green. This test opens the dialog the way a person does, from the
// palette, and requires that it shows the account: the groups, the state words and this session. The passkey rows (a numeric
// passkey id, also the path of the removal) are covered in passkey.test.mjs, which has the virtual authenticator.
import { runTest, signIn, runEntry, check, equal, poll } from "./lib.mjs";

await runTest("account", {}, async ({ harness, open }) => {
  const person = await harness.account({ role: "member", name: "ac-member" });
  const context = await open();
  const page = await context.context.newPage();
  await signIn(page, harness, person);

  // What the server really sends, read through the page's own bridge: the ids are numbers.
  const raw = await page.evaluate(async () => (await import(new URL("bridge.js", location.href).href)).http("GET", "/api/me"));
  equal(typeof raw.account.id, "number", "the server sends the account id as a number");

  await page.locator("canvas#term").click();
  await runEntry(page, "Account…");
  const dialog = page.getByRole("dialog", { name: "Account", exact: true });
  await dialog.waitFor();
  await poll(async () => ((await dialog.locator(".acc-group").count()) > 0 ? true : undefined), "the dialog shows the groups of the account");

  const legends = (await dialog.locator(".acc-group legend").allTextContents()).map((t) => t.trim());
  for (const legend of ["Password", "Authenticator", "Passkeys", "Sessions"]) check(legends.includes(legend), `the group ${legend} is shown (${legends.join(", ")})`);
  const group = (id) => dialog.locator(`[data-group="${id}"]`);
  equal((await group("password").locator(".acc-state").innerText()).trim(), "set", "the password is set");
  equal((await group("totp").locator(".acc-state").innerText()).trim(), "set", "the authenticator is set");
  equal((await group("sessions").locator(".acc-this").innerText()).trim(), "this session", "this session is marked");
  equal((await dialog.locator(".set-status").innerText()).trim(), "", "the dialog says nothing about an account it could not read");

  await dialog.getByRole("button", { name: "Close" }).click();
  await dialog.waitFor({ state: "detached" });
  context.expectClean("the context of the account dialog");
  console.log(`account: GET /api/me sends account.id as ${typeof raw.account.id}; the dialog showed ${legends.join(", ")} and marked this session`);
});
