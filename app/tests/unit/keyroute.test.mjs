// Who a key belongs to when no shortcut owns it: the rules in keyroute.ts, and the region actions of regions.ts that move the
// keyboard between the terminal and the bar. The DOM side (what a focused button does with the keys) is in the browser test
// tests/browser/regions.test.mjs; here are the facts the rules read.
import test from "node:test";
import assert from "node:assert/strict";
import { CONTROL_SELECTOR, isControlKey, isFunctionKeyChord } from "../../dist/keyroute.js";

const key = (k, mods = {}) => ({ key: k, ctrlKey: false, altKey: false, metaKey: false, ...mods });

test("Tab, Enter, Space and the four arrows are the focused control's", () => {
  for (const k of ["Tab", "Enter", " ", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"]) {
    assert.equal(isControlKey(key(k)), true, JSON.stringify(k));
  }
});

test("a key with Ctrl, Alt or Cmd is a chord and never the control's", () => {
  for (const k of ["Tab", "Enter", " ", "ArrowLeft"]) {
    for (const mod of ["ctrlKey", "altKey", "metaKey"]) assert.equal(isControlKey(key(k, { [mod]: true })), false, `${k} ${mod}`);
  }
});

test("a letter, Escape, Backspace, a function key and the paging keys stay with the terminal and the shortcuts", () => {
  for (const k of ["a", "A", "1", "Escape", "Backspace", "Delete", "Home", "End", "PageUp", "PageDown", "F6", "Insert", "Dead", ""]) {
    assert.equal(isControlKey(key(k)), false, JSON.stringify(k));
  }
});

test("Shift does not change who owns Tab: Shift+Tab walks back through the controls", () => {
  assert.equal(isControlKey({ ...key("Tab"), shiftKey: true }), true);
});

test("a function key chord is one of F1 to F24, alone or with Shift", () => {
  for (const c of ["F1", "F6", "Shift+F6", "F12", "F24", "Shift+F24"]) assert.equal(isFunctionKeyChord(c), true, c);
  for (const c of ["F0", "F25", "F06", "Ctrl+F6", "Alt+F6", "Cmd+F6", "Ctrl+Shift+F6", "Shift+Shift+F6", "F", "Shift+F", "f6", "D", ""]) {
    assert.equal(isFunctionKeyChord(c), false, JSON.stringify(c));
  }
});

test("the controls are buttons, links, summaries, anything with a role and anything in the tab order", () => {
  for (const part of ["button", "a[href]", "summary", "[role]", "[tabindex]"]) assert.ok(CONTROL_SELECTOR.split(", ").includes(part), part);
});
