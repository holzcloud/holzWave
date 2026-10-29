// Full screen and the keyboard lock of the browser build (docs/design.md 6.30), and the web entry that holds them. The palette
// entry `Toggle full screen` exists on the web backend only; running it enters full screen from the person's gesture and, in a
// browser that has `navigator.keyboard.lock`, asks it to pass the keys a browser keeps (and Escape) to the page for as long as the
// page is in full screen. The lock is requested only after full screen was granted and released on every way out. The desktop
// neither lists the entry nor requests the module. The test runs on both backends and reads the backend from the page.
import assert from "node:assert/strict";
import { actionTitles } from "./websession-lib.mjs";
import { runAction } from "./account-lib.mjs";

const TITLE = "Toggle full screen";
const ACTION = "window.fullscreen";
const CODES = ["KeyW", "KeyT", "KeyN", "Tab", "PageUp", "PageDown", "Escape"];
const NOTICE = "Browser shortcuts are off in full screen. Hold Escape to leave.";

const notice = (page) => page.evaluate(() => document.querySelector("#status .notice")?.textContent ?? "");
const log = (page) => page.evaluate(() => window.__fs.log);
const count = async (page, kind) => (await log(page)).filter((l) => l[0] === kind).length;
const wait = (page, ms) => page.waitForTimeout(ms);

/** Open the page as the backend names its platform: `web` on the web backend, the macOS of the desktop on the other. */
async function open(page, url, os) {
  await page.addInitScript((i) => window.__MOCK__.on("app_info", () => i), { version: "0.0.1-alpha1", os, hostname: "woodbox" });
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
}

/**
 * Replace full screen and the keyboard lock with recording ones. `lock` is `grant`, `reject`, `pending` (the answer waits for
 * `window.__fs.release()`) or `none` (the browser has no `navigator.keyboard`); `fullscreen` is `grant` or `deny`. A lock entry
 * of the log holds the codes and whether the page was in full screen when it asked.
 */
const install = (page, { lock = "grant", fullscreen = "grant" } = {}) =>
  page.evaluate(
    ([lockMode, fsMode]) => {
      const fs = { log: [], element: null, release: null };
      window.__fs = fs;
      const fire = () => document.dispatchEvent(new Event("fullscreenchange"));
      Object.defineProperty(document, "fullscreenElement", { configurable: true, get: () => fs.element });
      Element.prototype.requestFullscreen = function () {
        fs.log.push(["request"]);
        if (fsMode === "deny") return Promise.reject(new TypeError("not allowed"));
        fs.element = this;
        return new Promise((done) => setTimeout(() => { fire(); done(); }, 0));
      };
      document.exitFullscreen = () => {
        fs.log.push(["exit"]);
        fs.element = null;
        return new Promise((done) => setTimeout(() => { fire(); done(); }, 0));
      };
      window.__leave = () => { fs.element = null; fire(); }; // the person holds Escape: the browser leaves by itself
      Object.defineProperty(navigator, "keyboard", {
        configurable: true,
        value:
          lockMode === "none"
            ? undefined
            : {
                lock: (codes) => {
                  fs.log.push(["lock", codes, fs.element !== null]);
                  if (lockMode === "reject") return Promise.reject(new Error("not allowed"));
                  if (lockMode === "pending") return new Promise((done) => { fs.release = done; });
                  return Promise.resolve();
                },
                unlock: () => void fs.log.push(["unlock"]),
              },
      });
    },
    [lock, fullscreen],
  );

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  const requests = [];
  page.on("request", (r) => {
    if (/\/web\.js(\?|$)/.test(r.url())) requests.push(r.url());
  });
  await page.goto(`${url}index.html?renderer=canvas`); // the init scripts run on a page that was navigated
  const backend = await page.evaluate(() => window.__HW_BACKEND__);
  if (backend !== "ws") {
    await desktopCase(page, url, requests);
  } else {
    await startCase(page, url, requests);
    await grantedCase(page, url);
    await exitCase(page, url);
    await twiceCase(page, url);
    await noLockCase(page, url);
    await rejectedCase(page, url);
    await deniedCase(page, url);
    await pendingCase(page, url);
  }
  assert.deepEqual(errors, [], "the page reported errors");
}

/** On the desktop the palette does not list the entry and the web module is never requested. */
async function desktopCase(page, url, requests) {
  requests.length = 0;
  await open(page, url, "macos");
  assert.ok(!(await actionTitles(page)).includes(TITLE), "the palette does not list the full screen entry on the desktop");
  assert.equal(await runAction(page, ACTION), false, "and the action does not exist");
  await wait(page, 300);
  assert.deepEqual(requests, [], "the desktop never requests web.js");
}

/** On the web the module is loaded once, its entry runs after the listeners and before the first layout, and the palette lists it. */
async function startCase(page, url, requests) {
  requests.length = 0;
  await page.addInitScript(() => {
    window.__order = [];
    window.__MOCK__.on("get_layout", async () => {
      const registry = await import(new URL("actions.js", location.href).href);
      window.__order.push(registry.listActions().some((a) => a.id === "window.fullscreen") ? "entry-before-layout" : "layout-first");
      return window.__MOCK__.view();
    });
  });
  await open(page, url, "web");
  assert.equal(requests.length, 1, "the web module is requested once");
  assert.ok((await actionTitles(page)).includes(TITLE), "the palette lists the full screen entry on the web");
  assert.equal((await page.evaluate(() => window.__order))[0], "entry-before-layout", "the web entry ran before the first layout");

  // The palette shows it, without a chord.
  await runAction(page, "palette.open");
  await page.waitForFunction(() => document.activeElement?.classList.contains("palette-input") === true);
  await page.keyboard.type("full screen");
  const row = await page.evaluate(() => {
    const r = [...document.querySelectorAll(".palette-row")].find((n) => n.querySelector(".palette-title")?.textContent === "Toggle full screen");
    return r ? { chord: r.querySelector(".palette-chord")?.textContent ?? null } : null;
  });
  assert.ok(row, "the palette row is there");
  assert.equal(row.chord, "", "it has no shortcut");
  await page.keyboard.press("Escape");
}

/** The lock is asked of the seven codes once, after full screen was granted, and the status line says so. */
async function grantedCase(page, url) {
  await open(page, url, "web");
  await install(page);
  assert.equal(await runAction(page, ACTION), true);
  await page.waitForFunction(() => window.__fs.log.some((l) => l[0] === "lock"));
  assert.deepEqual(await log(page), [["request"], ["lock", CODES, true]], "full screen first, then one lock with the seven codes");
  await page.waitForFunction((s) => document.querySelector("#status .notice")?.textContent === s, NOTICE);
  assert.equal(await notice(page), NOTICE);
  assert.equal(await count(page, "unlock"), 0, "nothing is released while the page is in full screen");
}

/** Leaving by the action and leaving by the browser (Escape held) both release the lock, once each. */
async function exitCase(page, url) {
  await open(page, url, "web");
  await install(page);
  await runAction(page, ACTION);
  await page.waitForFunction(() => window.__fs.log.some((l) => l[0] === "lock"));
  await runAction(page, ACTION); // the second run leaves
  await page.waitForFunction(() => window.__fs.log.some((l) => l[0] === "unlock"));
  await wait(page, 100);
  assert.deepEqual((await log(page)).map((l) => l[0]), ["request", "lock", "exit", "unlock"], "leaving released the lock once");

  await install(page);
  await runAction(page, ACTION);
  await page.waitForFunction(() => window.__fs.log.some((l) => l[0] === "lock"));
  await page.evaluate(() => window.__leave());
  await page.waitForFunction(() => window.__fs.log.some((l) => l[0] === "unlock"));
  await wait(page, 100);
  assert.deepEqual((await log(page)).map((l) => l[0]), ["request", "lock", "unlock"], "the browser's own exit released the lock once");
}

/** Running the action twice in a row leaves full screen and unlocks; a third run enters again and locks again. */
async function twiceCase(page, url) {
  await open(page, url, "web");
  await install(page);
  await runAction(page, ACTION);
  await page.waitForFunction(() => window.__fs.log.some((l) => l[0] === "lock"));
  await runAction(page, ACTION);
  await page.waitForFunction(() => window.__fs.log.some((l) => l[0] === "unlock"));
  assert.equal(await page.evaluate(() => document.fullscreenElement), null, "the page left full screen");
  await runAction(page, ACTION);
  await page.waitForFunction(() => window.__fs.log.filter((l) => l[0] === "lock").length === 2);
  assert.deepEqual((await log(page)).map((l) => l[0]), ["request", "lock", "exit", "unlock", "request", "lock"]);
}

/** A browser without a keyboard object: the action still toggles, nothing is locked, and the notice is not shown. */
async function noLockCase(page, url) {
  await open(page, url, "web");
  await install(page, { lock: "none" });
  await runAction(page, ACTION);
  await page.waitForFunction(() => document.fullscreenElement !== null);
  await wait(page, 300);
  assert.deepEqual(await log(page), [["request"]], "full screen was requested and nothing else");
  assert.equal(await notice(page), "", "nothing is said about locking");
  await runAction(page, ACTION);
  await page.waitForFunction(() => document.fullscreenElement === null);
  await wait(page, 100);
  assert.deepEqual((await log(page)).map((l) => l[0]), ["request", "exit"]);
}

/** A lock that the browser refuses says nothing; the page stays in full screen and leaves as it would. */
async function rejectedCase(page, url) {
  await open(page, url, "web");
  await install(page, { lock: "reject" });
  await runAction(page, ACTION);
  await page.waitForFunction(() => window.__fs.log.some((l) => l[0] === "lock"));
  await wait(page, 300);
  assert.equal(await notice(page), "", "a refused lock shows no notice");
  assert.notEqual(await page.evaluate(() => document.fullscreenElement), null, "the page is still in full screen");
  await page.evaluate(() => window.__leave());
  await wait(page, 100);
  assert.ok((await count(page, "lock")) === 1, "the lock was asked once");
}

/** Full screen that the browser refuses: the lock is never requested. */
async function deniedCase(page, url) {
  await open(page, url, "web");
  await install(page, { fullscreen: "deny" });
  await runAction(page, ACTION);
  await wait(page, 300);
  assert.deepEqual(await log(page), [["request"]], "no lock outside full screen");
  assert.equal(await notice(page), "");
}

/** The page leaves full screen while the lock is still on its way: a late grant is released again and says nothing. */
async function pendingCase(page, url) {
  await open(page, url, "web");
  await install(page, { lock: "pending" });
  await runAction(page, ACTION);
  await page.waitForFunction(() => window.__fs.log.some((l) => l[0] === "lock"));
  await page.evaluate(() => window.__leave());
  await page.waitForFunction(() => window.__fs.log.some((l) => l[0] === "unlock"));
  await page.evaluate(() => window.__fs.release());
  await wait(page, 300);
  const kinds = (await log(page)).map((l) => l[0]);
  assert.equal(kinds.at(-1), "unlock", "a lock granted after the exit is released again");
  assert.equal(await notice(page), "", "and nothing is said");
}
