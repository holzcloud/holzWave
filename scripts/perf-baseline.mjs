#!/usr/bin/env node
// Writes or extends the baseline of this machine from a run file (POL-02, D-15, D-16). `perf-record.sh` calls it.
//
//   node scripts/perf-baseline.mjs <run.json> <perf/baseline-<os>-<arch>.json> [--replace]
//
// The file is a record of the machine it was taken on and of nothing else: the date (UTC), the operating system and
// architecture (`uname`), the CPU model string, the core count, the commit of the last recording and the metrics. No host
// name, user name, path or address is ever written; the keys are an allowlist and the metrics are copied field by field.
//
// A metric that is already in the file is replaced only with `--replace` (a baseline is replaced on purpose, with the
// reason in the commit message); a metric that is not yet in it is added, so a later group extends the file of a machine
// without touching the first. A file that was taken on another CPU is not extended without `--replace`.
//
// Exit status: 0 written, 2 an error. Node built-ins only.

import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { availableParallelism, cpus } from "node:os";
import { fileURLToPath } from "node:url";

/** The only top-level keys of a baseline file. A unit test reads every `perf/*.json` against this list. */
export const BASELINE_KEYS = ["date", "os", "arch", "cpu", "cores", "commit", "metrics"];
const METRIC_KEYS = ["median", "p95", "unit", "runs"];

function copyMetrics(source, what) {
  if (source === null || typeof source !== "object" || source.metrics === null || typeof source.metrics !== "object") {
    throw new TypeError(`${what} has no metrics object`);
  }
  const out = Object.create(null);
  for (const name of Object.keys(source.metrics)) {
    const m = source.metrics[name];
    for (const key of METRIC_KEYS) {
      if (m === null || typeof m !== "object" || !Object.hasOwn(m, key)) throw new TypeError(`${what}: ${name} lacks ${key}`);
    }
    for (const key of ["median", "p95"]) {
      if (typeof m[key] !== "number" || !Number.isFinite(m[key]) || m[key] <= 0) throw new TypeError(`${what}: ${key} of ${name} is not positive`);
    }
    if (typeof m.unit !== "string" || !Number.isInteger(m.runs) || m.runs < 1) throw new TypeError(`${what}: ${name} has a bad unit or runs`);
    out[name] = { median: m.median, p95: m.p95, unit: m.unit, runs: m.runs };
  }
  return out;
}

/**
 * The baseline after recording `run` over `existing` (or over nothing). `header` carries `date`, `os`, `arch`, `cpu`,
 * `cores` and `commit` and nothing else is taken from it. Returns `{ baseline, added, replaced, kept }`.
 */
export function mergeBaseline(existing, run, header, { replace = false } = {}) {
  const fresh = copyMetrics(run, "run");
  const metrics = existing ? copyMetrics(existing, "baseline") : Object.create(null);
  if (existing && !replace && existing.cpu !== header.cpu) {
    throw new Error("the baseline was taken on another CPU; record it again with --replace");
  }
  const added = [];
  const replaced = [];
  const kept = [];
  for (const name of Object.keys(fresh)) {
    if (!(name in metrics)) {
      metrics[name] = fresh[name];
      added.push(name);
    } else if (replace) {
      metrics[name] = fresh[name];
      replaced.push(name);
    } else {
      kept.push(name);
    }
  }
  const sorted = Object.fromEntries(Object.keys(metrics).sort().map((name) => [name, metrics[name]]));
  const baseline = {
    date: header.date,
    os: header.os,
    arch: header.arch,
    cpu: header.cpu,
    cores: header.cores,
    commit: header.commit,
    metrics: sorted,
  };
  return { baseline, added, replaced, kept };
}

const uname = (flag) => execFileSync("uname", [flag], { encoding: "utf8" }).trim().toLowerCase();

/** The facts about this machine that a baseline may carry. */
function machineHeader() {
  return {
    date: new Date().toISOString().slice(0, 10),
    os: uname("-s"),
    arch: uname("-m"),
    cpu: (cpus()[0]?.model ?? uname("-m")).trim(),
    cores: availableParallelism(),
    commit: execFileSync("git", ["rev-parse", "--short", "HEAD"], { encoding: "utf8" }).trim(),
  };
}

function main(argv) {
  const replace = argv.includes("--replace");
  const files = argv.filter((a) => a !== "--replace");
  if (files.length !== 2 || files.some((f) => f.startsWith("--"))) {
    process.stderr.write("usage: node scripts/perf-baseline.mjs <run.json> <baseline.json> [--replace]\n");
    return 2;
  }
  try {
    const run = JSON.parse(readFileSync(files[0], "utf8"));
    let existing = null;
    try {
      existing = JSON.parse(readFileSync(files[1], "utf8"));
    } catch (e) {
      if (e.code !== "ENOENT") throw e;
    }
    const { baseline, added, replaced, kept } = mergeBaseline(existing, run, machineHeader(), { replace });
    writeFileSync(files[1], `${JSON.stringify(baseline, null, 2)}\n`);
    process.stdout.write(`${files[1]}: ${added.length} added, ${replaced.length} replaced, ${kept.length} kept as they were\n`);
    if (kept.length > 0) process.stdout.write("kept (use --replace to record them again): " + kept.join(", ") + "\n");
    return 0;
  } catch (e) {
    process.stderr.write(`perf-baseline: ${e instanceof Error ? e.message : String(e)}\n`);
    return 2;
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) process.exit(main(process.argv.slice(2)));
