// The order of the reads of the server row in the Connections dialog (docs/design.md 6.34): a read that is slow does not pile up
// behind the five-second timer, and an older answer never overwrites a newer state. The stub plays the host. Desktop only: the
// browser build has no link to read and no Sign out. The focus comes back into the dialog from the sign-in, whose opener is redrawn.
import assert from "node:assert/strict";
import { SIGNED_IN, open, openDialog, rowButtons } from "./serversessions-lib.mjs";

export const backends = ["tauri"];
export const backendNote = "the link to a server and Sign out are the desktop's";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(8000);
  await orderCase(page, url);
  await focusCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/**
 * A read that is slow does not pile up behind the timer, and its answer, when it comes back after a sign-out, does not bring the
 * signed-in row back.
 */
async function orderCase(page, url) {
  await open(page, url);
  await openDialog(page);
  await page.waitForSelector(".conns .cx-sess");
  // The next read of the link waits until the test lets it go, and then answers the link as it was before the sign-out.
  await page.evaluate((signedIn) => {
    window.__held = 0;
    window.__let = null;
    window.__MOCK__.on("server_status", () => {
      window.__held++;
      return new Promise((go) => {
        window.__let = () => go(structuredClone(signedIn));
      });
    });
  }, SIGNED_IN);
  await page.waitForFunction(() => window.__held === 1, null, { timeout: 7000 });
  const signOut = page.locator(".conns .cx-srv-acts button").first();
  await signOut.click();
  await signOut.click();
  await page.waitForFunction(() => document.querySelector(".conns .cx-srv-state")?.textContent === "not signed in");
  // A whole tick later the held read is still the only one.
  await page.waitForTimeout(5600);
  assert.equal(await page.evaluate(() => window.__held), 1, "the timer does not start a second read while one is out");
  await page.evaluate(() => {
    window.__MOCK__.on("server_status", null);
    window.__let();
  });
  await page.waitForTimeout(300);
  assert.equal(await page.locator(".conns .cx-srv-state").textContent(), "not signed in", "the older answer is dropped");
  assert.deepEqual(await rowButtons(page), ["Sign in…"]);
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.conns", { state: "detached" });
}

/** The sign-in dialog closes after the row was redrawn without the button that opened it: the focus stays in the Connections dialog. */
async function focusCase(page, url) {
  await open(page, url, { view: { ...SIGNED_IN, state: "signed_out", remote_on: false } });
  await openDialog(page, "not signed in");
  await page.locator(".conns .cx-srv-acts button", { hasText: "Sign in…" }).click();
  const signIn = page.getByRole("dialog", { name: "Sign in to the server" });
  await signIn.waitFor();
  // The address is known, so the sign-in starts at once; the next read of the row says `signing in…` and draws no button.
  await page.waitForFunction(() => document.querySelector(".conns .cx-srv-state")?.textContent === "signing in…", null, { timeout: 7000 });
  assert.deepEqual(await rowButtons(page), []);
  await page.keyboard.press("Escape");
  await signIn.waitFor({ state: "detached" });
  await page.waitForFunction(() => document.activeElement?.dataset.key === "signin");
  assert.equal(await page.evaluate(() => Boolean(document.activeElement?.closest(".dlg.conns"))), true, "the focus is in the Connections dialog");
  await page.waitForTimeout(200);
  assert.notEqual(await page.evaluate(() => document.activeElement?.id), "ime", "never the terminal behind the dialog");
  await page.keyboard.press("Escape");
  await page.waitForSelector(".dlg.conns", { state: "detached" });
}
