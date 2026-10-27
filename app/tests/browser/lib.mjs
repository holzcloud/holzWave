// Shared helpers for the browser tests: playwright-core, the app's CSP, engine launch and page setup.
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const APP = new URL("../../", import.meta.url);
export const APP_DIR = fileURLToPath(APP);
export const DIST_DIR = fileURLToPath(new URL("dist/", APP));
export const MOCK_PATH = fileURLToPath(new URL("tests/browser/tauri-mock.js", APP));
export const WS_MOCK_PATH = fileURLToPath(new URL("tests/browser/ws-mock.js", APP));
export const ENGINES = ["webkit", "chromium"];
/** The two ways the page reaches the core: the Tauri global of the desktop app, or the socket of a server. */
export const BACKENDS = ["tauri", "ws"];

let backend = "tauri";
/** Choose the backend `newPage` sets up from now on (the runner calls this before each test). */
export function setBackend(name) {
  if (!BACKENDS.includes(name)) throw new Error(`unknown backend "${name}" (expected ${BACKENDS.join(" or ")})`);
  backend = name;
}
/** The backend the current test runs on. */
export const currentBackend = () => backend;

/** playwright-core as installed for app/package.json. */
export function loadPlaywright() {
  return createRequire(new URL("package.json", APP))("playwright-core");
}

/** The Content-Security-Policy the shipped app runs under (`app.security.csp` in tauri.conf.json). */
export function readCsp() {
  const conf = JSON.parse(readFileSync(new URL("src-tauri/tauri.conf.json", APP), "utf8"));
  const csp = conf?.app?.security?.csp;
  if (typeof csp !== "string" || csp.trim() === "") {
    throw new Error("app.security.csp is missing in app/src-tauri/tauri.conf.json");
  }
  return csp;
}

/** Launch one engine headless. Chromium needs ANGLE on Metal on macOS: software GL hung in Phase 1. */
export async function launch(engine) {
  const pw = loadPlaywright();
  if (engine === "webkit") return pw.webkit.launch();
  if (engine === "chromium") {
    const args = process.platform === "darwin" ? ["--use-angle=metal", "--enable-unsafe-swiftshader"] : [];
    return pw.chromium.launch({ args });
  }
  throw new Error(`unknown engine "${engine}" (expected ${ENGINES.join(" or ")})`);
}

/**
 * A fresh page with the core stub installed before any page script (the Tauri global, or on the ws backend the fake
 * socket of ws-mock.js after the same mock core), plus `errors`: one string for every
 * uncaught page error and every Content-Security-Policy violation. A test fails if `errors` is not empty.
 * `deviceScaleFactor` sets window.devicePixelRatio (1 by default).
 */
export async function newPage(browser, { viewport = { width: 1000, height: 640 }, deviceScaleFactor = 1 } = {}) {
  const page = await browser.newPage({ viewport, deviceScaleFactor });
  const errors = [];
  page.on("pageerror", (e) => errors.push(`page error: ${e.message}`));
  await page.exposeFunction("__reportCsp__", (what) => errors.push(`csp violation: ${what}`));
  await page.addInitScript(() => {
    window.addEventListener(
      "securitypolicyviolation",
      (e) => window.__reportCsp__(`${e.violatedDirective} blocked ${e.blockedURI || "inline"} (${e.sourceFile || "page"})`),
      true,
    );
  });
  await page.addInitScript((name) => {
    window.__HW_BACKEND__ = name;
  }, backend);
  await page.addInitScript({ path: MOCK_PATH });
  if (backend === "ws") await page.addInitScript({ path: WS_MOCK_PATH });
  return { page, errors };
}

/**
 * Take the expected policy reports of `safehtml.js` out of `errors` (in place: the runner reads the same array).
 * The page's policy also applies in the inert document that `safehtml.js` parses markup in, and a test feeds it hostile
 * markup on purpose, with an inline style and inline handlers. Every engine reports the style, Chromium 156 (the build of
 * Playwright 1.64) the handlers too; none of it reaches the page, which is what those tests assert. Any other report,
 * and any from another file, stays an error. The reports are events that arrive a moment after the call that caused them.
 */
export async function dropInertReports(page, errors) {
  await page.waitForTimeout(100);
  const expected = /(style-src-(elem|attr)|script-src-attr) blocked inline \(.*safehtml\.js\)/;
  errors.splice(0, errors.length, ...errors.filter((e) => !expected.test(e)));
}

/** One RGBA pixel of a WebGL canvas as [r, g, b, a]; (x, y) is in canvas pixels from the top-left corner. */
export async function glPixel(page, selector, x, y) {
  return page.evaluate(
    ([sel, px, py]) => {
      const canvas = document.querySelector(sel);
      if (!canvas) throw new Error(`no element matches ${sel}`);
      const gl = canvas.getContext("webgl2") ?? canvas.getContext("webgl");
      if (!gl) throw new Error(`${sel} has no WebGL context`);
      const out = new Uint8Array(4);
      gl.readPixels(px, gl.drawingBufferHeight - 1 - py, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, out);
      return Array.from(out);
    },
    [selector, x, y],
  );
}

/** The args of every call the page made to `cmd`, oldest first. */
export const callsOf = (page, cmd) => page.evaluate((c) => window.__MOCK__.callsOf(c), cmd);

/** Deliver an event to the page's listeners as the core would. */
export const emit = (page, name, payload) => page.evaluate(([n, p]) => window.__MOCK__.emit(n, p), [name, payload]);

/**
 * How many listeners the page holds for the event `name`, read from the page's own bridge (the same module the page
 * uses, so it is right on both backends: the bridge counts its own registrations).
 */
export const listenerCount = (page, name) =>
  page.evaluate(async (n) => (await import(new URL("bridge.js", location.href).href)).listenerCount(n), name);

/** Wait until the page holds exactly `n` listeners for `name`. */
export async function waitForListeners(page, name, n, timeoutMs = 5000) {
  const t0 = Date.now();
  let seen = -1;
  while (Date.now() - t0 < timeoutMs) {
    seen = await listenerCount(page, name);
    if (seen === n) return;
    await page.waitForTimeout(20);
  }
  throw new Error(`the page holds ${seen} listeners for "${name}", expected ${n}`);
}
