// The tab of the browser build calls the person back while the page is hidden: the title starts with the glyphs of the bar's
// marks in their fixed order and the icon gets the rose dot; both are put back the moment the tab is visible again and are
// never touched while it is. Neither ever holds a pane's title, a directory or a command. Notifications are opt-in: the Settings
// dialog of the browser build has the group Notifications, the browser is asked only when `Turn on` is pressed, the choice is one
// flag in local storage, and a notification (words only, one per kind in five seconds) is made only while the page is hidden.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { APP_DIR, callsOf, emit, waitForListeners } from "./lib.mjs";

export const backends = ["ws"];
export const backendNote = "the tab marks and the notifications exist only in the browser build";

const DIST = fileURLToPath(new URL("../../dist/", import.meta.url));
const BASE = "holzWave";
const PLAIN = "favicon.svg";
const ATTENTION = "favicon-attention.svg";

/** Open the page as the web build. */
async function open(page, url) {
  await page.addInitScript(() => window.__MOCK__.on("app_info", () => ({ os: "web", version: "t", hostname: "h" })));
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  // The scene listens to each of these once; the notice module is the second listener.
  await waitForListeners(page, "bell", 2);
  await waitForListeners(page, "command-end", 2);
}

/** Make the page hidden or visible the way a browser does: the two properties, then the event. */
const setHidden = (page, hidden) =>
  page.evaluate((h) => {
    Object.defineProperty(document, "hidden", { configurable: true, get: () => h });
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => (h ? "hidden" : "visible") });
    document.dispatchEvent(new Event("visibilitychange"));
  }, hidden);

const title = (page) => page.evaluate(() => document.title);
const icons = (page) =>
  page.evaluate(() => [...document.querySelectorAll('link[rel~="icon"]')].map((l) => new URL(l.href).pathname.split("/").pop()));
const iconNow = async (page) => (await icons(page)).at(-1);
const settle = (page) =>
  page.evaluate(async () => {
    for (let i = 0; i < 3; i++) await new Promise((done) => requestAnimationFrame(() => done()));
  });

/** Provide a waiting question for `panes` as a chat or a `wsh ai` request would. */
const provide = (page, panes) =>
  page.evaluate(async (p) => {
    const { attention } = await import(new URL("attentionstate.js", location.href).href);
    window.__asking = p;
    window.__off ??= attention.provide("test", () => window.__asking);
    attention.changed();
  }, panes);
const unprovide = (page) =>
  page.evaluate(async () => {
    const { attention } = await import(new URL("attentionstate.js", location.href).href);
    window.__off?.();
    window.__off = undefined;
    attention.changed();
  });

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(8000);
  await filesCase(page, url);
  await visibleCase(page, url);
  await hiddenCase(page, url);
  await secretCase(page, url);
  await onceCase(page, url);
  await groupCase(page, url);
  await turnOnCase(page, url);
  await blockedCase(page, url);
  await noApiCase(page, url);
  await desktopCase(page, url);
  await keyboardCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** The page file has no icon link; the page adds one, and both icon files answer from the server as SVG. */
async function filesCase(page, url) {
  for (const name of ["index.html", "login.html"]) {
    assert.ok(!/rel="[^"]*icon/.test(readFileSync(`${DIST}${name}`, "utf8")), `${name} has no icon link of its own`);
  }
  for (const file of [PLAIN, ATTENTION]) {
    const res = await page.request.get(`${url}${file}`);
    assert.equal(res.status(), 200, file);
    assert.match(res.headers()["content-type"] ?? "", /^image\/svg\+xml/, `${file} answers as SVG`);
    assert.match(await res.text(), /<svg\b/);
  }
  await open(page, url);
  assert.deepEqual(await icons(page), [PLAIN], "the page adds the plain icon, once");
  assert.equal(await title(page), BASE);
}

/** While the tab is visible nothing changes, whatever happens in the panes. */
async function visibleCase(page, url) {
  await open(page, url);
  await emit(page, "bell", { pane: 1 });
  await emit(page, "command-end", { pane: 2, code: 1 });
  await provide(page, [1]);
  await settle(page);
  assert.equal(await title(page), BASE, "a visible page keeps its title");
  assert.equal(await iconNow(page), PLAIN, "and its icon");
  await unprovide(page);
}

/** Hidden: a bell, a failure, a finish and a question add their glyphs in the fixed order; visible again puts everything back. */
async function hiddenCase(page, url) {
  await open(page, url);
  await setHidden(page, true);
  assert.equal(await title(page), BASE, "hidden with nothing happened: the title is the base");
  assert.equal(await iconNow(page), PLAIN);

  await emit(page, "bell", { pane: 1 });
  await page.waitForFunction(() => document.title === "▲ holzWave");
  assert.equal(await iconNow(page), ATTENTION, "a mark brings the dot");

  await emit(page, "bell", { pane: 2 });
  await settle(page);
  assert.equal(await title(page), "▲ holzWave", "a repeat of a kind changes nothing");

  await emit(page, "command-end", { pane: 2, code: 1 });
  await page.waitForFunction(() => document.title === "▲ ! holzWave");
  await emit(page, "command-end", { pane: 1, code: 0 });
  await page.waitForFunction(() => document.title === "▲ ! ✓ holzWave");
  await emit(page, "command-end", { pane: 1, code: null });
  await settle(page);
  assert.equal(await title(page), "▲ ! ✓ holzWave", "no status counts as finished, once");

  await provide(page, [2]);
  await page.waitForFunction(() => document.title === "? ▲ ! ✓ holzWave");
  assert.equal(await iconNow(page), ATTENTION);

  // Back in view: the title and the icon return at once, with no frame in between.
  await setHidden(page, false);
  assert.equal(await title(page), BASE, "restored in the same turn as the event");
  assert.equal(await iconNow(page), PLAIN);
  await emit(page, "bell", { pane: 1 });
  await settle(page);
  assert.equal(await title(page), BASE, "visible again: a bell changes nothing");

  // Hidden again with the question still waiting: it is there at once; the earlier kinds are not.
  await setHidden(page, true);
  assert.equal(await title(page), "? holzWave", "the question is a level, the rest began again");
  await emit(page, "bell", { pane: 1 });
  await page.waitForFunction(() => document.title === "? ▲ holzWave");
  // The question is answered while hidden: its glyph goes.
  await unprovide(page);
  await page.waitForFunction(() => document.title === "▲ holzWave");
  await setHidden(page, false);
  await setHidden(page, true);
  assert.equal(await title(page), BASE, "a later hidden bell starts from nothing");
  await emit(page, "bell", { pane: 1 });
  await page.waitForFunction(() => document.title === "▲ holzWave");
  await setHidden(page, false);
  assert.equal(await title(page), BASE);
}

/** Nothing a pane wrote reaches the title: not its title, its directory, its command or its output. */
async function secretCase(page, url) {
  await open(page, url);
  const secret = "zq-secret-7f3a91";
  await emit(page, "title", { pane: 1, title: `vim ${secret}` });
  await emit(page, "cwd", { pane: 1, cwd: `/home/u/${secret}` });
  await setHidden(page, true);
  await emit(page, "bell", { pane: 1, title: secret, text: secret });
  await emit(page, "command-end", { pane: 1, code: 2, command: `rm -rf ${secret}`, output: secret, title: secret });
  await page.waitForFunction(() => document.title === "▲ ! holzWave");
  const seen = await page.evaluate(() => ({
    title: document.title,
    links: [...document.querySelectorAll("link")].map((l) => l.outerHTML),
  }));
  assert.ok(!seen.title.includes(secret), "the title holds nothing a pane wrote");
  assert.ok(!seen.links.join("").includes(secret));
  await setHidden(page, false);
  assert.equal((await callsOf(page, "open_url")).length, 0);
}

/** However often the tab is switched, there is one icon link. */
async function onceCase(page, url) {
  await open(page, url);
  for (let i = 0; i < 5; i++) {
    await setHidden(page, true);
    await emit(page, "bell", { pane: 1 });
    await page.waitForFunction(() => document.title === "▲ holzWave");
    await setHidden(page, false);
    assert.equal((await icons(page)).length, 1, `round ${i}: one icon link`);
  }
  assert.deepEqual(await icons(page), [PLAIN]);
}

// ---- notifications ----------------------------------------------------------------------------------------------

const golden = JSON.parse(readFileSync(`${APP_DIR}tests/fixtures/protocol.json`, "utf8"));
const FLAG = "holzwave.notify";

/**
 * Open the page as the web build (or `os`) with a recording `Notification`: `permission` is its first answer, `answer` is what
 * `requestPermission` gives. `window.__asked` counts requests and `window.__made` records `[title, options]` of every notification.
 */
async function openWith(page, url, { permission = "default", answer = "granted", api = true, os = "web", flag = null } = {}) {
  await page.addInitScript(
    ([perm, ans, has, o, f, settings]) => {
      window.__MOCK__.on("app_info", () => ({ os: o, version: "t", hostname: "h" }));
      window.__MOCK__.on("get_settings", () => structuredClone(settings));
      window.__asked = 0;
      window.__made = [];
      if (!has) {
        Object.defineProperty(window, "Notification", { configurable: true, writable: true, value: undefined });
      } else {
        let current = perm;
        const Fake = class {
          static get permission() {
            return current;
          }
          static async requestPermission() {
            window.__asked++;
            current = ans;
            return ans;
          }
          constructor(title, options) {
            window.__made.push([title, options ?? null]);
          }
          close() {}
        };
        Object.defineProperty(window, "Notification", { configurable: true, writable: true, value: Fake });
      }
      localStorage.clear();
      if (f !== null) localStorage.setItem("holzwave.notify", f);
    },
    [permission, answer, api, os, flag, golden.settings_view],
  );
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  if (os === "web") {
    await waitForListeners(page, "bell", 2);
    await waitForListeners(page, "command-end", 2);
  }
}

const settingsDialog = (page) => page.getByRole("dialog", { name: "Settings" });
const group = (page) => page.locator("fieldset.set-notify");
const word = (page) => page.locator(".set-notify-state").first().textContent();
const buttons = (page) => page.locator(".set-notify-buttons button").evaluateAll((bs) => bs.map((b) => b.textContent));
const made = (page) => page.evaluate(() => window.__made.map((m) => [...m]));
const asked = (page) => page.evaluate(() => window.__asked);
const storage = (page) => page.evaluate(() => Object.fromEntries(Object.keys(localStorage).map((k) => [k, localStorage.getItem(k)])));
async function openSettings(page) {
  await page.evaluate(async () => (await import(new URL("actions.js", location.href).href)).runAction("settings.open"));
  await settingsDialog(page).waitFor();
}

/** The group stands after Accessibility, before the buttons; it asks nothing of the browser and stores nothing by being shown. */
async function groupCase(page, url) {
  await openWith(page, url);
  await openSettings(page);
  await group(page).waitFor();
  assert.equal(await group(page).count(), 1);
  const shape = await page.evaluate(() => {
    const sets = [...document.querySelectorAll(".dlg.settings .set-group")];
    const mine = document.querySelector("fieldset.set-notify");
    return {
      legends: sets.map((s) => s.querySelector("legend")?.textContent),
      role: mine.getAttribute("role"),
      label: document.getElementById(mine.getAttribute("aria-labelledby"))?.textContent,
      beforeActions: (mine.compareDocumentPosition(document.querySelector(".dlg.settings .dlg-actions")) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0,
    };
  });
  assert.deepEqual(shape.legends.slice(-2), ["Accessibility", "Notifications"], "after Accessibility");
  assert.equal(shape.role, "group");
  assert.equal(shape.label, "Notifications");
  assert.equal(shape.beforeActions, true, "before the buttons of the dialog");
  assert.equal(await page.locator(".set-notify-name").textContent(), "Notify me");
  assert.equal(await page.locator(".set-notify-meta").textContent(), "bell and attention");
  assert.equal(await word(page), "off", "off is the default");
  assert.equal(await page.locator(".set-notify-row").getAttribute("data-tone"), "plain");
  assert.equal(
    await page.locator(".set-notify-advice").textContent(),
    "Bell and attention marks also show in the tab title. Turn this on to get a browser notification as well.",
  );
  assert.deepEqual(await buttons(page), ["Turn on"]);
  assert.equal(await asked(page), 0, "the browser was not asked by showing the group");
  assert.deepEqual(await storage(page), {}, "and nothing is stored");

  // Off: a hidden bell makes no notification.
  await setHidden(page, true);
  await emit(page, "bell", { pane: 1 });
  await page.waitForFunction(() => document.title === "\u25B2 holzWave");
  assert.deepEqual(await made(page), [], "off: the title changes, no notification is made");
  assert.equal(await asked(page), 0);
}

/** `Turn on` asks the browser once; on a grant the state is on, one flag is stored, and the notifications follow the rule. */
async function turnOnCase(page, url) {
  await openWith(page, url);
  await openSettings(page);
  await group(page).waitFor();
  await page.getByRole("button", { name: "Turn on" }).click();
  await page.waitForFunction(() => document.querySelector(".set-notify-state")?.textContent === "on");
  assert.equal(await asked(page), 1, "one request, from the press");
  assert.deepEqual(await buttons(page), ["Turn off"]);
  assert.equal(await page.locator(".set-notify-row").getAttribute("data-tone"), "ok");
  assert.equal(
    await page.locator(".set-notify-advice").textContent(),
    "A browser notification appears when a pane out of view rings the bell or needs you. It never contains terminal text.",
  );
  assert.deepEqual(await storage(page), { [FLAG]: "1" }, "the one flag and nothing else");

  // Visible: none.
  await emit(page, "bell", { pane: 1 });
  await settle(page);
  assert.deepEqual(await made(page), [], "a visible page makes none");

  // Hidden: words only, one per kind in five seconds.
  const secret = "zq-secret-4c18e0";
  await emit(page, "title", { pane: 1, title: `vim ${secret}` });
  await setHidden(page, true);
  await emit(page, "bell", { pane: 1, title: secret, text: secret });
  await page.waitForFunction(() => window.__made.length === 1);
  await emit(page, "bell", { pane: 2 });
  await emit(page, "bell", { pane: 1 });
  await settle(page);
  assert.equal((await made(page)).length, 1, "a second bell within five seconds makes none");
  await emit(page, "command-end", { pane: 2, code: 1, command: `rm ${secret}`, output: secret });
  await page.waitForFunction(() => window.__made.length === 2);
  await emit(page, "command-end", { pane: 1, code: 0 });
  await page.waitForFunction(() => window.__made.length === 3);
  await provide(page, [2]);
  await page.waitForFunction(() => window.__made.length === 4);
  const all = await made(page);
  assert.deepEqual(all.map((m) => m[0]), ["Rang the bell", "Command failed", "Command finished", "Waits for you"]);
  for (const [, options] of all) {
    assert.equal(options?.silent, true, "no sound");
    assert.ok(options?.body === undefined, "no body");
  }
  assert.ok(!JSON.stringify(all).includes(secret), "nothing a pane wrote is in a notification");
  assert.deepEqual(await storage(page), { [FLAG]: "1" }, "still only the flag");
  await unprovide(page);

  // Turn off: the flag goes, nothing more is made.
  await setHidden(page, false);
  await page.getByRole("button", { name: "Turn off" }).click();
  await page.waitForFunction(() => document.querySelector(".set-notify-state")?.textContent === "off");
  assert.deepEqual(await storage(page), {}, "the flag is cleared");
  assert.deepEqual(await buttons(page), ["Turn on"]);
  await setHidden(page, true);
  await emit(page, "command-end", { pane: 2, code: 3 });
  await settle(page);
  assert.equal((await made(page)).length, 4, "off makes none");
  assert.equal(await asked(page), 1, "no second request");
  await setHidden(page, false);
}

/** A refusal shows `blocked by the browser` with no way to turn it on, stores nothing, and makes no notification. */
async function blockedCase(page, url) {
  await openWith(page, url, { answer: "denied" });
  await openSettings(page);
  await page.getByRole("button", { name: "Turn on" }).click();
  await page.waitForFunction(() => document.querySelector(".set-notify-state")?.textContent === "blocked by the browser");
  assert.equal(await asked(page), 1);
  assert.deepEqual(await buttons(page), ["Check again"], "no button can turn it on");
  assert.equal(await page.locator(".set-notify-row").getAttribute("data-tone"), "warn");
  assert.equal(
    await page.locator(".set-notify-advice").textContent(),
    "The browser blocks notifications for this site. Allow them in the site settings of the browser, then choose Check again.",
  );
  const cues = await page.evaluate(() => {
    const s = document.querySelector(".set-notify-state");
    return { line: getComputedStyle(s).textDecorationLine, mark: getComputedStyle(s, "::before").content };
  });
  assert.equal(cues.line, "underline", "a second cue besides the colour: the underline");
  assert.match(cues.mark, /!/, "and the mark");
  assert.deepEqual(await storage(page), {}, "nothing stored");
  await setHidden(page, true);
  await emit(page, "bell", { pane: 1 });
  await page.waitForFunction(() => document.title === "\u25B2 holzWave");
  assert.deepEqual(await made(page), [], "blocked makes none");
  await setHidden(page, false);

  // Allowed later in the site settings of the browser: Check again reads it; it is off until the person turns it on.
  await page.evaluate(() => {
    window.Notification.requestPermission = async () => {
      window.__asked++;
      return "granted";
    };
    Object.defineProperty(window.Notification, "permission", { configurable: true, get: () => "granted" });
  });
  await page.getByRole("button", { name: "Check again" }).click();
  await page.waitForFunction(() => document.querySelector(".set-notify-state")?.textContent === "off");
  assert.deepEqual(await buttons(page), ["Turn on"]);
  assert.equal(await asked(page), 1, "checking again does not ask");

  // A page that starts with the permission denied shows it at once, whatever the flag says.
  await openWith(page, url, { permission: "denied", flag: "1" });
  await openSettings(page);
  await page.waitForFunction(() => document.querySelector(".set-notify-state")?.textContent === "blocked by the browser");
  assert.equal(await asked(page), 0);
}

/** Without a notification API the group is absent. */
async function noApiCase(page, url) {
  await openWith(page, url, { api: false });
  await openSettings(page);
  await page.waitForFunction(() => document.querySelectorAll(".dlg.settings .set-group").length > 0);
  assert.equal(await group(page).count(), 0, "no API, no group");
  assert.deepEqual(await storage(page), {});
}

/** Where the platform is not the web there is no group and nothing is read or stored. */
async function desktopCase(page, url) {
  await openWith(page, url, { os: "macos" });
  await openSettings(page);
  await page.waitForFunction(() => document.querySelectorAll(".dlg.settings .set-group").length > 0);
  await page.waitForTimeout(200);
  assert.equal(await group(page).count(), 0, "no Notifications group on the desktop");
  assert.equal(await asked(page), 0);
  assert.deepEqual(await storage(page), {});
}

/** The button is reachable by the keyboard, shows the 2 px focus ring, and the touch size of the desktop. */
async function keyboardCase(page, url) {
  await openWith(page, url);
  await openSettings(page);
  await group(page).waitFor();
  for (let i = 0; i < 40; i++) {
    if (await page.evaluate(() => document.activeElement?.closest(".set-notify-buttons") !== null && document.activeElement?.closest(".set-notify-buttons") !== undefined)) break;
    await page.keyboard.press("Tab");
  }
  const ring = await page.evaluate(() => {
    const b = document.activeElement;
    const s = getComputedStyle(b);
    return { text: b.textContent, style: s.outlineStyle, width: s.outlineWidth, height: b.getBoundingClientRect().height };
  });
  assert.equal(ring.text, "Turn on", "the keyboard reaches the button");
  assert.equal(ring.style, "solid");
  assert.equal(ring.width, "2px", "the focus ring is 2 px");
  assert.ok(ring.height >= 28, `at least 28 px high on a desktop: ${ring.height}`);
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => document.querySelector(".set-notify-state")?.textContent === "on");
  assert.equal(await asked(page), 1);
  // The button changed to `Turn off`; the keyboard stays in the group.
  assert.equal(await page.evaluate(() => document.activeElement?.closest(".set-notify-buttons") !== null), true, "the keyboard stays on the group");
}
