// Checks that Chromium and WebKit negotiate permessage-deflate with the candidate and carry ten round trips of a
// 1 MiB text message with identical content. The browsers are the ones the page tests use (playwright-core from
// app/node_modules, launched by app/tests/browser/lib.mjs).
//
//   node browser-check.mjs [port]
//
// With a port it talks to a candidate that is already running on 127.0.0.1. Without one it starts target/debug/candidate
// (build it first with `cargo build --bin candidate`) and stops it at the end. The result of each engine is written into
// results.json under `browsers`; an engine that cannot be launched is recorded as not run, with the reason.

import { spawn } from "node:child_process";
import { readFileSync, renameSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { launch } from "../../app/tests/browser/lib.mjs";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const RESULTS = `${HERE}results.json`;
const ROUND_TRIPS = 10;
const MESSAGE_BYTES = 1024 * 1024;

function record(engine, value) {
  let all = {};
  try {
    all = JSON.parse(readFileSync(RESULTS, "utf8"));
  } catch {
    // no results yet
  }
  all.browsers = { ...(all.browsers ?? {}), [engine]: value };
  writeFileSync(`${RESULTS}.tmp`, JSON.stringify(all, null, 2));
  renameSync(`${RESULTS}.tmp`, RESULTS);
}

async function startCandidate() {
  const child = spawn(`${HERE}target/debug/candidate`, [], { stdio: ["ignore", "pipe", "inherit"] });
  const port = await new Promise((resolve, reject) => {
    let seen = "";
    child.once("error", reject);
    child.once("exit", () => reject(new Error("the candidate exited before it listened")));
    child.stdout.on("data", (chunk) => {
      seen += chunk;
      const m = /listening 127\.0\.0\.1:(\d+)/.exec(seen);
      if (m) resolve(Number(m[1]));
    });
  });
  return { child, port };
}

/** Runs in the page: connect, report the negotiated extensions, and do the round trips. */
async function inPage({ url, count, size }) {
  // Text that is not a repeat of one character: words from a small linear congruential generator.
  const words = ["terminal", "frame", "row", "cursor", "scroll", "color", "pane", "tab", "echo", "buffer", "line", "cell"];
  let seed = 12345;
  const parts = [];
  let length = 0;
  while (length < size) {
    seed = (seed * 1103515245 + 12345) & 0x7fffffff;
    const word = `${words[seed % words.length]}${seed % 977} `;
    parts.push(word);
    length += word.length;
  }
  const base = parts.join("").slice(0, size);

  const ws = new WebSocket(url);
  await new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = () => reject(new Error("the WebSocket could not connect"));
  });
  const extensions = ws.extensions;
  let identical = true;
  const started = performance.now();
  for (let i = 0; i < count; i++) {
    const sent = `${i}:${base}`.slice(0, size);
    const reply = new Promise((resolve, reject) => {
      ws.onmessage = (e) => resolve(e.data);
      ws.onclose = () => reject(new Error("the connection closed during a round trip"));
    });
    ws.send(sent);
    const got = await reply;
    if (got !== sent) identical = false;
  }
  const milliseconds = Math.round(performance.now() - started);
  ws.onclose = null;
  ws.close();
  return { extensions, identical, milliseconds };
}

let started = null;
let port = Number(process.argv[2]);
if (!port) {
  started = await startCandidate();
  port = started.port;
}

let failed = false;
for (const engine of ["chromium", "webkit"]) {
  let browser;
  try {
    browser = await launch(engine);
  } catch (e) {
    const reason = `${engine} could not be launched: ${String(e.message).split("\n")[0]}`;
    console.error(reason);
    record(engine, { ran: false, reason });
    failed = true;
    continue;
  }
  try {
    const page = await browser.newPage();
    // A blank page that the candidate serves from loopback: Chromium blocks a WebSocket to a local address from
    // about:blank, and from a page whose response the test script made up.
    await page.goto(`http://127.0.0.1:${port}/blank`);
    const r = await page.evaluate(inPage, { url: `ws://127.0.0.1:${port}/echo`, count: ROUND_TRIPS, size: MESSAGE_BYTES });
    const value = {
      ran: true,
      version: browser.version(),
      extensions: r.extensions,
      round_trips: ROUND_TRIPS,
      message_bytes: MESSAGE_BYTES,
      identical: r.identical,
      milliseconds: r.milliseconds,
    };
    record(engine, value);
    console.log(`${engine} ${value.version}: extensions "${r.extensions}", ${ROUND_TRIPS} round trips of ${MESSAGE_BYTES} bytes, identical ${r.identical}, ${r.milliseconds} ms`);
    if (!r.extensions.includes("permessage-deflate") || !r.identical) failed = true;
  } catch (e) {
    const reason = `${engine} failed: ${String(e.message).split("\n")[0]}`;
    console.error(reason);
    record(engine, { ran: true, extensions: "", identical: false, reason });
    failed = true;
  } finally {
    await browser.close();
  }
}

if (started) started.child.kill();
process.exit(failed ? 1 : 0);
