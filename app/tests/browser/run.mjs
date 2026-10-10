// Browser test runner: every tests/browser/*.test.mjs, once per engine, against the built app/dist.
//   node tests/browser/run.mjs [--engine webkit|chromium] [--only <substring of the file name>]
//
// A test file default-exports `async ({ engine, page, url, errors }) => {}` and fails by throwing. It also fails
// when the page reported an uncaught error or a Content-Security-Policy violation (`errors`).
import { existsSync, readdirSync } from "node:fs";
import { basename } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";
import { startServer } from "./serve.mjs";
import { DIST_DIR, ENGINES, launch, newPage, readCsp } from "./lib.mjs";

const TEST_TIMEOUT_MS = 30_000;

const { values } = parseArgs({
  options: { engine: { type: "string" }, only: { type: "string" } },
  strict: true,
});
if (values.engine !== undefined && !ENGINES.includes(values.engine)) {
  console.error(`--engine must be one of: ${ENGINES.join(", ")}`);
  process.exit(2);
}
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
const report = (ok, name, engine, ms, why = "") => {
  if (ok) passed++;
  else failed++;
  console.log(`${ok ? "PASS" : "FAIL"} ${name} [${engine}] ${ms} ms${why ? `: ${oneLine(why)}` : ""}`);
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
        const t0 = Date.now();
        let page;
        try {
          const mod = await import(pathToFileURL(fileURLToPath(new URL(file, here))).href);
          if (typeof mod.default !== "function") throw new Error(`${file} has no default-exported function`);
          const made = await newPage(browser);
          page = made.page;
          await withTimeout(mod.default({ engine, page, url: server.url, errors: made.errors }), TEST_TIMEOUT_MS, name);
          if (made.errors.length > 0) throw new Error(made.errors.join("; "));
          report(true, name, engine, Date.now() - t0);
        } catch (e) {
          report(false, name, engine, Date.now() - t0, e?.message ?? e);
        } finally {
          await page?.close().catch(() => {});
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
process.exitCode = failed > 0 ? 1 : 0;
