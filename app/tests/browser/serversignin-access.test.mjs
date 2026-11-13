// The sign-in dialog with the secrets locked, in every contrast, motion and pointer mode, and the Sign out of a row (docs/design.md 6.35 and 6.34). Against the stub that plays the host.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, currentBackend } from "./lib.mjs";
import { GOOD, buttons, count, dialog, gone, link, open, openDialog, result, stateText } from "./serversignin-lib.mjs";

export const backends = ["tauri"];
export const backendNote = "signing in to a server is a desktop and mobile app step; the browser window is the server's own page and has no such dialog";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(6000);
  if (currentBackend() === "tauri") {
    await lockedCase(page, url);
    await accessibilityCase(page, url);
    await signOutCase(page, url);
  }
  assert.deepEqual(errors, [], "the page reported errors");
}

/** The secrets are locked: the buttons that need them give way to the dim line, nothing is called, and they return after the unlock. */
async function lockedCase(page, url) {
  await open(page, url, { view: { locked: true } });
  await openDialog(page);
  assert.equal(await page.locator(".ss-locked").textContent(), "Locked: unlock to connect");
  assert.equal(await page.locator(".ss-locked").getAttribute("role"), "status");
  assert.deepEqual(await buttons(page), ["Cancel"]);
  await page.locator("#ss-address").fill(GOOD);
  await page.keyboard.press("Enter");
  await page.waitForTimeout(150);
  assert.equal(await count(page, "server_sign_in_start"), 0, "nothing is called while the secrets are locked");
  assert.equal(await page.locator(".ss-code").count(), 0);
  // After the unlock the dialog works again without being opened again, and what was typed is still there.
  await page.evaluate(() => window.__MOCK__.serverLink.set({ locked: false }));
  await page.waitForFunction(() => document.querySelector(".server-signin .web-solid")?.textContent === "Continue", undefined, { timeout: 5000 });
  assert.equal(await page.locator("#ss-address").inputValue(), GOOD);
  assert.equal(await page.locator(".ss-locked").count(), 0);
  await dialog(page).getByRole("button", { name: "Continue", exact: true }).click();
  await page.waitForFunction(() => document.querySelector(".ss-code")?.textContent === "BCDF-GHJK");
  await page.keyboard.press("Escape");
  await gone(page);

  // Cancel while locked and nothing started calls nothing.
  await open(page, url, { view: { locked: true } });
  await openDialog(page);
  await dialog(page).getByRole("button", { name: "Cancel", exact: true }).click();
  await gone(page);
  assert.equal((await page.evaluate(() => window.__MOCK__.serverLink.calls.filter((c) => c[0] !== "server_status"))).length, 0);

  // The store locks while the request waits: the host refuses the approval with its locked text, and the line replaces Start again.
  await open(page, url, { interval: 1 });
  await openDialog(page);
  await page.locator("#ss-address").fill(GOOD);
  await dialog(page).getByRole("button", { name: "Continue", exact: true }).click();
  await page.waitForFunction(() => document.querySelector(".ss-code")?.textContent === "BCDF-GHJK");
  await page.evaluate(() => {
    window.__MOCK__.serverLink.set({ locked: true });
    window.__MOCK__.on("server_sign_in_poll", () => {
      throw "secrets are locked";
    });
  });
  await page.waitForFunction(() => document.querySelector(".ss-state")?.textContent?.includes("Locked: unlock to connect"));
  assert.deepEqual(await buttons(page), ["Cancel"], "Start again waits for the unlock");
  await page.evaluate(() => {
    window.__MOCK__.on("server_sign_in_poll", null);
    window.__MOCK__.serverLink.set({ locked: false });
  });
  await page.waitForFunction(() => [...document.querySelectorAll(".server-signin button")].some((b) => b.textContent === "Start again"), undefined, { timeout: 5000 });
  assert.equal(await stateText(page), "!The sign-in failed.", "the locked sentence goes with the lock");
  await page.keyboard.press("Escape");
  await gone(page);

  // The address is known, so the request starts at once, and the store is locked: after the unlock `Start again` is there.
  await open(page, url, { view: { url: GOOD }, startFails: "secrets are locked" });
  await openDialog(page);
  await page.evaluate(() => window.__MOCK__.serverLink.set({ locked: true }));
  await page.waitForSelector(".server-signin .ss-locked");
  assert.deepEqual(await buttons(page), ["Cancel"]);
  await page.evaluate(() => {
    window.__MOCK__.serverLink.startFails(null);
    window.__MOCK__.serverLink.set({ locked: false });
  });
  await page.waitForFunction(() => [...document.querySelectorAll(".server-signin button")].some((b) => b.textContent === "Start again"), undefined, { timeout: 5000 });
  assert.deepEqual(await buttons(page), ["Start again", "Cancel"]);
  assert.equal(await page.locator(".server-signin .ss-locked").count(), 0);
  await dialog(page).getByRole("button", { name: "Start again", exact: true }).click();
  await page.waitForFunction(() => document.querySelector(".ss-code")?.textContent === "BCDF-GHJK");
  await page.keyboard.press("Escape");
  await gone(page);
}

/** The dialog in every contrast, motion and pointer mode: the words and borders stay, the ring is 2 px, nothing moves. */
async function accessibilityCase(page, url) {
  await open(page, url, { view: { url: GOOD }, polls: ["pending"], interval: 5 });
  await openDialog(page);
  await page.waitForFunction(() => document.querySelector(".ss-code")?.textContent === "BCDF-GHJK");
  // The code is real text in a named group, so assistive technology says what it is.
  assert.equal(await dialog(page).getByRole("group", { name: "Code", exact: true }).textContent(), "BCDF-GHJK");
  const probe = () =>
    page.evaluate(() => {
      const pick = (selector) => {
        const node = document.querySelector(selector);
        const c = getComputedStyle(node);
        const r = node.getBoundingClientRect();
        return {
          text: node.textContent.trim(), border: parseFloat(c.borderTopWidth), style: c.borderTopStyle, colour: c.borderTopColor, height: r.height,
          animation: c.animationName, transition: c.transitionDuration, dot: c.borderTopStyle,
        };
      };
      return {
        code: pick(".ss-code"), dot: pick(".ss-dot"), open: pick(".server-signin .web-btn"), cancel: pick(".server-signin .web-btn:last-child"),
        state: pick(".ss-state"), box: pick(".server-signin"),
      };
    });
  const plain = await probe();
  assert.equal(plain.code.text, "BCDF-GHJK");
  assert.ok(plain.open.height >= 27.99 && plain.cancel.height >= 27.99, `the buttons are at least 28 px high (${plain.open.height})`);
  assert.equal(plain.dot.style, "dotted", "the waiting dot is a dotted ring");

  await page.emulateMedia({ forcedColors: "active" });
  const forced = await probe();
  assert.equal(forced.code.text, "BCDF-GHJK", "the code stays readable");
  for (const name of ["code", "open", "cancel", "box"]) assert.ok(forced[name].border >= 1 && forced[name].style !== "none", `${name} keeps its border under forced colours`);
  assert.equal(forced.dot.style, "dotted", "the ring keeps its dots under forced colours");
  assert.equal(forced.state.text, "Waiting for approval…");
  await page.emulateMedia({ forcedColors: "none" });

  await page.emulateMedia({ contrast: "more" });
  await page.waitForFunction(() => document.documentElement.classList.contains("contrast-high"));
  const high = await probe();
  assert.notEqual(high.code.colour, plain.code.colour, "the hairline follows the stronger token");
  assert.equal(high.code.text, plain.code.text);
  await page.emulateMedia({ contrast: "no-preference" });
  await page.waitForFunction(() => !document.documentElement.classList.contains("contrast-high"));

  await page.emulateMedia({ reducedMotion: "reduce" });
  const still = await probe();
  for (const name of ["code", "dot", "open", "cancel"]) {
    assert.equal(still[name].animation, "none", `${name} has no animation`);
    assert.ok(/^0s(, 0s)*$/.test(still[name].transition), `${name} has no transition (${still[name].transition})`);
  }
  assert.equal(still.dot.style, "dotted", "a static ring");
  assert.equal(still.state.text, "Waiting for approval…", "and the word");
  await page.emulateMedia({ reducedMotion: "no-preference" });

  // The 2 px ring of the keyboard, and Tab stays inside the dialog.
  await page.keyboard.press("Tab");
  const ring = await page.evaluate(() => {
    const c = getComputedStyle(document.activeElement);
    return { ring: c.outlineWidth, style: c.outlineStyle, inside: document.activeElement.closest(".server-signin") !== null };
  });
  assert.equal(ring.ring, "2px");
  assert.notEqual(ring.style, "none");
  assert.ok(ring.inside);
  const order = [];
  for (let i = 0; i < 3; i++) {
    await page.keyboard.press("Tab");
    order.push(await page.evaluate(() => ({ inside: document.activeElement.closest(".server-signin") !== null, text: document.activeElement.textContent })));
  }
  assert.ok(order.every((o) => o.inside), "the focus never leaves the dialog");
  assert.equal(await page.locator('.server-signin[role="dialog"]').getAttribute("aria-modal"), "true");
  await page.keyboard.press("Escape");
  await gone(page);

  // A coarse pointer asks for 44 px: the sheet says so for the buttons and the field.
  const css = readFileSync(join(APP_DIR, "dist", "serversignin.css"), "utf8");
  const coarse = css.slice(css.indexOf("@media (pointer: coarse)"), css.indexOf("@media (forced-colors"));
  assert.ok(coarse.includes(":is(button, .web-input)"));
  assert.match(coarse, /min-height: max\(44px, calc\(44rem \/ 12\)\)/);
  assert.match(css, /min-height: max\(28px, calc\(28rem \/ 12\)\)/);
  assert.equal(/#[0-9a-fA-F]{3,8}\b/.test(css), false, "no literal colour");
}

/** The Sign out of a row: the first press arms, a press elsewhere or Escape disarms, the second press signs out and keeps the address. */
async function signOutCase(page, url) {
  await open(page, url, { view: { state: "signed_in", url: GOOD } });
  await page.evaluate(async () => {
    const { armSignOut } = await import(new URL("serversignin.js", location.href).href);
    const scope = document.createElement("div");
    scope.id = "row-scope";
    const other = document.createElement("button");
    other.id = "other";
    other.textContent = "other";
    const out = document.createElement("button");
    out.id = "out";
    scope.append(out, other);
    document.body.append(scope);
    window.__OUT = undefined;
    window.__ABORT = new AbortController();
    armSignOut(out, scope, window.__ABORT.signal, (r) => {
      window.__OUT = r;
    });
  });
  const out = page.locator("#out");
  assert.equal(await out.textContent(), "Sign out");
  await out.click();
  assert.equal(await out.textContent(), "Click again to sign out", "the first press changes the button");
  assert.equal(await count(page, "server_sign_out"), 0, "the first press never calls the command");
  await page.locator("#other").click();
  assert.equal(await out.textContent(), "Sign out", "a press elsewhere puts it back");
  await out.click();
  assert.equal(await out.textContent(), "Click again to sign out");
  await page.keyboard.press("Escape");
  assert.equal(await out.textContent(), "Sign out", "Escape puts it back");
  assert.equal(await count(page, "server_sign_out"), 0);
  await out.click();
  await out.click();
  await page.waitForFunction(() => window.__OUT !== undefined);
  assert.equal(await count(page, "server_sign_out"), 1);
  const outcome = await page.evaluate(() => window.__OUT);
  assert.equal(outcome.ok, true);
  assert.equal(outcome.view.state, "signed_out", "the host's view says signed out");
  assert.equal(outcome.view.url, GOOD, "the address is kept");

  // The next Sign in… opens on the second screen: the address is kept and the token is gone.
  await openDialog(page);
  await page.waitForFunction(() => document.querySelector(".ss-code")?.textContent === "BCDF-GHJK");
  assert.equal(await page.locator("#ss-address").count(), 0);
  assert.deepEqual(await link(page, "server_sign_in_start"), [{ address: GOOD }]);
  await page.keyboard.press("Escape");
  await gone(page);

  // A refusal is a fixed sentence, and the locked store is said in the record's words.
  await page.evaluate(() => window.__MOCK__.on("server_sign_out", () => {
    throw "secrets are locked";
  }));
  await page.evaluate(() => {
    const out = document.querySelector("#out");
    out.click();
    out.click();
  });
  await page.waitForFunction(() => window.__OUT?.ok === false);
  assert.deepEqual(await page.evaluate(() => window.__OUT), { ok: false, sentence: "Locked: unlock to connect" });
}

