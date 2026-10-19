// The per-OS side of chords.ts and the clipboard rules of the action registry (Phase 2.2). The OS setting of chords.ts is
// module state and node runs every test file in a process of its own, so these cases cannot disturb the macOS
// assertions of actions.test.mjs and a11y.test.mjs; each test sets the OS it needs.
import test from "node:test";
import assert from "node:assert/strict";
import { CHORDS, actionIdFor, chordFor, setBindingTable, setOs } from "../../dist/chords.js";
import { isRepeatable, keepsDefault, registerAction, runAction } from "../../dist/actions.js";

const CLIPBOARD = [
  ["Ctrl+Shift+C", "edit.copy"],
  ["Ctrl+Insert", "edit.copy"],
  ["Ctrl+Shift+V", "edit.paste"],
  ["Shift+Insert", "edit.paste"],
];

test("with no OS known the fallback is the macOS table", () => {
  setOs("");
  for (const [chord, id] of CHORDS) assert.equal(actionIdFor(chord), id, chord);
  assert.equal(actionIdFor("Cmd+D"), "pane.split_right");
  assert.equal(actionIdFor("Cmd+V"), "edit.paste", "the page owns Cmd+V on macOS as well");
  assert.equal(actionIdFor("Ctrl+Shift+V"), undefined, "the Windows and Linux paste chords are not macOS chords");
});

test("macOS keeps its static table, Cmd+C and Cmd+V", () => {
  setOs("macos");
  assert.equal(actionIdFor("Cmd+C"), "edit.copy");
  assert.equal(actionIdFor("Cmd+V"), "edit.paste");
  assert.equal(chordFor("edit.paste"), "Cmd+V");
  assert.equal(actionIdFor("Ctrl+V"), undefined, "plain Ctrl+V reaches the program as a key (Claude Code's image key)");
  assert.equal(actionIdFor("Ctrl+Shift+V"), undefined);
  assert.equal(actionIdFor("Cmd+W"), "pane.close");
  assert.equal(actionIdFor("Ctrl+Shift+C"), undefined);
  assert.equal(chordFor("edit.copy"), "Cmd+C");
});

test("on Windows the fallback holds only the fixed clipboard chords, with Ctrl+V pasting", () => {
  setOs("windows");
  for (const [chord, id] of [...CLIPBOARD, ["Ctrl+V", "edit.paste"]]) assert.equal(actionIdFor(chord), id, chord);
  assert.equal(actionIdFor("Ctrl+C"), undefined, "plain Ctrl+C is never a copy");
  assert.equal(actionIdFor("Cmd+C"), undefined);
  assert.equal(chordFor("edit.copy"), "Ctrl+Shift+C");
  assert.equal(chordFor("pane.split_right"), undefined, "no Cmd chord stands in for the core's rows");
});

test("on Linux the fallback is the same without Ctrl+V, and anything that is not macOS or Windows is Linux", () => {
  for (const name of ["linux", "freebsd"]) {
    setOs(name);
    for (const [chord, id] of CLIPBOARD) assert.equal(actionIdFor(chord), id, `${name} ${chord}`);
    assert.equal(actionIdFor("Ctrl+V"), undefined, `${name}: Ctrl+V stays with the program`);
    assert.equal(actionIdFor("Ctrl+C"), undefined, `${name}: plain Ctrl+C is never a copy`);
  }
});

test("no chord with Cmd resolves on Windows or Linux while the core's rows are missing", () => {
  for (const name of ["windows", "linux"]) {
    setOs(name);
    for (const chord of CHORDS.keys()) {
      if (!chord.includes("Cmd+")) continue;
      assert.equal(actionIdFor(chord), undefined, `${name}: ${chord}`);
    }
    for (const chord of ["Alt+Left", "Alt+Right", "Alt+Backspace"]) {
      assert.equal(actionIdFor(chord), undefined, `${name}: ${chord} is a macOS line-editing default`);
    }
  }
});

test("with the core's rows set the rows decide and the clipboard chords stay fixed", () => {
  setOs("windows");
  setBindingTable([
    { action: "pane.split_right", chord: "Ctrl+Shift+D" },
    { action: "pane.close", chord: null },
    { action: "tab.new", chord: "Ctrl+Shift+V" },
  ]);
  assert.equal(actionIdFor("Ctrl+Shift+D"), "pane.split_right");
  assert.equal(actionIdFor("Ctrl+Shift+W"), undefined);
  assert.equal(actionIdFor("Cmd+W"), undefined, "a chord the table left unbound stays unowned");
  assert.equal(actionIdFor("Ctrl+Shift+C"), "edit.copy", "copy is no action of the table: it stays fixed");
  assert.equal(actionIdFor("Shift+Insert"), "edit.paste");
  assert.equal(actionIdFor("Ctrl+Shift+V"), "tab.new", "a chord another action took is that action's");
  setBindingTable([]);
});

// ---- the registry ---------------------------------------------------------------------------------

let pasted = 0;

test("the paste action keeps the browser default of its key press and no other action does", () => {
  registerAction({ id: "edit.paste", title: "Paste", keepDefault: true, run: () => void pasted++ });
  registerAction({ id: "edit.copy", title: "Copy", run: () => {} });
  assert.equal(keepsDefault("edit.paste"), true);
  assert.equal(keepsDefault("edit.copy"), false);
  assert.equal(keepsDefault("pane.close"), false, "an unknown id keeps nothing");
  assert.equal(runAction("edit.paste"), true);
  assert.equal(pasted, 1);
});

test("a held copy or paste chord runs once: neither is a repeatable action", () => {
  assert.equal(isRepeatable("edit.paste"), false);
  assert.equal(isRepeatable("edit.copy"), false);
  const before = pasted;
  assert.equal(runAction("edit.paste", { repeat: true }), true, "the repeat is consumed");
  assert.equal(pasted, before, "and does not run");
});
