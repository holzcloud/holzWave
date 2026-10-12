// The pure side of the accessibility preferences on the compiled output in dist/ (DESK-24, DESK-25): how a setting
// and the OS signal resolve to motion, contrast and cursor blink, the measured contrast of the text on the accent
// fill, and the binding index built from the core's table. No DOM, so node imports it.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  contrastRatio, effectiveBlink, effectiveContrast, effectiveMotion, effectiveUiScale, onAccentOf, osTextScaleOf,
} from "../../dist/a11y.js";
import { rootFontPx } from "../../dist/settings-model.js";
import { CHORDS, actionIdFor, bindingIndex, setBindingTable } from "../../dist/chords.js";
import { chordsOf, primaryChord } from "../../dist/keymap.js";
import { readSettingsView, tokenEntries } from "../../dist/settings-model.js";

const fromRoot = (path) => readFileSync(fileURLToPath(new URL(`../../../${path}`, import.meta.url)), "utf8");
const golden = JSON.parse(readFileSync(fileURLToPath(new URL("../fixtures/protocol.json", import.meta.url)), "utf8"));

// ---- the UI scale and the OS text size -----------------------------------------------------------------

test("effectiveUiScale: an OS size of 100 or a missing or invalid one leaves the user's scale", () => {
  for (const user of [75, 100, 150, 200]) {
    assert.equal(effectiveUiScale(user, 100), user);
    assert.equal(effectiveUiScale(user), user, "no answer");
    for (const bad of [null, undefined, Number.NaN, Number.POSITIVE_INFINITY, "150", {}, [], true, 0, -50, 99, 301, 1e9]) {
      assert.equal(effectiveUiScale(user, bad), user, `${String(bad)} is read as 100`);
    }
  }
});

test("effectiveUiScale: the product is rounded and held to 75 to 300 percent", () => {
  assert.equal(effectiveUiScale(100, 150), 150);
  assert.equal(effectiveUiScale(110, 125), 138, "137.5 rounds up");
  assert.equal(effectiveUiScale(133, 133), 177, "176.89 rounds to the nearest");
  assert.equal(effectiveUiScale(150, 200), 300);
  assert.equal(effectiveUiScale(200, 150), 300, "the product 300 is the limit itself");
  assert.equal(effectiveUiScale(200, 225), 300, "450 is held to 300");
  assert.equal(effectiveUiScale(200, 300), 300);
  assert.equal(effectiveUiScale(75, 100), 75);
  assert.equal(effectiveUiScale(60, 100), 75, "a product below 75 is held to 75");
  assert.equal(effectiveUiScale(40, 130), 75);
  assert.equal(rootFontPx(effectiveUiScale(200, 150)), 36, "150 % of a 200 % scale is a 36 px root font size");
  assert.equal(rootFontPx(effectiveUiScale(100, 150)), 18);
});

test("osTextScaleOf: a whole percent in 100 to 300, anything else is 100", () => {
  assert.equal(osTextScaleOf(100), 100);
  assert.equal(osTextScaleOf(150), 150);
  assert.equal(osTextScaleOf(300), 300);
  assert.equal(osTextScaleOf(149.6), 150, "a fraction is rounded");
  assert.equal(osTextScaleOf(99), 100);
  assert.equal(osTextScaleOf(301), 100, "beyond what the shell may answer is no answer");
  for (const bad of [null, undefined, Number.NaN, "150", {}]) assert.equal(osTextScaleOf(bad), 100);
});

// ---- motion --------------------------------------------------------------------------------------------

test("effectiveMotion: system follows the OS, reduce and full are the user's and never ask the OS", () => {
  assert.equal(effectiveMotion("system", true), "reduce");
  assert.equal(effectiveMotion("system", false), "full");
  assert.equal(effectiveMotion("reduce", false), "reduce", "an explicit reduce holds without an OS request");
  assert.equal(effectiveMotion("reduce", true), "reduce");
  assert.equal(effectiveMotion("full", true), "full", "an explicit full beats the OS");
  assert.equal(effectiveMotion("full", false), "full");
});

test("effectiveMotion treats a value it does not know like system", () => {
  assert.equal(effectiveMotion("sometimes", true), "reduce");
  assert.equal(effectiveMotion(undefined, false), "full");
});

// ---- contrast ------------------------------------------------------------------------------------------

test("effectiveContrast: system is high when the OS asks for more contrast or forces colours", () => {
  assert.equal(effectiveContrast("system", true, false), "high");
  assert.equal(effectiveContrast("system", false, true), "high");
  assert.equal(effectiveContrast("system", true, true), "high");
  assert.equal(effectiveContrast("system", false, false), "normal");
  assert.equal(effectiveContrast("system", false), "normal", "forced colours default to off");
});

test("effectiveContrast: high is always high and normal is never high", () => {
  for (const [more, forced] of [[false, false], [true, false], [false, true], [true, true]]) {
    assert.equal(effectiveContrast("high", more, forced), "high", `high with ${more}/${forced}`);
    assert.equal(effectiveContrast("normal", more, forced), "normal", `normal with ${more}/${forced}`);
  }
});

// ---- cursor blink --------------------------------------------------------------------------------------

test("effectiveBlink: system blinks only when the engine asks and motion is not reduced", () => {
  assert.equal(effectiveBlink("system", false, true), true);
  assert.equal(effectiveBlink("system", true, true), false, "reduced motion keeps the cursor solid");
  assert.equal(effectiveBlink("system", false, false), false, "the engine did not ask for a blinking cursor");
  assert.equal(effectiveBlink("system", true, false), false);
});

test("effectiveBlink: on blinks only when the engine blinks, off never does", () => {
  assert.equal(effectiveBlink("on", false, true), true);
  assert.equal(effectiveBlink("on", true, true), true, "an explicit on is the user's choice");
  assert.equal(effectiveBlink("on", false, false), false, "a cursor the program made steady stays steady");
  assert.equal(effectiveBlink("off", false, true), false);
  assert.equal(effectiveBlink("off", true, true), false);
  assert.equal(effectiveBlink("off", false, false), false);
});

// ---- measured contrast ---------------------------------------------------------------------------------

test("contrastRatio is the WCAG ratio: 21 for black on white, 1 for a colour on itself, the design table for ink", () => {
  assert.equal(contrastRatio(0x000000, 0xffffff), 21);
  assert.equal(contrastRatio(0xffffff, 0x000000), 21, "the order does not matter");
  assert.equal(contrastRatio(0xc23b72, 0xc23b72), 1);
  // docs/design.md section 2: ink 16.99 and rose-deep 3.84 on the ground.
  assert.ok(Math.abs(contrastRatio(0xf6efe6, 0x14091a) - 16.99) < 0.01, String(contrastRatio(0xf6efe6, 0x14091a)));
  assert.ok(Math.abs(contrastRatio(0xc23b72, 0x14091a) - 3.84) < 0.01, String(contrastRatio(0xc23b72, 0x14091a)));
});

test("the text on the accent fill reaches 4.5:1 on the fill of every built-in theme (plan 20 deferred defect)", () => {
  const source = fromRoot("core/src/theme.rs");
  const fills = [...source.matchAll(/rose_deep:\s*hex\(0x([0-9A-Fa-f]{6})\)/g)].map((m) => Number.parseInt(m[1], 16));
  assert.equal(fills.length, 3, "aubergine, Schiefer and Papier each define a rose_deep");
  for (const fill of fills) {
    const text = onAccentOf(fill);
    const ratio = contrastRatio(text, fill);
    assert.ok(ratio >= 4.5, `text ${text.toString(16)} on ${fill.toString(16)} measures ${ratio.toFixed(2)}:1`);
  }
});

test("onAccentOf picks white or black, whichever measures higher, and reaches 4.5:1 on every fill", () => {
  assert.equal(onAccentOf(0xc23b72), 0xffffff);
  assert.equal(onAccentOf(0xffe6f0), 0x000000, "a light fill takes black text");
  assert.equal(onAccentOf(0x000000), 0xffffff);
  assert.equal(onAccentOf(0xffffff), 0x000000);
  // The best of white and black is at least 4.58:1 for every colour, so a fill from a user's theme is covered too.
  for (let r = 0; r < 256; r += 15) {
    for (let g = 0; g < 256; g += 15) {
      for (let b = 0; b < 256; b += 15) {
        const fill = (r << 16) | (g << 8) | b;
        const ratio = contrastRatio(onAccentOf(fill), fill);
        assert.ok(ratio >= 4.5, `fill ${fill.toString(16).padStart(6, "0")} gets ${ratio.toFixed(2)}:1`);
      }
    }
  }
});

test("the theme sets --hw-on-accent from its rose_deep and a bad rose_deep leaves none", () => {
  const view = readSettingsView(structuredClone(golden.settings_view));
  const value = Object.fromEntries(tokenEntries(view.theme));
  assert.equal(value["--hw-on-accent"], "#ffffff");

  const light = structuredClone(golden.settings_view.theme);
  light.chrome.rose_deep = "#FFE6F0";
  assert.equal(Object.fromEntries(tokenEntries(light))["--hw-on-accent"], "#000000");

  for (const bad of ["red", "#c23b7", "#c23b72; color: red", 0xc23b72, null]) {
    const theme = structuredClone(golden.settings_view.theme);
    theme.chrome.rose_deep = bad;
    const names = tokenEntries(theme).map(([n]) => n);
    assert.equal(names.includes("--hw-on-accent"), false, `rose_deep ${JSON.stringify(bad)} set a token`);
  }
});

// ---- the binding index (DESK-25) -----------------------------------------------------------------------

const row = (action, chord) => ({ action, title: action, chord, is_default: true });

test("bindingIndex maps chords to action ids and skips unbound rows", () => {
  const { index, conflicts } = bindingIndex([
    row("pane.split_right", "Cmd+D"),
    row("pane.close", null),
    row("tab.new", "Cmd+T"),
  ]);
  assert.deepEqual([...index], [["Cmd+D", "pane.split_right"], ["Cmd+T", "tab.new"]]);
  assert.deepEqual(conflicts, []);
});

test("two rows with the same chord keep the first and the later one is reported", () => {
  const { index, conflicts } = bindingIndex([
    row("pane.split_right", "Cmd+D"),
    row("tab.new", "Cmd+D"),
    row("pane.close", "Cmd+W"),
  ]);
  assert.equal(index.get("Cmd+D"), "pane.split_right");
  assert.equal(index.size, 2);
  assert.deepEqual(conflicts, [{ chord: "Cmd+D", kept: "pane.split_right", dropped: "tab.new" }]);
});

test("rows that are not rows are left out of the index", () => {
  const { index } = bindingIndex([null, 7, {}, { action: "a" }, { action: 5, chord: "Cmd+A" }, row("ok", "Cmd+O")]);
  assert.deepEqual([...index], [["Cmd+O", "ok"]]);
});

test("the static table agrees with the core's default table for every action the core knows", () => {
  const rows = golden.settings_view.keybindings;
  assert.equal(rows.length, 47);
  const { index, conflicts } = bindingIndex(rows);
  assert.deepEqual(conflicts, []);
  for (const [chord, action] of CHORDS) {
    if (action === "edit.copy") continue; // a fixed shortcut: the core has no such action to rebind
    assert.equal(index.get(chord), action, `${chord} is ${action} in chords.ts but ${index.get(chord)} in the core`);
  }
  for (const r of rows) {
    if (r.chord === null) continue; // an action the user binds himself (the rename actions)
    assert.equal(CHORDS.get(r.chord), r.action, `${r.action} has ${r.chord} in the core`);
  }
});

test("dispatch uses the core's table once it is set and the static table before and after an empty one", () => {
  assert.equal(actionIdFor("Cmd+D"), "pane.split_right", "static table before any settings");
  const rows = golden.settings_view.keybindings.map((r) => ({ ...r }));
  rows.find((r) => r.action === "pane.split_right").chord = "Cmd+Shift+Enter";
  rows.find((r) => r.action === "pane.close").chord = null;
  setBindingTable(rows);
  try {
    assert.equal(actionIdFor("Cmd+Shift+Enter"), "pane.split_right");
    assert.equal(actionIdFor("Cmd+D"), undefined, "the old binding stops working");
    assert.equal(actionIdFor("Cmd+W"), undefined, "an action the core left unbound is not revived by the static table");
    assert.equal(actionIdFor("Cmd+T"), "tab.new", "the other rows are as the core said");
    assert.equal(actionIdFor("Cmd+C"), "edit.copy", "a fixed shortcut the core's table does not know stays");
    setBindingTable([]);
    assert.equal(actionIdFor("Cmd+D"), "pane.split_right", "no rows: the static table again");
  } finally {
    setBindingTable([]);
  }
});

// ---- the chord a press records ------------------------------------------------------------------------

const ev = (code, key, extra = {}) => ({ code, key, shiftKey: false, ctrlKey: false, altKey: false, metaKey: false, ...extra });

test("primaryChord is the core's canonical spelling: Cmd, Ctrl, Alt, Shift and then the key", () => {
  assert.equal(primaryChord(ev("KeyD", "d", { metaKey: true, shiftKey: true })), "Cmd+Shift+D");
  assert.equal(primaryChord(ev("KeyD", "D", { metaKey: true, shiftKey: true })), "Cmd+Shift+D", "Shift types a capital");
  assert.equal(primaryChord(ev("KeyY", "y", { shiftKey: true, altKey: true, ctrlKey: true, metaKey: true })), "Cmd+Ctrl+Alt+Shift+Y");
  assert.equal(primaryChord(ev("Enter", "Enter", { metaKey: true, shiftKey: true })), "Cmd+Shift+Enter");
  assert.equal(primaryChord(ev("ArrowUp", "ArrowUp", { metaKey: true })), "Cmd+Up");
  assert.equal(primaryChord(ev("Space", " ", { metaKey: true, shiftKey: true })), "Cmd+Shift+Space");
  assert.equal(primaryChord(ev("F5", "F5", { metaKey: true })), "Cmd+F5");
  assert.equal(primaryChord(ev("Digit1", "1", { metaKey: true, altKey: true })), "Cmd+Alt+1");
});

test("primaryChord takes the key for letters and digits and the physical key for everything else", () => {
  assert.equal(primaryChord(ev("BracketLeft", "ü", { metaKey: true })), "Cmd+[", "a layout that types u-umlaut there");
  assert.equal(primaryChord(ev("KeyA", "å", { metaKey: true, altKey: true })), "Cmd+Alt+A", "Option composes a character");
  assert.equal(primaryChord(ev("Digit1", "&", { metaKey: true })), "Cmd+1", "AZERTY types & on the digit row");
  assert.equal(primaryChord(ev("KeyQ", "a", { metaKey: true })), "Cmd+A", "AZERTY: the typed letter, as the core's chords are");
  assert.equal(primaryChord(ev("Equal", "+", { metaKey: true, shiftKey: true })), "Cmd+Shift+=", "plus is never a key");
  assert.equal(primaryChord(ev("Minus", "-", { metaKey: true })), "Cmd+-");
});

test("primaryChord is null for a modifier on its own and for a key the grammar has no name for", () => {
  for (const [code, key] of [["MetaLeft", "Meta"], ["ShiftLeft", "Shift"], ["ControlLeft", "Control"], ["AltLeft", "Alt"], ["CapsLock", "CapsLock"]]) {
    assert.equal(primaryChord(ev(code, key, { metaKey: true })), null, key);
  }
  assert.equal(primaryChord(ev("", "Unidentified")), null);
  assert.equal(primaryChord(ev("MediaPlayPause", "MediaPlayPause", { metaKey: true })), null);
});

test("a recorded chord is one of the chords dispatch tries for the same event", () => {
  const events = [
    ev("KeyD", "d", { metaKey: true }),
    ev("KeyD", "D", { metaKey: true, shiftKey: true }),
    ev("BracketLeft", "ü", { metaKey: true }),
    ev("KeyA", "å", { metaKey: true, altKey: true }),
    ev("Digit1", "&", { metaKey: true }),
    ev("Digit1", "1", { metaKey: true, altKey: true }),
    ev("Equal", "=", { metaKey: true }),
    ev("Enter", "Enter", { metaKey: true }),
    ev("ArrowDown", "ArrowDown", { metaKey: true, altKey: true }),
    ev("F12", "F12", { ctrlKey: true, altKey: true }),
  ];
  for (const e of events) {
    const chord = primaryChord(e);
    assert.ok(chord !== null && chordsOf(e).includes(chord), `${chord} is not among ${JSON.stringify(chordsOf(e))}`);
  }
});

// ---- reading the table from the view -------------------------------------------------------------------

test("the settings view carries the binding rows and the overrides the user typed", () => {
  const raw = structuredClone(golden.settings_view);
  raw.settings.keybindings = { "pane.close": "Cmd+Shift+W", "tab.new": "", broken: 5 };
  const view = readSettingsView(raw);
  assert.equal(view.keybindings.length, 47);
  assert.deepEqual(view.keybindings[0], { action: "pane.split_right", title: "Split pane right", chord: "Cmd+D", is_default: true });
  assert.deepEqual(view.settings.keybindings, { "pane.close": "Cmd+Shift+W", "tab.new": "" }, "a non-text value is left out");
});

test("a view without rows has none, and malformed or repeated rows are left out one by one", () => {
  const plain = structuredClone(golden.settings_view);
  delete plain.keybindings;
  delete plain.settings.keybindings;
  const none = readSettingsView(plain);
  assert.deepEqual(none.keybindings, []);
  assert.deepEqual(none.settings.keybindings, {});

  const raw = structuredClone(golden.settings_view);
  raw.keybindings = [
    row("a", "Cmd+A"),
    { action: "b", title: "B", chord: 5, is_default: true },
    { action: "c", chord: "Cmd+C", is_default: true },
    { action: "d", title: "D", chord: null, is_default: false },
    row("a", "Cmd+Z"),
    "x",
  ];
  const view = readSettingsView(raw);
  assert.deepEqual(view.keybindings.map((r) => [r.action, r.chord]), [["a", "Cmd+A"], ["d", null]]);
});
