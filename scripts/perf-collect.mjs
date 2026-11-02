#!/usr/bin/env node
// The collector of the performance guard (POL-02, D-14): it turns the `PERF` lines of a log into the run file that
// `perf-compare.mjs` judges.
//
//   node scripts/perf-collect.mjs <log> <run.json>
//
// A measurement test prints one line per metric, `PERF {"name":..,"unit":..,"runs":..,"median":..,"p95":..}`, and writes no
// file, so the same lines work from a local run and from the log of a container run. A line that starts with `PERF {` is
// parsed as JSON only (never evaluated) and must have exactly those five fields; any other line is ignored, which includes
// the `PERF OK` and `PERF WORSE` verdict lines of the comparison. A log without a PERF line, a repeated name, a missing or
// extra field and a value that is not a positive finite number are errors and never an empty pass.
//
// Exit status: 0 the run file is written, 2 an error (nothing is written). Node built-ins only.

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const FIELDS = ["name", "unit", "runs", "median", "p95"];
// `group.metric` with lower-case words: never `__proto__`, `constructor` or a path.
const NAME = /^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$/;
const UNIT = /^[a-z][a-z_]*$/;
const PREFIX = "PERF {";

const positiveFinite = (v) => typeof v === "number" && Number.isFinite(v) && v > 0;

function parseLine(text, lineNumber) {
  let o;
  try {
    o = JSON.parse(text.slice("PERF ".length));
  } catch {
    throw new Error(`line ${lineNumber}: a PERF line that is not JSON`);
  }
  if (o === null || typeof o !== "object" || Array.isArray(o)) {
    throw new Error(`line ${lineNumber}: a PERF line that is not an object`);
  }
  for (const key of Object.keys(o)) {
    if (!FIELDS.includes(key)) throw new Error(`line ${lineNumber}: unknown field ${JSON.stringify(key)}`);
  }
  for (const key of FIELDS) {
    if (!Object.hasOwn(o, key)) throw new Error(`line ${lineNumber}: missing field ${key}`);
  }
  if (typeof o.name !== "string" || !NAME.test(o.name)) throw new Error(`line ${lineNumber}: bad name`);
  if (typeof o.unit !== "string" || !UNIT.test(o.unit)) throw new Error(`line ${lineNumber}: bad unit for ${o.name}`);
  if (!Number.isInteger(o.runs) || o.runs < 1) throw new Error(`line ${lineNumber}: runs of ${o.name} is not a positive integer`);
  for (const key of ["median", "p95"]) {
    if (!positiveFinite(o[key])) throw new Error(`line ${lineNumber}: ${key} of ${o.name} is not a positive finite number`);
  }
  return { name: o.name, entry: { median: o.median, p95: o.p95, unit: o.unit, runs: o.runs } };
}

/** The run of a log: `{ metrics: { name: { median, p95, unit, runs } } }`. Throws on anything that is not a clean log. */
export function collect(text) {
  const metrics = Object.create(null);
  let count = 0;
  const lines = String(text).split(/\r?\n/);
  for (let i = 0; i < lines.length; i += 1) {
    if (!lines[i].startsWith(PREFIX)) continue;
    const { name, entry } = parseLine(lines[i], i + 1);
    if (name in metrics) throw new Error(`line ${i + 1}: repeated metric ${name}`);
    metrics[name] = entry;
    count += 1;
  }
  if (count === 0) throw new Error("no PERF line in the log: the measurement did not run");
  return { metrics };
}

function main(argv) {
  if (argv.length !== 2) {
    process.stderr.write("usage: node scripts/perf-collect.mjs <log> <run.json>\n");
    return 2;
  }
  try {
    const run = collect(readFileSync(argv[0], "utf8"));
    writeFileSync(argv[1], `${JSON.stringify(run, null, 2)}\n`);
    process.stdout.write(`collected ${Object.keys(run.metrics).length} metrics into ${argv[1]}\n`);
    return 0;
  } catch (e) {
    process.stderr.write(`perf-collect: ${argv[0]}: ${e instanceof Error ? e.message : String(e)}\n`);
    return 2;
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) process.exit(main(process.argv.slice(2)));
