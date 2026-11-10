// The device features of the page against the real server: a code in the fragment of the address survives the sign-in and opens the
// dialog that shows who asks and from where; approving and denying (with a fresh sign-in) reach the app that is waiting; a code
// that is not valid, a hidden feature and a malformed code say or open nothing more; and the code and the token are kept nowhere.
import { runTest, signIn, nativeClient, setSwitches, pageHolds, addressesHold, confirmItsYou, openPeople, runEntry, check, equal, poll } from "./lib.mjs";

const NOT_VALID = "That code is not valid or has expired.";

await runTest("devices", {}, async ({ harness, open }) => {
  const admin = await harness.account({ role: "admin", name: "dv-admin" });
  const native = nativeClient(harness);

  // ---- the features are off: the server hides them, and the page does not guess ------------------------------------------------
  const adminContext = await open();
  const adminPage = await adminContext.context.newPage();
  await signIn(adminPage, harness, admin);
  equal((await native.start()).status, 404, "a native client asking for a grant while both switches are off");
  {
    const page = await adminContext.context.newPage();
    await page.goto(`${harness.origin}/#device=BCDF-GHJK`);
    const dialog = page.getByRole("dialog", { name: "Authorize device" });
    await dialog.waitFor();
    const said = await poll(async () => ((await dialog.locator(".dev-status").innerText()).trim() || undefined), "the dialog says something");
    equal(said.replace(/^!\s*/, ""), NOT_VALID, "the sentence for a hidden feature is the one for a code that is not valid");
    equal(await dialog.locator(".dev-kv").count(), 0, "no row when the feature is off");
    equal(await dialog.getByRole("button", { name: "Approve" }).count(), 0, "nothing to approve");
    equal(await dialog.getByRole("button", { name: "Close" }).count(), 1, "Close is the way out");
    check(!/remote|sync|feature|switch|device access/i.test(said), "nothing about the feature is said");
    equal(new URL(page.url()).hash, "", "the fragment is not in the address bar");
    await dialog.getByRole("button", { name: "Close" }).click();
    await dialog.waitFor({ state: "detached" });
    await page.close();
  }

  // ---- a code in the fragment survives the sign-in and opens the dialog --------------------------------------------------------
  await setSwitches(adminPage, { remote: true });
  const first = await native.start("Test laptop", "macos");
  equal(first.status, 200, "a native client can start a grant once a switch is on");
  check(/^[BCDFGHJKLMNPQRSTVWXZ]{4}-[BCDFGHJKLMNPQRSTVWXZ]{4}$/.test(first.userCode), "the user code has the form of the record");
  check(first.uri === `${harness.origin}/#device=${first.userCode}`, "the address the app opens is the server's root with the code in the fragment");

  const person = await open();
  const page = await person.context.newPage();
  // The link is opened while signed out: the sign-in page keeps the code through the password and the authenticator code.
  await signIn(page, harness, admin, `/#device=${first.userCode}`);
  const dialog = page.getByRole("dialog", { name: "Authorize device" });
  await dialog.waitFor();
  await poll(async () => ((await dialog.locator(".dev-kv").count()) > 0 ? true : undefined), "the dialog shows who asks");
  const facts = await dialog.locator(".dev-kv").evaluate((dl) => Array.from(dl.children).map((c) => c.textContent));
  equal(facts.join(" | "), `Device | Test laptop | Platform | macOS | Address | ${facts[5]} | Requested | just now`, "the rows of the dialog, in the order of the record");
  check(/^[0-9a-f.:]+$/.test(facts[5]), `the address the request came from is shown (${facts[5]})`);
  equal((await dialog.locator(".dev-code").innerText()).trim(), first.userCode, "the code to compare with the app");
  equal((await dialog.locator(".dev-sentence").innerText()).trim(), "Approve only if you started this on your own device.", "the sentence");
  equal(await dialog.getByRole("button").evaluateAll((b) => b.map((x) => x.textContent).join(" | ")), "Deny | Approve", "the two buttons, Deny first");
  equal(new URL(page.url()).hash, "", "the fragment is not in the address bar after the sign-in");
  equal((await pageHolds(page, first.userCode.replace("-", ""))).join(","), "", "the code is not in the address, the title, the cookies or the storage (no dash)");
  equal((await pageHolds(page, first.userCode)).join(","), "", "the code is not in the address, the title, the cookies or the storage");

  // A sign-in that has grown old: the first press asks who the person is.
  await harness.backdate(admin.username);
  await dialog.getByRole("button", { name: "Approve" }).click();
  await confirmItsYou(page, admin.password);
  await poll(async () => ((await dialog.locator(".dev-status").innerText()).includes("Device approved.") ? true : undefined), "the dialog says Device approved.");
  equal((await dialog.locator(".dev-glyph").innerText()).trim(), "●", "the result has a second cue besides colour");
  equal(await dialog.getByRole("button").evaluateAll((b) => b.map((x) => x.textContent).join(" | ")), "Close", "after a result Close is all that is left");
  const approved = await native.poll(first.deviceCode);
  equal(approved.state, "approved", "the app that started the request gets its token");
  check(/^hwd_[A-Za-z0-9_-]{43}$/.test(approved.token ?? ""), "the token has the form of the record");
  equal((await native.me(approved.token)).status, 200, "the token signs the device in");
  equal((await pageHolds(page, approved.token)).join(","), "", "the token never reached the page");
  equal(addressesHold(person, [first.userCode, approved.token, first.deviceCode]), false, "no request of the page carried a code or a token in its address");
  await dialog.getByRole("button", { name: "Close" }).click();
  await dialog.waitFor({ state: "detached" });

  // ---- Deny, a used code, a code nobody made, a malformed code, and a person who is already signed in -------------------------------
  const second = await native.start("Test phone", "android");
  {
    const open2 = await person.context.newPage();
    await open2.goto(`${harness.origin}/#device=${second.userCode.toLowerCase().replace("-", "")}`);
    const d = open2.getByRole("dialog", { name: "Authorize device" });
    await d.waitFor();
    await poll(async () => ((await d.locator(".dev-kv").count()) > 0 ? true : undefined), "a person who is signed in sees the dialog at once, with the code typed in lower case and without the dash");
    equal((await d.locator(".dev-code").innerText()).trim(), second.userCode, "the code is shown in its normal form");
    equal(await d.locator(".dev-v").nth(1).innerText(), "Android", "the platform of the request");
    await d.getByRole("button", { name: "Deny" }).click();
    await poll(async () => ((await d.locator(".dev-status").innerText()).includes("Request denied.") ? true : undefined), "the dialog says Request denied.");
    equal((await d.locator(".dev-glyph").innerText()).trim(), "○", "the second cue of a refusal");
    const denied = await native.poll(second.deviceCode);
    equal(denied.state, "denied", "the app gets its refusal");
    equal(denied.token, undefined, "a denied request has no token");
    await open2.close();
  }
  const refused = async (code, what) => {
    const p = await person.context.newPage();
    await p.goto(`${harness.origin}/#device=${code}`);
    const d = p.getByRole("dialog", { name: "Authorize device" });
    await d.waitFor();
    const text = await poll(async () => ((await d.locator(".dev-status").innerText()).trim() || undefined), `${what}: the dialog says something`);
    equal(text.replace(/^!\s*/, ""), NOT_VALID, `${what}: the one sentence`);
    equal(await d.locator(".dev-kv").count(), 0, `${what}: no row`);
    equal(await d.getByRole("button", { name: "Approve" }).count(), 0, `${what}: nothing to approve`);
    equal(new URL(p.url()).hash, "", `${what}: the fragment is not in the address bar`);
    await p.close();
  };
  await refused(first.userCode, "a code that was approved already");
  await refused(second.userCode, "a code that was denied already");
  await refused("BCDF-GHJK", "a code nobody made");
  {
    const p = await person.context.newPage();
    await p.goto(`${harness.origin}/#device=NOT-A-CODE`);
    await p.waitForSelector("canvas#term");
    await p.waitForTimeout(500);
    equal(await p.getByRole("dialog").count(), 0, "a malformed code opens nothing");
    equal(new URL(p.url()).hash, "", "a malformed code is taken out of the address as well");
    await p.close();
  }
  // ---- the lists: a person sees and ends their own devices only, an administrator everybody's -------------------------------------------
  const ana = await harness.account({ role: "member", name: "dv-ana" });
  const ben = await harness.account({ role: "member", name: "dv-ben" });
  const anaContext = await open();
  const anaPage = await anaContext.context.newPage();
  await signIn(anaPage, harness, ana);
  const HOSTILE = '<img src=x onerror="window.__pwned=1"> <b>phone</b>';

  /** Approve a device of `context`'s person from the address a link would open (`typed`: by the palette entry and a typed code instead). */
  const approveFor = async (context, name, platform, typed = false) => {
    const grant = await native.start(name, platform);
    equal(grant.status, 200, `a grant for ${name}`);
    const p = await context.context.newPage();
    if (typed) {
      await p.goto(`${harness.origin}/`);
      await p.waitForSelector("canvas#term");
      await runEntry(p, "Authorize device…");
      const field = p.getByLabel("Code", { exact: true });
      await field.fill(grant.userCode.toLowerCase());
      await p.getByRole("button", { name: "Look up" }).click();
    } else await p.goto(`${harness.origin}/#device=${grant.userCode}`);
    const d = p.getByRole("dialog", { name: "Authorize device" });
    await d.waitFor();
    await d.getByRole("button", { name: "Approve" }).click();
    await poll(async () => ((await d.locator(".dev-status").innerText()).includes("Device approved.") ? true : undefined), `${name} is approved`);
    await p.close();
    const got = await native.poll(grant.deviceCode);
    equal(got.state, "approved", `${name} got its token`);
    return { name, token: got.token, id: got.deviceId };
  };
  const laptop = await approveFor(anaContext, "Ana laptop", "macos", true);
  const phone = await approveFor(anaContext, HOSTILE, "android");
  const adminLaptop = await approveFor(person, "Admin desktop", "linux");

  const accountDialog = async (page) => {
    await runEntry(page, "Account…");
    const d = page.getByRole("dialog", { name: "Account", exact: true });
    await d.waitFor();
    await d.locator('[data-group="devices"]').waitFor();
    await poll(async () => ((await d.locator('[data-group="devices"] .dev-item, [data-group="devices"] .dev-empty').count()) > 0 ? true : undefined), "the Devices group is drawn");
    return d;
  };
  const names = (d) => d.locator('[data-group="devices"] .dev-item .dev-name').allInnerTexts();
  {
    const d = await accountDialog(anaPage);
    const seen = (await names(d)).sort();
    equal(seen.join(" | "), [HOSTILE, "Ana laptop"].sort().join(" | "), "a person sees exactly the two devices they approved, a hostile name as text");
    equal(await d.locator("img, b").count(), 0, "no element was made from a name");
    equal(await anaPage.evaluate(() => window.__pwned), undefined, "no script ran");
    equal(await d.locator('[data-group="devices"]').innerText().then((t) => t.includes("Admin desktop")), false, "another person's device is not in the list");
    const meta = await d.locator('[data-group="devices"] .dev-item', { hasText: "Ana laptop" }).locator(".dev-meta").innerText();
    check(/^macOS · (just now|\d+ min ago)$/.test(meta), `the line of a device says its platform and when it was seen (${meta})`);
    await d.getByRole("button", { name: "Close" }).click();
    await d.waitFor({ state: "detached" });
  }
  {
    // A person who has no device sees none of another's.
    const benContext = await open();
    const benPage = await benContext.context.newPage();
    await signIn(benPage, harness, ben);
    const d = await accountDialog(benPage);
    equal(await d.locator('[data-group="devices"] .dev-empty').innerText(), "No devices.", "a second person has none");
    equal(await d.locator('[data-group="devices"] .dev-item').count(), 0, "and sees none of the others");
    // A member has no People entry and no way to the switches.
    await d.getByRole("button", { name: "Close" }).click();
    await d.waitFor({ state: "detached" });
    const titles = await benPage.evaluate(async () => (await import(new URL("actions.js", location.href).href)).listActions().map((a) => a.title));
    check(titles.includes("Account…") && titles.includes("Authorize device…"), "the member has the entries of a person");
    check(!titles.includes("People…"), "a member has no People entry");
    const refusedAccess = await benContext.context.request.get(`${harness.origin}/api/admin/device-access`);
    check(refusedAccess.status() === 403 || refusedAccess.status() === 401, `the switches are not a member's (${refusedAccess.status()})`);
    equal((await benContext.context.request.get(`${harness.origin}/api/admin/devices`)).status() === 200, false, "nor is the list of everybody's devices");
    benContext.expectClean("the second person's context");
  }

  // The administrator sees every device with its owner; a revoke after an old sign-in asks who the person is.
  await harness.backdate(admin.username);
  await adminPage.reload();
  await adminPage.waitForSelector("canvas#term");
  {
    const dialog = await openPeople(adminPage);
    await dialog.locator('[data-group="devices"] .dev-row').first().waitFor();
    const rows = await dialog.locator('[data-group="devices"] .dev-row').evaluateAll((trs) => trs.map((tr) => Array.from(tr.children).slice(0, 3).map((c) => c.textContent).join(" | ")));
    equal(rows.length, 4, "an administrator sees the devices of everybody (the first one of this file was approved by the administrator)");
    check(rows.includes("Test laptop | dv-admin | macOS") && rows.includes("Ana laptop | dv-ana | macOS") && rows.includes(`${HOSTILE} | dv-ana | Android`) && rows.includes("Admin desktop | dv-admin | Linux"), `each row names its owner (${rows.join("; ")})`);
    equal(await dialog.locator("img, b").count(), 0, "a hostile name is text in the administrator's list as well");
    // The person revokes their own laptop first, with a second press: its token is refused on the very next request.
    equal((await native.me(laptop.token)).status, 200, "the token works before the revoke");
    const own = anaPage;
    const anaDialog = await accountDialog(own);
    const row = anaDialog.locator('[data-group="devices"] .dev-item', { hasText: "Ana laptop" });
    const button = row.getByRole("button");
    await button.click();
    equal((await button.innerText()).trim().toLowerCase(), "click again to revoke", "the first press asks");
    equal((await native.me(laptop.token)).status, 200, "and revokes nothing");
    await button.click();
    await poll(async () => ((await anaDialog.locator('[data-group="devices"] .dev-item').count()) === 1 ? true : undefined), "the list shows one device left");
    equal((await native.me(laptop.token)).status, 401, "the revoked token is refused on the very next request");
    equal((await native.me(phone.token)).status, 200, "the other device still works");
    equal((await native.me(adminLaptop.token)).status, 200, "so does another person's");
    await anaDialog.getByRole("button", { name: "Close" }).click();
    // The administrator ends the other one of that person; the page of the administrator needs a fresh sign-in for it.
    await poll(async () => ((await dialog.locator('[data-group="devices"] .dev-row').count()) === 4 ? true : undefined), "the list is the one of the server");
    await dialog.getByRole("button", { name: "Close" }).click();
    const again = await openPeople(adminPage);
    await poll(async () => ((await again.locator('[data-group="devices"] .dev-row').count()) === 3 ? true : undefined), "the list of the administrator no longer has the revoked device");
    const target = again.locator('[data-group="devices"] .dev-row', { hasText: "dv-ana" });
    equal(await target.count(), 1, "one device of that person is left");
    await target.getByRole("button").click();
    await target.getByRole("button").click();
    await confirmItsYou(adminPage, admin.password);
    await poll(async () => ((await again.locator('[data-group="devices"] .dev-row').count()) === 2 ? true : undefined), "the list shows only the administrator's own devices");
    equal((await native.me(phone.token)).status, 401, "a device that the administrator revoked is refused on the next request");
    equal((await native.me(adminLaptop.token)).status, 200, "the administrator's own device is untouched");

    // ---- the switches: both are drawn as the server holds them, and turning one on or off reaches the routes it guards ------------------
    const switches = async () => again.locator('[data-group="device-access"] .dev-switch').evaluateAll((bs) => bs.map((b) => `${b.getAttribute("aria-labelledby") ? document.getElementById(b.getAttribute("aria-labelledby")).textContent : "?"}=${b.textContent}`));
    equal((await switches()).join(", "), "Sync target=OFF, Remote sessions=ON", "the switches show what the server holds (remote was turned on above)");
    await again.getByRole("switch", { name: "Sync target" }).click();
    await poll(async () => ((await switches()).join(", ") === "Sync target=ON, Remote sessions=ON" ? true : undefined), "Sync target is on");
    await again.getByRole("switch", { name: "Remote sessions" }).click();
    await poll(async () => ((await switches()).join(", ") === "Sync target=ON, Remote sessions=OFF" ? true : undefined), "Remote sessions is off");
    // The grant routes serve either feature, so they stay while one switch is on; the stored devices are never touched by a switch.
    equal((await native.start("Second try", "linux")).status, 200, "the grant routes are there while one of the switches is on");
    await again.getByRole("switch", { name: "Sync target" }).click();
    await poll(async () => ((await switches()).join(", ") === "Sync target=OFF, Remote sessions=OFF" ? true : undefined), "both switches are off");
    equal((await native.start()).status, 404, "with both switches off a native client cannot start a grant");
    equal((await native.me(adminLaptop.token)).status, 404, "nor can a device ask who it is: the feature is hidden, not refused");
    await again.getByRole("button", { name: "Close" }).click();
    const last = await openPeople(adminPage);
    await poll(async () => ((await last.locator('[data-group="devices"] .dev-row').count()) === 2 ? true : undefined), "turning the switches off ended no stored device");
    equal((await last.locator('[data-group="device-access"] .dev-switch').evaluateAll((bs) => bs.map((b) => b.textContent).join(","))), "OFF,OFF", "both off is the state the server holds");
    await last.getByRole("button", { name: "Close" }).click();
  }
  anaContext.expectClean("the context of the person whose devices were listed");
  person.expectClean("the context of the person who signed devices in");
  adminContext.expectClean("the administrator's context");
  console.log(
    `devices: a code in the fragment survived the sign-in; the dialog showed ${facts[1]} / ${facts[3]} and the code; Approve asked for a fresh sign-in and the app got ${approved.state}; ` +
      `Deny gave ${"denied"}; an approved, a denied and an unknown code and a hidden feature said one sentence; a malformed code opened nothing`,
  );
});
