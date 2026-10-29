// The pure rules of the browser build's page code (webstate.ts) and the web platform of chords.ts. The OS setting of
// chords.ts is module state and node runs every test file in a process of its own, so each test sets the OS it needs.
import test from "node:test";
import assert from "node:assert/strict";
import { actionIdFor, chordFor, setBindingTable, setOs } from "../../dist/chords.js";
import { CLIPBOARD_REFUSED } from "../../dist/webstate.js";

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
