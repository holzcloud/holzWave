// Accessibility preferences in the real page (DESK-24): reduced motion, higher contrast and forced colours follow the
// OS media queries until the user overrides them in the settings, live and without a reload; the text on the accent
// fill measures at least 4.5:1 on every theme; the cursor blink is gated by the engine, the setting and reduced
// motion and costs no timer while it is solid. The browser stub plays the core.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, emit } from "./lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await motionCase(page, url);
  await contrastCase(page, url);
  await forcedColoursCase(page, url);
  await accentCase(page, url);
  await blinkCase(page, url);
  await blinkResetCase(page, url);
  await blinkPreferenceCase(page, url);
  await blinkCursorCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

const golden = JSON.parse(readFileSync(join(APP_DIR, "tests/fixtures/protocol.json"), "utf8"));
const AUBERGINE = () => structuredClone(golden.settings_view);

/** Papier, enough of it for the accent text to be measured on a light theme. */
const PAPIER = {
  version: 1, name: "Papier", kind: "light",
  terminal: { foreground: "#14091A", background: "#FFF3F8", cursor: "#C23B72", selection: "#C23B72", selection_alpha: 0.25, ansi: [] },
  chrome: {
    ground: "#FFF3F8", ground_2: "#FFE6F0", pane: "#FFF9FB", edge: "#E5C6D5", ink: "#14091A", ink_2: "#5B4F5D",
    ink_3: "#6B5C70", rose: "#C23B72", rose_deep: "#C23B72", ok: "#2A7A18", warn: "#8A5A00", danger: "#B3261E",
    info: "#1F5FA8",
  },
};

/** Open the page against the stub; `view` is what `get_settings` answers (null: the core has nothing to say). */
async function open(page, url, view = AUBERGINE(), query = "?renderer=canvas") {
  await page.addInitScript((v) => {
    window.__MOCK__.on("get_settings", () => structuredClone(v));
    window.__MOCK__.on("reload_settings", () => structuredClone(v));
  }, view);
  await page.goto(`${url}index.html${query}`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(async () => {
    window.__marker = "same page"; // a reload would lose it
    // The page's own state module, for synchronous predicates (waitForFunction does not await an async one).
    window.__state = (await import(new URL("state.js", location.href).href)).state;
  });
}

const rootClass = (page, name) => page.evaluate((n) => document.documentElement.classList.contains(n), name);
const waitClass = (page, name, present) =>
  page.waitForFunction(([n, p]) => document.documentElement.classList.contains(n) === p, [name, present]);

/** The longest transition duration, in seconds, of any element of the chrome and of the dialogs. */
const longestTransition = (page) =>
  page.evaluate(() => {
    const parse = (v) => v.split(",").map((t) => (t.trim().endsWith("ms") ? parseFloat(t) / 1000 : parseFloat(t) || 0));
    const nodes = document.querySelectorAll("#bar, #bar *, #status, #status *, #overlay, #overlay *, .dlg, .dlg *");
    let longest = 0;
    for (const n of nodes) longest = Math.max(longest, ...parse(getComputedStyle(n).transitionDuration));
    return { longest, buttons: document.querySelectorAll("#bar button").length };
  });

/** The colour a token resolves to, in the form getComputedStyle reports it. */
const token = (page, name) =>
  page.evaluate((n) => {
    const probe = document.createElement("span");
    probe.style.color = `var(${n})`;
    document.body.append(probe);
    const c = getComputedStyle(probe).color;
    probe.remove();
    return c;
  }, name);

/** Reduced motion follows the OS until the setting says otherwise, and follows an OS change while the page runs. */
async function motionCase(page, url) {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await open(page, url);
  assert.equal(await rootClass(page, "motion-reduce"), true, "the OS asks for reduced motion");
  const reduced = await longestTransition(page);
  assert.ok(reduced.buttons > 0, "the bar has buttons");
  assert.equal(reduced.longest, 0, "no chrome element has a transition under reduced motion");

  // The setting beats the OS: full motion although the OS asks to reduce.
  const full = AUBERGINE();
  full.settings.motion = "full";
  await emit(page, "settings", full);
  await waitClass(page, "motion-reduce", false);
  assert.ok((await longestTransition(page)).longest > 0, "full motion has the bar's 160 ms transitions back");

  // The OS changes while the page runs: the setting is "system" again, so the class follows without a reload.
  await emit(page, "settings", AUBERGINE());
  await waitClass(page, "motion-reduce", true);
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await waitClass(page, "motion-reduce", false);
  assert.ok((await longestTransition(page)).longest > 0);
  await page.emulateMedia({ reducedMotion: "reduce" });
  await waitClass(page, "motion-reduce", true);
  assert.equal(await page.evaluate(() => window.__marker), "same page", "the page was not reloaded");

  // An explicit reduce holds although the OS has no preference.
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await waitClass(page, "motion-reduce", false);
  const reduce = AUBERGINE();
  reduce.settings.motion = "reduce";
  await emit(page, "settings", reduce);
  await waitClass(page, "motion-reduce", true);
  assert.equal((await longestTransition(page)).longest, 0);
}

/** Higher contrast follows prefers-contrast and the setting; the focus line is 2 px and the edges use ink-3. */
async function contrastCase(page, url) {
  await page.emulateMedia({ reducedMotion: null, contrast: "no-preference", forcedColors: null });
  await open(page, url);
  const look = () =>
    page.evaluate(() => ({
      head: getComputedStyle(document.querySelector(".panehead.focus-line")).borderTopWidth,
      bar: getComputedStyle(document.getElementById("bar")).borderBottomColor,
      status: getComputedStyle(document.getElementById("status")).borderTopColor,
    }));
  assert.equal(await rootClass(page, "contrast-high"), false);
  const normal = await look();
  assert.equal(normal.head, "1px");
  assert.equal(normal.bar, await token(page, "--hw-edge"), "edges are the edge token in the normal look");

  // The OS asks for more contrast: the class follows live and the look changes.
  await page.emulateMedia({ contrast: "more" });
  await waitClass(page, "contrast-high", true);
  const high = await look();
  assert.equal(high.head, "2px", "the focused pane's top line is 2 px");
  assert.equal(high.bar, await token(page, "--hw-ink-3"), "edges use the ink-3 token");
  assert.equal(high.status, await token(page, "--hw-ink-3"));
  const outline = await page.evaluate(() => getComputedStyle(document.querySelector("#status .seg.ws.active")).outlineStyle);
  assert.notEqual(outline, "none", "the shown status segment has a border of its own in higher contrast");

  // The setting beats the OS in both directions.
  const normalView = AUBERGINE();
  normalView.settings.contrast = "normal";
  await emit(page, "settings", normalView);
  await waitClass(page, "contrast-high", false);
  assert.equal((await look()).head, "1px");
  await page.emulateMedia({ contrast: "no-preference" });
  const highView = AUBERGINE();
  highView.settings.contrast = "high";
  await emit(page, "settings", highView);
  await waitClass(page, "contrast-high", true);
  assert.equal((await look()).head, "2px");
  assert.equal(await page.evaluate(() => window.__marker), "same page", "the page was not reloaded");
}

/** Forced colours count as high contrast, and the rules written for them use system colours only. */
async function forcedColoursCase(page, url) {
  await page.emulateMedia({ contrast: "no-preference", forcedColors: "active" });
  await open(page, url);
  assert.equal(await page.evaluate(() => matchMedia("(forced-colors: active)").matches), true);
  assert.equal(await rootClass(page, "contrast-high"), true, "forced colours are higher contrast");

  const rules = await page.evaluate(() => {
    const out = [];
    for (const sheet of document.styleSheets) {
      let list;
      try {
        list = sheet.cssRules;
      } catch {
        continue;
      }
      for (const rule of list) {
        if (rule instanceof CSSMediaRule && /forced-colors/.test(rule.conditionText)) {
          for (const inner of rule.cssRules) out.push(inner.cssText);
        }
      }
    }
    return out;
  });
  assert.ok(rules.length >= 4, `the forced-colors block has rules (${rules.length})`);
  for (const text of rules) {
    assert.doesNotMatch(text, /#[0-9a-fA-F]{3,8}\b|\b(?:rgb|rgba|hsl|hsla|color-mix)\(|var\(--hw-(?!focus)/, `a forced-colors rule hard-codes a colour: ${text}`);
  }
  await page.emulateMedia({ forcedColors: null });
}

/** WCAG relative luminance ratio of two "rgb(r, g, b)" strings. */
function ratio(a, b) {
  const lum = (css) => {
    const [r, g, bl] = css.match(/\d+(\.\d+)?/g).slice(0, 3).map(Number).map((v) => {
      const s = v / 255;
      return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
    });
    return 0.2126 * r + 0.7152 * g + 0.0722 * bl;
  };
  const [hi, lo] = [lum(a), lum(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/** The text of the shown status segment measures at least 4.5:1 on its fill, on every theme and without a core. */
async function accentCase(page, url) {
  const measure = () =>
    page.evaluate(() => {
      const seg = document.querySelector("#status .seg.ws.active");
      const css = getComputedStyle(seg);
      return { fill: css.backgroundColor, text: css.color, weight: css.fontWeight, size: css.fontSize };
    });

  await page.emulateMedia({ contrast: "no-preference", forcedColors: null, reducedMotion: null });
  await open(page, url);
  const aubergine = await measure();
  assert.ok(ratio(aubergine.text, aubergine.fill) >= 4.5, `aubergine: ${aubergine.text} on ${aubergine.fill}`);
  assert.equal(aubergine.fill, "rgb(194, 59, 114)", "the fill is still rose-deep");

  const papier = AUBERGINE();
  papier.theme = structuredClone(PAPIER);
  papier.settings.theme = "Papier";
  await emit(page, "settings", papier);
  await page.waitForFunction(() => getComputedStyle(document.documentElement).getPropertyValue("--hw-ground").trim() === "#fff3f8");
  const light = await measure();
  assert.ok(ratio(light.text, light.fill) >= 4.5, `Papier: ${light.text} on ${light.fill}`);

  // A theme whose accent is light gets dark text.
  const pale = AUBERGINE();
  pale.theme.chrome.rose_deep = "#FFE6F0";
  await emit(page, "settings", pale);
  await page.waitForFunction(() => getComputedStyle(document.documentElement).getPropertyValue("--hw-rose-deep").trim() === "#ffe6f0");
  const paleLook = await measure();
  assert.ok(ratio(paleLook.text, paleLook.fill) >= 4.5, `a pale accent: ${paleLook.text} on ${paleLook.fill}`);
  assert.equal(paleLook.text, "rgb(0, 0, 0)");

  // The stylesheet's own value covers a page whose core cannot answer.
  await page.goto("about:blank");
  await page.addInitScript(() => {
    window.__MOCK__.on("get_settings", () => {
      throw new Error("the core is not ready");
    });
  });
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  const bare = await measure();
  assert.ok(ratio(bare.text, bare.fill) >= 4.5, `no core: ${bare.text} on ${bare.fill}`);
  assert.equal((await callsOf(page, "get_settings")).length, 1);
}

// ---- cursor blink --------------------------------------------------------------------------------------

const CURSOR = 0xff8fb8; // the aubergine cursor colour
const BG = 0x14091a;

/** One full frame for `pane` (a 60 x 4 grid of blanks) with the cursor at (3, 1). */
const frame = (pane, seq, cursor = { x: 3, y: 1, style: "block", blink: true }) => ({
  pane, seq, full: true, cols: 60, rows: 4, fg: 0xf6efe6, bg: BG,
  cursor,
  scrollbar: [0, 4, 4], selection: [], matches: [], search: null, copy_cursor: null,
  modes: { alt: false, bracketed_paste: false, mouse: false }, changed: [],
});

/** Count the intervals the page has running: a blink timer exists only while a cursor blinks. */
function countIntervals() {
  const live = new Set();
  const set = window.setInterval.bind(window);
  const clear = window.clearInterval.bind(window);
  window.setInterval = (...args) => {
    const id = set(...args);
    live.add(id);
    return id;
  };
  window.clearInterval = (id) => {
    live.delete(id);
    return clear(id);
  };
  window.__liveIntervals = () => live.size;
}

/** Open with the interval counter installed, a canvas 2D renderer (its pixels can be read at any time) and frames. */
async function openBlink(page, url, { view = AUBERGINE() } = {}) {
  await page.addInitScript(countIntervals);
  await open(page, url, view);
}

const blink = (page) => page.evaluate(() => ({ on: window.__state.blink.on, running: window.__state.blink.running }));
const intervals = (page) => page.evaluate(() => window.__liveIntervals());
const settle = (page) =>
  page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => done()))));
async function waitBlink(page, want) {
  try {
    await page.waitForFunction(
      (w) => window.__state.blink.on === w.on && window.__state.blink.running === w.running,
      want,
    );
  } catch {
    throw new Error(`the blink state never became ${JSON.stringify(want)}, it is ${JSON.stringify(await blink(page))}`);
  }
}

/** The colour of the cell (x, y) of the focused pane as the canvas shows it now. */
const cellColor = (page, x = 3, y = 1) =>
  page.evaluate(
    async ([cx, cy]) => {
      const { state } = await import(new URL("state.js", location.href).href);
      const { gridRect } = await import(new URL("geometry.js", location.href).href);
      const entry = state.view.panes.find(([id]) => id === state.view.focus);
      const g = gridRect(state.metrics, entry[1], state.headerPx);
      const m = state.metrics;
      const px = Math.floor((g.x + (cx + 0.5) * m.cellW) * m.dpr);
      const py = Math.floor((g.y + (cy + 0.5) * m.cellH) * m.dpr);
      const [r, gr, b] = document.getElementById("term").getContext("2d").getImageData(px, py, 1, 1).data;
      return (r << 16) | (gr << 8) | b;
    },
    [x, y],
  );

/** The pixel of the cursor cell once the state has settled (two samples of the phase around it must agree). */
async function shown(page) {
  for (let i = 0; i < 8; i++) {
    const before = await blink(page);
    await settle(page);
    const colour = await cellColor(page);
    const after = await blink(page);
    if (before.on === after.on) return { on: after.on, colour };
  }
  throw new Error("the blink phase never held still for a sample");
}

/** The engine's blinking cursor on the focused pane blinks with the 530 ms phase; solid means no timer. */
async function blinkCase(page, url) {
  await page.emulateMedia({ reducedMotion: "no-preference", contrast: "no-preference", forcedColors: null });
  await openBlink(page, url);
  const base = await intervals(page);

  // Before any frame there is no cursor: no timer.
  assert.deepEqual(await blink(page), { on: true, running: false });

  await page.evaluate((f) => window.__MOCK__.emit("frame", f), frame(2, 1));
  await waitBlink(page, { on: true, running: true });
  assert.equal(await intervals(page), base + 1, "one timer while the cursor blinks");
  const first = await shown(page);
  if (first.on) assert.equal(first.colour, CURSOR, "the visible phase draws the cursor");

  // The phase flips: the cursor cell is the pane background in the off phase and the cursor colour in the on phase.
  await waitBlink(page, { on: false, running: true });
  const off = await shown(page);
  if (!off.on) assert.equal(off.colour, BG, "the off phase omits the cursor");
  await waitBlink(page, { on: true, running: true });
  const on = await shown(page);
  if (on.on) assert.equal(on.colour, CURSOR);
  assert.equal(await intervals(page), base + 1, "the phase does not stack timers");

  // The engine says the cursor is steady: solid and no timer, whatever the setting.
  await page.evaluate((f) => window.__MOCK__.emit("frame", f), frame(2, 2, { x: 3, y: 1, style: "block", blink: false }));
  await waitBlink(page, { on: true, running: false });
  assert.equal(await intervals(page), base, "a steady cursor runs no timer");
  await page.waitForTimeout(700);
  assert.deepEqual(await blink(page), { on: true, running: false });
  assert.equal((await shown(page)).colour, CURSOR);
}

/** Output and keys restart the phase at visible, so a cursor never vanishes while typing. */
async function blinkResetCase(page, url) {
  await openBlink(page, url);
  await page.evaluate((f) => window.__MOCK__.emit("frame", f), frame(2, 1));
  await waitBlink(page, { on: false, running: true });

  // A frame of the focused pane: visible at once, and it stays visible for most of a phase.
  const afterFrame = await page.evaluate((f) => {
    window.__MOCK__.emit("frame", f);
    return window.__state.blink.on;
  }, frame(2, 2));
  assert.equal(afterFrame, true, "a frame restarts the phase at visible");
  await page.waitForTimeout(250);
  assert.equal((await blink(page)).on, true, "the restarted phase lasts a whole period");

  // A key press does the same.
  await waitBlink(page, { on: false, running: true });
  await page.keyboard.press("a");
  assert.equal((await blink(page)).on, true, "a key press restarts the phase at visible");
  await page.waitForTimeout(250);
  assert.equal((await blink(page)).on, true);

  // A frame of a pane without the keyboard is not the focused cursor's output: it does not reset anything.
  await waitBlink(page, { on: false, running: true });
  const other = await page.evaluate((f) => {
    window.__MOCK__.emit("frame", f);
    return window.__state.blink.on;
  }, frame(1, 1));
  assert.equal(other, false, "output of another pane leaves the phase alone");
}

/** The setting and the OS preference start and stop the timer live. */
async function blinkPreferenceCase(page, url) {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await openBlink(page, url);
  const base = await intervals(page);
  await page.evaluate((f) => window.__MOCK__.emit("frame", f), frame(2, 1));
  await waitBlink(page, { on: true, running: true });
  assert.equal(await intervals(page), base + 1);

  const setting = (value) => {
    const v = AUBERGINE();
    v.settings.cursor_blink = value;
    return emit(page, "settings", v);
  };

  // off: solid at once, no timer, and the draw shows the cursor.
  await setting("off");
  await waitBlink(page, { on: true, running: false });
  assert.equal(await intervals(page), base, "the setting off stops the timer");
  await page.waitForTimeout(650);
  assert.deepEqual(await blink(page), { on: true, running: false }, "and it stays solid");
  assert.equal((await shown(page)).colour, CURSOR);

  // system under reduced motion: solid; the OS changes while the page runs.
  await emit(page, "settings", AUBERGINE());
  await waitBlink(page, { on: true, running: true });
  await page.emulateMedia({ reducedMotion: "reduce" });
  await waitBlink(page, { on: true, running: false });
  assert.equal(await intervals(page), base, "reduced motion keeps the cursor solid");

  // on: the user's choice blinks although the OS asks for reduced motion.
  await setting("on");
  await waitBlink(page, { on: true, running: true });
  assert.equal(await intervals(page), base + 1);

  // The Motion setting full beats the OS for system as well.
  const full = AUBERGINE();
  full.settings.motion = "full";
  await emit(page, "settings", full);
  await waitBlink(page, { on: true, running: true });
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await emit(page, "settings", AUBERGINE());
  await waitBlink(page, { on: true, running: true });
  assert.equal(await page.evaluate(() => window.__marker), "same page", "the page was not reloaded");
}

/** Only the focused pane's own cursor counts: a hollow cursor of another pane and a cursor off the grid never blink. */
async function blinkCursorCase(page, url) {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await openBlink(page, url);
  const base = await intervals(page);

  // Pane 1 has no keyboard: its blinking cursor is drawn hollow and steady; pane 2 (focused) has none.
  await page.evaluate((f) => window.__MOCK__.emit("frame", f), frame(1, 1));
  await page.evaluate((f) => window.__MOCK__.emit("frame", f), frame(2, 1, null));
  await settle(page);
  assert.deepEqual(await blink(page), { on: true, running: false });
  assert.equal(await intervals(page), base);

  // Focus moves to pane 1: now its cursor qualifies.
  await page.evaluate(() => {
    const v = window.__MOCK__.view();
    v.focus = 1;
    window.__MOCK__.emit("layout", v);
  });
  await waitBlink(page, { on: true, running: true });
  assert.equal(await intervals(page), base + 1);

  // The focused pane exits: nothing blinks.
  await page.evaluate(() => window.__MOCK__.emit("pane-exit", { pane: 1, code: 0 }));
  await waitBlink(page, { on: true, running: false });
  assert.equal(await intervals(page), base, "an exited pane leaves no timer");
}
