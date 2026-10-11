// The pure side of the accessibility preferences on the compiled output in dist/ (DESK-24, DESK-25): how a setting
// and the OS signal resolve to motion, contrast and cursor blink, the measured contrast of the text on the accent
// fill, and the binding index built from the core's table. No DOM, so node imports it.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { contrastRatio, effectiveBlink, effectiveContrast, effectiveMotion, onAccentOf } from "../../dist/a11y.js";
import { readSettingsView, tokenEntries } from "../../dist/settings-model.js";

const fromRoot = (path) => readFileSync(fileURLToPath(new URL(`../../../${path}`, import.meta.url)), "utf8");
const golden = JSON.parse(readFileSync(fileURLToPath(new URL("../fixtures/protocol.json", import.meta.url)), "utf8"));

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
