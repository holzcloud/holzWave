// Shared helpers for the browser tests: playwright-core, the app's CSP, engine launch and page setup.
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const APP = new URL("../../", import.meta.url);
export const APP_DIR = fileURLToPath(APP);
export const DIST_DIR = fileURLToPath(new URL("dist/", APP));
export const MOCK_PATH = fileURLToPath(new URL("tests/browser/tauri-mock.js", APP));
export const ENGINES = ["webkit", "chromium"];

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
 * A fresh page with the Tauri stub installed before any page script, plus `errors`: one string for every
 * uncaught page error and every Content-Security-Policy violation. A test fails if `errors` is not empty.
 */
export async function newPage(browser, { viewport = { width: 1000, height: 640 } } = {}) {
  const page = await browser.newPage({ viewport });
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
  await page.addInitScript({ path: MOCK_PATH });
  return { page, errors };
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
