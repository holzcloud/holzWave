// The session marks of the browser build (docs/design.md 6.30) and the unlock dialog (6.28) on the main page, against a scripted
// server: `GET /api/session` tells the page what it is, the status line shows what is wrong in a glyph and a word, and the
// dialog opens the saved passwords. Web backend only; the desktop has its own file (websession-tauri.test.mjs).
import assert from "node:assert/strict";
import { actionTitles, focusedName, holdsSecret, leftover, marksOf, openMain, sessionRoute, terminalHasFocus, webText } from "./websession-lib.mjs";

export const backends = ["ws"];
export const backendNote = "the marks and the dialogs exist on the web backend only: http() throws on the Tauri backend";

const PASSPHRASE = "correct horse";

export default async function ({ page, url, errors }) {
  const sessionLocked = sessionRoute({ state: "locked" });

  // ---- tracer: a locked session shows the mark, the dialog opens it, the mark goes ---------------------------------------
  {
    const server = await openMain(page, url, [
      sessionLocked,
      { method: "POST", path: "/auth/unlock", body: { state: "unlocked" }, delay: 400 },
    ]);
    await page.waitForSelector('#status .web-mark[data-mark="locked"]');
    const before = await webText(page);
    assert.deepEqual(await marksOf(page), ["secrets locked"]);
    assert.equal(await page.locator("#status button.web-act").textContent(), "Unlock");
    assert.ok((await actionTitles(page)).includes("Unlock…"), "the palette lists Unlock…");
    assert.equal(await page.locator("#status .seg.web").getAttribute("role"), "status");

    await page.locator("#status button.web-act").click();
    const dialog = page.getByRole("dialog", { name: "Your saved passwords are locked" });
    await dialog.waitFor();
    assert.equal(await focusedName(page), "Passphrase", "the field has the focus");
    assert.equal(await page.getByLabel("Passphrase").getAttribute("type"), "password");
    await page.keyboard.type(PASSPHRASE);
    await page.keyboard.press("Enter");
    // While the request is out: the label adds the dots, the box is busy, the passphrase already left the field.
    await page.waitForFunction(() => document.querySelector(".dlg.web-unlock")?.getAttribute("aria-busy") === "true");
    assert.equal(await page.getByRole("button", { name: "Unlock…" }).count(), 1);
    assert.equal(await page.getByLabel("Passphrase").inputValue(), "", "the field is empty before the answer arrives");
    assert.deepEqual(await holdsSecret(page, [PASSPHRASE]), [], "the passphrase is nowhere in the page while the request is out");

    await dialog.waitFor({ state: "detached" });
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
    const l = leftover(server);
    assert.deepEqual(l.foreign, [], "a request left the page's origin");
    assert.deepEqual(l.unscripted, [], "a request nobody scripted");
    console.log(`Tracer: POST /auth/unlock {"passphrase":"<${PASSPHRASE.length} characters>"}; the mark before: ${JSON.stringify(before)}, after: ${JSON.stringify(after)}`);
  }

  assert.deepEqual(errors, [], "the page reported errors");
}
