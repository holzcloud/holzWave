// The device features of the page against the real server: a code in the fragment of the address survives the sign-in and opens the
// dialog that shows who asks and from where; approving and denying (with a fresh sign-in) reach the app that is waiting; a code
// that is not valid, a hidden feature and a malformed code say or open nothing more; and the code and the token are kept nowhere.
import { runTest, signIn, nativeClient, setSwitches, pageHolds, addressesHold, confirmItsYou, check, equal, poll } from "./lib.mjs";

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
  person.expectClean("the context of the person who signed devices in");
  adminContext.expectClean("the administrator's context");
  console.log(
    `devices: a code in the fragment survived the sign-in; the dialog showed ${facts[1]} / ${facts[3]} and the code; Approve asked for a fresh sign-in and the app got ${approved.state}; ` +
      `Deny gave ${"denied"}; an approved, a denied and an unknown code and a hidden feature said one sentence; a malformed code opened nothing`,
  );
});
