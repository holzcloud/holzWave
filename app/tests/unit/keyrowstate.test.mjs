// The pure model of the key row on the compiled output in dist/: sticky modifiers, the keys of the row and of a soft keyboard,
// the hardware-keyboard detector and the mapping of an on-screen keyboard's events. No DOM, no clock: the time is an argument.
import test from "node:test";
import assert from "node:assert/strict";
import {
  LOCK_WINDOW_MS,
  SENTINEL,
  anySticky,
  consume,
  idleSticky,
  keyEvents,
  keyForChar,
  keyForInputType,
  modsFor,
  plainKey,
  rowVisible,
  sourceAfter,
  stripSentinel,
  tap,
} from "../../dist/keyrowstate.js";
import { keyPayload } from "../../dist/keymap.js";

// The bits of the `key` command as keymap.ts makes them; the model must agree with them and never carry its own copy.
const SHIFT = keyPayload({ code: "", key: "", shiftKey: true }).mods;
const ALT = keyPayload({ code: "", key: "", altKey: true }).mods;
const CTRL = keyPayload({ code: "", key: "", ctrlKey: true }).mods;

test("the bits are the ones of keymap.ts", () => {
  assert.deepEqual([SHIFT, ALT, CTRL], [1, 2, 4]);
});

test("a sticky key: one tap arms, a second within 350 ms locks, a later one cancels, a tap on a locked one releases", () => {
  assert.equal(LOCK_WINDOW_MS, 350);
  let s = idleSticky();
  assert.equal(s.ctrl.mode, "idle");
  s = tap(s, "ctrl", 1000);
  assert.equal(s.ctrl.mode, "armed");
  assert.equal(s.alt.mode, "idle", "the other modifier is its own");
  assert.equal(tap(s, "ctrl", 1349).ctrl.mode, "locked", "349 ms locks");
  assert.equal(tap(s, "ctrl", 1350).ctrl.mode, "locked", "350 ms locks");
  assert.equal(tap(s, "ctrl", 1351).ctrl.mode, "idle", "351 ms cancels");
  assert.equal(tap(s, "ctrl", 5000).ctrl.mode, "idle", "much later cancels");
  const locked = tap(s, "ctrl", 1100);
  assert.equal(tap(locked, "ctrl", 9000).ctrl.mode, "idle", "a tap on a locked key releases it, whenever it comes");
  assert.equal(tap(locked, "ctrl", 1101).ctrl.mode, "idle", "even at once");
});

test("a tap never changes the state it was given", () => {
  const s = Object.freeze(idleSticky());
  const t = tap(s, "alt", 5);
  assert.equal(s.alt.mode, "idle");
  assert.equal(t.alt.mode, "armed");
});

test("consume spends the armed modifiers and leaves the locked ones; ctrl and alt are independent", () => {
  let s = tap(idleSticky(), "ctrl", 0);
  s = tap(s, "alt", 10);
  assert.equal(s.ctrl.mode, "armed");
  assert.equal(s.alt.mode, "armed");
  assert.equal(anySticky(s), true);
  const spent = consume(s);
  assert.equal(spent.ctrl.mode, "idle");
  assert.equal(spent.alt.mode, "idle");
  assert.equal(anySticky(spent), false);

  const mixed = tap(tap(idleSticky(), "ctrl", 0), "ctrl", 100); // locked
  const both = tap(mixed, "alt", 200); // alt armed
  const after = consume(both);
  assert.equal(after.ctrl.mode, "locked");
  assert.equal(after.alt.mode, "idle");
  assert.equal(consume(after).ctrl.mode, "locked", "a locked modifier survives every key");
  assert.equal(consume(idleSticky()).ctrl.mode, "idle");
});

test("modsFor ors the armed and locked modifiers into the key's own bits", () => {
  const idle = idleSticky();
  assert.equal(modsFor(idle, 0), 0);
  assert.equal(modsFor(idle, SHIFT), SHIFT);
  const ctrl = tap(idle, "ctrl", 0);
  assert.equal(modsFor(ctrl, 0), CTRL);
  assert.equal(modsFor(ctrl, SHIFT), SHIFT | CTRL);
  const both = tap(ctrl, "alt", 1);
  assert.equal(modsFor(both, 0), CTRL | ALT);
  const locked = tap(ctrl, "ctrl", 10);
  assert.equal(locked.ctrl.mode, "locked");
  assert.equal(modsFor(locked, 0), CTRL, "locked counts too");
});

test("the plain keys make a press and a release in the shape keyPayload makes", () => {
  const expect = {
    esc: ["Escape", "Escape"],
    tab: ["Tab", "Tab"],
    up: ["ArrowUp", "ArrowUp"],
    down: ["ArrowDown", "ArrowDown"],
  };
  for (const [name, [code, key]] of Object.entries(expect)) {
    const k = plainKey(name);
    assert.deepEqual(k, { code, key, mods: 0 }, name);
    const { press, release } = keyEvents(k);
    assert.deepEqual(press, { code, key, mods: 0, repeat: false, release: false, composing: false }, `${name} press`);
    assert.deepEqual(release, { code, key, mods: 0, repeat: false, release: true, composing: false }, `${name} release`);
    assert.deepEqual(press, keyPayload({ code, key }, false), `${name} is what a real key event would send`);
  }
});

test("keyEvents carries the modifier bits of the key", () => {
  const { press, release } = keyEvents({ code: "Escape", key: "Escape", mods: CTRL | ALT });
  assert.equal(press.mods, CTRL | ALT);
  assert.equal(release.mods, CTRL | ALT);
  assert.equal(keyEvents({ code: "KeyC", key: "C", mods: SHIFT | CTRL }).press.mods, SHIFT | CTRL);
});

test("keyForChar: letters, digits and punctuation of a US layout; anything else is text", () => {
  assert.deepEqual(keyForChar("c"), { code: "KeyC", key: "c", mods: 0 });
  assert.deepEqual(keyForChar("C"), { code: "KeyC", key: "C", mods: SHIFT });
  for (let d = 0; d <= 9; d++) assert.deepEqual(keyForChar(String(d)), { code: `Digit${d}`, key: String(d), mods: 0 });
  const expect = {
    "[": "BracketLeft",
    "]": "BracketRight",
    "\\": "Backslash",
    "/": "Slash",
    "-": "Minus",
    "=": "Equal",
    ",": "Comma",
    ".": "Period",
    ";": "Semicolon",
    "'": "Quote",
    " ": "Space",
  };
  for (const [ch, code] of Object.entries(expect)) assert.deepEqual(keyForChar(ch), { code, key: ch, mods: 0 }, JSON.stringify(ch));
  assert.deepEqual(keyForChar("_"), { code: "Minus", key: "_", mods: SHIFT }, "Ctrl+_ is a shell's undo");
  assert.deepEqual(keyForChar("@"), { code: "Digit2", key: "@", mods: SHIFT });
  for (const text of ["é", "€", "😀", "\n", "\t", "\u0003", "", "ab", "ls -l", "cé"]) {
    assert.equal(keyForChar(text), null, JSON.stringify(text));
  }
});

test("the source detector: real keys mean hardware, an input method means the soft keyboard, the rest changes nothing", () => {
  const down = (init) => ({ type: "keydown", key: "a", code: "KeyA", keyCode: 65, ...init });
  assert.equal(sourceAfter(false, down({})), true);
  assert.equal(sourceAfter(false, down({ key: "A" })), true);
  assert.equal(sourceAfter(false, down({ key: "ArrowUp", code: "ArrowUp", keyCode: 38 })), true, "a key a soft keyboard lacks");
  assert.equal(sourceAfter(false, down({ key: "Escape", code: "Escape", keyCode: 27 })), true);
  assert.equal(sourceAfter(false, down({ key: "F5", code: "F5", keyCode: 116 })), true);
  // a soft keyboard's keys
  assert.equal(sourceAfter(true, down({ keyCode: 229 })), false);
  assert.equal(sourceAfter(true, down({ key: "Unidentified", keyCode: 0 })), false);
  assert.equal(sourceAfter(true, down({ key: "Process", keyCode: 229 })), false);
  assert.equal(sourceAfter(true, { type: "compositionstart" }), false);
  assert.equal(sourceAfter(true, { type: "beforeinput", isComposing: true }), false);
  // everything else leaves it
  assert.equal(sourceAfter(true, { type: "beforeinput", isComposing: false }), true);
  assert.equal(sourceAfter(false, { type: "beforeinput", isComposing: false }), false);
  assert.equal(sourceAfter(true, down({ key: "Enter", code: "Enter", keyCode: 13 })), true, "Enter is ambiguous: unchanged");
  assert.equal(sourceAfter(false, down({ key: "Enter", code: "Enter", keyCode: 13 })), false);
  assert.equal(sourceAfter(false, down({ key: "Shift", code: "ShiftLeft", keyCode: 16 })), false, "a modifier alone says nothing");
  assert.equal(sourceAfter(false, down({ code: "" })), false, "no code: unchanged");
  assert.equal(sourceAfter(true, down({ code: "" })), true);
  // the last source decides
  let h = false;
  h = sourceAfter(h, down({}));
  assert.equal(h, true);
  h = sourceAfter(h, down({ keyCode: 229 }));
  assert.equal(h, false);
  h = sourceAfter(h, down({}));
  assert.equal(h, true);
});

test("rowVisible: a touch layout, the input focused, no hardware keyboard", () => {
  for (const layout of ["phone", "tablet"]) {
    assert.equal(rowVisible({ layout, imeFocused: true, hardware: false }), true, layout);
    assert.equal(rowVisible({ layout, imeFocused: false, hardware: false }), false, `${layout} without the focus`);
    assert.equal(rowVisible({ layout, imeFocused: true, hardware: true }), false, `${layout} with a hardware keyboard`);
  }
  assert.equal(rowVisible({ layout: "desktop", imeFocused: true, hardware: false }), false);
});

test("beforeinput: Backspace, Enter, the shell's word kill, and nothing else", () => {
  assert.deepEqual(keyForInputType("deleteContentBackward"), { code: "Backspace", key: "Backspace", mods: 0 });
  assert.deepEqual(keyForInputType("insertLineBreak"), { code: "Enter", key: "Enter", mods: 0 });
  assert.deepEqual(keyForInputType("insertParagraph"), { code: "Enter", key: "Enter", mods: 0 });
  assert.deepEqual(keyForInputType("deleteWordBackward"), { code: "KeyW", key: "w", mods: CTRL });
  for (const type of ["insertText", "insertCompositionText", "deleteContentForward", "deleteByCut", "insertFromPaste", "historyUndo", ""]) {
    assert.equal(keyForInputType(type), null, type);
  }
});

test("stripSentinel removes U+200B and nothing else", () => {
  assert.equal(SENTINEL, "​");
  assert.equal(stripSentinel(SENTINEL), "");
  assert.equal(stripSentinel(`${SENTINEL}c`), "c");
  assert.equal(stripSentinel(`a${SENTINEL}b${SENTINEL}`), "ab");
  assert.equal(stripSentinel("a‌b c ‍"), "a‌b c ‍", "other invisible characters stay");
  assert.equal(stripSentinel("😀 é"), "😀 é");
});
