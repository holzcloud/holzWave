// The tab of the browser build calls the person back while the page is hidden: the title starts with the glyphs of the bar's
// marks in their fixed order and the icon gets the rose dot; both are put back the moment the tab is visible again and are
// never touched while it is. Neither ever holds a pane's title, a directory or a command.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { callsOf, emit, waitForListeners } from "./lib.mjs";

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
