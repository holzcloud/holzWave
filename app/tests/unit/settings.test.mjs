// The pure half of the settings UI on the compiled output in dist/: colours from the core's theme become CSS custom
// property values only when they are exactly #rrggbb (T-02-71), and the view the core sends is read with its types
// checked. The golden sample is the `settings_view` of tests/fixtures/protocol.json.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  DEFAULT_FONT_SIZE, fontStep, lookOf, parseHex, readSettingsView, rootFontPx, tokenEntries, typedInteger,
} from "../../dist/settings-model.js";

const golden = JSON.parse(readFileSync(fileURLToPath(new URL("../fixtures/protocol.json", import.meta.url)), "utf8"));
const sample = () => structuredClone(golden.settings_view);

const CHROME_TOKENS = [
  "--hw-ground", "--hw-ground-2", "--hw-pane", "--hw-edge", "--hw-ink", "--hw-ink-2", "--hw-ink-3", "--hw-rose",
  "--hw-rose-deep", "--hw-ok", "--hw-warn", "--hw-danger", "--hw-info",
];

test("parseHex reads exactly #rrggbb, in either case", () => {
  assert.equal(parseHex("#ff8fb8"), 0xff8fb8);
  assert.equal(parseHex("#FF8FB8"), 0xff8fb8);
  assert.equal(parseHex("#000000"), 0);
  assert.equal(parseHex("#ffffff"), 0xffffff);
});

test("parseHex rejects everything else", () => {
  const bad = [
    "ff8fb8", "#fff", "#ff8fb", "#ff8fb80", "#ff8fb8aa", " #ff8fb8", "#ff8fb8 ", "#ff8fb8\n", "#ff8fb8;", "#gg0000",
    "red", "rgb(1 2 3)", "url(http://example.test/x)", "#ff8fb8;background:red", "", "#", 0xff8fb8, null, undefined, {}, [],
  ];
  for (const value of bad) assert.equal(parseHex(value), null, `${JSON.stringify(value)} was accepted`);
});

test("the golden theme produces the thirteen chrome tokens, the selection and the rose wash", () => {
  const entries = tokenEntries(sample().theme);
  const names = entries.map(([name]) => name);
  assert.deepEqual([...names].sort(), [...CHROME_TOKENS, "--hw-on-accent", "--hw-rose-wash", "--hw-selection"].sort());
  assert.equal(new Set(names).size, names.length, "a token is set twice");

  const value = Object.fromEntries(entries);
  assert.equal(value["--hw-ground"], "#14091a");
  assert.equal(value["--hw-ground-2"], "#1d0f26");
  assert.equal(value["--hw-ink-2"], "#b2aaa9");
  assert.equal(value["--hw-rose"], "#ff8fb8");
  assert.equal(value["--hw-rose-deep"], "#c23b72");
  assert.equal(value["--hw-on-accent"], "#ffffff", "the text on the accent fill is measured against rose_deep");
  assert.equal(value["--hw-selection"], "rgb(255 143 184 / 0.35)");
  assert.equal(value["--hw-rose-wash"], "rgb(255 143 184 / 0.14)");
  for (const token of CHROME_TOKENS) assert.match(value[token], /^#[0-9a-f]{6}$/, `${token}: ${value[token]}`);
});

test("a value that is not #rrggbb never reaches a token", () => {
  const theme = sample().theme;
  theme.chrome.ground = "red";
  theme.chrome.ink = "#12345";
  theme.chrome.rose = "#ff8fb8; background: url(http://example.test/x)";
  theme.chrome.edge = "var(--hw-ink)";
  theme.chrome.pane = 0x1b0e20;
  const value = Object.fromEntries(tokenEntries(theme));
  for (const token of ["--hw-ground", "--hw-ink", "--hw-rose", "--hw-edge", "--hw-pane"]) {
    assert.equal(token in value, false, `${token} was set from a bad value: ${value[token]}`);
  }
  assert.equal(value["--hw-ground-2"], "#1d0f26", "the good values stay");
  // The rose wash and the selection come from a colour each: a bad rose leaves no wash behind.
  assert.equal("--hw-rose-wash" in value, false);
  for (const [name, text] of tokenEntries(theme)) {
    assert.match(text, /^(#[0-9a-f]{6}|rgb\(\d{1,3} \d{1,3} \d{1,3} \/ [01](\.\d{1,3})?\))$/, `${name}: ${text}`);
  }
});

test("the selection needs a colour and an opacity between 0 and 1", () => {
  for (const alpha of [Number.NaN, -0.1, 1.01, "0.3", null, undefined, Infinity]) {
    const theme = sample().theme;
    theme.terminal.selection_alpha = alpha;
    const names = tokenEntries(theme).map(([n]) => n);
    assert.equal(names.includes("--hw-selection"), false, `opacity ${String(alpha)} was accepted`);
  }
  const theme = sample().theme;
  theme.terminal.selection = "#C23B72";
  theme.terminal.selection_alpha = 0.25;
  assert.equal(Object.fromEntries(tokenEntries(theme))["--hw-selection"], "rgb(194 59 114 / 0.25)");
  theme.terminal.selection_alpha = 1;
  assert.equal(Object.fromEntries(tokenEntries(theme))["--hw-selection"], "rgb(194 59 114 / 1)");
});

test("a theme that is not an object has no tokens", () => {
  for (const theme of [null, undefined, 5, "aubergine", [], {}, { chrome: null }, { chrome: "x" }]) {
    assert.deepEqual(tokenEntries(theme), [], JSON.stringify(theme));
  }
});

test("the golden settings view is read as it is", () => {
  const view = readSettingsView(sample());
  assert.ok(view, "the golden view was refused");
  assert.equal(view.settings.theme, "aubergine");
  assert.equal(view.settings.font_size, 14);
  assert.equal(view.settings.ui_scale, 100);
  assert.equal(view.settings.confirm_multiline_paste, true);
  assert.deepEqual(view.themes.map((t) => t.name), ["aubergine", "Schiefer", "Papier"]);
  assert.deepEqual(view.themes.map((t) => t.kind), ["dark", "dark", "light"]);
  assert.equal(view.theme.name, "aubergine");
  assert.deepEqual(view.warnings, ["settings.json: font_size 9999 is outside 8..=40; 40 is used"]);
});

test("a view with a value of the wrong type or kind is refused as a whole", () => {
  const broken = [
    (v) => (v.settings.font_size = "14"),
    (v) => (v.settings.font_size = Number.NaN),
    (v) => (v.settings.ui_scale = null),
    (v) => (v.settings.option_as_alt = "yes"),
    (v) => (v.settings.confirm_multiline_paste = 1),
    (v) => (v.settings.cursor_blink = "sometimes"),
    (v) => (v.settings.motion = "fast"),
    (v) => (v.settings.contrast = 3),
    (v) => (v.settings.theme = 4),
    (v) => (v.settings.scrollback_lines = "10000"),
    (v) => (v.themes = "aubergine"),
    (v) => (v.theme = null),
    (v) => delete v.settings,
  ];
  for (const [i, change] of broken.entries()) {
    const view = sample();
    change(view);
    assert.equal(readSettingsView(view), null, `case ${i} was accepted`);
  }
  for (const raw of [null, undefined, 0, "x", [], {}]) assert.equal(readSettingsView(raw), null);
});

test("malformed theme entries and warnings are left out, the rest stays", () => {
  const view = sample();
  view.themes.push({ name: 5, kind: "dark", builtin: true }, null, { name: "Odd", kind: "dim", builtin: false });
  view.themes.push({ name: "Mine", kind: "light", builtin: false });
  view.warnings.push(7, null);
  const read = readSettingsView(view);
  assert.ok(read);
  assert.deepEqual(read.themes.map((t) => t.name), ["aubergine", "Schiefer", "Papier", "Mine"]);
  assert.deepEqual(read.warnings, ["settings.json: font_size 9999 is outside 8..=40; 40 is used"]);
});

test("the UI scale is the root font size: 12 px at 100 %", () => {
  assert.equal(rootFontPx(100), 12);
  assert.equal(rootFontPx(150), 18);
  assert.equal(rootFontPx(75), 9);
  assert.equal(rootFontPx(200), 24);
  assert.equal(rootFontPx(110), 13.2);
});

test("a view that is in range is the new look; a value out of range or not a number keeps the previous one", () => {
  const before = { fontPx: 14, uiScale: 100 };
  assert.deepEqual(lookOf({ font_size: 20, ui_scale: 150 }, before), { fontPx: 20, uiScale: 150 });
  assert.deepEqual(lookOf({ font_size: 8, ui_scale: 75 }, before), { fontPx: 8, uiScale: 75 });
  assert.deepEqual(lookOf({ font_size: 40, ui_scale: 200 }, before), { fontPx: 40, uiScale: 200 });
  // Each value stands for itself: a bad scale does not cost a good font size.
  assert.deepEqual(lookOf({ font_size: 20, ui_scale: 9999 }, before), { fontPx: 20, uiScale: 100 });
  assert.deepEqual(lookOf({ font_size: 7, ui_scale: 150 }, before), { fontPx: 14, uiScale: 150 });
  assert.deepEqual(lookOf({ font_size: Number.NaN, ui_scale: Infinity }, before), before);
  assert.deepEqual(lookOf({ font_size: "20", ui_scale: null }, before), before);
  // A fractional value is rounded to the whole pixel the core stores.
  assert.deepEqual(lookOf({ font_size: 14.4, ui_scale: 100.6 }, before), { fontPx: 14, uiScale: 101 });
});

test("a typed number is a whole number or nothing", () => {
  assert.equal(typedInteger("18"), 18);
  assert.equal(typedInteger(" 18 "), 18);
  assert.equal(typedInteger("14.6"), 15);
  assert.equal(typedInteger("-3"), -3);
  assert.equal(typedInteger("0"), 0);
  assert.equal(typedInteger("1e3"), 1000);
  for (const text of ["", "   ", "abc", "1,5", "12px", "NaN", "Infinity", "-Infinity", "0x10", "1e999"]) {
    assert.equal(typedInteger(text), null, JSON.stringify(text));
  }
});

test("a font size step is the current size plus the step, from the default when there is none", () => {
  assert.equal(fontStep(14, 1), 15);
  assert.equal(fontStep(14, -1), 13);
  assert.equal(fontStep(40, 1), 41, "the core clamps; the page asks for what the key means");
  assert.equal(fontStep(Number.NaN, 1), DEFAULT_FONT_SIZE + 1);
  assert.equal(fontStep(Infinity, -1), DEFAULT_FONT_SIZE - 1);
  assert.equal(DEFAULT_FONT_SIZE, 14);
});
