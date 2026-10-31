// The unlock dialog in a real browser: a person whose store is keyed by a passphrase (the password does not open it) is signed
// in and locked, sees the mark and the Unlock button, is refused a wrong passphrase with one sentence, and opens the store with
// the right one; a second person opens theirs with the recovery code; a third, whose password keys the store, is unlocked at once.
// The field is empty after every submit.
import { runTest, signIn, check, equal, poll } from "./lib.mjs";

const REFUSED = "That did not unlock your secrets.";
const TITLE = "Your saved passwords are locked";

await runTest("unlock", {}, async ({ harness, open }) => {
  const pass = await harness.account({ role: "admin", name: "pu-pass", passphrase: true, locked: true });
  const rescue = await harness.account({ role: "member", name: "pu-rescue", locked: true });
  const plain = await harness.account({ role: "member", name: "pu-plain" });
  check(typeof pass.passphrase === "string" && pass.passphrase.length > 0, "the harness gave the passphrase of the first person");

  const stateOf = async (context) => (await (await context.context.request.get(`${harness.origin}/api/me`)).json()).secrets.state;
  const mark = (page) => page.locator('.web-mark[data-mark="locked"]');
  const dialogOf = (page) => page.getByRole("dialog", { name: TITLE });
  const opened = async (page) => {
    await mark(page).waitFor();
    equal((await mark(page).locator(".web-word").textContent()).trim(), "secrets locked", "the word of the mark");
    await mark(page).getByRole("button", { name: "Unlock" }).click();
    await dialogOf(page).waitFor();
    return dialogOf(page);
  };
  const submit = async (dialog, label, text) => {
    await dialog.getByLabel(label, { exact: true }).fill(text);
    await dialog.getByRole("button", { name: "Unlock", exact: true }).click();
  };

  // ---- a passphrase ---------------------------------------------------------------------------------------------------------------
  const seen = {};
  {
    const context = await open();
    const page = await context.context.newPage();
    await signIn(page, harness, pass);
    equal(await stateOf(context), "locked", "the store after a sign-in with a password that does not key it");
    const dialog = await opened(page);
    equal(await dialog.getByLabel("Passphrase", { exact: true }).count(), 1, "the dialog asks for the passphrase first");

    await submit(dialog, "Passphrase", `${pass.passphrase}-not`);
    await poll(async () => ((await dialog.locator("#web-unlock-error").innerText()).trim() !== "" ? true : undefined), "the sentence after a wrong passphrase");
    seen.wrong = (await dialog.locator("#web-unlock-error").innerText()).trim();
    equal(seen.wrong, REFUSED, "the sentence for a wrong passphrase");
    equal(await dialog.getByLabel("Passphrase", { exact: true }).inputValue(), "", "the field after a wrong passphrase");
    equal(await stateOf(context), "locked", "the store after a wrong passphrase");
    check(await mark(page).count() === 1, "the mark stays after a wrong passphrase");

    await submit(dialog, "Passphrase", pass.passphrase);
    await dialog.waitFor({ state: "detached" });
    equal(await dialog.count(), 0, "the dialog is gone after the right passphrase");
    await poll(async () => ((await mark(page).count()) === 0 ? true : undefined), "the mark is gone");
    equal(await stateOf(context), "unlocked", "the store after the right passphrase");
    seen.right = "dialog closed, mark gone, store unlocked";
    context.expectClean("the context of the passphrase");
  }

  // ---- the recovery code --------------------------------------------------------------------------------------------------------------
  {
    const context = await open();
    const page = await context.context.newPage();
    await signIn(page, harness, rescue);
    equal(await stateOf(context), "locked", "the store of the second person after the sign-in");
    const dialog = await opened(page);
    await dialog.getByRole("button", { name: "Use recovery code" }).click();
    equal(await dialog.getByLabel("Recovery code", { exact: true }).count(), 1, "the dialog asks for the recovery code");
    equal(await dialog.getByRole("button", { name: "Use passphrase" }).count(), 1, "the other way is offered back");
    await submit(dialog, "Recovery code", "AAAA-AAAA-AAAA-AAAA-AAAA-AAAA");
    await poll(async () => ((await dialog.locator("#web-unlock-error").innerText()).trim() !== "" ? true : undefined), "the sentence after a wrong recovery code");
    seen.wrongRecovery = (await dialog.locator("#web-unlock-error").innerText()).trim();
    equal(seen.wrongRecovery, REFUSED, "the sentence for a wrong recovery code");
    equal(await dialog.getByLabel("Recovery code", { exact: true }).inputValue(), "", "the field after a wrong recovery code");
    await submit(dialog, "Recovery code", rescue.recovery);
    await dialog.waitFor({ state: "detached" });
    await poll(async () => ((await mark(page).count()) === 0 ? true : undefined), "the mark is gone after the recovery code");
    equal(await stateOf(context), "unlocked", "the store after the recovery code");
    context.expectClean("the context of the recovery code");
  }

  // ---- a password that keys the store ---------------------------------------------------------------------------------------------------
  {
    const context = await open();
    const page = await context.context.newPage();
    await signIn(page, harness, plain);
    equal(await stateOf(context), "unlocked", "the store after a sign-in with the password that keys it");
    equal(await mark(page).count(), 0, "no locked mark for the third person");
    equal(await dialogOf(page).count(), 0, "no dialog for the third person");
    context.expectClean("the context of the password-keyed store");
  }

  console.log(
    `unlock: wrong passphrase: "${seen.wrong}"; right passphrase: ${seen.right}; wrong recovery code: "${seen.wrongRecovery}"; recovery code: store unlocked; ` +
      `password-keyed store: unlocked at once, no mark; the field was empty after each submit`,
  );
});
