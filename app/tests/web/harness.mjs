// The client of the Rust harness (server/tests/web_harness.rs): builds it once, starts one server for a test file, sends it
// commands as JSON lines and stops it. Closing the harness's standard input is what stops the server, so a test that dies
// still leaves nothing behind.
//
// Secrets: the harness prints the one-time setup token in its `ready` line and the secrets of a person in the reply to the
// `account` command, nowhere else. This module hands them to the caller and never prints them; the tail kept for a failed
// start holds only the lines that carry no secret.
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { Authenticator, stepOf } from "./totp.mjs";

const ROOT = fileURLToPath(new URL("../../../", import.meta.url));
const START_LIMIT_MS = 90_000;
const STOP_LIMIT_MS = 20_000;
const TAIL_LINES = 40;

/** Build the test binary of the harness (once per run) and return its path. */
export function buildHarness() {
  const built = spawnSync(
    "cargo",
    ["test", "--locked", "--offline", "-p", "holzwave-server", "--features", "in-process-workers", "--test", "web_harness", "--no-run", "--message-format=json"],
    { cwd: ROOT, encoding: "utf8", maxBuffer: 256 * 1024 * 1024 },
  );
  if (built.error) throw new Error(`cargo could not be started (source scripts/env.sh first): ${built.error.message}`);
  let executable = null;
  for (const line of built.stdout.split("\n")) {
    if (!line.startsWith("{")) continue;
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      continue;
    }
    if (message.reason === "compiler-artifact" && message.target?.name === "web_harness" && message.executable) executable = message.executable;
  }
  if (built.status !== 0 || executable === null) {
    const diagnostics = built.stderr.split("\n").slice(-30).join("\n");
    throw new Error(`the harness did not build (cargo status ${built.status}):\n${diagnostics}`);
  }
  return executable;
}

let binary = process.env.HW_HARNESS_BIN ?? null;
const harnessBinary = () => (binary ??= buildHarness());

const children = new Set();
process.on("exit", () => {
  for (const child of children) child.kill("SIGKILL");
});

/** The lines of a stream, one callback each. */
function lines(stream, each) {
  let rest = "";
  stream.setEncoding("utf8");
  stream.on("data", (chunk) => {
    rest += chunk;
    let at;
    while ((at = rest.indexOf("\n")) >= 0) {
      each(rest.slice(0, at));
      rest = rest.slice(at + 1);
    }
  });
}

/**
 * Start one server. `options` go to the harness's `start` command (`upload_max_bytes`, `oidc`). Resolves with
 * `{ origin, port, host, bootstrap, certificate, account(options), command(object), stop() }`.
 */
export async function startHarness(options = {}) {
  const child = spawn(harnessBinary(), ["--ignored", "--nocapture", "serve"], { cwd: ROOT, stdio: ["pipe", "pipe", "pipe"] });
  children.add(child);
  const tail = [];
  const keep = (line) => {
    tail.push(line);
    if (tail.length > TAIL_LINES) tail.shift();
  };
  const waiting = new Map();
  let nextId = 1;
  let ready;
  let failed;
  const readyPromise = new Promise((resolve, reject) => {
    ready = resolve;
    failed = reject;
  });
  let exited = false;
  const exit = new Promise((resolve) => child.once("exit", (code) => { exited = true; resolve(code); }));

  lines(child.stdout, (line) => {
    if (!line.startsWith("HARNESS ")) return keep(line);
    let message;
    try {
      message = JSON.parse(line.slice("HARNESS ".length));
    } catch {
      return keep("(a harness line that is not JSON)");
    }
    if (message.event === "ready") return ready(message);
    if (message.event === "error") {
      keep(`harness error: ${message.text}`);
      return failed(new Error(`the harness could not start: ${message.text}`));
    }
    if (message.event === "info" || message.event === "stopped") return keep(`harness: ${message.text ?? message.event}`);
    if ("reply" in message) {
      const entry = waiting.get(message.reply);
      waiting.delete(message.reply);
      if (entry) entry(message);
    }
  });
  lines(child.stderr, keep);
  child.stdin.on("error", () => {});
  child.once("exit", (code) => {
    children.delete(child);
    failed(new Error(`the harness ended with status ${code}`));
    for (const entry of waiting.values()) entry({ ok: false, error: "the harness ended" });
    waiting.clear();
  });

  const send = (object) => child.stdin.write(`${JSON.stringify(object)}\n`);
  send({ op: "start", ...options });

  const timer = setTimeout(() => failed(new Error(`no ready line from the harness after ${START_LIMIT_MS / 1000} s`)), START_LIMIT_MS);
  let info;
  try {
    info = await readyPromise;
  } catch (e) {
    child.kill("SIGKILL");
    throw new Error(`${e.message}\nlast output of the harness:\n${tail.join("\n")}`);
  } finally {
    clearTimeout(timer);
  }

  const command = (object, limitMs = 60_000) =>
    new Promise((resolve, reject) => {
      const id = nextId++;
      const timeout = setTimeout(() => {
        waiting.delete(id);
        reject(new Error(`timeout: no answer from the harness to ${object.op} within ${limitMs / 1000} s`));
      }, limitMs);
      waiting.set(id, (message) => {
        clearTimeout(timeout);
        resolve(message);
      });
      send({ ...object, id });
    });

  const url = new URL(info.origin);
  return {
    origin: info.origin,
    host: url.host,
    hostname: url.hostname,
    port: Number(info.port),
    bootstrap: info.bootstrap,
    certificate: info.cert_der_b64,
    command,
    /**
     * A person made through the enrolment routes: `{ username, password, secret, recovery, passphrase, authenticator }`.
     * `authenticator` hands out the codes the server can still accept for this person.
     */
    async account(request = {}) {
      const answer = await command({ op: "account", ...request });
      if (!answer.ok) throw new Error(`the harness could not make the person: ${answer.error}`);
      const { reply, ok, error, ...person } = answer;
      // The harness spent the step before the one it was in; this is a bound that is never too low.
      const authenticator = new Authenticator(person.secret, stepOf(Math.floor(Date.now() / 1000)) - 1);
      return { ...person, authenticator };
    },
    async stop() {
      if (exited) return;
      child.stdin.end();
      const code = await Promise.race([exit, new Promise((done) => setTimeout(() => done("timeout"), STOP_LIMIT_MS))]);
      if (code === "timeout") {
        child.kill("SIGKILL");
        throw new Error(`the harness did not stop within ${STOP_LIMIT_MS / 1000} s`);
      }
      if (code !== 0) throw new Error(`the harness ended with status ${code}`);
    },
  };
}
