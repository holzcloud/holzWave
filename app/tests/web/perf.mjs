// The performance measurements of the browser against the real server (POL-02, D-14, D-16).
//   node tests/web/perf.mjs [--engine webkit|chromium]
//
// Per engine: the time from a key to the next painted frame, and the time from a reload to the first painted frame of a pane that
// kept running. Both are taken in the page itself, with `performance.now()`, and not from the timing of the test driver:
//
//   web.<engine>.key_to_paint_p50_ms, web.<engine>.key_to_paint_p99_ms
//       100 keys one at a time, 150 ms apart, in 5 rounds (one p50 and one p99 per round). A key is the moment of the `keydown`
//       event at its capture phase; its frame is the first frame message that the page's socket reads after it; and the paint
//       is the task that runs after the animation frame that follows that message (the page draws in its own animation frame,
//       which was asked for after ours, so ours runs first and the task after it sees the frame drawn).
//   web.<engine>.reattach_paint_ms
//       10 reloads of a signed-in page with the pane still running: from the start of the navigation (the origin of
//       `performance.now()` of the new document) to the same paint of the first frame message.
//
// A frame of the real server is a JSON text message `{"event":"frame","payload":{...}}` on the page's WebSocket (docs/architecture.md,
// "Frame protocol"; only a file or a thumbnail travels as a binary message). The hook below recognises a frame by that prefix.
// A key after which no frame message arrives, or a reload that never gets one, fails the file with that reason; a value is never
// guessed or filled in with zero.
//
// This file is not named `*.test.mjs`, so `test:web` and the runner (run.mjs) never find it: a person runs it on request, through
// `sh scripts/perf-check.sh web`. Without `HW_ENGINE` it is the driver: it builds the harness once and runs itself once per engine.
// With `HW_ENGINE` set it is the body for that engine. It prints `PERF {json}` lines (one per metric) and nothing else about the
// person: the secrets that `harness.account()` hands to the caller (password, authenticator, recovery code) are used for the
// sign-in and never printed.
//
// Only the real-server test library (lib.mjs, harness.mjs) and the browser library (../browser/lib.mjs) are used, none edited.
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { DIST_DIR, ENGINES } from "../browser/lib.mjs";
import { buildHarness } from "./harness.mjs";
import { check, runTest, signIn } from "./lib.mjs";

const here = fileURLToPath(import.meta.url);

const KEYS_PER_ROUND = 100;
const ROUNDS = 5;
const KEY_PAUSE_MS = 150;
const WARM_UP_KEYS = 20;
const RELOADS = 10;
const FRAME_WITHIN_MS = 5_000;

// ---- the driver ---------------------------------------------------------------------------------------------------------

if (process.env.HW_ENGINE === undefined) {
  const { values } = parseArgs({ options: { engine: { type: "string" } }, strict: true });
  if (values.engine !== undefined && !ENGINES.includes(values.engine)) {
    console.error(`--engine must be one of: ${ENGINES.join(", ")}`);
    process.exit(2);
  }
  if (!existsSync(`${DIST_DIR}index.html`)) {
    console.error("app/dist/index.html is missing: run `npm run build --prefix app` first");
    process.exit(2);
  }
  let binary;
  try {
    binary = buildHarness();
  } catch (e) {
    console.error(`perf-web: ${e.message}`);
    process.exit(1);
  }
  const engines = values.engine ? [values.engine] : [...ENGINES].sort().reverse(); // chromium, webkit
  let failed = 0;
  for (const engine of engines) {
    const child = spawn(process.execPath, [here], {
      cwd: fileURLToPath(new URL("../../", import.meta.url)),
      env: { ...process.env, HW_ENGINE: engine, HW_HARNESS_BIN: binary },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let output = "";
    child.stdout.on("data", (d) => (output += d));
    child.stderr.on("data", (d) => (output += d));
    const kill = () => child.kill("SIGKILL");
    process.once("exit", kill);
    const status = await new Promise((resolve) => child.on("exit", (code, signal) => resolve(code ?? (signal ? 1 : 0))));
    process.off("exit", kill);
    const lines = output.split("\n");
    // A PERF line must start its own line for the collector; print them as they are.
    for (const line of lines) if (line.startsWith("PERF {")) console.log(line);
    if (status !== 0) {
      failed += 1;
      console.error(`perf-web: ${engine} FAILED (status ${status})`);
      for (const line of lines.filter((l) => !l.startsWith("PERF {")).slice(-25)) console.error(`    ${line}`);
    } else {
      console.log(`perf-web: ${engine} ok`);
    }
  }
  process.exit(failed > 0 ? 1 : 0);
}

// ---- the hook in the page -----------------------------------------------------------------------------------------------

/**
 * Runs in every document of the context before the page's own scripts. It wraps the constructor of the WebSocket so that each
 * socket gets a `message` listener before the page can set its own handler (listeners run in the order they were added), and
 * it records the `keydown` events at the capture phase. Nothing it records leaves the page until the test asks.
 */
function hook() {
  const state = {
    keys: 0, // keydown events seen
    waiting: false, // a key was pressed and its frame has not come yet
    keyAt: 0,
    samples: [], // ms from a keydown to the paint of its frame
    frames: 0, // frame messages seen
    firstFramePaint: null, // performance.now() at the paint of the first frame message of this document
    nonFrameAfterKey: 0,
  };
  window.__hwPerf = state;

  const afterPaint = (done) => {
    // The task that follows the animation frame: the page's own draw (asked for later in the same message task) has run.
    requestAnimationFrame(() => {
      const channel = new MessageChannel();
      channel.port1.onmessage = () => {
        channel.port1.close();
        done(performance.now());
      };
      channel.port2.postMessage(0);
    });
  };

  const onMessage = (ev) => {
    const data = ev.data;
    if (typeof data !== "string" || !data.startsWith('{"event":"frame"')) {
      if (state.waiting) state.nonFrameAfterKey += 1;
      return;
    }
    state.frames += 1;
    if (state.frames === 1) afterPaint((now) => (state.firstFramePaint = now));
    if (state.waiting) {
      state.waiting = false;
      const keyAt = state.keyAt;
      afterPaint((now) => state.samples.push(now - keyAt));
    }
  };

  const Native = window.WebSocket;
  window.WebSocket = new Proxy(Native, {
    construct(target, args) {
      const socket = new target(...args);
      socket.addEventListener("message", onMessage);
      return socket;
    },
  });

  window.addEventListener(
    "keydown",
    () => {
      state.keys += 1;
      state.keyAt = performance.now();
      state.waiting = true;
    },
    true,
  );
}

// ---- statistics ---------------------------------------------------------------------------------------------------------

/** The nearest-rank percentile, as the Rust helper (`core/tests/perf_report`) computes it. */
function percentile(values, p) {
  check(values.length > 0, "a percentile of no values");
  const sorted = [...values].sort((a, b) => a - b);
  const rank = Math.ceil((p / 100) * sorted.length);
  return sorted[Math.min(Math.max(rank, 1), sorted.length) - 1];
}

const median = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  const n = sorted.length;
  return n % 2 === 1 ? sorted[(n - 1) / 2] : (sorted[n / 2 - 1] + sorted[n / 2]) / 2;
};

/** One `PERF` line: the value of each run, as the other measurements print them. */
function report(name, unit, values) {
  check(values.length > 0, `${name}: no run`);
  for (const v of values) check(Number.isFinite(v) && v > 0, `${name}: a run measured ${v}, which is not a positive finite number`);
  console.log(`PERF ${JSON.stringify({ name, unit, runs: values.length, median: median(values), p95: percentile(values, 95) })}`);
}

// ---- the body -----------------------------------------------------------------------------------------------------------

/** Waits until `fn` (run in the page, with `arg`) returns something other than undefined/null/false, or fails with `what`. */
async function until(page, fn, what, ms = FRAME_WITHIN_MS, arg = undefined) {
  const deadline = Date.now() + ms;
  for (;;) {
    const value = await page.evaluate(fn, arg);
    if (value !== undefined && value !== null && value !== false) return value;
    if (Date.now() > deadline) throw new Error(`${what} (nothing within ${ms / 1000} s)`);
    await page.waitForTimeout(20);
  }
}

await runTest("perf", {}, async ({ engine, browser, harness }) => {
  const person = await harness.account({ role: "admin", name: "perf-person" });
  // A plain context, not the recording one of the test library: it listens to nothing, so that no driver event is in the numbers.
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1100, height: 700 } });
  await context.addInitScript(hook);
  const page = await context.newPage();
  await signIn(page, harness, person);
  await page.waitForSelector("canvas#term");

  // The shell becomes `cat`, so that every typed character is one echo and a frame, with nothing a prompt adds.
  await until(page, () => window.__hwPerf?.frames > 0 || undefined, "the page never showed a first frame of the pane");
  await page.locator("canvas#term").click();
  await page.keyboard.type("exec cat");
  await page.keyboard.press("Enter");
  await page.waitForTimeout(1_000);

  /** Types `count` characters one at a time and returns the milliseconds from each keydown to the paint of its frame. */
  const typeRun = async (count) => {
    await page.evaluate(() => {
      window.__hwPerf.samples.length = 0;
      window.__hwPerf.waiting = false;
      window.__hwPerf.nonFrameAfterKey = 0;
    });
    for (let i = 0; i < count; i++) {
      const before = await page.evaluate(() => window.__hwPerf.samples.length);
      await page.keyboard.press(String.fromCharCode(97 + (i % 26)));
      try {
        await until(page, (n) => (window.__hwPerf.samples.length > n ? true : undefined), "no sample", FRAME_WITHIN_MS, before);
      } catch {
        throw new Error(
          `no frame message reached the page after key ${i + 1} of the run; the hook looks for a text message that starts with {"event":"frame"} ` +
            `on the page's WebSocket (messages after the key that were something else: ${await page.evaluate(() => window.__hwPerf.nonFrameAfterKey)})`,
        );
      }
      await page.waitForTimeout(KEY_PAUSE_MS);
    }
    const samples = await page.evaluate(() => window.__hwPerf.samples.slice());
    check(samples.length === count, `${count} keys gave ${samples.length} samples`);
    return samples;
  };

  // Warm up (the page's code paths, the glyph atlas, the server's allocator), then Enter for a fresh line.
  await typeRun(WARM_UP_KEYS);
  await page.keyboard.press("Enter");
  await page.waitForTimeout(300);

  const p50 = [];
  const p99 = [];
  for (let round = 0; round < ROUNDS; round++) {
    const samples = await typeRun(KEYS_PER_ROUND);
    p50.push(percentile(samples, 50));
    p99.push(percentile(samples, 99));
    await page.keyboard.press("Enter");
    await page.waitForTimeout(300);
  }
  report(`web.${engine}.key_to_paint_p50_ms`, "ms", p50);
  report(`web.${engine}.key_to_paint_p99_ms`, "ms", p99);

  // Reattach: the same signed-in page, reloaded, with the pane still running.
  const paints = [];
  for (let i = 0; i < RELOADS; i++) {
    await page.reload();
    const painted = await until(
      page,
      () => window.__hwPerf?.firstFramePaint ?? undefined,
      `reload ${i + 1}: the page painted no frame of the pane`,
      15_000,
    );
    paints.push(painted);
    await page.waitForTimeout(300);
  }
  report(`web.${engine}.reattach_paint_ms`, "ms", paints);

  await context.close().catch(() => {});
});
