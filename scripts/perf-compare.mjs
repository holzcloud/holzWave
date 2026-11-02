#!/usr/bin/env node
// The comparison of the performance guard (POL-02, D-15): a run against the committed baseline of this machine.
//
//   node scripts/perf-compare.mjs <baseline.json> <run.json> [--scope core.,server.]
//
// There is no fixed absolute target: a metric is judged by the baseline that was recorded on this machine, with the
// tolerance table below (the planner's choice under D-15, the one place that holds the numbers):
//
//   time (ms)          worse when the median is more than  baseline * 1.5 + 0.1 ms
//   throughput (mib_s) worse when the median is less than  baseline * 0.5
//   memory (mib)       worse when the median is more than  baseline * 1.5 + 4 MiB
//
// `--scope` is a comma separated list of name prefixes that says which metrics the run was meant to measure; a baseline
// metric outside it is `not-measured` and does not fail. Inside the scope a metric that the run lacks, a unit that differs
// and a baseline with nothing in the scope all fail: a guard that cannot judge never passes.
//
// Output: one line per metric (`<verdict> <name> <baseline> -> <current> (limit <limit>)`), then `PERF OK` or `PERF WORSE`.
// Exit status: 0 pass, 1 a metric is worse or cannot be judged, 2 a file that cannot be read or wrong arguments. Node
// built-ins only.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

/** Per unit: the factor and the slack of the limit, and which direction is better. */
export const TOLERANCE = {
  ms: { factor: 1.5, slack: 0.1, better: "lower" },
  mib_s: { factor: 0.5, slack: 0, better: "higher" },
  mib: { factor: 1.5, slack: 4, better: "lower" },
};

const positiveFinite = (v) => typeof v === "number" && Number.isFinite(v) && v > 0;

function checkSet(set, what) {
  if (set === null || typeof set !== "object" || set.metrics === null || typeof set.metrics !== "object" || Array.isArray(set.metrics)) {
    throw new TypeError(`${what} has no metrics object`);
  }
  for (const [name, m] of Object.entries(set.metrics)) {
    if (m === null || typeof m !== "object") throw new TypeError(`${what}: ${name} is not an object`);
    if (typeof m.unit !== "string" || !Object.hasOwn(TOLERANCE, m.unit)) throw new TypeError(`${what}: ${name} has an unknown unit`);
    if (!positiveFinite(m.median)) throw new TypeError(`${what}: median of ${name} is not a positive finite number`);
  }
}

/** The limit a baseline median sets, and whether `current` is worse than it. `better` is the mirror, for the listing. */
function judge(unit, baseline, current) {
  const t = TOLERANCE[unit];
  const limit = baseline * t.factor + t.slack;
  if (t.better === "lower") {
    if (current > limit) return { verdict: "worse", limit };
    return { verdict: current * t.factor + t.slack < baseline ? "better" : "ok", limit };
  }
  if (current < limit) return { verdict: "worse", limit };
  return { verdict: current > baseline / t.factor ? "better" : "ok", limit };
}

/**
 * `{ ok, results: [{ name, verdict, baseline, current, limit }] }`. `scope` is an array of name prefixes (default: all).
 * `ok` is false for `worse`, `missing-in-run`, `unit-mismatch` and for a baseline with no metric in the scope.
 */
export function compare(baseline, run, { scope = [""] } = {}) {
  checkSet(baseline, "baseline");
  checkSet(run, "run");
  if (!Array.isArray(scope) || scope.some((p) => typeof p !== "string")) throw new TypeError("scope is a list of prefixes");
  const inScope = (name) => scope.some((prefix) => name.startsWith(prefix));
  const has = (set, name) => Object.hasOwn(set.metrics, name);

  const results = [];
  let judged = 0;
  let ok = true;
  for (const name of Object.keys(baseline.metrics)) {
    const b = baseline.metrics[name];
    if (!inScope(name)) {
      results.push({ name, verdict: "not-measured", baseline: b.median, current: null, limit: null });
      continue;
    }
    judged += 1;
    if (!has(run, name)) {
      results.push({ name, verdict: "missing-in-run", baseline: b.median, current: null, limit: null });
      ok = false;
      continue;
    }
    const r = run.metrics[name];
    if (r.unit !== b.unit) {
      results.push({ name, verdict: "unit-mismatch", baseline: b.median, current: r.median, limit: null });
      ok = false;
      continue;
    }
    const { verdict, limit } = judge(b.unit, b.median, r.median);
    if (verdict === "worse") ok = false;
    results.push({ name, verdict, baseline: b.median, current: r.median, limit });
  }
  for (const name of Object.keys(run.metrics)) {
    if (inScope(name) && !has(baseline, name)) {
      results.push({ name, verdict: "missing-in-baseline", baseline: null, current: run.metrics[name].median, limit: null });
    }
  }
  if (judged === 0) ok = false;
  return { ok, results };
}

// ---- the command line ----------------------------------------------------------------------------------------------

const USAGE = "usage: node scripts/perf-compare.mjs <baseline.json> <run.json> [--scope core.,server.]\n";

function readJson(path, what) {
  let text;
  try {
    text = readFileSync(path, "utf8");
  } catch (e) {
    throw new Error(`cannot read the ${what} ${path}: ${e.code ?? "error"}`);
  }
  try {
    return JSON.parse(text);
  } catch {
    throw new Error(`the ${what} ${path} is not JSON`);
  }
}

const show = (v) => (v === null ? "-" : String(Math.round(v * 1000) / 1000));

function main(argv) {
  const files = [];
  let scope;
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === "--scope") {
      i += 1;
      if (i >= argv.length || argv[i] === "" || scope !== undefined) {
        process.stderr.write(`perf-compare: --scope needs one list of prefixes\n${USAGE}`);
        return 2;
      }
      scope = argv[i].split(",");
    } else if (argv[i].startsWith("--")) {
      process.stderr.write(`perf-compare: unknown option ${argv[i]}\n${USAGE}`);
      return 2;
    } else {
      files.push(argv[i]);
    }
  }
  if (files.length !== 2) {
    process.stderr.write(USAGE);
    return 2;
  }
  let result;
  try {
    const baseline = readJson(files[0], "baseline");
    const run = readJson(files[1], "run file");
    result = compare(baseline, run, scope === undefined ? {} : { scope });
  } catch (e) {
    process.stderr.write(`perf-compare: ${e instanceof Error ? e.message : String(e)}\n`);
    return 2;
  }
  for (const r of result.results) {
    const limit = r.limit === null ? "" : ` (limit ${show(r.limit)})`;
    process.stdout.write(`${r.verdict} ${r.name} ${show(r.baseline)} -> ${show(r.current)}${limit}\n`);
  }
  process.stdout.write(result.ok ? "PERF OK\n" : "PERF WORSE\n");
  return result.ok ? 0 : 1;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) process.exit(main(process.argv.slice(2)));
