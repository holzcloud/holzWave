// The pure rules of the browser build's page code (webstate.ts) and the web platform of chords.ts. The OS setting of
// chords.ts is module state and node runs every test file in a process of its own, so each test sets the OS it needs.
import test from "node:test";
import assert from "node:assert/strict";
import { actionIdFor, chordFor, setBindingTable, setOs } from "../../dist/chords.js";
import {
  CLIPBOARD_REFUSED, DESKTOP_ONLY_ACTIONS, FULLSCREEN_NOTICE, LOCK_KEYS, NOT_AVAILABLE, canLockKeys,
} from "../../dist/webstate.js";
import { findTargets, isLinkModifier, linkAction } from "../../dist/links.js";
import { isAvailable, listActions, listAvailableActions, registerAction, runAction, setAvailability } from "../../dist/actions.js";
import { readdirSync, readFileSync } from "node:fs";

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

// ---- controls the browser does not have -----------------------------------------------------------------------------

test("the answer for a control the browser does not have is the sentence the design gives", () => {
  assert.equal(NOT_AVAILABLE, "Not available in the browser.");
});

test("the two tear-off actions are the ones the browser build cannot offer", () => {
  assert.deepEqual([...DESKTOP_ONLY_ACTIONS], ["pane.move_to_new_window", "tab.move_to_new_window"]);
});

test("an action is available unless it carries a test that says no; unavailable actions stay in the registry and still run", () => {
  let ran = 0;
  registerAction({ id: "t45.plain", title: "Plain", run: () => ran++ });
  registerAction({ id: "t45.gated", title: "Gated", run: () => ran++ });
  assert.equal(setAvailability("t45.gated", () => false), true);
  assert.equal(setAvailability("t45.unknown", () => false), false, "an unknown id is not an error");
  const plain = listActions().find((a) => a.id === "t45.plain");
  const gated = listActions().find((a) => a.id === "t45.gated");
  assert.equal(isAvailable(plain), true, "no test: available, as every action was before");
  assert.equal(isAvailable(gated), false);
  assert.ok(listActions().some((a) => a.id === "t45.gated"), "the registry still holds it");
  const listed = listAvailableActions().map((a) => a.id);
  assert.ok(listed.includes("t45.plain"));
  assert.ok(!listed.includes("t45.gated"), "the palette's list leaves it out");
  assert.equal(runAction("t45.gated"), true);
  assert.equal(ran, 1, "a stored shortcut or a script still reaches it");
  setAvailability("t45.gated", () => true);
  assert.ok(listAvailableActions().some((a) => a.id === "t45.gated"), "the test is asked each time");
});

test("an availability test that throws makes the action unavailable, not the palette broken", () => {
  registerAction({ id: "t45.broken", title: "Broken", run: () => {} });
  setAvailability("t45.broken", () => {
    throw new Error("boom");
  });
  assert.equal(isAvailable(listActions().find((a) => a.id === "t45.broken")), false);
  assert.ok(Array.isArray(listAvailableActions()));
});

test("only the browser build's entry file sets an availability test", () => {
  const dist = new URL("../../dist/", import.meta.url);
  const users = readdirSync(dist)
    .filter((f) => f.endsWith(".js"))
    .filter((f) => /\bsetAvailability\(/.test(readFileSync(new URL(f, dist), "utf8")))
    .sort();
  assert.deepEqual(users, ["actions.js", "web.js"]);
});
