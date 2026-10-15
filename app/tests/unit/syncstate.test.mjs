// The sync status the page reads from the core, and the words for it (src/syncstate.ts, built into dist/).
import test from "node:test";
import assert from "node:assert/strict";
import { OFF, conflictText, endpointText, lastText, readSync, syncLabel } from "../../dist/syncstate.js";

test("anything that is not an object is the off state", () => {
  for (const raw of [null, undefined, 3, "x", []]) assert.deepEqual(readSync(raw), OFF);
});

test("a well formed answer is read and a bad field is dropped, not trusted", () => {
  const s = readSync({ configured: true, endpoint: "https://h/dav", user: "u", state: "syncing", last: 5, files: 2, conflicts: -1, error: "", restart: true });
  assert.equal(s.state, "syncing");
  assert.equal(s.files, 2);
  assert.equal(s.conflicts, 0, "a negative count is zero");
  assert.equal(s.error, null);
  assert.equal(s.restart, true);
  assert.equal(readSync({ state: "exploding" }).state, "off");
  assert.equal(readSync({ error: "x".repeat(1000) }).error?.length, 400);
});

test("every state has its own word", () => {
  const labels = ["off", "idle", "syncing", "error"].map((state) => syncLabel({ ...OFF, state }));
  assert.equal(new Set(labels).size, 4);
  assert.ok(labels.every((l) => l.startsWith("☁ ")));
});

test("the texts of the popover", () => {
  assert.equal(lastText(OFF), "not yet");
  assert.match(lastText({ ...OFF, last: 1760000000, files: 1 }), /^\d\d:\d\d · 1 file$/);
  assert.equal(conflictText(OFF), "none");
  assert.match(conflictText({ ...OFF, conflicts: 2 }), /^2 · /);
  assert.equal(endpointText({ ...OFF, endpoint: "https://c.example/remote.php/dav/", user: "holz" }), "c.example/remote.php/dav · holz");
  assert.equal(endpointText({ ...OFF, endpoint: "nonsense", user: "u" }), "nonsense · u");
});
