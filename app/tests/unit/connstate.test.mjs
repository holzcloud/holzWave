// The words and the reading of connections on the page (connstate.ts): no DOM is needed.
import test from "node:test";
import assert from "node:assert/strict";
import {
  connDot, connSummary, connText, paneConns, readConnections, readPaneConns, setPaneConns,
} from "../../dist/connstate.js";

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
