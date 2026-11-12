// The words and the reading of connections on the page (connstate.ts): no DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  DETACH, PHASES, TAKE_OVER, WRITERS, WRITER_LABELS, connDot, connSummary, connText, opensHere, paneConns, paneOfSession, readConnections,
  readPaneConns, readRemote, setPaneConns,
} from "../../dist/connstate.js";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const record = readFileSync(join(ROOT, "docs", "design.md"), "utf8");
const sessions = record.slice(record.indexOf("### 6.34 Server connection and sessions"), record.indexOf("### 6.35 Devices and signing in to a server"));

const conn = (over = {}) => ({ pane: 1, profile: "talos", name: "Talos", state: "connected", attempt: 0, of: 0, secs: 0, ...over });

test("an answer is read entry by entry and what is not well formed is dropped", () => {
  const list = readPaneConns([
    conn(),
    { pane: "x", profile: "a", name: "b", state: "connected" },
    { pane: 2, profile: "a", name: "b", state: "exploded" },
    null,
    conn({ pane: 3, attempt: -4, of: 5.9, secs: Number.NaN }),
  ]);
  assert.deepEqual(list.map((c) => c.pane), [1, 3]);
  assert.deepEqual([list[1].attempt, list[1].of, list[1].secs], [0, 5, 0], "counts are whole and not negative");
  assert.deepEqual(readPaneConns(null), []);
  assert.deepEqual(readPaneConns({}), []);
});

test("the same answer twice is no change, a different one is", () => {
  paneConns.clear();
  assert.equal(setPaneConns([conn()]), true);
  assert.equal(setPaneConns([conn()]), false);
  assert.equal(setPaneConns([conn({ state: "reconnecting", attempt: 1, of: 5, secs: 3 })]), true);
  assert.equal(setPaneConns([]), true);
  assert.equal(paneConns.size, 0);
});

test("the header says what a connection is doing", () => {
  assert.equal(connText(conn()), "Talos");
  assert.equal(connText(conn({ state: "connecting" })), "Talos · connecting");
  assert.equal(connText(conn({ state: "reconnecting", attempt: 2, of: 5, secs: 3 })), "Talos · ⟳ 2/5 in 3 s");
  assert.equal(connText(conn({ state: "reconnecting", attempt: 2, of: 5, secs: 0 })), "Talos · ⟳ 2/5");
  assert.equal(connText(conn({ state: "failed" })), "Talos · failed, Enter retries");
  assert.equal(connText(conn({ state: "closed" })), "Talos · closed");
});

test("the dot of a pane that is not fine has a shape of its own", () => {
  assert.equal(connDot(undefined), null);
  assert.equal(connDot(conn()), null);
  assert.equal(connDot(conn({ state: "reconnecting" })), "wait");
  assert.equal(connDot(conn({ state: "connecting" })), "wait");
  assert.equal(connDot(conn({ state: "failed" })), "failed");
});

test("the status line counts the connections and the ones that are down", () => {
  assert.equal(connSummary([]), "");
  assert.equal(connSummary([conn(), conn({ pane: 2 })]), "ssh 2");
  assert.equal(connSummary([conn(), conn({ pane: 2, state: "reconnecting" })]), "ssh 2 · 1 down");
});

test("the saved list is read with defaults for what is missing", () => {
  const list = readConnections([
    { id: "a", name: "A", kind: "ssh", host: "h", port: 2222, auth: "password", has_secret: true, jump: "b" },
    { id: "b", name: "B", kind: "docker" },
    { id: "c", name: "C", kind: "telnet" },
    { name: "no id", kind: "ssh" },
  ]);
  assert.deepEqual(list.map((c) => c.id), ["a", "b"]);
  assert.equal(list[0].port, 2222);
  assert.equal(list[0].auth, "password");
  assert.equal(list[0].jump, "b");
  assert.equal(list[0].has_secret, true);
  assert.equal(list[1].port, 22);
  assert.equal(list[1].has_secret, false);
  assert.equal(list[1].auth, "key");
});

test("readConnections reads wsh as a strict boolean", () => {
  const entry = (over = {}) => ({ id: "a", name: "A", kind: "ssh", ...over });
  const wsh = (over) => readConnections([entry(over)])[0].wsh;
  assert.equal(wsh({ wsh: true }), true);
  assert.equal(wsh({}), false, "absent is false");
  assert.equal(wsh({ wsh: false }), false);
  assert.equal(wsh({ wsh: 1 }), false, "a number is not true");
  assert.equal(wsh({ wsh: "true" }), false, "a string is not true");
  assert.equal(wsh({ wsh: null }), false);
});

test("readConnections reads agent as a login", () => {
  const login = (auth) => readConnections([{ id: "a", name: "A", kind: "ssh", ...(auth === undefined ? {} : { auth }) }])[0].auth;
  assert.equal(login("agent"), "agent");
  assert.equal(login("interactive"), "interactive");
  assert.equal(login("password"), "password");
  assert.equal(login("key"), "key");
  assert.equal(login(undefined), "key", "absent is the key file, as before");
  for (const odd of ["Agent", "Interactive", "", 1, null, true, {}]) assert.equal(login(odd), "key", `${JSON.stringify(odd)} falls back to key`);
});

test("readConnections_reads_interactive_as_a_login", () => {
  const login = (auth) => readConnections([{ id: "a", name: "A", kind: "ssh", auth }])[0].auth;
  assert.equal(login("interactive"), "interactive");
  // Only the exact word: anything else is the key file, as before.
  for (const odd of ["INTERACTIVE", "interactive ", "keyboard-interactive", "code", 2, [], {}]) {
    assert.equal(login(odd), "key", `${JSON.stringify(odd)} falls back to key`);
  }
});

test("a WSL connection opens on Windows only and a Docker connection not on Android", () => {
  for (const os of ["windows", ""]) assert.equal(opensHere("wsl", os), true, os);
  for (const os of ["macos", "linux", "android", "web"]) assert.equal(opensHere("wsl", os), false, os);
  for (const os of ["windows", "macos", "linux", "web", ""]) assert.equal(opensHere("docker", os), true, os);
  assert.equal(opensHere("docker", "android"), false);
  for (const os of ["windows", "macos", "linux", "android", "web", ""]) assert.equal(opensHere("ssh", os), true, os);
});

// ---- a pane that shows a session of a server (docs/design.md 6.34) ------------------------------------------------------------------

const REMOTE = { writer: "watching", session: 7, cols: 120, rows: 40, more: true, phase: "" };
const remoteConn = (over = {}) => ({ pane: 4, profile: "", name: "holz.example.net", state: "connected", attempt: 0, of: 0, secs: 0, remote: { ...REMOTE, ...over } });

test("the remote member is read from its closed sets and within the clamps", () => {
  const [read] = readPaneConns([remoteConn()]);
  assert.deepEqual(read.remote, REMOTE);
  for (const writer of WRITERS) assert.equal(readRemote({ ...REMOTE, writer }).writer, writer);
  for (const phase of PHASES) assert.equal(readRemote({ ...REMOTE, phase }).phase, phase);
  assert.equal(readRemote({ ...REMOTE, phase: "exploding" }).phase, "", "a phase the page does not know reads as none");
  assert.equal(readRemote({ ...REMOTE, phase: 3 }).phase, "");
  for (const bad of [
    null, [], "remote", { ...REMOTE, writer: "WRITING" }, { ...REMOTE, writer: "owner" }, { ...REMOTE, session: -1 }, { ...REMOTE, session: 1.5 },
    { ...REMOTE, cols: 0 }, { ...REMOTE, cols: 1001 }, { ...REMOTE, rows: 501 }, { ...REMOTE, more: "yes" },
  ]) {
    assert.equal(readRemote(bad), null, JSON.stringify(bad));
  }
  const canary = readRemote({ ...REMOTE, token: "hwd_" + "A".repeat(43) });
  assert.deepEqual(Object.keys(canary).sort(), ["cols", "more", "phase", "rows", "session", "writer"], "no member the page does not name");
});

test("a pane without a well formed remote member reads exactly as before", () => {
  const plain = conn();
  assert.deepEqual(readPaneConns([plain]), [plain]);
  assert.equal("remote" in readPaneConns([plain])[0], false);
  const malformed = readPaneConns([{ ...plain, remote: { writer: "nobody" } }])[0];
  assert.deepEqual(malformed, plain, "a malformed member is ignored and leaves no key");
});

test("the header of a remote pane says the server's host, then the session's size when it differs from the tile", () => {
  const [read] = readPaneConns([remoteConn()]);
  assert.equal(connText(read, { cols: 80, rows: 24 }), "holz.example.net · 120×40");
  assert.equal(connText(read, { cols: 120, rows: 40 }), "holz.example.net");
  assert.equal(connText(read), "holz.example.net", "before the tile is known nothing is said about the size");
  // The name is the host the core gives (never a text of the server); a title that looks like the label stays a title.
  const [writing] = readPaneConns([{ ...remoteConn(), name: "WRITING" }]);
  assert.equal(connText(writing, { cols: 120, rows: 40 }), "WRITING");
});

test("the words of the writer label, Take over and Detach session are the record's", () => {
  assert.deepEqual(Object.values(WRITER_LABELS).map((l) => l.word), ["WRITING", "WATCHING", "FREE"]);
  assert.deepEqual(Object.values(WRITER_LABELS).map((l) => l.shape), ["filled", "hollow", "dashed"]);
  assert.ok(sessions.includes("`WRITING`, `WATCHING`, `FREE`"));
  assert.ok(sessions.includes("| `WRITING` | filled square |") && sessions.includes("| `WATCHING` | hollow square |") && sessions.includes("| `FREE` | dashed square"));
  assert.ok(sessions.includes("`" + TAKE_OVER + "`") && sessions.includes("`" + DETACH + "`"));
});

test("a session of a server is not counted as an SSH connection, and its pane is found by its session", () => {
  assert.equal(connSummary(readPaneConns([remoteConn(), conn({ pane: 2 })])), "ssh 1");
  assert.equal(connSummary(readPaneConns([remoteConn()])), "");
  paneConns.clear();
  setPaneConns(readPaneConns([remoteConn(), conn({ pane: 2 })]));
  assert.equal(paneOfSession(7, [1, 2, 4]), 4);
  assert.equal(paneOfSession(7, [1, 2]), null, "a pane of another window is not this window's");
  assert.equal(paneOfSession(8, [4]), null);
  paneConns.clear();
});
