// The boot script of the application document (dist/floor.js) on an Android WebView below the floor: one notice in the words
// and the look of docs/design.md 6.6 (`Web view too old`), no request for /main.js, no call to the core, no page error and no
// policy violation. Each check of the script is live: an Android user agent with only that one feature taken away sees the
// notice. A current Android WebView, and a desktop page even with a feature taken away, load the application as before.
//
// The user agent and the missing features are set by an init script before any script of the page runs, from the query of the
// address (`ua`, `take`), so one page can open the document in every state. The check acts on Android only; that is its
// documented scope, and the desktop case below proves it.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

export const backends = ["tauri"];
export const backendNote = "the subject is the boot of the document, which is the same on both backends; the socket backend starts through it in the smoke and web backend tests";

const FLOOR_JS = readFileSync(fileURLToPath(new URL("../../dist/floor.js", import.meta.url)), "utf8");
const FLOOR = Number(FLOOR_JS.match(/\bFLOOR = (\d+);/)?.[1]);

/** The user agent of the system WebView of an Android 12 image, with the given major version. */
const android = (version) =>
  `Mozilla/5.0 (Linux; Android 12; sdk_gphone64_arm64 Build/SE1A.220826.008; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/${version}.0.4472.114 Mobile Safari/537.36`;
/** An Android user agent that names no Chrome version. */
const ANDROID_NO_VERSION = "Mozilla/5.0 (Linux; Android 14; Pixel 8)";

/** How each built-in check of the boot script is taken away: the property to delete, keyed by the check as the script writes it. */
const BUILTIN = {
  'typeof "".replaceAll === "function"': "String.prototype.replaceAll",
  'typeof Element.prototype.replaceChildren === "function"': "Element.prototype.replaceChildren",
  'typeof [].at === "function" && typeof "".at === "function"': "Array.prototype.at",
  'typeof Object.hasOwn === "function"': "Object.hasOwn",
  '"inert" in HTMLElement.prototype': "HTMLElement.prototype.inert",
};

/** The checks of the boot script, read from the built file: the built-ins, the `CSS.supports` strings and the media queries. */
function checksOf(js) {
  const block = (name) => {
    const m = js.match(new RegExp(`var ${name} = \\[([\\s\\S]*?)\\];`));
    assert.ok(m, `dist/floor.js has no list ${name}`);
    return m[1];
  };
  const builtins = [...block("BUILTINS").matchAll(/return (.+?); \}/g)].map((m) => m[1]);
  const strings = (name) => [...block(name).matchAll(/"([^"]+)"/g)].map((m) => m[1]);
  return { builtins, supports: strings("SUPPORTS"), media: strings("MEDIA") };
}

/** Removes features and sets the user agent from the query of the address, before any script of the page. */
function takeAway() {
  const q = new URLSearchParams(location.search);
  const agent = q.get("ua");
  if (agent !== null) Object.defineProperty(Navigator.prototype, "userAgent", { configurable: true, get: () => agent });
  const take = q.get("take");
  if (take === null) return;
  const at = take.indexOf(":");
  const kind = take.slice(0, at);
  const what = take.slice(at + 1);
  if (kind === "builtin") {
    const path = what.split(".");
    const name = path.pop();
    let owner = window;
    for (const part of path) owner = owner[part];
    delete owner[name];
  } else if (kind === "supports") {
    const real = CSS.supports.bind(CSS);
    CSS.supports = (...args) => (args.length === 1 && args[0] === what ? false : real(...args));
  } else if (kind === "media") {
    const real = window.matchMedia.bind(window);
    window.matchMedia = (query) => (query === what ? { matches: false, media: query, addEventListener() {}, removeEventListener() {} } : real(query));
  }
}

export default async function ({ engine, page, url, errors }) {
  assert.ok(Number.isInteger(FLOOR) && FLOOR >= 93, `dist/floor.js declares the floor ${FLOOR}`);
  const requests = [];
  page.on("request", (r) => requests.push(new URL(r.url()).pathname));
  await page.addInitScript(takeAway);
  await page.setViewportSize({ width: 412, height: 915 });

  const open = async ({ agent, take } = {}) => {
    requests.length = 0;
    const q = new URLSearchParams({ renderer: "canvas" });
    if (agent !== undefined) q.set("ua", agent);
    if (take !== undefined) q.set("take", take);
    await page.goto(`${url}index.html?${q}`);
  };

  /** The notice is shown, styled, alone, and nothing of the application was asked for. */
  const expectNotice = async (what, version) => {
    await page.waitForSelector(".floor", { timeout: 5000 }).catch(() => {
      throw new Error(`${what}: no notice`);
    });
    await page.waitForFunction(() => getComputedStyle(document.querySelector(".floor")).borderTopStyle === "solid");
    await page.waitForTimeout(150); // a late request for the application would show up here
    const seen = await page.evaluate(() => ({
      label: document.querySelector(".floor-label")?.textContent,
      lines: [...document.querySelectorAll(".floor-line")].map((n) => n.textContent),
      app: document.getElementById("app") !== null,
      notices: document.querySelectorAll(".floor").length,
      calls: window.__MOCK__.calls.length,
      modules: [...document.querySelectorAll("script")].map((s) => s.getAttribute("src")),
    }));
    const has = version === undefined ? "" : ` This phone has version ${version}.`;
    assert.equal(seen.label, "!Web view too old", `${what}: the label`);
    assert.deepEqual(
      seen.lines,
      [`holzWave needs Android System WebView ${FLOOR} or later.${has}`, "Update Android System WebView in the Play Store, then open holzWave again."],
      `${what}: the two lines`,
    );
    assert.equal(seen.notices, 1, `${what}: one notice`);
    assert.equal(seen.app, false, `${what}: nothing of the application is drawn`);
    assert.equal(seen.calls, 0, `${what}: the core was called`);
    assert.deepEqual(seen.modules, ["/floor.js"], `${what}: a script other than the boot script is in the document`);
    assert.ok(!requests.includes("/main.js"), `${what}: /main.js was requested`);
    assert.ok(requests.includes("/floor.css"), `${what}: the notice's stylesheet was not loaded`);
  };

  /** The application started: /main.js was asked for and the layout is drawn. */
  const expectApp = async (what) => {
    await page.waitForSelector(".divider", { timeout: 10000 }).catch(() => {
      throw new Error(`${what}: the application did not draw its layout`);
    });
    assert.ok(requests.includes("/main.js"), `${what}: /main.js was not requested`);
    assert.equal(await page.locator(".floor").count(), 0, `${what}: a notice was shown`);
    assert.ok(!requests.includes("/floor.css"), `${what}: the notice's stylesheet was loaded`);
  };

  // ---- an Android WebView 91 without Object.hasOwn: the notice, and nothing else -------------------------------------------
  await open({ agent: android(91), take: "builtin:Object.hasOwn" });
  await expectNotice("WebView 91", 91);

  // The look of the record: a 1 px hairline box, the label in the label style with the `!` mark, the warn tone and an underline.
  const look = () =>
    page.evaluate(() => {
      const probe = (prop, value) => {
        const p = document.createElement("span");
        p.style.setProperty(prop, value);
        document.body.appendChild(p);
        const out = getComputedStyle(p).getPropertyValue(prop);
        p.remove();
        return out;
      };
      const box = getComputedStyle(document.querySelector(".floor"));
      const label = getComputedStyle(document.querySelector(".floor-label"));
      const line = getComputedStyle(document.querySelector(".floor-line"));
      const r = document.querySelector(".floor").getBoundingClientRect();
      return {
        border: [box.borderTopWidth, box.borderTopStyle, box.borderTopColor],
        sides: [box.borderRightStyle, box.borderBottomStyle, box.borderLeftStyle],
        lineToken: probe("color", "var(--hw-line)"),
        ink3: probe("color", "var(--hw-ink-3)"),
        warn: probe("color", "var(--hw-warn)"),
        ink2: probe("color", "var(--hw-ink-2)"),
        ground: probe("color", "var(--hw-ground)"),
        page: getComputedStyle(document.body).backgroundColor,
        label: { color: label.color, transform: label.textTransform, spacing: label.letterSpacing, size: label.fontSize, deco: label.textDecorationLine, font: label.fontFamily },
        mark: document.querySelector(".floor-label > .floor-mark")?.textContent,
        markHidden: document.querySelector(".floor-mark")?.getAttribute("aria-hidden"),
        role: document.querySelector(".floor")?.getAttribute("role"),
        line: { color: line.color, size: line.fontSize },
        shadow: box.boxShadow,
        fits: r.left >= 0 && r.right <= innerWidth,
        moving: document.getAnimations().length,
        transition: box.transitionDuration,
      };
    });
  const plain = await look();
  assert.deepEqual(plain.border.slice(0, 2), ["1px", "solid"], "the box has a 1 px hairline");
  assert.equal(plain.border[2], plain.lineToken, "the hairline is --hw-line");
  assert.deepEqual(plain.sides, ["solid", "solid", "solid"], "the hairline goes round the box");
  assert.equal(plain.page, plain.ground, "the notice is on the ground colour");
  assert.equal(plain.label.color, plain.warn, "the label is in the warn tone");
  assert.equal(plain.label.transform, "uppercase", "the label is in the label style");
  assert.notEqual(plain.label.spacing, "normal", "the label is tracked");
  assert.equal(plain.label.size, "11px", "the label is 11/12 rem at the default scale");
  assert.match(plain.label.deco, /underline/, "the label is underlined, a cue besides the colour");
  assert.match(plain.label.font, /JetBrains Mono/, "the notice is in the mono face of the chrome");
  assert.equal(plain.mark, "!", "the label carries the ! mark");
  assert.equal(plain.markHidden, "true", "the mark is not read out on top of the label");
  assert.equal(plain.role, "alert", "the notice is announced");
  assert.equal(plain.line.color, plain.ink2, "the lines are in the secondary ink");
  assert.equal(plain.line.size, "14px", "the lines are 14/12 rem at the default scale");
  assert.equal(plain.shadow, "none", "the notice is flat");
  assert.ok(plain.fits, "the notice fits a phone's width");

  // A larger UI scale moves every size: the sizes are in rem.
  await page.evaluate(() => document.documentElement.style.setProperty("font-size", "18px"));
  const scaled = await page.evaluate(() => getComputedStyle(document.querySelector(".floor-label")).fontSize);
  assert.equal(scaled, "16.5px", "the label follows the root size");
  await page.evaluate(() => document.documentElement.style.removeProperty("font-size"));

  // Higher contrast: the hairline takes the stronger token; the border, the mark and the underline stay.
  await page.evaluate(() => document.documentElement.classList.add("contrast-high"));
  const high = await look();
  assert.deepEqual(high.border.slice(0, 2), ["1px", "solid"], "contrast-high keeps the border");
  assert.equal(high.border[2], high.ink3, "contrast-high draws the hairline in --hw-ink-3");
  assert.equal(high.mark, "!", "contrast-high keeps the mark");
  assert.match(high.label.deco, /underline/, "contrast-high keeps the underline");
  await page.evaluate(() => document.documentElement.classList.remove("contrast-high"));

  // Forced colours: the system colours replace the tokens; the border, the mark and the underline stay.
  await page.emulateMedia({ forcedColors: "active" });
  if (await page.evaluate(() => matchMedia("(forced-colors: active)").matches)) {
    const forced = await look();
    assert.deepEqual(forced.border.slice(0, 2), ["1px", "solid"], "forced colours keep the border");
    assert.notEqual(forced.border[2], "rgba(0, 0, 0, 0)", "forced colours keep the border visible");
    assert.equal(forced.mark, "!", "forced colours keep the mark");
    assert.match(forced.label.deco, /underline/, "forced colours keep the underline");
  } else {
    console.log(`webviewfloor [${engine}]: this engine does not emulate forced colours; the rule is checked in Chromium`);
  }
  await page.emulateMedia({ forcedColors: "none" });

  // Reduced motion: nothing moves, with or without it.
  await page.emulateMedia({ reducedMotion: "reduce" });
  const still = await look();
  assert.equal(still.moving, 0, "nothing moves under reduced motion");
  assert.equal(still.transition, "0s", "the notice has no transition");
  await page.emulateMedia({ reducedMotion: "no-preference" });
  assert.equal((await look()).moving, 0, "nothing moves at all");

  // ---- every check is live: one feature taken away from an otherwise current Android WebView shows the notice --------------
  const checks = checksOf(FLOOR_JS);
  assert.deepEqual(checks.builtins.slice().sort(), Object.keys(BUILTIN).sort(), "every built-in check of the boot script has a way to take it away here");
  assert.ok(checks.supports.length >= 10 && checks.media.length >= 2, "the boot script checks the stylesheets' features and the media features");
  const current = android(FLOOR + 31);
  for (const check of checks.builtins) {
    await open({ agent: current, take: `builtin:${BUILTIN[check]}` });
    await expectNotice(`without ${BUILTIN[check]}`, FLOOR + 31);
  }
  for (const s of checks.supports) {
    await open({ agent: current, take: `supports:${s}` });
    await expectNotice(`without CSS.supports("${s}")`, FLOOR + 31);
  }
  for (const m of checks.media) {
    await open({ agent: current, take: `media:${m}` });
    await expectNotice(`without the media query ${m}`, FLOOR + 31);
  }
  // The version stands for what no feature test can see (syntax, layout contexts, the vendored libraries).
  await open({ agent: android(FLOOR - 1) });
  await expectNotice("one version below the floor", FLOOR - 1);
  // A WebView that names no version is judged by its features alone, and the notice leaves the second sentence out.
  await open({ agent: ANDROID_NO_VERSION, take: "builtin:Object.hasOwn" });
  await expectNotice("an Android user agent with no version", undefined);

  // ---- a current Android WebView loads the application -------------------------------------------------------------------
  if (engine === "chromium") {
    await open({ agent: android(FLOOR) });
    await expectApp("an Android WebView at the floor");
    await open({ agent: ANDROID_NO_VERSION });
    await expectApp("an Android WebView that names no version");
  } else {
    // WebKit is the engine of no Android WebView, and it lacks `forced-color-adjust`, which the page uses: with an Android user
    // agent it rightly gets the notice. The current-WebView case runs in Chromium, the engine of the Android System WebView.
    console.log(`webviewfloor [${engine}]: the current-Android case runs in Chromium; WebKit is no Android engine`);
  }

  // ---- the desktop is never checked: without Object.hasOwn the application script still loads ------------------------------
  await open({ take: "builtin:Object.hasOwn" });
  await page.waitForFunction(() => document.querySelector('script[src="/main.js"]') !== null);
  await page.waitForTimeout(300);
  assert.ok(requests.includes("/main.js"), "a desktop page loads /main.js whatever it lacks: the check is Android's only");
  assert.equal(await page.locator(".floor").count(), 0, "a desktop page never shows the notice");
  // The application then meets the missing built-in itself; that error is the documented scope of the check, not the subject here.
  errors.splice(0, errors.length, ...errors.filter((e) => !/hasOwn/.test(e)));

  // And with everything present the desktop page starts as before.
  await open();
  await expectApp("a desktop page");
}
