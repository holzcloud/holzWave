// The pure side of keyboard input on the compiled output in dist/: what the page tells the core about a key event
// (facts only) and which canonical chords an event can stand for. No DOM, so node imports it.
import test from "node:test";
import assert from "node:assert/strict";
import { chordsOf, isComposingKey, isModifierKey, keyPayload } from "../../dist/keymap.js";

/** A KeyboardEvent-like object; every modifier is off unless `extra` turns it on. */
const ev = (code, key, extra = {}) => ({
  code, key, shiftKey: false, ctrlKey: false, altKey: false, metaKey: false, ...extra,
});

// ---- the payload ------------------------------------------------------------------------------------

test("keyPayload of a plain ArrowUp carries code, key and zero modifier bits", () => {
  assert.deepEqual(keyPayload(ev("ArrowUp", "ArrowUp")), {
    code: "ArrowUp", key: "ArrowUp", mods: 0, repeat: false, release: false, composing: false,
  });
});

test("keyPayload modifier bits are SHIFT 1, ALT 2, CTRL 4, SUPER 8", () => {
  assert.equal(keyPayload(ev("KeyC", "c", { shiftKey: true, ctrlKey: true })).mods, 5, "shift and ctrl");
  assert.equal(keyPayload(ev("KeyD", "d", { metaKey: true })).mods, 8, "meta");
  assert.equal(keyPayload(ev("KeyB", "b", { altKey: true })).mods, 2, "alt");
  assert.equal(
    keyPayload(ev("KeyA", "a", { shiftKey: true, altKey: true, ctrlKey: true, metaKey: true })).mods,
    15,
    "all four",
  );
});

test("keyPayload copies the repeat flag and takes the release flag from the caller", () => {
  assert.equal(keyPayload(ev("KeyA", "a", { repeat: true })).repeat, true, "repeat is copied");
  assert.equal(keyPayload(ev("KeyA", "a")).release, false, "a press is not a release");
  assert.equal(keyPayload(ev("KeyA", "a"), true).release, true, "a release is marked");
});

test("keyPayload reports composing for isComposing and for keyCode 229", () => {
  assert.equal(keyPayload(ev("KeyA", "a", { isComposing: true })).composing, true, "isComposing");
  assert.equal(keyPayload(ev("KeyA", "Process", { keyCode: 229 })).composing, true, "keyCode 229");
  assert.equal(keyPayload(ev("KeyA", "a", { keyCode: 65 })).composing, false, "an ordinary key");
});

test("an astral emoji key (one code point, two UTF-16 units) is passed through unchanged", () => {
  for (const key of ["\u{1F600}", "\u{1D4B3}"]) {
    assert.equal([...key].length, 1);
    assert.equal(key.length, 2);
    assert.equal(keyPayload(ev("", key)).key, key);
  }
});

test("dead and process keys count as composing, modifier keys are recognised", () => {
  assert.equal(isComposingKey(ev("KeyE", "Dead")), true);
  assert.equal(isComposingKey(ev("KeyA", "Process")), true);
  assert.equal(isComposingKey(ev("KeyA", "a")), false);
  for (const [code, key] of [["ShiftLeft", "Shift"], ["ControlRight", "Control"], ["AltLeft", "Alt"], ["MetaLeft", "Meta"]]) {
    assert.equal(isModifierKey(ev(code, key)), true, key);
  }
  assert.equal(isModifierKey(ev("KeyA", "a")), false);
  assert.equal(isModifierKey(ev("Enter", "Enter")), false);
});

// ---- chord candidates -------------------------------------------------------------------------------

test("chordsOf names letters upper case and keeps Shift in the chord", () => {
  assert.ok(chordsOf(ev("KeyD", "d", { metaKey: true })).includes("Cmd+D"));
  assert.ok(chordsOf(ev("KeyD", "D", { metaKey: true, shiftKey: true })).includes("Cmd+Shift+D"));
});

test("chordsOf matches by physical key on a layout that types something else (Swiss Cmd+[)", () => {
  assert.ok(chordsOf(ev("BracketLeft", "\u00fc", { metaKey: true })).includes("Cmd+["));
  assert.ok(chordsOf(ev("BracketRight", "\u00a8", { metaKey: true })).includes("Cmd+]"));
});

test("chordsOf with Option held falls back to the digit or letter of the physical key", () => {
  assert.ok(chordsOf(ev("Digit1", "1", { metaKey: true, altKey: true })).includes("Cmd+Alt+1"));
  assert.ok(chordsOf(ev("Digit1", "\u00a1", { metaKey: true, altKey: true })).includes("Cmd+Alt+1"));
  assert.ok(chordsOf(ev("KeyB", "\u222b", { altKey: true, ctrlKey: true })).includes("Ctrl+Alt+B"));
});

test("chordsOf names Space, arrows, page keys and function keys as the grammar spells them", () => {
  assert.ok(chordsOf(ev("Space", " ", { ctrlKey: true, metaKey: true })).includes("Cmd+Ctrl+Space"));
  assert.ok(chordsOf(ev("ArrowUp", "ArrowUp", { ctrlKey: true, altKey: true, shiftKey: true })).includes("Ctrl+Alt+Shift+Up"));
  assert.ok(chordsOf(ev("ArrowLeft", "ArrowLeft", { metaKey: true })).includes("Cmd+Left"));
  assert.ok(chordsOf(ev("PageDown", "PageDown", { ctrlKey: true, shiftKey: true })).includes("Ctrl+Shift+PageDown"));
  assert.ok(chordsOf(ev("F12", "F12", { metaKey: true })).includes("Cmd+F12"));
  assert.ok(chordsOf(ev("Enter", "Enter", { metaKey: true, altKey: true })).includes("Cmd+Alt+Enter"));
});

/** The 14 chord strings of core/tests/keybindings.rs SHARED_SAMPLES, each with an event that must produce it. */
const SAMPLES = [
  ["Cmd+D", ev("KeyD", "d", { metaKey: true })],
  ["Cmd+Shift+D", ev("KeyD", "D", { metaKey: true, shiftKey: true })],
  ["Cmd+]", ev("BracketRight", "]", { metaKey: true })],
  ["Cmd+Alt+1", ev("Digit1", "\u00a1", { metaKey: true, altKey: true })],
  ["Cmd+Ctrl+Space", ev("Space", " ", { metaKey: true, ctrlKey: true })],
  ["Ctrl+Alt+Shift+Up", ev("ArrowUp", "ArrowUp", { ctrlKey: true, altKey: true, shiftKey: true })],
  ["Cmd+,", ev("Comma", ",", { metaKey: true })],
  ["Cmd+=", ev("Equal", "=", { metaKey: true })],
  ["Cmd+-", ev("Minus", "-", { metaKey: true })],
  ["Cmd+Shift+Space", ev("Space", " ", { metaKey: true, shiftKey: true })],
  ["Ctrl+Shift+PageDown", ev("PageDown", "PageDown", { ctrlKey: true, shiftKey: true })],
  ["Cmd+F12", ev("F12", "F12", { metaKey: true })],
  ["Cmd+Alt+Enter", ev("Enter", "Enter", { metaKey: true, altKey: true })],
  ["Cmd+Tab", ev("Tab", "Tab", { metaKey: true })],
];

test("every one of the 14 shared sample chords is produced by chordsOf", () => {
  assert.equal(SAMPLES.length, 14);
  for (const [chord, event] of SAMPLES) {
    assert.ok(chordsOf(event).includes(chord), `${chord} is not among ${JSON.stringify(chordsOf(event))}`);
  }
});

test("chordsOf never names the plus sign as a key and gives nothing for a modifier alone", () => {
  const plus = chordsOf(ev("Equal", "+", { metaKey: true, shiftKey: true }));
  assert.ok(plus.includes("Cmd+Shift+="), JSON.stringify(plus));
  assert.ok(plus.every((c) => !c.endsWith("++")), JSON.stringify(plus));
  for (const [code, key] of [["ShiftLeft", "Shift"], ["MetaLeft", "Meta"], ["AltRight", "Alt"], ["ControlLeft", "Control"]]) {
    assert.deepEqual(chordsOf(ev(code, key, { metaKey: true })), [], `${key} alone is not a chord`);
  }
});

test("chordsOf lists the key's own name first and the physical key after it", () => {
  assert.deepEqual(chordsOf(ev("KeyE", "e", { metaKey: true })), ["Cmd+E"], "both spell the same chord once");
  assert.deepEqual(chordsOf(ev("KeyD", "e", { metaKey: true })), ["Cmd+E", "Cmd+D"], "Dvorak: typed E, physical D");
});

test("a key name that is a property of Object.prototype is not a chord", () => {
  for (const key of ["constructor", "__proto__", "toString"]) {
    assert.deepEqual(chordsOf(ev("", key, { metaKey: true })), []);
  }
});

test("a dead key under Option still names the chord by its physical key", () => {
  assert.ok(chordsOf(ev("KeyE", "Dead", { metaKey: true, altKey: true })).includes("Cmd+Alt+E"));
});
