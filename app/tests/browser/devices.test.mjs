// The device features of the web page (docs/design.md 6.35) against a scripted server: the Devices group of the Account dialog and
// the administrator's list in the People dialog (a second press to revoke, the list read again after every change, a refusal that
// leaves the row, hostile names as text), the group Device access (switches that show the server's answer and the operator's lock),
// the palette entry that asks for a typed code, and the whole set under forced colours, higher contrast, reduced motion and the
// two target sizes.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR } from "./lib.mjs";
import { actionTitles, openMain, sessionRoute } from "./websession-lib.mjs";
import { STEP, clean, confirmWith, dialog as accountDialog, fixture, layer, meRoute, openAccountDialog, runAction, statusLine as accountStatus, stepUpRoute } from "./account-lib.mjs";
import { dialog as peopleDialog, openPeople, people, statusLine as peopleStatus } from "./people.test.mjs";

export const backends = ["ws"];
export const backendNote = "the dialogs exist on the web backend only: http() throws on the Tauri backend";

const iso = (secondsAgo) => new Date(Date.now() - secondsAgo * 1000).toISOString();
const dev = (id, name, platform, seenAgo, over = {}) => ({
  id,
  name,
  platform,
  created_at: iso(86_400 * 9),
  last_seen: seenAgo === null ? null : iso(seenAgo),
  expires_at: iso(-86_400 * 80),
  ...over,
});
const OFF = { on: false, allowed: true };
const NOT_VALID = "That code is not valid or has expired.";
const HOSTILE = '<img src=x onerror="window.__pwned=1"> <b>MacBook</b>';

/** The rows of the person's list, as text, in order. */
const itemsOf = (page) =>
  page.evaluate(() => Array.from(document.querySelectorAll('[data-group="devices"] .dev-item')).map((li) => Array.from(li.children).map((c) => c.textContent).join(" | ")));
/** The rows of the administrator's table, as text, in order. */
const tableRows = (page) =>
  page.evaluate(() => Array.from(document.querySelectorAll('[data-group="devices"] .dev-row')).map((tr) => Array.from(tr.children).map((c) => c.textContent).join(" | ")));
/** The switches of the group Device access as a person reads them. */
const switchesOf = (page) =>
  page.evaluate(() =>
    Array.from(document.querySelectorAll('[data-group="device-access"] .dev-sw-row')).map((row) => {
      const b = row.querySelector("button");
      return {
        label: row.querySelector(".dev-sw-label")?.textContent,
        word: b?.querySelector(".vault-switch-word")?.textContent,
        checked: b?.getAttribute("aria-checked"),
        disabled: b?.getAttribute("aria-disabled"),
        locked: b?.dataset.locked ?? null,
        named: b?.getAttribute("aria-labelledby") === row.querySelector(".dev-sw-label")?.id,
        role: b?.getAttribute("role"),
      };
    }),
  );
const lines = (page) => page.evaluate(() => Array.from(document.querySelectorAll('[data-group="device-access"] .dev-dim, [data-group="device-access"] .dev-help')).map((p) => p.textContent));
const revokeButton = (page, name) => page.locator('[data-group="devices"] .dev-item, [data-group="devices"] .dev-row').filter({ hasText: name }).getByRole("button");

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  // ---- the Devices group of the Account dialog: its place, its rows, the second press -------------------------------------------------
  {
    const state = { me: fixture(), devices: [dev(7, "Holz MacBook", "macos", 30), dev(8, "Pixel 8", "android", 3 * 3600), dev(9, "Old tablet", "other", null)] };
    const gone = (id) => ({
      method: "DELETE",
      path: `/api/me/devices/${id}`,
      reply: () => {
        state.devices = state.devices.filter((d) => String(d.id) !== String(id));
        return { status: 204 };
      },
    });
    const server = await openAccountDialog(page, url, state, [gone(7), gone(8), gone(9)]);
    const group = page.locator('.account [data-group="devices"]');
    assert.equal(await group.count(), 1);
    assert.equal(await group.evaluate((g) => g.tagName), "FIELDSET");
    assert.equal(await group.getAttribute("role"), "group");
    assert.equal(await group.locator("legend").textContent(), "Devices");
    // It follows the group Sessions, after the others.
    assert.equal(
      await page.evaluate(() => {
        const s = document.querySelector('.account [data-group="sessions"]');
        const d = document.querySelector('.account [data-group="devices"]');
        return Boolean(s && d && s.compareDocumentPosition(d) & Node.DOCUMENT_POSITION_FOLLOWING);
      }),
      true,
      "the group comes after Sessions",
    );
    assert.deepEqual(await itemsOf(page), ["Holz MacBook | macOS · just now | Revoke", "Pixel 8 | Android · 3 h ago | Revoke", "Old tablet | Other · never | Revoke"]);
    assert.equal(server.count("GET", "/api/me/devices"), 1, "the list is read once when the dialog opens");
    // The first press asks and changes nothing; a click elsewhere takes the question back.
    const pixel = revokeButton(page, "Pixel 8");
    await pixel.click();
    assert.equal(await pixel.textContent(), "Click again to revoke");
    assert.equal(await pixel.getAttribute("data-armed"), "true", "an armed button has its own look as well as its words");
    assert.equal(server.count("DELETE", "/api/me/devices/8"), 0, "the first press sends nothing");
    await page.locator("#acc-title").click();
    assert.equal(await pixel.textContent(), "Revoke", "a click elsewhere cancels");
    await pixel.click();
    await revokeButton(page, "Holz MacBook").click();
    assert.equal(await pixel.textContent(), "Revoke", "arming another button disarms the first");
    assert.equal(server.count("DELETE", "/api/me/devices/8"), 0);
    // The second press does the work, and the list is read again: the page does not edit its own copy.
    await pixel.click();
    await pixel.click();
    await page.waitForFunction(() => document.querySelectorAll('[data-group="devices"] .dev-item').length === 2);
    assert.deepEqual(await itemsOf(page), ["Holz MacBook | macOS · just now | Revoke", "Old tablet | Other · never | Revoke"]);
    assert.equal(server.count("DELETE", "/api/me/devices/8"), 1);
    assert.equal(server.count("GET", "/api/me/devices"), 2, "the list was read again after the change");
    assert.equal(await accountStatus(page), "Saved.");
    assert.equal(server.requests("DELETE", "/api/me/devices/8")[0].headers["x-csrf-token"], "csrf-token-1", "the request carries the token of the session");
    assert.equal(server.count("POST", "/auth/step-up"), 0, "no layer while the sign-in is recent");
    // A device that someone else ended meanwhile is gone from the list after the next change: the page shows what the server holds.
    state.devices = state.devices.filter((d) => d.id !== 7);
    await revokeButton(page, "Old tablet").click();
    await revokeButton(page, "Old tablet").click();
    await page.waitForFunction(() => document.querySelector('[data-group="devices"] .dev-empty') !== null);
    assert.equal(await page.locator('[data-group="devices"] .dev-empty').textContent(), "No devices.");
    await clean(page, server, [], "the person's list");
  }

  // ---- a refusal leaves the row, and a change after an old sign-in asks who you are ---------------------------------------------------
  {
    const state = { me: fixture(), devices: [dev(7, "Holz MacBook", "macos", 30), dev(8, "Pixel 8", "android", 60)] };
    const server = await openAccountDialog(page, url, state, [
      { method: "DELETE", path: "/api/me/devices/7", status: 500, body: { code: "internal" } },
      {
        method: "DELETE",
        path: "/api/me/devices/8",
        reply: (request, n) => {
          if (n === 0) return STEP;
          state.devices = state.devices.filter((d) => d.id !== 8);
          return { status: 204 };
        },
      },
      stepUpRoute,
    ]);
    const holz = revokeButton(page, "Holz MacBook");
    await holz.click();
    await holz.click();
    await page.waitForFunction(() => document.querySelector(".account .set-status")?.textContent !== "");
    assert.equal(await accountStatus(page), "Could not reach the server. Try again.", "a server error says what the dialog says for one");
    assert.equal((await itemsOf(page)).length, 2, "a failed revoke leaves the row");
    assert.equal(await holz.textContent(), "Revoke", "and the button is disarmed again");
    const pixel = revokeButton(page, "Pixel 8");
    await pixel.click();
    await pixel.click();
    await confirmWith(page, "step up password");
    await page.waitForFunction(() => document.querySelectorAll('[data-group="devices"] .dev-item').length === 1);
    assert.equal(server.count("POST", "/auth/step-up"), 1, "the change after an old sign-in asked once");
    assert.equal(server.count("DELETE", "/api/me/devices/8"), 2, "and was repeated once");
    assert.equal(await layer(page).count(), 0);
  }
  {
    const state = { me: fixture(), devices: [dev(7, "Holz MacBook", "macos", 30)] };
    await openAccountDialog(page, url, state, [{ method: "DELETE", path: "/api/me/devices/7", status: 403, body: { code: "forbidden" } }]);
    const holz = revokeButton(page, "Holz MacBook");
    await holz.click();
    await holz.click();
    await page.waitForFunction(() => document.querySelector(".account .set-status")?.textContent !== "");
    assert.equal(await accountStatus(page), "That change was not made. Try again.", "a refused revoke says the generic change-failed sentence");
    assert.equal((await itemsOf(page)).length, 1, "and leaves the row");
  }

  // ---- hostile names are text, and a list that is not the documented shape draws nothing -----------------------------------------------
  {
    const evil = ["\u0007\u001b[31m" + "x".repeat(300), HOSTILE, "a‮b"];
    const state = { me: fixture(), devices: [dev(1, evil[0], "linux", 5), dev(2, evil[1], "macos", 5), dev(3, evil[2], "windows", 5)] };
    await openAccountDialog(page, url, state);
    const rows = await itemsOf(page);
    assert.equal(rows.length, 3);
    const first = rows[0].split(" | ")[0];
    assert.ok(first.length <= 64, `a long name is cut (${first.length})`);
    assert.ok(!/[\u0000-\u001f‮]/.test(rows.join("")), "no control or direction character is shown");
    assert.equal(rows[1].split(" | ")[0], HOSTILE, "markup is text, shown as it is");
    assert.equal(await page.locator(".account img, .account b").count(), 0, "no element was made from a name");
    assert.equal(await page.evaluate(() => window.__pwned), undefined, "no script ran");
  }
  {
    // More than a person can have: the list is refused as a whole, and the dialog says it cannot read it.
    const state = { me: fixture(), devices: Array.from({ length: 65 }, (_, i) => dev(i + 1, `d${i}`, "linux", 5)) };
    await openAccountDialog(page, url, state);
    assert.equal(await page.locator('[data-group="devices"] .dev-item').count(), 0, "nothing is drawn from a list the page does not take");
    assert.equal(await accountStatus(page), "Could not reach the server. Try again.");
  }

  // ---- the People dialog: everybody's devices with the owner, a revoke through the step-up rule -------------------------------------------
  {
    const state = {
      ...people(),
      devices: [
        { ...dev(21, "Holz MacBook", "macos", 20), username: "holz", display_name: "Holz" },
        { ...dev(22, "Studio PC", "windows", 2 * 86_400), username: "ben", display_name: "Ben Ito" },
        { ...dev(23, "Pixel 8", "android", 3 * 3600), username: "ben", display_name: "Ben Ito" },
      ],
      access: { sync: OFF, remote: OFF },
    };
    const server = await openPeople(page, url, state, [
      {
        method: "DELETE",
        path: "/api/admin/devices/22",
        reply: (request, n) => {
          if (n === 0) return STEP;
          state.devices = state.devices.filter((d) => d.id !== 22);
          return { status: 204 };
        },
      },
      stepUpRoute,
    ]);
    await page.waitForSelector('.people [data-group="devices"] .dev-row');
    const heads = await page.evaluate(() => Array.from(document.querySelectorAll('.people [data-group="devices"] thead th')).map((th) => th.textContent));
    assert.deepEqual(heads, ["Name", "Owner", "Platform", "Last seen", ""]);
    assert.deepEqual(await tableRows(page), ["Holz MacBook | holz | macOS | just now | Revoke", "Pixel 8 | ben | Android | 3 h ago | Revoke", "Studio PC | ben | Windows | 2 days ago | Revoke"]);
    assert.equal(await page.locator('.people [data-group="devices"] thead th').first().getAttribute("scope"), "col");
    // The groups come below the table of people and its note, the devices first and the switches after them.
    assert.deepEqual(
      await page.evaluate(() => Array.from(document.querySelectorAll(".people .ppl-body > *")).map((n) => n.className.split(" ")[0] + (n.dataset.group ? `:${n.dataset.group}` : ""))),
      ["ppl-table", "ppl-note", "set-group:devices", "set-group:device-access"],
    );
    assert.equal(server.count("GET", "/api/admin/devices"), 1);
    assert.equal(server.count("GET", "/api/admin/device-access"), 1, "the state of the switches is read each time the dialog opens");
    const studio = revokeButton(page, "Studio PC");
    await studio.click();
    assert.equal(await studio.textContent(), "Click again to revoke");
    assert.equal(server.count("DELETE", "/api/admin/devices/22"), 0);
    await studio.click();
    await confirmWith(page, "step up password");
    await page.waitForFunction(() => document.querySelectorAll('.people [data-group="devices"] .dev-row').length === 2);
    assert.deepEqual(
      (await tableRows(page)).map((r) => r.split(" | ")[0]),
      ["Holz MacBook", "Pixel 8"],
    );
    assert.equal(server.count("DELETE", "/api/admin/devices/22"), 2, "the revoke was repeated once after the confirmation");
    assert.equal(server.count("GET", "/api/admin/devices"), 2, "the list was read again");
    assert.equal(await peopleStatus(page), "", "the list says nothing of its own change");
    await page.keyboard.press("Escape");
    await peopleDialog(page).waitFor({ state: "detached" });
  }

  // ---- the group Device access: two switches that show what the server answers ------------------------------------------------------------
  {
    const state = { ...people(), devices: [], access: { sync: { on: false, allowed: true }, remote: { on: false, allowed: false } } };
    const server = await openPeople(page, url, state, [
      {
        method: "POST",
        path: "/api/admin/device-access",
        reply: (request, n) => {
          if (n === 0) return STEP;
          const body = request.json;
          if (state.deny) return { status: 409, body: { code: "conflict", field: Object.keys(body)[0] } };
          if (state.fail) return { status: 400, body: { code: "bad_request" } };
          for (const [k, v] of Object.entries(body)) state.access[k] = { ...state.access[k], on: v };
          return { status: 200, body: structuredClone(state.access) };
        },
      },
      stepUpRoute,
    ]);
    const group = page.locator('.people [data-group="device-access"]');
    await group.waitFor();
    assert.equal(await group.locator("legend").textContent(), "Device access");
    assert.equal(await group.getAttribute("role"), "group");
    assert.deepEqual(await switchesOf(page), [
      { label: "Sync target", word: "OFF", checked: "false", disabled: null, locked: null, named: true, role: "switch" },
      { label: "Remote sessions", word: "OFF", checked: "false", disabled: "true", locked: "true", named: true, role: "switch" },
    ]);
    assert.deepEqual(await lines(page), ["Turned off in the server configuration.", "Off by default. Turning one off ends the matching connections; devices stay signed in."]);
    assert.equal(await page.getByRole("switch", { name: "Remote sessions" }).getAttribute("aria-describedby") !== null, true, "the lock has its reason as a description");
    // A switch that the configuration closed cannot be operated: no request.
    await page.getByRole("switch", { name: "Remote sessions" }).click({ force: true });
    assert.equal(server.count("POST", "/api/admin/device-access"), 0, "a locked switch sends nothing");
    assert.equal((await switchesOf(page))[1].word, "OFF");
    // The first press after an old sign-in asks who you are, posts once more after it, and draws the answer of the server.
    const sync = page.getByRole("switch", { name: "Sync target" });
    await sync.click();
    await confirmWith(page, "step up password");
    await page.waitForFunction(() => document.querySelector('[data-group="device-access"] [aria-checked="true"]') !== null);
    assert.equal((await switchesOf(page))[0].word, "ON");
    assert.equal((await switchesOf(page))[0].checked, "true");
    assert.deepEqual(server.requests("POST", "/api/admin/device-access").map((r) => r.json), [{ sync: true }, { sync: true }], "one switch in the body, and the change was repeated once");
    // Turning it off again, from the keyboard: nothing is drawn but the answer, and the focus stays on the switch.
    await sync.focus();
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.querySelector('[data-group="device-access"] [aria-checked="true"]') === null);
    assert.equal(await page.evaluate(() => document.activeElement?.dataset.switch), "sync", "the focus stays on the switch");
    assert.equal((await switchesOf(page))[0].word, "OFF");
    // A refusal puts nothing on screen that the server did not say, and the dialog says the generic sentence.
    state.fail = true;
    await sync.click();
    await page.waitForFunction(() => document.querySelector(".people .set-status")?.textContent !== "");
    assert.equal(await peopleStatus(page), "That change was not made. Try again.");
    assert.equal((await switchesOf(page))[0].word, "OFF", "the switch is where the server has it");
    // The configuration may have closed the switch in the meantime: a 409 reads the state again and draws the lock.
    state.fail = false;
    state.deny = true;
    state.access.sync.allowed = false;
    await sync.click();
    await page.waitForFunction(() => document.querySelector('[data-group="device-access"] [data-locked="true"]') !== null && document.querySelectorAll('[data-group="device-access"] [data-locked="true"]').length === 2);
    assert.deepEqual((await switchesOf(page)).map((s) => s.locked), ["true", "true"]);
    assert.equal(server.count("GET", "/api/admin/device-access"), 2, "the state was read again after the refusal");
  }

  // ---- a member never sees the switches or the administrator's list --------------------------------------------------------------------------
  {
    const state = { me: fixture({ account: { id: "acc2", username: "ben", display_name: "Ben", role: "member" } }), devices: [dev(7, "Holz MacBook", "macos", 30)] };
    const server = await openMain(page, url, [{ path: "/api/session", body: { ...sessionRoute().body, account: { id: "acc2", username: "ben", display_name: "Ben", role: "member" } } }, meRoute(state), { path: "/api/me/devices", body: () => ({ devices: state.devices }) }]);
    await runAction(page, "account.open");
    await accountDialog(page).waitFor();
    await page.waitForSelector('[data-group="devices"] .dev-item');
    assert.equal(await page.locator('[data-group="device-access"]').count(), 0, "a member has no Device access group");
    assert.ok(!(await actionTitles(page)).includes("People…"));
    assert.equal(server.count("GET", "/api/admin/devices"), 0);
    assert.equal(server.count("GET", "/api/admin/device-access"), 0);
  }

  // ---- the palette entry that asks for a typed code (web build only) ---------------------------------------------------------------------------
  {
    const state = { me: fixture(), devices: [] };
    const looked = [];
    const server = await openMain(page, url, [
      sessionRoute(),
      meRoute(state),
      { method: "POST", path: "/api/device/lookup", reply: (request) => { looked.push(request.json); return { status: 200, body: { name: "Holz MacBook", platform: "macos", addr: "203.0.113.7", created_at: iso(10) } }; } },
      { method: "POST", path: "/api/device/approve", status: 204 },
    ]);
    await page.waitForFunction(async () => (await import(new URL("actions.js", location.href).href)).listActions().some((a) => a.id === "device.authorize"));
    assert.ok((await actionTitles(page)).includes("Authorize device…"), "the palette lists Authorize device…");
    await runAction(page, "device.authorize");
    const small = page.getByRole("dialog", { name: "Authorize device" });
    await small.waitFor();
    assert.equal(await small.getAttribute("aria-modal"), "true");
    assert.equal(await page.getByLabel("Code", { exact: true }).evaluate((i) => i === document.activeElement), true, "the field has the focus");
    assert.deepEqual(await small.getByRole("button").evaluateAll((b) => b.map((x) => x.textContent)), ["Cancel", "Look up"]);
    // A code that is not well formed is refused here, before any request, with the one sentence.
    for (const bad of ["", "BCDF-GHJ", "BCDF GHJK", "ABCD-EFGH", "BCDF-GHJK1", "<b>"]) {
      await page.getByLabel("Code", { exact: true }).fill(bad);
      await small.getByRole("button", { name: "Look up" }).click();
      assert.equal(await page.locator(".device-code .web-err").textContent(), NOT_VALID, JSON.stringify(bad));
      assert.equal(await page.getByLabel("Code", { exact: true }).getAttribute("aria-invalid"), "true");
    }
    assert.equal(server.count("POST", "/api/device/lookup"), 0, "nothing was requested for a malformed code");
    // Typing again takes the sentence away; a well formed code (any case, the dash optional) opens the same dialog.
    await page.getByLabel("Code", { exact: true }).fill("bcdfghjk");
    assert.equal(await page.locator(".device-code .web-err").textContent(), "");
    await page.keyboard.press("Enter");
    await page.waitForSelector(".device-authorize .dev-kv");
    assert.deepEqual(looked, [{ code: "BCDF-GHJK" }], "the code is sent in its normal form");
    assert.equal(await page.locator(".device-authorize .dev-code").textContent(), "BCDF-GHJK");
    assert.equal(await page.locator(".device-code").count(), 0, "the small layer is gone");
    assert.equal(await page.getByRole("dialog").count(), 1, "one dialog at a time");
    await page.getByRole("button", { name: "Approve" }).click();
    await page.waitForFunction(() => document.querySelector(".dev-status")?.textContent.includes("Device approved."));
    assert.equal(await page.locator(".device-authorize .dev-glyph").textContent(), "●");
    await page.keyboard.press("Escape");
    await page.getByRole("dialog").waitFor({ state: "detached" });
    // Cancel leaves with nothing sent; Escape does the same.
    await runAction(page, "device.authorize");
    await small.waitFor();
    await small.getByRole("button", { name: "Cancel" }).click();
    await small.waitFor({ state: "detached" });
    assert.equal(server.count("POST", "/api/device/lookup"), 1);
    await page.waitForFunction(() => document.activeElement?.id === "ime");
    // The code is a request for approval: it was kept nowhere.
    assert.deepEqual(
      await page.evaluate(() => [location.href, JSON.stringify([Object.entries(localStorage), Object.entries(sessionStorage)]), document.cookie].filter((t) => /BCDF/i.test(t))),
      [],
      "the code is not in the address, the storage or a cookie",
    );
  }

  // ---- the whole set under forced colours, higher contrast, reduced motion and on both pointers ------------------------------------------------------
  {
    const state = { ...people(), devices: [{ ...dev(21, "Holz MacBook", "macos", 20), username: "holz", display_name: "Holz" }], access: { sync: { on: true, allowed: true }, remote: { on: false, allowed: false } } };
    await openPeople(page, url, state);
    await page.waitForSelector('.people [data-group="device-access"] .dev-switch');
    // A change of a media feature or of the root class is drawn by the next frame: wait for two before reading styles.
    const probe = () =>
      page.evaluate(async () => {
        await new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
        const out = {};
        const pick = (name, el) => {
          const c = getComputedStyle(el);
          const r = el.getBoundingClientRect();
          out[name] = {
            border: parseFloat(c.borderTopWidth),
            style: c.borderTopStyle,
            colour: c.borderTopColor,
            height: r.height,
            text: el.textContent.trim(),
            transition: c.transitionDuration,
            animation: c.animationName,
          };
        };
        pick("revoke", document.querySelector(".people .dev-revoke"));
        const switches = document.querySelectorAll(".people .dev-switch");
        pick("on", switches[0]);
        pick("locked", switches[1]);
        pick("knob", switches[0].querySelector("i"));
        return out;
      });
    const plain = await probe();
    assert.ok(plain.revoke.height >= 27.99, `Revoke is at least 28 px high (${plain.revoke.height})`);
    assert.ok(plain.on.height >= 27.99 && plain.locked.height >= 27.99, `the switches are at least 28 px high (${plain.on.height})`);
    assert.equal(plain.on.text, "ON");
    assert.equal(plain.locked.text, "OFF");
    assert.equal(plain.locked.style, "dashed", "a switch closed by the configuration has a dashed outline besides its word");
    assert.notEqual(plain.on.colour, plain.locked.colour, "the two states differ in colour too, but never in colour alone");
    // A UI scale under 100 % makes the root font smaller; the targets keep their 28 px floor.
    await page.evaluate(() => document.documentElement.style.setProperty("font-size", "9px"));
    const small = await probe();
    await page.evaluate(() => document.documentElement.style.removeProperty("font-size"));
    assert.ok(small.revoke.height >= 27.99, `Revoke stays 28 px high at 75 % (${small.revoke.height})`);
    assert.ok(small.on.height >= 27.99 && small.locked.height >= 27.99, `the switches stay 28 px high at 75 % (${small.on.height})`);

    await page.emulateMedia({ forcedColors: "active" });
    const forced = await probe();
    for (const name of ["revoke", "on", "locked"]) {
      assert.ok(forced[name].border >= 1 && forced[name].style !== "none", `${name} keeps its border under forced colours`);
      assert.ok(forced[name].text !== "", `${name} keeps its word`);
    }
    assert.equal(forced.locked.style, "dashed", "the dashes stay under forced colours");
    assert.ok(forced.on.border >= 1, "the knob position and the word still tell ON from OFF");
    assert.equal(await page.locator(".people .dev-switch").first().evaluate((b) => b.querySelector("i") !== null && b.dataset.on), "true");
    await page.emulateMedia({ forcedColors: "none" });

    // Higher contrast: the page's class swaps the hairline and the dim text for the stronger tokens; the controls follow them.
    await page.emulateMedia({ contrast: "more" });
    await page.waitForFunction(() => document.documentElement.classList.contains("contrast-high"));
    const high = await probe();
    assert.notEqual(high.revoke.colour, plain.revoke.colour, `the hairline of a control follows the stronger token (${plain.revoke.colour} / ${high.revoke.colour})`);
    assert.equal(high.revoke.text, plain.revoke.text);
    await page.emulateMedia({ contrast: "no-preference" });
    await page.waitForFunction(() => !document.documentElement.classList.contains("contrast-high"));

    await page.emulateMedia({ reducedMotion: "reduce" });
    const still = await probe();
    for (const name of ["revoke", "on", "locked", "knob"]) {
      assert.ok(/^0s(, 0s)*$/.test(still[name].transition), `${name} has no transition (${still[name].transition})`);
      assert.equal(still[name].animation, "none", `${name} has no animation`);
    }
    await page.emulateMedia({ reducedMotion: "no-preference" });

    // The focus ring of the keyboard is 2 px, and the order of Tab is the order on screen.
    await page.locator(".people .dev-revoke").focus();
    await page.keyboard.press("Shift+Tab");
    await page.keyboard.press("Tab");
    assert.equal(await page.evaluate(() => getComputedStyle(document.activeElement).outlineWidth), "2px", "Revoke has a 2 px focus ring");
    const order = [];
    for (let i = 0; i < 2; i++) {
      await page.keyboard.press("Tab");
      order.push(await page.evaluate(() => {
        const a = document.activeElement;
        const c = getComputedStyle(a);
        return { id: a.getAttribute("role") === "switch" ? `switch:${a.dataset.switch}` : a.textContent.trim(), ring: c.outlineWidth, style: c.outlineStyle };
      }));
    }
    assert.deepEqual(order.map((o) => o.id), ["switch:sync", "switch:remote"], "Tab goes down the group, in the order it is drawn");
    for (const o of order) {
      assert.equal(o.ring, "2px", `${o.id} has a 2 px focus ring`);
      assert.notEqual(o.style, "none");
    }

    // A coarse pointer asks for 44 px: the sheet says so for each control (a pointer cannot be switched on from the page).
    const css = readFileSync(join(APP_DIR, "dist", "devices.css"), "utf8");
    const coarse = css.slice(css.indexOf("@media (pointer: coarse)"), css.indexOf("@media (forced-colors"));
    for (const selector of [".dev-revoke", ".dev-switch", "device-authorize button", "device-code button", ".dev-input"]) {
      assert.ok(coarse.includes(selector), `the coarse-pointer rule covers ${selector}`);
    }
    assert.match(coarse, /min-height: max\(44px, calc\(44rem \/ 12\)\)/);
    // The desktop floor of the two dialogs' buttons is in pixels as well, so a small UI scale cannot take them under 28 px.
    assert.match(css, /\.dlg\.device-authorize button, \.dlg\.device-code button \{ min-height: max\(28px, calc\(28rem \/ 12\)\); \}/);
    await page.keyboard.press("Escape");
  }
  assert.deepEqual(errors, [], "the page reported errors");
}
