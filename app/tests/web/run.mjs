// The runner of the real-server browser tests.
//   node tests/web/run.mjs [--only <substring of the file name>] [--engine webkit|chromium] [--self-check]
//
// For each test file and each engine it runs the file in a child process (`HW_ENGINE` set), and each file starts its own
// server through the harness (server/tests/web_harness.rs): the product's own assembly, the embedded page built by
// `npm run build`, a generated certificate, workers in the server's own process. Exit status of a file: 0 is `ok`, 77 is
// `SKIP` (the reason is the last line it printed after `SKIP:`), anything else is `FAIL`. Nothing is retried.
//
// `--self-check` runs the files that carry deliberately wrong expectations (they print `selfcheck.fail` lines) and requires
// that exactly those expectations fail in each engine: a suite that cannot fail proves nothing.
import { spawn } from "node:child_process";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { DIST_DIR, ENGINES, loadPlaywright } from "../browser/lib.mjs";
import { buildHarness } from "./harness.mjs";

const { values } = parseArgs({
  options: { only: { type: "string" }, engine: { type: "string" }, "self-check": { type: "boolean", default: false } },
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
const selfCheck = values["self-check"];
const here = fileURLToPath(new URL(".", import.meta.url));
const SELF_CHECKS = 2;

let files = readdirSync(here)
  .filter((f) => f.endsWith(".test.mjs"))
  .filter((f) => values.only === undefined || f.includes(values.only))
  .sort();
if (selfCheck) files = files.filter((f) => readFileSync(`${here}${f}`, "utf8").includes("selfcheck.fail"));
if (files.length === 0) {
  console.error(values.only === undefined ? "no test files found" : `no test file matches "${values.only}"`);
  process.exit(2);
}
const engines = values.engine ? [values.engine] : ENGINES;

// The engines' versions, so a result names what it ran on.
{
  const playwright = loadPlaywright();
  const versions = [];
  for (const engine of engines) {
    const browser = await playwright[engine].launch();
    versions.push(`${engine} ${browser.version()}`);
    await browser.close();
  }
  console.log(`web: engines: ${versions.join(", ")}`);
}

let binary;
try {
  binary = buildHarness();
} catch (e) {
  console.error(`web: ${e.message}`);
  process.exit(1);
}

/** Run one file in one engine; resolves with `{ status, output, ms }`. */
function runFile(file, engine) {
  return new Promise((resolve) => {
    const t0 = Date.now();
    const env = { ...process.env, HW_ENGINE: engine, HW_HARNESS_BIN: binary };
    if (selfCheck) env.HW_SELF_CHECK = "1";
    const child = spawn(process.execPath, [`${here}${file}`], { cwd: fileURLToPath(new URL("../../", import.meta.url)), env, stdio: ["ignore", "pipe", "pipe"] });
    let output = "";
    child.stdout.on("data", (d) => (output += d));
    child.stderr.on("data", (d) => (output += d));
    const kill = () => child.kill("SIGKILL");
    process.once("exit", kill);
    child.on("exit", (code, signal) => {
      process.off("exit", kill);
      resolve({ status: code ?? (signal ? 1 : 0), output, ms: Date.now() - t0 });
    });
  });
}

const counts = { ok: 0, FAIL: 0, SKIP: 0 };
const perEngine = Object.fromEntries(engines.map((e) => [e, { ok: 0, FAIL: 0, SKIP: 0 }]));
for (const file of files) {
  const name = file.replace(/\.test\.mjs$/, "");
  for (const engine of engines) {
    const { status, output, ms } = await runFile(file, engine);
    let verdict;
    let note = "";
    if (selfCheck) {
      const failed = output.split("\n").filter((l) => l.startsWith("selfcheck.fail ")).length;
      const passed = output.split("\n").filter((l) => l.startsWith("selfcheck.pass ")).length;
      verdict = status === 0 && failed === SELF_CHECKS && passed === 0 ? "ok" : "FAIL";
      note = `${failed} of ${SELF_CHECKS} wrong expectations failed${passed > 0 ? `, ${passed} wrongly passed` : ""}`;
    } else if (status === 0) verdict = "ok";
    else if (status === 77) {
      verdict = "SKIP";
      const reason = output.split("\n").filter((l) => l.startsWith("SKIP:")).pop() ?? "SKIP: no reason given";
      note = reason.slice("SKIP:".length).trim();
      if (note === "no reason given") verdict = "FAIL";
    } else verdict = "FAIL";
    counts[verdict]++;
    perEngine[engine][verdict]++;
    console.log(`web: ${name} ${engine} ${selfCheck ? "self-check " : ""}${verdict}${note ? ` (${note})` : ""} ${ms} ms`);
    if (verdict === "FAIL") {
      for (const line of output.trim().split("\n").slice(-25)) console.log(`    ${line}`);
    }
  }
}
console.log(`web: ${counts.ok} ok, ${counts.FAIL} FAIL, ${counts.SKIP} SKIP (${engines.map((e) => `${e}: ${perEngine[e].ok} ok, ${perEngine[e].FAIL} FAIL, ${perEngine[e].SKIP} SKIP`).join("; ")})`);
process.exitCode = counts.FAIL > 0 ? 1 : 0;
