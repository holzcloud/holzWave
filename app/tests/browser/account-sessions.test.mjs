// The account dialog (docs/design.md 6.29), the sessions and the strings a server sends: the rows (device, address, last seen,
// `this session` by word and dot), the sign-out of another session after `Confirm it is you`, `Sign out everywhere` after a second
// press, the palette entry `Sign out`, a device or passkey name that is not text, and an account that cannot be read.
import assert from "node:assert/strict";
import { actionTitles } from "./websession-lib.mjs";
import { STEP, clean, confirmWith, dialog, fixture, focusedName, homeStub, itemsOf, legends, openAccountDialog, runAction, statusLine, stepUpRoute } from "./account-lib.mjs";
import { sessionRoute, openMain } from "./websession-lib.mjs";

export const backends = ["ws"];
export const backendNote = "the dialog exists on the web backend only: http() throws on the Tauri backend";

const STEP_PW = "step up password";
const HOSTILE_DEVICE = '<img src=x onerror="window.__pwned=1"> on <b>Linux</b>';
const HOSTILE_KEY = "<script>window.__pwned=2</script><i>key</i>";
const HOSTILE_ADDR = '"><svg onload=window.__pwned=3>';

export default async function ({ page, url, errors }) {
  const atRoot = () => page.waitForURL((u) => u.pathname === "/" && u.search === "");

  // ---- the rows: this session first and marked by a dot and words, the others with `Sign out` -----------------------------------------
  {
    const state = { me: fixture() };
    const server = await openAccountDialog(page, url, state, [
      {
        method: "DELETE",
        path: "/api/me/sessions/s2",
        reply: () => {
          state.me.sessions = state.me.sessions.filter((s) => s.id !== "s2");
          return { status: 204 };
        },
      },
      stepUpRoute,
    ]);
    assert.deepEqual(await itemsOf(page, "sessions"), [
      "● | Firefox on macOS | 203.0.113.7 | · now | this session",
      "Chrome on Linux | 198.51.100.24 | · 3 h ago | Sign out",
      "Safari on iOS | 192.0.2.9 | · 7 h ago | Sign out",
    ]);
    assert.equal(await page.locator('.account [data-group="sessions"] .acc-this').count(), 1, "`this session` only on the current row");
    assert.equal(await page.locator('.account [data-group="sessions"] .acc-item:first-child button').count(), 0, "no Sign out on this session");
    assert.equal(await page.locator(".account .acc-addr").first().textContent(), "203.0.113.7");
    // `Sign out` of another session removes the row; the sign-in is recent here, so no layer is shown.
    assert.equal(server.requests("DELETE", "/api/me/sessions/s2").length, 0);
    await page.locator('.account [data-group="sessions"] .acc-item:nth-child(2) button').click();
    await page.waitForFunction(() => document.querySelectorAll('.account [data-group="sessions"] .acc-item').length === 2);
    assert.equal(await statusLine(page), "Saved.");
    assert.deepEqual((await itemsOf(page, "sessions")).map((r) => r.split(" | ")[0]), ["●", "Safari on iOS"]);
    assert.equal(server.count("DELETE", "/api/me/sessions/s2"), 1);
    assert.equal(server.count("POST", "/auth/step-up"), 0, "no layer while the sign-in is recent");
    await clean(page, server, [], "sign out of another session");
  }
  {
    const state = { me: fixture() };
    const server = await openAccountDialog(page, url, state, [
      {
        method: "DELETE",
        path: "/api/me/sessions/s3",
        reply: (request, n) => {
          if (n === 0) return STEP;
          state.me.sessions = state.me.sessions.filter((s) => s.id !== "s3");
          return { status: 204 };
        },
      },
      stepUpRoute,
    ]);
    await page.locator('.account [data-group="sessions"] .acc-item:nth-child(3) button').click();
    await confirmWith(page, STEP_PW);
    await page.waitForFunction(() => document.querySelectorAll('.account [data-group="sessions"] .acc-item').length === 2);
    assert.equal(server.count("DELETE", "/api/me/sessions/s3"), 2, "repeated once after the confirmation");
    assert.equal(server.count("POST", "/auth/step-up"), 1);
    await clean(page, server, [STEP_PW], "sign out with a step-up");
  }

  // ---- `Sign out everywhere`: armed by a second press, then the window shows the start page ------------------------------------------------
  {
    const state = { me: fixture() };
    const server = await openAccountDialog(page, url, state, [
      homeStub,
      { method: "DELETE", path: "/api/me/sessions", answers: [STEP, { status: 204 }] },
      stepUpRoute,
    ]);
    const everywhere = page.locator(".account .acc-end .acc-danger");
    await everywhere.click();
    assert.equal(await everywhere.textContent(), "Click again to sign out everywhere");
    assert.equal(server.count("DELETE", "/api/me/sessions"), 0, "the first press only arms the button");
    await page.keyboard.press("Tab");
    assert.equal(await everywhere.textContent(), "Sign out everywhere", "leaving the button disarms it");
    await everywhere.click();
    await everywhere.click();
    await confirmWith(page, STEP_PW);
    await atRoot();
    assert.equal(server.count("DELETE", "/api/me/sessions"), 2);
    assert.equal(new URL(page.url()).pathname, "/", "the location was replaced by the start page");
    assert.equal(await page.evaluate(() => history.length), 2, "replaced, not pushed: the account page is not a step back");
    assert.deepEqual(server.foreign, []);
  }

  // ---- the palette entry `Sign out`: the request first, then the location, even when the request fails ---------------------------------------
  {
    const server = await openMain(page, url, [sessionRoute(), homeStub, { method: "POST", path: "/auth/logout", status: 204 }]);
    assert.ok((await actionTitles(page)).includes("Sign out"));
    await runAction(page, "account.signout");
    await atRoot();
    const posted = server.requests("POST", "/auth/logout");
    assert.equal(posted.length, 1);
    assert.deepEqual(posted[0].json, {});
    assert.equal(posted[0].headers["x-csrf-token"], "csrf-token-1");
  }
  {
    await openMain(page, url, [sessionRoute(), homeStub, { method: "POST", path: "/auth/logout", abort: true }]);
    await runAction(page, "account.signout");
    await atRoot();
    assert.equal(new URL(page.url()).pathname, "/", "the page leaves even when the server cannot be reached");
  }

  // ---- what a server sends is text: a device, an address and a passkey name that look like markup -----------------------------------------------
  {
    const state = {
      me: fixture({
        passkeys: [{ id: "k1", name: HOSTILE_KEY, created_at: new Date().toISOString(), last_used: null }],
        sessions: [{ id: "s1", device: HOSTILE_DEVICE, addr: HOSTILE_ADDR, created_at: new Date().toISOString(), last_seen: new Date().toISOString(), current: true }],
      }),
    };
    const server = await openAccountDialog(page, url, state);
    assert.deepEqual(await itemsOf(page, "passkeys"), [`${HOSTILE_KEY} | not used yet | Remove`]);
    assert.deepEqual(await itemsOf(page, "sessions"), [`● | ${HOSTILE_DEVICE} | ${HOSTILE_ADDR} | · now | this session`]);
    assert.equal(await page.locator(".account img, .account b, .account i, .account script, .account svg").count(), 0, "no element was made from them");
    assert.equal(await page.evaluate(() => window.__pwned), undefined, "no handler ran");
    await clean(page, server, [], "hostile strings");
  }

  // ---- an account that cannot be read: the sentence, and `Close` is all there is ----------------------------------------------------------------
  {
    const state = { me: fixture() };
    await openMain(page, url, [sessionRoute(), { path: "/api/me", abort: true }]);
    await runAction(page, "account.open");
    await dialog(page).waitFor();
    await page.waitForFunction(() => document.querySelector(".account .set-status")?.textContent !== "");
    assert.equal(await statusLine(page), "Could not reach the server. Try again.");
    assert.deepEqual(await legends(page), []);
    assert.equal(await focusedName(page), "Close");
    await page.keyboard.press("Escape");
    await dialog(page).waitFor({ state: "detached" });
    // An answer that is not the documented shape is no account either.
    state.me.account.role = "root";
    await openMain(page, url, [sessionRoute(), { path: "/api/me", body: () => state.me }]);
    await runAction(page, "account.open");
    await dialog(page).waitFor();
    await page.waitForFunction(() => document.querySelector(".account .set-status")?.textContent !== "");
    assert.equal(await statusLine(page), "Could not reach the server. Try again.");
    assert.deepEqual(await legends(page), []);
  }

  assert.deepEqual(errors, [], "the page reported errors");
}
