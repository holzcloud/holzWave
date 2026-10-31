// The people dialog in a real browser: the list says name, role and state; the menu changes the role and the state; a sign-in
// lock-out shows and is lifted; Delete asks first and names the person; the last active administrator cannot be demoted,
// disabled or deleted; every change goes through the step-up layer when the sign-in is old; and nothing in the page or in the
// routes reaches another person's secrets.
import { runTest, signIn, openPeople, personRow, personFacts, openMenu, confirmItsYou, provokeLockout, check, equal, poll } from "./lib.mjs";

await runTest("people", {}, async ({ harness, open }) => {
  const admin = await harness.account({ role: "admin", name: "pa-admin" });
  const lena = await harness.account({ role: "member", name: "pa-lena" });
  const omar = await harness.account({ role: "member", name: "pa-omar" });
  const nina = await harness.account({ role: "member", name: "pa-nina" });
  const gone = await harness.account({ role: "member", name: "pa-gone" });

  const adminContext = await open();
  const page = await adminContext.context.newPage();
  await signIn(page, harness, admin);
  await harness.backdate(admin.username);
  const layer = page.getByRole("dialog", { name: "Confirm it is you" });
  const dialog = await openPeople(page);

  // ---- the list ----------------------------------------------------------------------------------------------------------------
  const listed = {};
  for (const person of [admin, lena, omar, nina, gone]) listed[person.username] = await personFacts(page, person.username);
  equal(listed[admin.username].role, "admin", "the administrator's role");
  for (const person of [lena, omar, nina, gone]) equal(listed[person.username].role, "member", `the role of ${person.username}`);
  for (const person of [admin, lena, omar, nina, gone]) equal(listed[person.username].state, "active", `the state of ${person.username}`);
  equal(await page.locator(".people tbody tr").count(), 5, "the list has the five people");

  // ---- a role change: the first action after an old sign-in asks who you are --------------------------------------------------
  const entriesOfMember = (await openMenu(page, lena.username)).out;
  equal(entriesOfMember.map((e) => e.label).join(" | "), "Make admin | Disable | Reset secrets | Delete", "the menu of an active member");
  check(entriesOfMember.every((e) => !e.held), "no entry of an active member is held back");
  await page.getByRole("menuitem", { name: "Make admin" }).click();
  await confirmItsYou(page, admin.password);
  await poll(async () => ((await personFacts(page, lena.username)).role === "admin" ? true : undefined), "the list shows lena as an administrator");
  const stepUps = ["the first action asked for the password"];
  // Within the step-up time the next action does not ask.
  const second = await openMenu(page, lena.username);
  equal(second.out.map((e) => e.label).join(" | "), "Make member | Disable | Reset secrets | Delete", "the menu of an administrator");
  await page.getByRole("menuitem", { name: "Make member" }).click();
  await poll(async () => ((await personFacts(page, lena.username)).role === "member" ? true : undefined), "the list shows lena as a member again");
  equal(await layer.count(), 0, "the second action within the step-up time did not ask again");
  stepUps.push("the second action did not");

  // ---- disable and enable ------------------------------------------------------------------------------------------------------
  await openMenu(page, omar.username);
  await page.getByRole("menuitem", { name: "Disable" }).click();
  await poll(async () => ((await personFacts(page, omar.username)).state === "disabled" ? true : undefined), "the list shows omar as disabled");
  const disabledMenu = (await openMenu(page, omar.username)).out.map((e) => e.label);
  check(disabledMenu.includes("Enable") && !disabledMenu.includes("Disable"), `the menu of a disabled person offers Enable (${disabledMenu.join(", ")})`);
  await page.getByRole("menuitem", { name: "Enable" }).click();
  await poll(async () => ((await personFacts(page, omar.username)).state === "active" ? true : undefined), "the list shows omar as active again");

  // ---- a lock-out of the password path shows and is lifted -----------------------------------------------------------------
  const attempts = await provokeLockout(open, harness, nina.username);
  await dialog.getByRole("button", { name: "Close" }).click();
  await openPeople(page);
  await poll(async () => ((await personFacts(page, nina.username)).state === "locked out" ? true : undefined), "the list shows nina as locked out");
  const lockedMenu = (await openMenu(page, nina.username)).out.map((e) => e.label);
  check(lockedMenu.includes("Unlock sign-in"), `the menu of a locked out person offers Unlock sign-in (${lockedMenu.join(", ")})`);
  await page.getByRole("menuitem", { name: "Unlock sign-in" }).click();
  await poll(async () => ((await personFacts(page, nina.username)).state === "active" ? true : undefined), "the list shows nina as active after the lock was lifted");
  {
    const context = await open();
    const ninaPage = await context.context.newPage();
    await signIn(ninaPage, harness, nina);
    context.expectClean("the context of the person whose sign-in was unlocked");
  }

  // ---- Delete asks first and names the person ---------------------------------------------------------------------------------
  await openMenu(page, gone.username);
  await page.getByRole("menuitem", { name: "Delete" }).click();
  const ask = page.getByRole("dialog", { name: `Delete ${gone.username}?` });
  await ask.waitFor();
  const askText = (await ask.innerText()).replace(/\s+/g, " ");
  check(askText.includes("They can no longer sign in"), "the question says what Delete does");
  await ask.getByRole("button", { name: "Cancel" }).click();
  await ask.waitFor({ state: "detached" });
  equal(await personRow(page, gone.username).count(), 1, "Cancel leaves the person in the list");
  await openMenu(page, gone.username);
  await page.getByRole("menuitem", { name: "Delete" }).click();
  await ask.waitFor();
  await ask.getByRole("button", { name: "Delete", exact: true }).click();
  await poll(async () => ((await personRow(page, gone.username).count()) === 0 ? true : undefined), "the person is gone from the list");
  equal(await page.locator(".people tbody tr").count(), 4, "four people are left");

  // ---- the last active administrator ---------------------------------------------------------------------------------------
  const own = await openMenu(page, admin.username);
  const held = own.out.filter((e) => e.held).map((e) => e.label);
  equal(held.join(" | "), "Make member | Disable | Delete", "the entries held back for the only active administrator");
  const reason = (await own.menu.locator(".ppl-why").innerText()).trim();
  equal(reason, "The only active administrator.", "the reason shown for the held entries");
  await page.getByRole("menuitem", { name: "Make member" }).click({ force: true });
  equal((await personFacts(page, admin.username)).role, "admin", "a held entry changed nothing");
  equal(await page.getByRole("dialog", { name: /^Delete/ }).count(), 0, "no question for a held entry");
  await personRow(page, admin.username).locator("button.ppl-more").click();
  await page.getByRole("menu").waitFor({ state: "detached" });

  // ---- no way to another person's secrets ------------------------------------------------------------------------------------
  const allLabels = new Set();
  for (const person of [admin, lena, omar, nina]) {
    for (const entry of (await openMenu(page, person.username)).out) allLabels.add(entry.label);
    // The same button closes the menu again.
    await personRow(page, person.username).locator("button.ppl-more").click();
    await page.getByRole("menu").waitFor({ state: "detached" });
  }
  const labels = [...allLabels];
  check(!labels.some((l) => /scrollback|files|history|open|view|read|secret(?!s)/i.test(l.replace(/^Reset secrets$/, ""))), `no menu entry reaches another person's data (${labels.join(", ")})`);
  const api = adminContext.context.request;
  const list = await (await api.get(`${harness.origin}/api/admin/accounts`)).json();
  const people = Array.isArray(list) ? list : (list.accounts ?? list.people ?? []);
  check(people.length >= 4, "the account list was read");
  const statuses = new Set();
  for (const person of people.slice(0, 4)) {
    for (const tail of ["secrets", "scrollback", "files"]) {
      const answer = await api.get(`${harness.origin}/api/admin/people/${person.id}/${tail}`);
      statuses.add(answer.status());
      equal(answer.status(), 404, `GET /api/admin/people/<id>/${tail}`);
    }
  }
  adminContext.expectClean("the administrator's context");
  console.log(
    `people: list: ${Object.entries(listed).map(([n, f]) => `${n.replace(/^pa-/, "")}=${f.role}/${f.state}`).join(", ")}; ` +
      `menus: [${entriesOfMember.map((e) => e.label).join(", ")}] / [${second.out.map((e) => e.label).join(", ")}] / disabled [${disabledMenu.join(", ")}] / locked out [${lockedMenu.join(", ")}]; ` +
      `held for the only administrator: ${held.join(", ")} (${reason}); step-up: ${stepUps.join(", ")}; lock-out after ${attempts} wrong passwords, lifted; ` +
      `Delete asked "Delete <name>?" and Cancel kept the person; secrets routes answered ${[...statuses].join(",")}`,
  );
});
