// Browser test runner: every tests/browser/*.test.mjs, once per engine, against the built app/dist.
//   node tests/browser/run.mjs [--engine webkit|chromium] [--backend tauri|ws|both] [--only <substring of the file name>]
//
// A test runs once per backend it supports: `tauri` (the page finds the Tauri stub) and `ws` (the page has no Tauri
// global and talks to a fake socket, ws-mock.js). A test file that only makes sense on the desktop exports
// `backends = ["tauri"]` and `backendNote` (one sentence: the desktop-only subject); the run prints it as `NOT RUN`.

// A test file default-exports `async ({ engine, page, url, errors }) => {}` and fails by throwing. It also fails
// when the page reported an uncaught error or a Content-Security-Policy violation (`errors`).
import { existsSync, readdirSync } from "node:fs";
import { basename } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";
import { startServer } from "./serve.mjs";
import { BACKENDS, DIST_DIR, ENGINES, launch, newPage, readCsp, setBackend } from "./lib.mjs";

const TEST_TIMEOUT_MS = 30_000;

const { values } = parseArgs({
  options: { engine: { type: "string" }, backend: { type: "string" }, only: { type: "string" } },
  strict: true,
});
if (values.engine !== undefined && !ENGINES.includes(values.engine)) {
  console.error(`--engine must be one of: ${ENGINES.join(", ")}`);
  process.exit(2);
}
const wanted = values.backend ?? "both";
if (wanted !== "both" && !BACKENDS.includes(wanted)) {
  console.error(`--backend must be one of: ${BACKENDS.join(", ")}, both`);
  process.exit(2);
}
const backends = wanted === "both" ? BACKENDS : [wanted];
if (!existsSync(`${DIST_DIR}index.html`)) {
  console.error("app/dist/index.html is missing: run `npm run build --prefix app` first");
  process.exit(2);
}

const here = new URL(".", import.meta.url);
const files = readdirSync(fileURLToPath(here))
  .filter((f) => f.endsWith(".test.mjs"))
  .filter((f) => values.only === undefined || f.includes(values.only))
  .sort();
if (files.length === 0) {
  console.error(values.only === undefined ? "no browser tests found" : `no browser test file matches "${values.only}"`);
  process.exit(2);
}
const engines = values.engine ? [values.engine] : ENGINES;

function withTimeout(promise, ms, what) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${what} timed out after ${ms} ms`)), ms);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

const oneLine = (s) => String(s).replace(/\s+/g, " ").trim();
let failed = 0;
let passed = 0;
const perBackend = Object.fromEntries(BACKENDS.map((b) => [b, { passed: 0, failed: 0 }]));
/** The engine as the result line shows it: `webkit` on the Tauri backend, `webkit ws` on the web backend. */
const label = (engine, backend) => (backend === "tauri" ? engine : `${engine} ${backend}`);
const report = (ok, name, engine, ms, why = "", backend = "tauri") => {
  if (ok) passed++;
  else failed++;
  perBackend[backend][ok ? "passed" : "failed"]++;
  console.log(`${ok ? "PASS" : "FAIL"} ${name} [${label(engine, backend)}] ${ms} ms${why ? `: ${oneLine(why)}` : ""}`);
};

const server = await startServer({ root: DIST_DIR, csp: readCsp() });
try {
  for (const engine of engines) {
    let browser;
    try {
      browser = await withTimeout(launch(engine), TEST_TIMEOUT_MS, `launching ${engine}`);
    } catch (e) {
      report(false, "launch", engine, 0, e?.message ?? e);
      continue;
    }
    try {
      for (const file of files) {
        const name = basename(file, ".test.mjs");
        let mod;
        try {
          mod = await import(pathToFileURL(fileURLToPath(new URL(file, here))).href);
        } catch (e) {
          report(false, name, engine, 0, e?.message ?? e);
          continue;
        }
        const supported = mod.backends ?? BACKENDS;
        for (const backend of backends) {
          if (!supported.includes(backend)) {
            console.log(`NOT RUN ${name} [${backend}]: ${oneLine(mod.backendNote ?? "no reason given")}`);
            continue;
          }
          const t0 = Date.now();
          let page;
          try {
            if (typeof mod.default !== "function") throw new Error(`${file} has no default-exported function`);
            setBackend(backend);
            const made = await newPage(browser);
            page = made.page;
            await withTimeout(mod.default({ engine, page, url: server.url, errors: made.errors }), TEST_TIMEOUT_MS, name);
            if (made.errors.length > 0) throw new Error(made.errors.join("; "));
            report(true, name, engine, Date.now() - t0, "", backend);
          } catch (e) {
            report(false, name, engine, Date.now() - t0, e?.message ?? e, backend);
          } finally {
            await page?.close().catch(() => {});
          }
        }
      }
    } finally {
      await browser.close().catch(() => {});
    }
  }
} finally {
  await server.close();
}

console.log(`${passed} passed, ${failed} failed`);
console.log(`backends: ${backends.map((b) => `${b} ${perBackend[b].passed} passed, ${perBackend[b].failed} failed`).join("; ")}`);
process.exitCode = failed > 0 ? 1 : 0;
