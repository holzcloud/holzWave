// The unlock dialog (docs/design.md 6.28) and the layer of an ended session (6.30) on the main page, against a scripted
// server: `GET /api/session` tells the page what it is, the status line shows what is wrong in a glyph and a word, and the
// dialog opens the saved passwords. The marks that depend on the clock are in websession-marks.test.mjs. Web backend only;
// the desktop has its own file (websession-tauri.test.mjs).
import assert from "node:assert/strict";
import {
  actionTitles,
  errorText,
  focusedName,
  holdsSecret,
  homeStub,
  leftover,
  marksOf,
  openMain,
  sessionRoute,
  terminalHasFocus,
  webText,
} from "./websession-lib.mjs";

export const backends = ["ws"];
export const backendNote = "the marks and the dialogs exist on the web backend only: http() throws on the Tauri backend";

const PASSPHRASE = "correct horse";
const RECOVERY = "ABCD-EFGH-IJKL-MNOP-QRST-UVWX";
const FAILED_UNLOCK = "That did not unlock your secrets.";
const UNREACHABLE = "Could not reach the server. Try again.";
const ENDED = "Your session ended. Sign in again.";

export default async function ({ page, url, errors }) {
  const sessionLocked = sessionRoute({ state: "locked" });
  const dialog = () => page.getByRole("dialog", { name: "Your saved passwords are locked" });
  const unlockButton = () => page.locator("#status button.web-act");
  const field = () => page.locator("#web-unlock-field");
  const emit = (state, seconds) => page.evaluate(([s, n]) => window.__MOCK__.emit("hw-session", n === undefined ? { state: s } : { state: s, seconds: n }), [state, seconds]);
  const noOtherTraffic = (server, what) => {
    const l = leftover(server);
    assert.deepEqual(l.foreign, [], `${what}: a request left the page's origin`);
    assert.deepEqual(l.unscripted, [], `${what}: a request nobody scripted`);
  };

  // ---- tracer: a locked session shows the mark, the dialog opens it, the mark goes ---------------------------------------
  {
    const server = await openMain(page, url, [
      sessionLocked,
      { method: "POST", path: "/auth/unlock", body: { state: "unlocked" }, delay: 400 },
    ]);
    await page.waitForSelector('#status .web-mark[data-mark="locked"]');
    const before = await webText(page);
    assert.deepEqual(await marksOf(page), ["secrets locked"]);
    assert.equal(await unlockButton().textContent(), "Unlock");
    assert.ok((await actionTitles(page)).includes("Unlock…"), "the palette lists Unlock…");
    assert.equal(await page.locator("#status .seg.web").getAttribute("role"), "status");

    await unlockButton().click();
    await dialog().waitFor();
    assert.equal(await focusedName(page), "Passphrase", "the field has the focus");
    assert.equal(await page.getByLabel("Passphrase").getAttribute("type"), "password");
    assert.equal(await page.getByLabel("Passphrase").getAttribute("autocomplete"), "off");
    assert.equal(await page.getByLabel("Passphrase").inputValue(), "", "no field is pre-filled");
    await page.keyboard.type(PASSPHRASE);
    await page.keyboard.press("Enter");
    // While the request is out: the label adds the dots, the box is busy, the passphrase already left the field.
    await page.waitForFunction(() => document.querySelector(".dlg.web-unlock")?.getAttribute("aria-busy") === "true");
    assert.equal(await page.getByRole("button", { name: "Unlock…" }).count(), 1);
    assert.equal(await page.getByLabel("Passphrase").inputValue(), "", "the field is empty before the answer arrives");
    assert.equal(await page.getByLabel("Passphrase").evaluate((el) => el.readOnly), true, "read-only while working");
    assert.deepEqual(await holdsSecret(page, [PASSPHRASE]), [], "the passphrase is nowhere in the page while the request is out");

    await dialog().waitFor({ state: "detached" });
    await page.waitForFunction(() => document.querySelectorAll("#status .web-mark").length === 0);
    const after = await webText(page);
    assert.ok(!(await actionTitles(page)).includes("Unlock…"), "the palette entry is withdrawn");
    await page.waitForFunction(() => document.activeElement?.id === "ime");
    assert.equal(await terminalHasFocus(page), true, "the keyboard is back with the terminal");

    const sent = server.requests("POST", "/auth/unlock");
    assert.equal(sent.length, 1, "exactly one unlock request");
    assert.deepEqual(sent[0].json, { passphrase: PASSPHRASE });
    assert.equal(sent[0].headers["x-csrf-token"], "csrf-token-1");
    assert.deepEqual(await holdsSecret(page, [PASSPHRASE]), []);
    noOtherTraffic(server, "tracer");
    console.log(`Tracer: POST /auth/unlock {"passphrase":"<${PASSPHRASE.length} characters>"}; the mark before: ${JSON.stringify(before)}, after: ${JSON.stringify(after)}`);
  }

  // ---- the unlock failures, the recovery code, the keys -----------------------------------------------------------------
  {
    const server = await openMain(page, url, [
      sessionLocked,
      {
        method: "POST",
        path: "/auth/unlock",
        answers: [
          { status: 401, body: { code: "invalid" } },
          { status: 429, body: { code: "throttled", retry_seconds: 720 } },
          { abort: true },
          { status: 401, body: { code: "invalid" } },
          { status: 200, body: { state: "unlocked" } },
        ],
      },
    ]);
    await page.waitForSelector('#status .web-mark[data-mark="locked"]');
    await unlockButton().click();
    await dialog().waitFor();
    // The order of the keys: Passphrase, Not now, Unlock, Use recovery code, and round again.
    const order = [];
    for (let i = 0; i < 5; i++) {
      order.push(await focusedName(page));
      await page.keyboard.press("Tab");
    }
    assert.deepEqual(order, ["Passphrase", "Not now", "Unlock", "Use recovery code", "Passphrase"]);
    await page.keyboard.press("Shift+Tab"); // from `Not now` back to the field
    assert.equal(await focusedName(page), "Passphrase");
    await page.keyboard.press("Shift+Tab");
    assert.equal(await focusedName(page), "Use recovery code", "Shift+Tab goes back, and round");
    await page.keyboard.press("Tab");
    assert.equal(await focusedName(page), "Passphrase");
    assert.equal(
      await page.locator(".web-unlock .web-note").textContent(),
      "If you lost both, an admin can reset your secrets. That deletes your saved passwords and keys, and keeps your account, layout and history.",
    );

    // A wrong passphrase: the one sentence, the field emptied, the focus in it, the dialog stays.
    await field().fill("wrong one");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.querySelector("#web-unlock-error")?.textContent !== "");
    assert.equal(await errorText(page), FAILED_UNLOCK);
    assert.equal(await field().inputValue(), "", "nothing is put back into the field");
    assert.equal(await field().getAttribute("aria-invalid"), "true");
    assert.equal(await field().getAttribute("aria-describedby"), "web-unlock-error");
    assert.equal(await focusedName(page), "Passphrase");
    assert.equal(await dialog().count(), 1);
    assert.equal(await page.getByRole("button", { name: "Unlock", exact: true }).count(), 2, "the button reads Unlock again, and the mark has its own");

    // Throttled: the wait from the server's retry_seconds.
    await field().fill("again");
    await page.getByRole("button", { name: "Unlock", exact: true }).last().click();
    await page.waitForFunction(() => document.querySelector("#web-unlock-error")?.textContent.startsWith("Too many"));
    assert.equal(await errorText(page), "Too many attempts. Try again in 12 minutes.");

    // The network fails: the sentence for an unreachable server.
    await field().fill("third");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.querySelector("#web-unlock-error")?.textContent.startsWith("Could not"));
    assert.equal(await errorText(page), UNREACHABLE);
    assert.equal(await field().inputValue(), "");

    // The recovery code: the field is swapped, an error goes, the next submit sends `recovery_code`.
    await field().fill("typed passphrase");
    await page.getByRole("button", { name: "Use recovery code" }).click();
    assert.equal(await page.locator(".web-unlock .web-label").textContent(), "Recovery code");
    assert.equal(await field().inputValue(), "", "what was typed does not move into the other field");
    assert.equal(await errorText(page), "", "the error goes with the swap");
    assert.equal(await page.locator(".web-unlock .web-lead").textContent(), "Enter your recovery code to use your saved passwords and keys.");
    assert.equal(await page.getByRole("button", { name: "Use passphrase" }).count(), 1);
    assert.equal(await focusedName(page), "Recovery code");
    await field().fill(RECOVERY);
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.querySelector("#web-unlock-error")?.textContent !== "");
    assert.equal(await errorText(page), FAILED_UNLOCK, "a wrong recovery code says the same");
    await page.getByRole("button", { name: "Use passphrase" }).click();
    assert.equal(await page.locator(".web-unlock .web-label").textContent(), "Passphrase");

    // Escape closes without a request; the mark stays.
    const requests = server.requests("POST", "/auth/unlock").length;
    await page.keyboard.press("Escape");
    await dialog().waitFor({ state: "detached" });
    assert.equal(server.requests("POST", "/auth/unlock").length, requests, "Escape sends nothing");
    assert.deepEqual(await marksOf(page), ["secrets locked"], "the mark stays");
    await page.waitForFunction(() => document.activeElement?.id === "ime");

    // `Not now` does the same.
    await unlockButton().click();
    await dialog().waitFor();
    await page.getByRole("button", { name: "Not now" }).click();
    await dialog().waitFor({ state: "detached" });

    // A final success; the bodies were the passphrases and then the recovery code.
    await unlockButton().click();
    await dialog().waitFor();
    await page.getByRole("button", { name: "Use recovery code" }).click();
    await field().fill(RECOVERY);
    await page.keyboard.press("Enter");
    await dialog().waitFor({ state: "detached" });
    assert.deepEqual(
      server.requests("POST", "/auth/unlock").map((r) => r.json),
      [{ passphrase: "wrong one" }, { passphrase: "again" }, { passphrase: "third" }, { recovery_code: RECOVERY }, { recovery_code: RECOVERY }],
    );
    assert.deepEqual(await holdsSecret(page, ["wrong one", "again", "third", RECOVERY]), []);
    noOtherTraffic(server, "unlock failures");
  }

  // ---- the same state change in another tab: an `unlocked` event closes an open dialog ------------------------------------
  {
    const server = await openMain(page, url, [sessionLocked]);
    await page.waitForSelector('#status .web-mark[data-mark="locked"]');
    await unlockButton().click();
    await dialog().waitFor();
    await emit("unlocked");
    await dialog().waitFor({ state: "detached" });
    await page.waitForFunction(() => document.querySelectorAll("#status .web-mark").length === 0);
    assert.ok(!(await actionTitles(page)).includes("Unlock…"));
    // And a `locked` event brings the mark and the entry back.
    await emit("locked");
    await page.waitForSelector('#status .web-mark[data-mark="locked"]');
    assert.ok((await actionTitles(page)).includes("Unlock…"));
    assert.equal(server.count("POST", "/auth/unlock"), 0);
  }

  // ---- an ended session: a layer with one button, no navigation by itself ----------------------------------------------
  {
    const ended = () => page.getByRole("alertdialog", { name: ENDED });
    for (const how of ["event", "code"]) {
      const server = await openMain(page, url, [sessionRoute(), homeStub]);
      await page.waitForFunction(() => window.__WS_MOCK__.sockets.length >= 1);
      assert.equal(await ended().count(), 0, "nothing is shown while the session lives");
      if (how === "event") await emit("ended", 0);
      else await page.evaluate(() => window.__WS_MOCK__.drop(4401));
      await ended().waitFor();
      assert.equal(await focusedName(page), "Sign in", "the button has the focus");
      assert.equal(await page.locator(".web-ended .web-lead").textContent(), ENDED);
      assert.deepEqual(await marksOf(page), ["session ended"]);
      assert.equal(new URL(page.url()).pathname, "/index.html", `${how}: the page does not navigate by itself`);
      // Keys do nothing: the layer cannot be dismissed, and the focus stays on its button.
      await page.keyboard.press("Escape");
      await page.keyboard.press("Tab");
      assert.equal(await ended().count(), 1);
      assert.equal(await focusedName(page), "Sign in");
      // The terminal output stays readable behind the layer.
      assert.ok((await page.locator("#term").count()) === 1 && (await page.locator("#term").isVisible()));
      await page.getByRole("button", { name: "Sign in" }).click();
      await page.waitForURL((u) => u.pathname === "/" && u.search === "?ended=1");
      noOtherTraffic(server, `ended by ${how}`);
    }
  }

  // ---- the code 4403: offline, no layer, and nothing reconnects ---------------------------------------------------------
  {
    await openMain(page, url, [sessionRoute()]);
    await page.waitForFunction(() => window.__WS_MOCK__.sockets.length === 1);
    await page.evaluate(() => window.__WS_MOCK__.drop(4403));
    await page.waitForSelector('#status .web-mark[data-mark="offline"]');
    assert.deepEqual(await marksOf(page), ["offline"]);
    assert.equal(await page.getByRole("alertdialog").count(), 0, "no layer for a refused page");
    await page.waitForTimeout(1500);
    assert.equal(await page.evaluate(() => window.__WS_MOCK__.sockets.length), 1, "nothing reconnects");
    assert.deepEqual(await marksOf(page), ["offline"], "for good");
  }

  // ---- no route for the lookup: the page is as it was, with no mark and no error -----------------------------------------
  {
    const server = await openMain(page, url, []);
    await page.waitForFunction(() => window.__MOCK__.callsOf("get_layout").length >= 1);
    await page.waitForTimeout(300);
    assert.deepEqual(await marksOf(page), []);
    assert.equal(await webText(page), "");
    assert.equal(await errorText(page), "");
    assert.equal(await page.getByRole("dialog").count() + (await page.getByRole("alertdialog").count()), 0);
    assert.ok(server.unscripted.includes("GET /api/session"), "the page asked, got 404 and left it at that");
    assert.ok(!(await actionTitles(page)).includes("Unlock…"));
    assert.equal(await page.locator("#status .seg.web").count(), 0, "the strip is the one it always was");
  }

  // ---- a lookup that is not an answer leaves the page as it is -----------------------------------------------------------
  {
    await openMain(page, url, [{ path: "/api/session", status: 500, body: { code: "busy" } }]);
    await page.waitForTimeout(200);
    assert.deepEqual(await marksOf(page), []);
    await openMain(page, url, [{ path: "/api/session", body: { state: "locked", csrf: 7 } }]);
    await page.waitForTimeout(200);
    assert.deepEqual(await marksOf(page), [], "a shape that is not the documented one is not an answer");
  }

  assert.deepEqual(errors, [], "the page reported errors");
}
