// The performance guard (POL-02, D-14, D-15): the collector that turns the PERF lines of a log into a run file, and the pure
// comparison of a run against a committed baseline. Neither touches the engine; this file is the proof that the guard can
// fail: a missing metric, a missing baseline, a unit mismatch and a log without a PERF line are failures and never a pass.
import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { hostname, tmpdir, userInfo } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { collect } from "../../../scripts/perf-collect.mjs";
import { compare, TOLERANCE } from "../../../scripts/perf-compare.mjs";
import { BASELINE_KEYS, mergeBaseline, parseMachine } from "../../../scripts/perf-baseline.mjs";

const script = (name) => fileURLToPath(new URL(`../../../scripts/${name}`, import.meta.url));

const line = (name, unit, median, p95 = median, runs = 5) =>
  `PERF ${JSON.stringify({ name, unit, runs, median, p95 })}`;
const metric = (unit, median) => ({ median, p95: median, unit, runs: 5 });
const verdictOf = (result, name) => result.results.find((r) => r.name === name).verdict;

// ---- the collector --------------------------------------------------------------------------------------------------

test("collect reads the PERF lines and ignores every other line", () => {
  const log = [
    "running 4 tests",
    "test perf_update_encode ... ",
    line("core.update_encode_ms", "ms", 1.25, 1.5),
    "PERF OK",
    "BUDGET update+encode: average 1ms",
    line("core.parse_mib_s", "mib_s", 310.5, 330),
    "test result: ok. 4 passed",
  ].join("\n");
  const { metrics } = collect(log);
  assert.deepEqual(Object.keys(metrics).sort(), ["core.parse_mib_s", "core.update_encode_ms"]);
  assert.deepEqual(metrics["core.update_encode_ms"], { median: 1.25, p95: 1.5, unit: "ms", runs: 5 });
  assert.equal(metrics["core.parse_mib_s"].unit, "mib_s");
});

test("collect throws on a log without any PERF line", () => {
  assert.throws(() => collect("test result: ok. 0 passed\n"), /no PERF line/);
  assert.throws(() => collect(""), /no PERF line/);
  // The verdict line of the comparison is not a measurement.
  assert.throws(() => collect("PERF OK\nPERF WORSE\n"), /no PERF line/);
});

test("collect throws on a repeated name", () => {
  const log = `${line("core.a_ms", "ms", 1)}\n${line("core.a_ms", "ms", 2)}`;
  assert.throws(() => collect(log), /repeated.*core\.a_ms/);
});

test("collect throws on a missing field and on an extra one", () => {
  for (const field of ["name", "unit", "runs", "median", "p95"]) {
    const o = { name: "core.a_ms", unit: "ms", runs: 5, median: 1, p95: 2 };
    delete o[field];
    assert.throws(() => collect(`PERF ${JSON.stringify(o)}`), /field|name/, `without ${field}`);
  }
  const extra = { name: "core.a_ms", unit: "ms", runs: 5, median: 1, p95: 2, host: "x" };
  assert.throws(() => collect(`PERF ${JSON.stringify(extra)}`), /unknown field/);
});

test("collect throws on a value that is not a positive finite number", () => {
  for (const bad of [0, -1, "5", null, true, [1]]) {
    for (const key of ["median", "p95"]) {
      const o = { name: "core.a_ms", unit: "ms", runs: 5, median: 1, p95: 2, [key]: bad };
      assert.throws(() => collect(`PERF ${JSON.stringify(o)}`), /positive finite/, `${key}=${JSON.stringify(bad)}`);
    }
  }
  // JSON has no NaN or Infinity; a line that spells them is not JSON and is refused as such.
  assert.throws(() => collect('PERF {"name":"core.a_ms","unit":"ms","runs":5,"median":NaN,"p95":1}'), /not JSON/);
  assert.throws(() => collect('PERF {"name":"core.a_ms","unit":"ms","runs":5,"median":1e999,"p95":1}'), /positive finite/);
});

test("collect throws on a runs that is not a positive integer", () => {
  for (const runs of [0, 2.5, -3, "5"]) {
    const o = { name: "core.a_ms", unit: "ms", runs, median: 1, p95: 2 };
    assert.throws(() => collect(`PERF ${JSON.stringify(o)}`), /runs/, `runs=${JSON.stringify(runs)}`);
  }
});

test("collect refuses a name that could address the prototype and a PERF line that is not JSON", () => {
  for (const name of ["__proto__", "constructor", "core", "Core.a", "core..a", "core.a b", "../x"]) {
    assert.throws(() => collect(line(name, "ms", 1)), /name/, name);
  }
  assert.throws(() => collect("PERF {not json"), /not JSON/);
  assert.throws(() => collect('PERF ["core.a_ms"]'), /no PERF line|not an object/);
});

test("collect returns an object that has no prototype to pollute", () => {
  const { metrics } = collect(line("core.a_ms", "ms", 1));
  assert.equal(Object.getPrototypeOf(metrics), null);
});

// ---- the comparison: the three tolerances at their exact limits -----------------------------------------------------

test("a time metric is ok up to 1.5 times the baseline plus 0.1 ms and worse above", () => {
  const baseline = { metrics: { "core.t_ms": metric("ms", 1.0) } };
  const at = (median) => compare(baseline, { metrics: { "core.t_ms": metric("ms", median) } });
  assert.equal(verdictOf(at(1.6), "core.t_ms"), "ok");
  assert.equal(at(1.6).ok, true);
  assert.equal(verdictOf(at(1.7), "core.t_ms"), "worse");
  assert.equal(at(1.7).ok, false);
  assert.equal(at(1.7).results[0].limit, 1.6);
});

test("a throughput metric is ok down to half the baseline and worse below", () => {
  const baseline = { metrics: { "core.p_mib_s": metric("mib_s", 300) } };
  const at = (median) => compare(baseline, { metrics: { "core.p_mib_s": metric("mib_s", median) } });
  assert.equal(verdictOf(at(150), "core.p_mib_s"), "ok");
  assert.equal(verdictOf(at(149), "core.p_mib_s"), "worse");
  assert.equal(at(149).ok, false);
  // A faster run is never worse, however fast.
  assert.equal(at(3000).ok, true);
});

test("a memory metric is ok up to 1.5 times the baseline plus 4 MiB and worse above", () => {
  const baseline = { metrics: { "core.m_mib": metric("mib", 20) } };
  const at = (median) => compare(baseline, { metrics: { "core.m_mib": metric("mib", median) } });
  assert.equal(verdictOf(at(34), "core.m_mib"), "ok");
  assert.equal(verdictOf(at(34.1), "core.m_mib"), "worse");
  assert.equal(at(34.1).results[0].limit, 34);
  assert.equal(at(1).ok, true);
});

test("a clearly better run is listed as better and does not fail", () => {
  const baseline = {
    metrics: { "core.t_ms": metric("ms", 4), "core.p_mib_s": metric("mib_s", 100), "core.m_mib": metric("mib", 40) },
  };
  const run = {
    metrics: { "core.t_ms": metric("ms", 0.5), "core.p_mib_s": metric("mib_s", 500), "core.m_mib": metric("mib", 10) },
  };
  const r = compare(baseline, run);
  assert.deepEqual(r.results.map((x) => x.verdict), ["better", "better", "better"]);
  assert.equal(r.ok, true);
});

test("the tolerance table is the one place that holds the numbers", () => {
  assert.deepEqual(Object.keys(TOLERANCE).sort(), ["mib", "mib_s", "ms"]);
  assert.deepEqual(TOLERANCE.ms, { factor: 1.5, slack: 0.1, better: "lower" });
  assert.deepEqual(TOLERANCE.mib_s, { factor: 0.5, slack: 0, better: "higher" });
  assert.deepEqual(TOLERANCE.mib, { factor: 1.5, slack: 4, better: "lower" });
});

// ---- the comparison: what it must refuse to pass --------------------------------------------------------------------

test("a baseline metric that the run lacks fails", () => {
  const baseline = { metrics: { "core.a_ms": metric("ms", 1), "core.b_ms": metric("ms", 1) } };
  const r = compare(baseline, { metrics: { "core.a_ms": metric("ms", 1) } });
  assert.equal(verdictOf(r, "core.b_ms"), "missing-in-run");
  assert.equal(r.ok, false);
});

test("a metric that the baseline lacks is listed and does not fail", () => {
  const baseline = { metrics: { "core.a_ms": metric("ms", 1) } };
  const r = compare(baseline, { metrics: { "core.a_ms": metric("ms", 1), "core.new_ms": metric("ms", 9) } });
  assert.equal(verdictOf(r, "core.new_ms"), "missing-in-baseline");
  assert.equal(r.ok, true);
});

test("a unit mismatch fails", () => {
  const baseline = { metrics: { "core.a_ms": metric("ms", 1) } };
  const r = compare(baseline, { metrics: { "core.a_ms": metric("mib", 1) } });
  assert.equal(verdictOf(r, "core.a_ms"), "unit-mismatch");
  assert.equal(r.ok, false);
});

test("a baseline without metrics, or without any in the scope, fails", () => {
  const run = { metrics: { "core.a_ms": metric("ms", 1) } };
  assert.equal(compare({ metrics: {} }, run).ok, false);
  const baseline = { metrics: { "core.a_ms": metric("ms", 1) } };
  const wrong = compare(baseline, run, { scope: ["server."] });
  assert.equal(wrong.ok, false, "a scope that matches nothing in the baseline is not a pass");
  assert.equal(compare(baseline, run, { scope: [] }).ok, false);
});

test("a scope judges only the metrics it names; the others are not measured and do not fail", () => {
  const baseline = { metrics: { "core.a_ms": metric("ms", 1), "server.b_ms": metric("ms", 1) } };
  const run = { metrics: { "core.a_ms": metric("ms", 1) } };
  const scoped = compare(baseline, run, { scope: ["core."] });
  assert.equal(verdictOf(scoped, "server.b_ms"), "not-measured");
  assert.equal(scoped.ok, true);
  // Without the scope the same run is incomplete.
  const whole = compare(baseline, run);
  assert.equal(verdictOf(whole, "server.b_ms"), "missing-in-run");
  assert.equal(whole.ok, false);
  // A metric of the run outside the scope is not judged either.
  const extra = compare(baseline, { metrics: { ...run.metrics, "server.b_ms": metric("ms", 99) } }, { scope: ["core."] });
  assert.equal(extra.ok, true);
});

test("a name that is an Object.prototype member is looked up as an own property only", () => {
  const baseline = { metrics: JSON.parse('{"toString":{"median":1,"p95":1,"unit":"ms","runs":5}}') };
  const r = compare(baseline, { metrics: {} });
  assert.equal(verdictOf(r, "toString"), "missing-in-run");
  assert.equal(r.ok, false);
});

test("compare throws on input that is not a baseline or a run", () => {
  assert.throws(() => compare(null, { metrics: {} }), /baseline/);
  assert.throws(() => compare({ metrics: {} }, {}), /run/);
  assert.throws(() => compare({ metrics: { "core.a": { median: 1, p95: 1, unit: "furlong", runs: 5 } } }, { metrics: {} }), /unit/);
  assert.throws(() => compare({ metrics: { "core.a_ms": { median: 0, p95: 1, unit: "ms", runs: 5 } } }, { metrics: {} }), /median/);
});

// ---- the command line -----------------------------------------------------------------------------------------------

function withFiles(files, body) {
  const dir = mkdtempSync(join(tmpdir(), "hw-perf-"));
  try {
    const paths = {};
    for (const [name, content] of Object.entries(files)) {
      paths[name] = join(dir, name);
      writeFileSync(paths[name], typeof content === "string" ? content : JSON.stringify(content));
    }
    return body(paths, dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}
const run = (name, args) => spawnSync(process.execPath, [script(name), ...args], { encoding: "utf8" });

test("perf-compare.mjs prints a line per metric, PERF OK and exits 0 on a pass", () => {
  const b = { metrics: { "core.a_ms": metric("ms", 1) } };
  withFiles({ "b.json": b, "r.json": b }, (p) => {
    const out = run("perf-compare.mjs", [p["b.json"], p["r.json"], "--scope", "core."]);
    assert.equal(out.status, 0, out.stderr);
    assert.match(out.stdout, /^ok core\.a_ms 1 -> 1 \(limit 1\.6\)$/m);
    assert.match(out.stdout, /^PERF OK$/m);
  });
});

test("perf-compare.mjs prints PERF WORSE and exits 1 on a regression", () => {
  const b = { metrics: { "core.a_ms": metric("ms", 1) } };
  const r = { metrics: { "core.a_ms": metric("ms", 3) } };
  withFiles({ "b.json": b, "r.json": r }, (p) => {
    const out = run("perf-compare.mjs", [p["b.json"], p["r.json"]]);
    assert.equal(out.status, 1);
    assert.match(out.stdout, /^worse core\.a_ms 1 -> 3 \(limit 1\.6\)$/m);
    assert.match(out.stdout, /^PERF WORSE$/m);
  });
});

test("perf-compare.mjs exits 2 and says why when a file is missing, unparsable or the arguments are wrong", () => {
  const b = { metrics: { "core.a_ms": metric("ms", 1) } };
  withFiles({ "b.json": b, "bad.json": "{not json" }, (p, dir) => {
    const missing = run("perf-compare.mjs", [join(dir, "nope.json"), p["b.json"]]);
    assert.equal(missing.status, 2);
    assert.match(missing.stderr, /nope\.json/);
    const bad = run("perf-compare.mjs", [p["b.json"], p["bad.json"]]);
    assert.equal(bad.status, 2);
    assert.match(bad.stderr, /bad\.json/);
    assert.equal(run("perf-compare.mjs", [p["b.json"]]).status, 2);
    assert.equal(run("perf-compare.mjs", [p["b.json"], p["b.json"], "--scope"]).status, 2);
    assert.equal(run("perf-compare.mjs", [p["b.json"], p["b.json"], "--bogus"]).status, 2);
    assert.doesNotMatch(run("perf-compare.mjs", [p["b.json"], p["bad.json"]]).stdout, /PERF OK/);
  });
});

test("perf-collect.mjs writes the run file, and exits 2 without writing it for a log with no PERF line", () => {
  withFiles({ "ok.log": `${line("core.a_ms", "ms", 1.5)}\n`, "empty.log": "nothing here\n" }, (p, dir) => {
    const out = join(dir, "run.json");
    const good = run("perf-collect.mjs", [p["ok.log"], out]);
    assert.equal(good.status, 0, good.stderr);
    assert.equal(JSON.parse(readFileSync(out, "utf8")).metrics["core.a_ms"].median, 1.5);
    const none = join(dir, "none.json");
    const bad = run("perf-collect.mjs", [p["empty.log"], none]);
    assert.equal(bad.status, 2);
    assert.match(bad.stderr, /no PERF line/);
    assert.equal(existsSync(none), false);
  });
});

// ---- the baseline files of the tree -----------------------------------------------------------------------------------

const root = fileURLToPath(new URL("../../../", import.meta.url));
const header = { date: "2026-11-02", os: "darwin", arch: "arm64", cpu: "Test CPU", cores: 8, commit: "abc1234" };

test("mergeBaseline adds new metrics, keeps recorded ones without --replace and replaces them with it", () => {
  const first = mergeBaseline(null, { metrics: { "core.a_ms": metric("ms", 1) } }, header);
  assert.deepEqual(first.added, ["core.a_ms"]);
  const later = { metrics: { "core.a_ms": metric("ms", 5), "server.b_ms": metric("ms", 2) } };
  const kept = mergeBaseline(first.baseline, later, { ...header, commit: "def5678" });
  assert.deepEqual(kept.added, ["server.b_ms"]);
  assert.deepEqual(kept.kept, ["core.a_ms"]);
  assert.equal(kept.baseline.metrics["core.a_ms"].median, 1, "a recorded metric is not overwritten");
  assert.equal(kept.baseline.commit, "def5678", "the header is the last recording");
  const replaced = mergeBaseline(first.baseline, later, header, { replace: true });
  assert.deepEqual(replaced.replaced, ["core.a_ms"]);
  assert.equal(replaced.baseline.metrics["core.a_ms"].median, 5);
});

test("mergeBaseline writes only the allowlisted keys and copies metrics field by field", () => {
  const dirty = { ...header, host: "my-mac", user: "me", path: "/Users/me" };
  const run = { metrics: { "core.a_ms": { ...metric("ms", 1), cwd: "/Users/me" } } };
  const { baseline } = mergeBaseline(null, run, dirty);
  assert.deepEqual(Object.keys(baseline).sort(), [...BASELINE_KEYS].sort());
  assert.deepEqual(Object.keys(baseline.metrics["core.a_ms"]).sort(), ["median", "p95", "runs", "unit"]);
});

test("mergeBaseline refuses to extend the baseline of another CPU and a metric that is not a record", () => {
  const first = mergeBaseline(null, { metrics: { "core.a_ms": metric("ms", 1) } }, header).baseline;
  assert.throws(() => mergeBaseline(first, { metrics: {} }, { ...header, cpu: "Other CPU" }), /another CPU/);
  assert.doesNotThrow(() => mergeBaseline(first, { metrics: {} }, { ...header, cpu: "Other CPU" }, { replace: true }));
  assert.throws(() => mergeBaseline(null, { metrics: { "core.a_ms": { median: 0, p95: 1, unit: "ms", runs: 5 } } }, header), /positive/);
});

test("parseMachine reads the one PERF-MACHINE line of a container log and nothing the file name could be built from badly", () => {
  const line = (o) => `PERF-MACHINE ${JSON.stringify(o)}`;
  const good = { os: "Linux", arch: "aarch64", cpu: "aarch64", cores: 10 };
  assert.deepEqual(parseMachine(`noise\n${line(good)}\nPERF {"name":"core.a_ms"}\n`), { os: "linux", arch: "aarch64", cpu: "aarch64", cores: 10 });
  assert.throws(() => parseMachine("no line here\n"), /0 PERF-MACHINE lines/);
  assert.throws(() => parseMachine(`${line(good)}\n${line(good)}\n`), /2 PERF-MACHINE lines/);
  assert.throws(() => parseMachine("PERF-MACHINE {not json}"), /not JSON/);
  assert.throws(() => parseMachine(line({ ...good, host: "box" })), /unknown field/);
  assert.throws(() => parseMachine(line({ os: "Linux", arch: "aarch64", cpu: "x" })), /lacks cores/);
  for (const bad of [{ os: "../etc" }, { os: "linux/x" }, { arch: "aarch64; rm" }, { arch: "a/b" }, { cpu: "x\ny" }, { cpu: "/Users/me" + "x".repeat(80) }, { cores: 0 }, { cores: 1.5 }, { cores: "8" }]) {
    assert.throws(() => parseMachine(line({ ...good, ...bad })), /PERF-MACHINE/, JSON.stringify(bad));
  }
});

function everyString(value, out = []) {
  if (typeof value === "string") out.push(value);
  else if (value !== null && typeof value === "object") for (const v of Object.values(value)) everyString(v, out);
  return out;
}

test("every perf/*.json of the tree has the allowlisted keys, valid metrics and no personal data", () => {
  const dir = `${root}perf/`;
  const files = existsSync(dir) ? readdirSync(dir).filter((f) => f.endsWith(".json")) : [];
  for (const file of files) {
    const doc = JSON.parse(readFileSync(`${dir}${file}`, "utf8"));
    assert.deepEqual(Object.keys(doc).filter((k) => !BASELINE_KEYS.includes(k)), [], `${file}: a key outside the allowlist`);
    assert.ok(Object.keys(doc.metrics).length > 0, `${file}: no metrics`);
    for (const [name, m] of Object.entries(doc.metrics)) {
      assert.deepEqual(Object.keys(m).sort(), ["median", "p95", "runs", "unit"], `${file}: ${name}`);
      for (const key of ["median", "p95"]) {
        assert.ok(typeof m[key] === "number" && Number.isFinite(m[key]) && m[key] > 0, `${file}: ${name} ${key}`);
      }
      assert.ok(Number.isInteger(m.runs) && m.runs >= 1, `${file}: ${name} runs`);
      assert.ok(Object.hasOwn(TOLERANCE, m.unit), `${file}: ${name} unit`);
    }
    const strings = everyString(doc);
    for (const text of strings) {
      assert.doesNotMatch(text, /\/(Users|home|root|var|private|tmp)\//, `${file}: a path in ${text}`);
    }
    for (const own of [hostname(), userInfo().username, hostname().split(".")[0]]) {
      assert.ok(own.length > 0 && !strings.includes(own), `${file}: carries the name of this host or user`);
    }
  }
});

test("no file under .github mentions perf- (the guard is run by a person, never by CI: D-12)", () => {
  const walk = (dir) =>
    readdirSync(dir).flatMap((name) => {
      const path = `${dir}/${name}`;
      return statSync(path).isDirectory() ? walk(path) : [path];
    });
  const dir = `${root}.github`;
  for (const file of existsSync(dir) ? walk(dir) : []) {
    assert.doesNotMatch(readFileSync(file, "utf8"), /perf-/, `${file} reaches the performance guard`);
  }
});
