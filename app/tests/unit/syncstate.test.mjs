// The sync status the page reads from the core, and the words for it (src/syncstate.ts, built into dist/).
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  CHANGE_SENTENCE, NO_ADDRESS, OFF, SERVER_ADDRESS, SYNC_OFF_ON_SERVER, TRANSPORT, conflictText, describeTarget, endpointText, lastText,
  readSync, removedText, syncLabel, targetTitle,
} from "../../dist/syncstate.js";
import { SIGNED_OUT } from "../../dist/serverstate.js";

const record = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "docs", "design.md"), "utf8");
const sync = record.slice(record.indexOf("### 6.9 Sync"), record.indexOf("### 6.10 History")).replace(/\s+/g, " ");

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

test("removedText is empty at zero and a sentence otherwise", () => {
  assert.equal(removedText(OFF), "");
  assert.equal(removedText({ ...OFF, removed: 1 }), "Removed on another computer: 1. Copies are kept in sync-conflicts.");
  assert.equal(removedText({ ...OFF, removed: 3 }), "Removed on another computer: 3. Copies are kept in sync-conflicts.");
});

test("readSync reads removed as a non-negative whole number and defaults to 0", () => {
  assert.equal(readSync({ configured: true }).removed, 0);
  assert.equal(readSync({ removed: 2 }).removed, 2);
  assert.equal(readSync({ removed: 2.9 }).removed, 2);
  assert.equal(readSync({ removed: -4 }).removed, 0);
  assert.equal(readSync({ removed: "7" }).removed, 0);
  assert.equal(readSync({ removed: Number.NaN }).removed, 0);
});

// ---- the choice of the target (docs/design.md 6.9) ------------------------------------------------------------------------------

test("the transport member reads as webdav unless it is exactly server", () => {
  assert.equal(readSync({ configured: true }).transport, "webdav", "an old answer reads as before");
  assert.equal(readSync({ transport: "webdav" }).transport, "webdav");
  assert.equal(readSync({ transport: "server" }).transport, "server");
  for (const odd of ["Server", "server ", "", 1, null, true, {}, "__proto__"]) assert.equal(readSync({ transport: odd }).transport, "webdav", String(odd));
  assert.equal(OFF.transport, "webdav");
  assert.equal(readSync(null).transport, "webdav");
});

test("the state signed_out is read, not taken for off", () => {
  assert.equal(readSync({ configured: true, transport: "server", state: "signed_out" }).state, "signed_out");
  assert.equal(readSync({ state: "Signed_out" }).state, "off");
  // A run that needs a new sign-in takes the word of an error on the button, and the title says so.
  assert.equal(syncLabel({ ...OFF, state: "signed_out" }), syncLabel({ ...OFF, state: "error" }));
});

test("the words of the choice are the record's", () => {
  assert.equal(TRANSPORT.group, "Transport");
  for (const word of [TRANSPORT.webdav, TRANSPORT.server, SERVER_ADDRESS, SYNC_OFF_ON_SERVER]) assert.ok(sync.includes("`" + word + "`"), word);
  assert.ok(sync.includes(CHANGE_SENTENCE), "the sentence under the choice");
  assert.ok(sync.includes("a dim `" + NO_ADDRESS + "`"));
  assert.ok(sync.includes("`not signed in`") && sync.includes("`Sign in…`") && sync.includes("`signed in`"));
});

test("the title and the line under it say the server for the server transport", () => {
  assert.equal(targetTitle("webdav"), "Nextcloud");
  assert.equal(targetTitle("server"), "holzWave server");
  const server = { ...OFF, configured: true, transport: "server", endpoint: "https://holz.example.net", state: "idle" };
  assert.equal(endpointText(server), "holz.example.net · signed in");
  assert.equal(endpointText({ ...server, state: "signed_out" }), "holz.example.net · not signed in");
  assert.equal(endpointText({ ...server, endpoint: "nonsense" }), "nonsense · signed in");
  // The title bar button's word does not change with the transport.
  assert.equal(syncLabel(server), syncLabel({ ...server, transport: "webdav" }));
  // WebDAV reads exactly as before.
  assert.equal(endpointText({ ...OFF, endpoint: "https://c.example/remote.php/dav/", user: "holz" }), "c.example/remote.php/dav · holz");
});

test("the state line of the server variant says why Connect cannot be pressed", () => {
  const view = { ...SIGNED_OUT, url: "https://holz.example.net" };
  assert.deepEqual(describeTarget(view, true), { dot: "hollow", word: "not signed in", bad: false, link: true, connect: false });
  assert.deepEqual(describeTarget({ ...view, state: "signing_in" }, true).link, true);
  const signedIn = { ...view, state: "signed_in", syncOn: true };
  assert.deepEqual(describeTarget(signedIn, true), { dot: "filled", word: "signed in", bad: false, link: false, connect: true });
  assert.deepEqual(describeTarget({ ...signedIn, syncOn: false }, true), { dot: "mark", word: "Sync is off on this server.", bad: true, link: false, connect: false });
  // Until the server has answered, its switch is not known and the line does not say that it is off.
  assert.deepEqual(describeTarget({ ...signedIn, syncOn: false }, false), { dot: "filled", word: "signed in", bad: false, link: false, connect: true });
  assert.deepEqual(describeTarget({ ...signedIn, locked: true }, true), { dot: "dash", word: "Locked: unlock to connect", bad: false, link: false, connect: false });
  assert.deepEqual(describeTarget({ ...signedIn, state: "unreachable" }, true), { dot: "dash", word: "unreachable", bad: false, link: false, connect: false });
});
