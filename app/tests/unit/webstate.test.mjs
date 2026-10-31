// The pure rules of the browser build's page code (webstate.ts) and the web platform of chords.ts. The OS setting of
// chords.ts is module state and node runs every test file in a process of its own, so each test sets the OS it needs.
import test from "node:test";
import assert from "node:assert/strict";
import { actionIdFor, chordFor, setBindingTable, setOs } from "../../dist/chords.js";
import { CLIPBOARD_REFUSED, FULLSCREEN_NOTICE, LOCK_KEYS, canLockKeys } from "../../dist/webstate.js";
import { findTargets, isLinkModifier, linkAction } from "../../dist/links.js";

const CLIPBOARD = [
  ["Ctrl+Shift+C", "edit.copy"],
  ["Ctrl+Insert", "edit.copy"],
  ["Ctrl+Shift+V", "edit.paste"],
  ["Shift+Insert", "edit.paste"],
];

test("on the web the fallback holds the fixed clipboard chords of Windows and Linux", () => {
  setBindingTable([]);
  setOs("web");
  for (const [chord, id] of CLIPBOARD) assert.equal(actionIdFor(chord), id, chord);
  assert.equal(chordFor("edit.copy"), "Ctrl+Shift+C");
  assert.equal(chordFor("edit.paste"), "Ctrl+Shift+V");
});

test("on the web Ctrl+V, plain Ctrl+C and the Cmd chords stay with the browser or the program", () => {
  setBindingTable([]);
  setOs("web");
  assert.equal(actionIdFor("Ctrl+V"), undefined, "Ctrl+V reaches the program as a key");
  assert.equal(actionIdFor("Ctrl+C"), undefined, "plain Ctrl+C is never a copy");
  assert.equal(actionIdFor("Cmd+C"), undefined);
  assert.equal(actionIdFor("Cmd+V"), undefined);
  assert.equal(actionIdFor("Cmd+W"), undefined, "Cmd+W is not a web chord");
  assert.equal(actionIdFor("Cmd+D"), undefined);
});

test("the web is its own platform: it is not the macOS table and no Win-key chord stands in", () => {
  setBindingTable([]);
  setOs("web");
  assert.equal(chordFor("pane.split_right"), undefined);
  assert.equal(chordFor("pane.close"), undefined);
});

test("with the core's table set the fixed clipboard chords of the web stay fixed", () => {
  setOs("web");
  setBindingTable([
    { action: "pane.split_right", chord: "Ctrl+Shift+D" },
    { action: "pane.close", chord: "Ctrl+Shift+W" },
  ]);
  assert.equal(actionIdFor("Ctrl+Shift+D"), "pane.split_right", "a row of the core's web table runs its action");
  for (const [chord, id] of CLIPBOARD) assert.equal(actionIdFor(chord), id, chord);
  assert.equal(actionIdFor("Ctrl+V"), undefined);
});

test("the refused-read sentence is the one the design gives", () => {
  assert.equal(CLIPBOARD_REFUSED, "The browser did not allow reading the clipboard.");
});

test("the keys locked in full screen are the six a browser keeps and Escape, in this order", () => {
  assert.deepEqual([...LOCK_KEYS], ["KeyW", "KeyT", "KeyN", "Tab", "PageUp", "PageDown", "Escape"]);
  assert.equal(new Set(LOCK_KEYS).size, 7, "no code twice");
});

test("canLockKeys is true only for a navigator whose keyboard has a lock function", () => {
  assert.equal(canLockKeys({ keyboard: { lock() {}, unlock() {} } }), true);
  assert.equal(canLockKeys({ keyboard: { lock: async () => {} } }), true);
  assert.equal(canLockKeys({}), false, "no keyboard object");
  assert.equal(canLockKeys({ keyboard: {} }), false, "a keyboard without lock");
  assert.equal(canLockKeys({ keyboard: { lock: "yes" } }), false, "a lock that is not a function");
  assert.equal(canLockKeys({ keyboard: { lock: null } }), false);
  assert.equal(canLockKeys({ keyboard: null }), false);
  assert.equal(canLockKeys(null), false);
  assert.equal(canLockKeys(undefined), false);
  assert.equal(canLockKeys("keyboard"), false);
});

test("the full-screen notice is the sentence the design gives", () => {
  assert.equal(FULLSCREEN_NOTICE, "Browser shortcuts are off in full screen. Hold Escape to leave.");
});

// ---- links ----------------------------------------------------------------------------------------------------------

test("on the web the link modifier is Ctrl or Cmd, and neither alone is no modifier", () => {
  assert.equal(isLinkModifier({ ctrlKey: true }, "web"), true, "Ctrl");
  assert.equal(isLinkModifier({ metaKey: true }, "web"), true, "Cmd");
  assert.equal(isLinkModifier({ ctrlKey: true, metaKey: true }, "web"), true, "both");
  assert.equal(isLinkModifier({}, "web"), false, "neither");
  assert.equal(isLinkModifier({ ctrlKey: false, metaKey: false, shiftKey: true }, "web"), false, "Shift is not a link modifier");
});

test("the link modifier of macOS and of the other platforms is what it was", () => {
  assert.equal(isLinkModifier({ metaKey: true }, "macos"), true);
  assert.equal(isLinkModifier({ ctrlKey: true }, "macos"), false, "Ctrl+click is the menu on macOS");
  assert.equal(isLinkModifier({ ctrlKey: true }, "linux"), true);
  assert.equal(isLinkModifier({ metaKey: true }, "linux"), false);
  assert.equal(isLinkModifier({ ctrlKey: true }, "windows"), true);
  assert.equal(isLinkModifier({}, "linux"), false);
});

test("a path is no link where a folder cannot be shown, and a URL is the same action either way", () => {
  const [url] = findTargets(Array.from("https://example.com/a?b=1"));
  const [path] = findTargets(Array.from("./src/main.rs"));
  assert.equal(url.kind, "url");
  assert.equal(path.kind, "path");
  const want = { command: "open_url", args: { url: "https://example.com/a?b=1" } };
  assert.deepEqual(linkAction(url, "/home/u", false), want);
  assert.deepEqual(linkAction(url, "/home/u"), want, "the default keeps every existing call");
  assert.deepEqual(linkAction(url, "/home/u", true), want);
  assert.equal(linkAction(path, "/home/u", false), null, "no path where nothing can be revealed");
  assert.deepEqual(linkAction(path, "/home/u"), { command: "reveal_path", args: { path: "./src/main.rs", base: "/home/u" } });
  assert.deepEqual(linkAction(path, null, true), { command: "reveal_path", args: { path: "./src/main.rs", base: null } });
});
