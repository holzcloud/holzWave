// The key row of the touch layouts, in the real page against the Tauri stub: the six keys, sticky ctrl and alt, the hardware-keyboard
// rule, a soft keyboard's Backspace and Enter in an empty field, the keyboard-following viewport, and the desktop that stays as it was.
// An Android WebView, a real soft keyboard and a real hardware keyboard are NOT used: the platform is mocked (`app_info` answers
// `android`), the screen size is overridden, and the keys are synthetic `KeyboardEvent`, `InputEvent` and `PointerEvent` objects (and
// Playwright's own key presses) in desktop Chromium and WebKit. What an input method really sends is built to documented behaviour only.
import assert from "node:assert/strict";
import { callsOf } from "./lib.mjs";

// The touch layouts are a property of the Android shell; the web backend never reports `android`.
export const backends = ["tauri"];
export const backendNote = "the key row belongs to the Android shell, which the web backend never is";

export default async function ({ engine, page, url }) {
  await rowCase(page, url);
  await plainCase(page, url);
  await stickyCase(page, url);
  await hardwareCase(page, url);
  await softCase(page, url);
  await viewportCase(page, url);
  await tabletCase(page, url);
  await desktopCase(page, url);
  await cuesCase(page, url, engine);
}

const CTRL = 4;
const ALT = 2;
const SENTINEL = "​";

const VIEW = {
  rev: 1,
  workspaces: [{ name: "main", active: true, panes: 1, pane_ids: [1] }],
  tabs: [{ title: "shell", active: true, focus: 1, panes: 1, pane_ids: [1] }],
  focus: 1,
  panes: [[1, { x: 0, y: 0, w: 1, h: 1 }]],
  dividers: [],
  pane_names: [],
  windows: 1,
};

/** Open the page as a device would: the platform, the viewport and the size of the screen (as the phone test does). */
async function open(page, url, { os = "android", width = 390, height = 844, screen = [390, 844] } = {}) {
  await page.setViewportSize({ width, height });
  await page.addInitScript(([w, h]) => {
    for (const [key, value] of [["width", w], ["height", h], ["availWidth", w], ["availHeight", h]]) {
      Object.defineProperty(window.screen, key, { get: () => value, configurable: true });
    }
  }, screen);
  await page.addInitScript(
    ([info, v]) => {
      window.__MOCK__.on("app_info", () => info);
      window.__MOCK__.setView(v);
    },
    [{ version: "0.0.1-alpha1", os, hostname: "pixel" }, VIEW],
  );
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 1);
}

/** Open as an Android phone and wait until the row is drawn. */
async function openPhone(page, url, options) {
  await open(page, url, options);
  await page.waitForSelector("#keyrow:not([hidden])");
}

const clearCalls = (page) => page.evaluate(() => window.__MOCK__.calls.splice(0));
/** The key commands since the last clear, reduced to what a test compares. */
const keys = async (page) =>
  (await callsOf(page, "key")).map(({ code, mods, release }) => ({ code, mods, release }));
/** A press and a release of one key. */
const stroke = (code, mods = 0) => [
  { code, mods, release: false },
  { code, mods, release: true },
];

const stateOf = (page, name) => page.locator(`#keyrow [data-key="${name}"]`).getAttribute("data-state");
const rowShown = (page) => page.evaluate(() => { const r = document.getElementById("keyrow"); return r !== null && !r.hidden && r.getBoundingClientRect().height > 0; });
const imeFocused = (page) => page.evaluate(() => document.activeElement === document.getElementById("ime"));

/** Tap a key of the row with synthetic pointer events, `times` taps back to back in one task (an exact time between them). */
const tapKey = (page, name, times = 1) =>
  page.evaluate(
    ([n, k]) => {
      const b = document.querySelector(`#keyrow [data-key="${n}"]`);
      const init = { bubbles: true, cancelable: true, pointerId: 7, pointerType: "touch", isPrimary: true, button: 0 };
      for (let i = 0; i < k; i++) {
        b.dispatchEvent(new PointerEvent("pointerdown", init));
        b.dispatchEvent(new PointerEvent("pointerup", init));
      }
    },
    [name, times],
  );

/** What a soft keyboard does for the text `s`: a key of an input method (key code 229), then the edit of the field. */
const soft = (page, s) =>
  page.evaluate((text) => {
    const t = document.getElementById("ime");
    t.dispatchEvent(new KeyboardEvent("keydown", { key: "Unidentified", code: "", keyCode: 229, bubbles: true, cancelable: true }));
    t.dispatchEvent(new InputEvent("beforeinput", { inputType: "insertText", data: text, bubbles: true, cancelable: true }));
    t.value = t.value + text;
    t.dispatchEvent(new InputEvent("input", { inputType: "insertText", data: text, bubbles: true }));
  }, s);

/** A `beforeinput` of the soft keyboard on the terminal input; returns whether the page cancelled it. */
const beforeInput = (page, inputType) =>
  page.evaluate((type) => {
    const t = document.getElementById("ime");
    const e = new InputEvent("beforeinput", { inputType: type, bubbles: true, cancelable: true });
    t.dispatchEvent(e);
    return e.defaultPrevented;
  }, inputType);

const imeValue = (page) => page.evaluate(() => document.getElementById("ime").value);

// ---- the row ---------------------------------------------------------------------------------------------

async function rowCase(page, url) {
  await openPhone(page, url);
  assert.ok(await imeFocused(page), "the terminal input has the focus");
  const row = await page.evaluate(() => {
    const root = document.getElementById("keyrow");
    return {
      role: root.getAttribute("role"),
      names: [...root.querySelectorAll("button")].map((b) => b.dataset.key),
      labels: [...root.querySelectorAll("button")].map((b) => b.textContent),
      sizes: [...root.querySelectorAll("button")].map((b) => b.getBoundingClientRect().toJSON()),
      pressed: [...root.querySelectorAll("button")].map((b) => b.getAttribute("aria-pressed")),
      gap: getComputedStyle(root).columnGap,
      box: root.getBoundingClientRect().toJSON(),
      stage: document.getElementById("stage").getBoundingClientRect().toJSON(),
      nav: document.getElementById("phone-nav").getBoundingClientRect().toJSON(),
      win: [innerWidth, innerHeight],
    };
  });
  assert.equal(row.role, "toolbar");
  assert.deepEqual(row.names, ["esc", "tab", "ctrl", "alt", "up", "down"], "the six keys in the order of the design");
  assert.deepEqual(row.labels, ["esc", "tab", "ctrl", "alt", "↑", "↓"]);
  for (const [i, s] of row.sizes.entries()) {
    assert.ok(s.width >= 43.99 && s.height >= 43.99, `${row.names[i]} is ${s.width} by ${s.height} CSS px`);
  }
  assert.deepEqual(row.pressed, [null, null, "false", "false", null, null], "only ctrl and alt are toggles");
  assert.equal(row.gap, "6px", "6 px between the keys");
  assert.ok(Math.abs(row.stage.bottom - row.box.top) <= 1 && Math.abs(row.box.bottom - row.nav.top) <= 1, "stage, row, bottom bar in a column");
  assert.ok(row.box.width >= row.win[0] - 1, "the row has the width of the screen");

  // The row is drawn only while the terminal input has the focus (a control that took it, such as the bottom bar's, hides the row).
  await page.evaluate(() => document.querySelector("#phone-nav button").focus());
  await page.waitForSelector("#keyrow[hidden]", { state: "attached" });
  assert.equal(await rowShown(page), false, "without the focus in the terminal input the row is gone");
  await page.evaluate(() => document.getElementById("ime").focus());
  await page.waitForSelector("#keyrow:not([hidden])");

  // The same size on a desktop platform has no row at all.
  await open(page, url, { os: "macos" });
  assert.equal(await page.locator("#keyrow").count(), 0, "no key row on macos at the same size");
}

// ---- plain keys --------------------------------------------------------------------------------------------

async function plainCase(page, url) {
  await openPhone(page, url);
  const expect = { esc: "Escape", tab: "Tab", up: "ArrowUp", down: "ArrowDown" };
  for (const [name, code] of Object.entries(expect)) {
    await clearCalls(page);
    await page.locator(`#keyrow [data-key="${name}"]`).click();
    assert.deepEqual(await keys(page), stroke(code), `${name} sends one press and one release of ${code} with no modifier`);
    assert.ok(await imeFocused(page), `the terminal input keeps the focus after ${name}`);
    const first = (await callsOf(page, "key"))[0];
    assert.deepEqual({ key: first.key, repeat: first.repeat, composing: first.composing, pane: first.pane }, { key: code, repeat: false, composing: false, pane: 1 });
  }
  assert.equal((await callsOf(page, "text")).length, 0, "a key of the row is a key, never text");
  assert.ok(await rowShown(page), "the row is still there");
}

// ---- sticky ctrl and alt --------------------------------------------------------------------------------------

async function stickyCase(page, url) {
  await openPhone(page, url);

  // One tap arms; the next character of the soft keyboard is that key with the modifier, not text; then the key is idle again.
  assert.equal(await stateOf(page, "ctrl"), "idle");
  await tapKey(page, "ctrl");
  assert.equal(await stateOf(page, "ctrl"), "armed");
  assert.equal(await page.locator('#keyrow [data-key="ctrl"]').getAttribute("aria-pressed"), "true");
  await clearCalls(page);
  await soft(page, "c");
  assert.deepEqual(await keys(page), stroke("KeyC", CTRL), "ctrl then c is Ctrl+C");
  assert.equal((await callsOf(page, "text")).length, 0, "and not the text c");
  assert.equal(await stateOf(page, "ctrl"), "idle", "the arm is spent");
  assert.equal(await imeValue(page), SENTINEL, "the field is back at its sentinel");
  assert.ok(await imeFocused(page));

  // A plain commit without a modifier is text.
  await clearCalls(page);
  await soft(page, "c");
  assert.deepEqual(await callsOf(page, "text"), [{ pane: 1, text: "c" }], "without a modifier c is text");
  assert.deepEqual(await keys(page), []);

  // Two taps within 350 ms lock it: a square mark appears, every key carries ctrl, a tap releases it.
  await tapKey(page, "ctrl", 2);
  assert.equal(await stateOf(page, "ctrl"), "locked");
  assert.equal(await page.locator('#keyrow [data-key="ctrl"] .keyrow-mark').count(), 1, "the filled square of a locked key is drawn");
  await clearCalls(page);
  await soft(page, "a");
  await soft(page, "b");
  assert.deepEqual(await keys(page), [...stroke("KeyA", CTRL), ...stroke("KeyB", CTRL)], "both carry ctrl");
  assert.equal(await stateOf(page, "ctrl"), "locked", "and the lock stays");
  await tapKey(page, "ctrl");
  assert.equal(await stateOf(page, "ctrl"), "idle", "a tap unlocks");
  assert.equal(await page.locator('#keyrow [data-key="ctrl"] .keyrow-mark').count(), 0, "and the square goes");

  // A second tap later than 350 ms cancels the arm.
  await tapKey(page, "alt");
  assert.equal(await stateOf(page, "alt"), "armed");
  await page.waitForTimeout(420);
  await tapKey(page, "alt");
  assert.equal(await stateOf(page, "alt"), "idle", "a slow second tap cancels");

  // An armed ctrl is spent by a key of the row, which carries the bit on its press and its release.
  await tapKey(page, "ctrl");
  await clearCalls(page);
  await page.locator('#keyrow [data-key="esc"]').click();
  assert.deepEqual(await keys(page), stroke("Escape", CTRL), "ctrl then esc: the Escape key carries ctrl");
  assert.equal(await stateOf(page, "ctrl"), "idle");

  // ctrl and alt armed together both apply to the next key.
  await tapKey(page, "ctrl");
  await tapKey(page, "alt");
  await clearCalls(page);
  await soft(page, "x");
  assert.deepEqual(await keys(page), stroke("KeyX", CTRL | ALT), "both modifiers on one key");
  assert.equal(await stateOf(page, "alt"), "idle");

  // A character with no key on a US layout, or more than one character, is text and spends the arm.
  await tapKey(page, "alt");
  await clearCalls(page);
  await soft(page, "é");
  assert.deepEqual(await callsOf(page, "text"), [{ pane: 1, text: "é" }]);
  assert.deepEqual(await keys(page), []);
  assert.equal(await stateOf(page, "alt"), "idle", "the arm is spent by the text");
}

// ---- hardware keyboard ----------------------------------------------------------------------------------------

async function hardwareCase(page, url) {
  await openPhone(page, url);
  // A sticky modifier armed from the row applies to a hardware key too, and the row goes away.
  await tapKey(page, "ctrl");
  await clearCalls(page);
  await page.keyboard.press("a");
  assert.deepEqual(await keys(page), stroke("KeyA", CTRL), "the armed ctrl is on the hardware key, press and release");
  assert.equal(await stateOf(page, "ctrl"), "idle");
  await page.waitForSelector("#keyrow[hidden]", { state: "attached" });
  assert.equal(await rowShown(page), false, "a key event of a hardware keyboard hides the row");

  // The terminal's own shortcut keys still work as the desktop's.
  await clearCalls(page);
  await page.keyboard.press("b");
  assert.deepEqual(await keys(page), stroke("KeyB", 0), "a hardware key is sent as before");

  // The soft keyboard again (a key code 229, or a composition) shows the row: the last source decides.
  await page.evaluate(() =>
    document.getElementById("ime").dispatchEvent(new KeyboardEvent("keydown", { key: "Unidentified", code: "", keyCode: 229, bubbles: true, cancelable: true })),
  );
  await page.waitForSelector("#keyrow:not([hidden])");
  assert.equal(await rowShown(page), true, "a key code 229 brings the row back");

  await page.keyboard.press("a");
  await page.waitForSelector("#keyrow[hidden]", { state: "attached" });
  await page.evaluate(() =>
    document.getElementById("ime").dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true, data: "" })),
  );
  await page.waitForSelector("#keyrow:not([hidden])");
  assert.equal(await rowShown(page), true, "a composition brings the row back");
  await page.evaluate(() =>
    document.getElementById("ime").dispatchEvent(new CompositionEvent("compositionend", { bubbles: true, data: "" })),
  );
}

// ---- Backspace and Enter on an empty field ----------------------------------------------------------------------

async function softCase(page, url) {
  await openPhone(page, url);
  assert.equal(await imeValue(page), SENTINEL, "the field holds one sentinel on a touch layout");

  await clearCalls(page);
  assert.equal(await beforeInput(page, "deleteContentBackward"), true, "the page takes the Backspace over");
  assert.deepEqual(await keys(page), stroke("Backspace"), "one Backspace key");
  assert.equal(await imeValue(page), SENTINEL, "the field stays at the sentinel");

  await clearCalls(page);
  assert.equal(await beforeInput(page, "insertLineBreak"), true);
  assert.deepEqual(await keys(page), stroke("Enter"), "insertLineBreak is Enter");
  await clearCalls(page);
  await beforeInput(page, "insertParagraph");
  assert.deepEqual(await keys(page), stroke("Enter"), "insertParagraph is Enter");
  await clearCalls(page);
  await beforeInput(page, "deleteWordBackward");
  assert.deepEqual(await keys(page), stroke("KeyW", CTRL), "a deleted word is the shell's Ctrl+W");
  await clearCalls(page);
  assert.equal(await beforeInput(page, "insertText"), false, "an edit that is text is left to `input`");
  assert.deepEqual(await keys(page), []);
  assert.equal(await imeValue(page), SENTINEL);

  // A commit of several characters is one text command and spends an armed modifier; the sentinel is not part of it.
  await tapKey(page, "alt");
  await clearCalls(page);
  await soft(page, "ls -l");
  assert.deepEqual(await callsOf(page, "text"), [{ pane: 1, text: "ls -l" }], "one text command, without the sentinel");
  assert.deepEqual(await keys(page), []);
  assert.equal(await stateOf(page, "alt"), "idle", "and it clears the armed alt");
  assert.equal(await imeValue(page), SENTINEL);

  // An armed modifier applies to a soft Backspace as well (Ctrl+Backspace is a key of its own).
  await tapKey(page, "ctrl");
  await clearCalls(page);
  await beforeInput(page, "deleteContentBackward");
  assert.deepEqual(await keys(page), stroke("Backspace", CTRL));
}

// ---- the keyboard-following viewport ---------------------------------------------------------------------------

async function viewportCase(page, url) {
  await openPhone(page, url);
  const vvh = () => page.evaluate(() => document.documentElement.style.getPropertyValue("--hw-vvh"));
  assert.equal(await vvh(), "844px", "the variable is set to the visual viewport at start");
  await page.setViewportSize({ width: 390, height: 500 });
  await page.waitForFunction(() => document.documentElement.style.getPropertyValue("--hw-vvh") === "500px");
  assert.equal(await vvh(), "500px", "the variable follows the visual viewport");
  assert.equal(await page.evaluate(() => document.documentElement.classList.contains("layout-phone")), true, "the layout class is the same");
  assert.equal(await rowShown(page), true, "the row stays");
  const box = await page.evaluate(() => ({
    row: document.getElementById("keyrow").getBoundingClientRect().toJSON(),
    nav: document.getElementById("phone-nav").getBoundingClientRect().toJSON(),
    app: document.getElementById("app").getBoundingClientRect().toJSON(),
    h: innerHeight,
  }));
  assert.ok(box.row.bottom <= box.h + 0.5 && box.row.bottom <= box.nav.top + 1, "the row is above the bottom edge and the bar");
  assert.ok(Math.abs(box.app.height - 500) <= 1, `the page is as tall as the visual viewport (${box.app.height})`);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.waitForFunction(() => document.documentElement.style.getPropertyValue("--hw-vvh") === "844px");
}

// ---- the tablet ---------------------------------------------------------------------------------------------------

async function tabletCase(page, url) {
  await openPhone(page, url, { width: 1280, height: 800, screen: [1280, 800] });
  assert.equal(await page.evaluate(() => document.documentElement.classList.contains("layout-tablet")), true);
  const where = await page.evaluate(() => ({
    ids: [...document.getElementById("app").children].map((n) => n.id),
    nav: document.getElementById("phone-nav") !== null,
    vvh: document.documentElement.style.getPropertyValue("--hw-vvh"),
    row: document.getElementById("keyrow").getBoundingClientRect().toJSON(),
    status: document.getElementById("status").getBoundingClientRect().toJSON(),
    h: innerHeight,
  }));
  assert.deepEqual(where.ids.slice(-2), ["keyrow", "status"], "the row is directly above the status line, the last strip of the window");
  assert.equal(where.nav, false, "the tablet has no phone bar");
  assert.equal(where.vvh, "800px");
  assert.ok(Math.abs(where.row.bottom - where.status.top) <= 1, "and touches the status line");
  assert.ok(Math.abs(where.status.bottom - where.h) <= 1, "which touches the bottom edge");
  await clearCalls(page);
  await page.locator('#keyrow [data-key="tab"]').click();
  assert.deepEqual(await keys(page), stroke("Tab"));
}

// ---- unchanged elsewhere ---------------------------------------------------------------------------------------------

async function desktopCase(page, url) {
  for (const os of ["macos", "web", "linux", "windows"]) {
    await open(page, url, { os });
    await page.waitForTimeout(150);
    const seen = await page.evaluate(() => ({
      row: document.getElementById("keyrow") !== null,
      vvh: document.documentElement.style.getPropertyValue("--hw-vvh"),
      sheet: document.querySelector('link[href*="keyrow.css"]') !== null,
      value: document.getElementById("ime").value,
    }));
    assert.deepEqual({ ...seen, value: undefined }, { row: false, vvh: "", sheet: false, value: undefined }, `${os}: no row, no variable, no stylesheet`);
    assert.equal(seen.value, "", `${os}: the input has no sentinel`);

    // The soft keyboard's text goes through `input` as before, and the field is emptied.
    await page.evaluate(() => {
      const t = document.getElementById("ime");
      t.value = "q";
      t.dispatchEvent(new InputEvent("input", { inputType: "insertText", data: "q", bubbles: true }));
    });
    assert.equal(await imeValue(page), "", `${os}: the input is emptied, not refilled with a sentinel`);
    // A `beforeinput` is nobody's business on a desktop: no key is made of it.
    await clearCalls(page);
    assert.equal(await beforeInput(page, "deleteContentBackward"), false, `${os}: Backspace of the field is the browser's`);
    assert.deepEqual(await keys(page), [], `${os}: no key from a beforeinput`);

    await clearCalls(page);
    await page.keyboard.press("a");
    assert.deepEqual(await keys(page), stroke("KeyA"), `${os}: a keydown of a sends one key as before`);
  }
}

// ---- cues for forced colours -----------------------------------------------------------------------------------------

async function cuesCase(page, url, engine) {
  if (engine !== "chromium") {
    console.log(`NOT RUN keyrow forced colours [${engine}]: this engine cannot emulate forced colours`);
    return;
  }
  await page.emulateMedia({ forcedColors: "active" });
  try {
    await openPhone(page, url);
    const cue = (name) =>
      page.evaluate((n) => {
        const b = document.querySelector(`#keyrow [data-key="${n}"]`);
        const mark = b.querySelector(".keyrow-mark");
        return {
          deco: getComputedStyle(b.querySelector(".keyrow-label")).textDecorationLine,
          border: getComputedStyle(b).borderTopStyle,
          mark: mark ? getComputedStyle(mark).borderTopStyle : null,
        };
      }, name);
    const idle = await cue("ctrl");
    assert.ok(!idle.deco.includes("underline") && idle.mark === null, "idle has no underline and no square");
    await tapKey(page, "ctrl");
    const armed = await cue("ctrl");
    assert.ok(armed.deco.includes("underline") && armed.border !== "none", "armed keeps its underline under forced colours");
    await soft(page, "z"); // spends the arm
    await tapKey(page, "ctrl", 2);
    assert.equal(await stateOf(page, "ctrl"), "locked");
    const locked = await cue("ctrl");
    assert.ok(locked.deco.includes("underline") && locked.mark !== null && locked.mark !== "none", "locked keeps the underline and the square");
  } finally {
    await page.emulateMedia({ forcedColors: "none" });
  }
}
