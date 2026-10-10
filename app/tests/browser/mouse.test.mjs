// Mouse reporting and the wheel on the page side (D-24): the wheel always goes to the core as a `wheel` command with
// grid-relative device pixels, and the core decides what it means.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { callsOf, emit } from "./lib.mjs";

const PAD = 6; // CSS px between a pane's edge and its grid (geometry.ts)
const BG = 0x14091a;
const COLS = 20;
const ROWS = 4;
const golden = JSON.parse(readFileSync(new URL("../fixtures/protocol.json", import.meta.url), "utf8"));

/** A frame message for `pane`: 20 x 4 cells with the given mode flags. */
const frame = (pane, seq, modes = {}, over = {}) => ({
  pane, seq, full: false, cols: COLS, rows: ROWS, fg: 0xf6efe6, bg: BG, cursor: null, scrollbar: [0, ROWS, ROWS],
  selection: [], matches: [], search: null, copy_cursor: null,
  modes: { alt: false, bracketed_paste: false, mouse: false, ...modes }, changed: [], ...over,
});

/** Open the page against the stub with a 20 x 4 mirror for both panes; returns the geometry helpers. */
async function open(page, url, modes = {}) {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(() => document.getElementById("term").focus());
  const start = (await callsOf(page, "start_pane"))[0];
  const c = await page.evaluate(() => {
    const r = document.getElementById("term").getBoundingClientRect();
    return { left: r.left, top: r.top, width: r.width };
  });
  const cw = start.cell_w; // dpr 1: device pixels are CSS pixels
  const ch = start.cell_h;
  // Pane 1 is the left half, pane 2 (focused) the right half of the stub's default layout.
  const origin = (pane) => c.left + (pane === 1 ? 0 : c.width / 2) + PAD;
  const geo = {
    cw, ch,
    /** The page point at the centre of cell (cx, cy) of `pane`. */
    pt: (pane, cx, cy) => [origin(pane) + (cx + 0.5) * cw, c.top + PAD + (cy + 0.5) * ch],
    /** The same point relative to the grid origin, in device pixels (what the core must hear). */
    grid: (cx, cy) => [Math.round((cx + 0.5) * cw), Math.round((cy + 0.5) * ch)],
  };
  await emit(page, "frame", frame(1, 1, modes, { full: true }));
  await emit(page, "frame", frame(2, 1, modes, { full: true }));
  return geo;
}

/** Dispatch one wheel event on the canvas as the browser would; resolves to whether the page cancelled it. */
const wheel = (page, [x, y], init) =>
  page.evaluate(
    ([px, py, i]) => {
      const e = new WheelEvent("wheel", { clientX: px, clientY: py, bubbles: true, cancelable: true, ...i });
      document.getElementById("term").dispatchEvent(e);
      return e.defaultPrevented;
    },
    [x, y, init],
  );

const wheels = (page) => callsOf(page, "wheel");
const mouseCalls = (page) => callsOf(page, "mouse");
/** The `mouse` calls that are not hover or drag motion. */
const buttonCalls = async (page) => (await mouseCalls(page)).filter((a) => a.action !== "motion");
const motions = async (page) => (await mouseCalls(page)).filter((a) => a.action === "motion");
const selects = async (page) => (await callsOf(page, "select")).map((a) => ({ pane: a.pane, ...a.op }));
const reset = (page) => page.evaluate(() => window.__MOCK__.reset());

/** Animation frames: whatever was coalesced into one command has been sent. */
const frames = (page, n = 2) =>
  page.evaluate(async (count) => {
    for (let i = 0; i < count; i++) await new Promise((done) => requestAnimationFrame(() => done()));
  }, n);

/** Let the multi-click window of the click before run out, so the next click starts a new count. */
async function fresh(page) {
  await reset(page);
  await page.waitForTimeout(450);
}

/** Dispatch one synthetic pointer event on the canvas at a page point. `init` carries button, buttons, modifiers. */
const pointer = (page, type, [x, y], init = {}) =>
  page.evaluate(
    ([t, px, py, i]) => {
      const e = new PointerEvent(t, {
        clientX: px, clientY: py, pointerId: 7, pointerType: "mouse", isPrimary: true, bubbles: true, cancelable: true, ...i,
      });
      document.getElementById("term").dispatchEvent(e);
    },
    [type, x, y, init],
  );

/** Open with a frame header on both panes that says whether the program tracks the mouse; pane 2 has the keyboard. */
const openTracking = (page, url, mouse = true) => open(page, url, { mouse });

/** Make a header change reach the mirror of `pane`: a full frame replaces the mirror whatever its sequence number. */
const setModes = (page, pane, modes) => emit(page, "frame", frame(pane, 1, modes, { full: true }));

export default async function ({ page, url, errors }) {
  await wheelCase(page, url);
  await wheelRemainderCase(page, url);
  await wheelModesCase(page, url);
  await wheelRealCase(page, url);
  await reportCase(page, url);
  await buttonsCase(page, url);
  await motionCase(page, url);
  await selectionCase(page, url);
  await cancelCase(page, url);
  await unfocusedCase(page, url);
  await contextMenuCase(page, url);
  await handlerCase(page, url);
  await gestureModeCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** Whole rows, grid-relative integers, the pane under the pointer, the modifier bits; never `scroll`. */
async function wheelCase(page, url) {
  const g = await open(page, url);
  const at = g.pt(1, 3, 1);
  const prevented = await wheel(page, at, { deltaY: g.ch * 2.5, deltaMode: 0 });
  assert.equal(prevented, true, "the page takes the wheel");
  assert.deepEqual(await wheels(page), [{ pane: 1, rows: 2, x: g.grid(3, 1)[0], y: g.grid(3, 1)[1], mods: 0 }], "two whole rows");
  assert.deepEqual(await callsOf(page, "scroll"), [], "the page no longer sends scroll for the wheel");

  // Towards older output is a negative delta; the pane is the one under the pointer; Shift and Ctrl are bits.
  await page.evaluate(() => window.__MOCK__.reset());
  const other = g.pt(2, 5, 2);
  await wheel(page, other, { deltaY: -g.ch * 3, deltaMode: 0, shiftKey: true, ctrlKey: true });
  assert.deepEqual(await wheels(page), [{ pane: 2, rows: -3, x: g.grid(5, 2)[0], y: g.grid(5, 2)[1], mods: 5 }]);

  // Line mode is three rows per line.
  await page.evaluate(() => window.__MOCK__.reset());
  await wheel(page, other, { deltaY: 1, deltaMode: 1, altKey: true, metaKey: true });
  assert.deepEqual((await wheels(page)).map((a) => [a.rows, a.mods]), [[3, 10]], "one line is three rows; alt and super are bits");

  // The payload has the golden's keys and integer coordinates inside the grid.
  const sent = (await wheels(page))[0];
  assert.deepEqual(Object.keys(sent).sort(), [...golden.commands.wheel].sort(), "the wheel payload has the golden's keys");
  assert.ok(Number.isInteger(sent.x) && sent.x >= 0 && sent.x < COLS * g.cw, "x is an integer inside the grid");
  assert.ok(Number.isInteger(sent.y) && sent.y >= 0 && sent.y < ROWS * g.ch, "y is an integer inside the grid");

  // Below the last row (inside the pane, the frame has only four rows) the position is clamped, not extrapolated.
  await page.evaluate(() => window.__MOCK__.reset());
  await wheel(page, [g.pt(2, 2, 0)[0], g.pt(2, 0, 0)[1] + g.ch * 10], { deltaY: g.ch, deltaMode: 0 });
  const low = (await wheels(page))[0];
  assert.equal(low.y, ROWS * g.ch - 1, "below the grid is the last device row");
  assert.equal(low.x, g.grid(2, 0)[0], "x is that of the pointer");
}

/** A wheel smaller than a row sends nothing until the remainder adds up to one. */
async function wheelRemainderCase(page, url) {
  const g = await open(page, url);
  const at = g.pt(2, 4, 1);
  await wheel(page, at, { deltaY: g.ch * 0.4, deltaMode: 0 });
  await wheel(page, at, { deltaY: g.ch * 0.4, deltaMode: 0 });
  assert.deepEqual(await wheels(page), [], "two thirds of a row sends nothing");
  await wheel(page, at, { deltaY: g.ch * 0.4, deltaMode: 0 });
  assert.deepEqual((await wheels(page)).map((a) => a.rows), [1], "the remainder added up to one row");
  await wheel(page, at, { deltaY: g.ch * 0.8, deltaMode: 0 });
  assert.deepEqual((await wheels(page)).map((a) => a.rows), [1, 1], "0.2 + 0.8 is one more row, nothing was lost");
  await wheel(page, at, { deltaY: g.ch * 0.1, deltaMode: 0 });
  assert.equal((await wheels(page)).length, 2, "a tenth of a row sends nothing");
}

/** The page does not decide: the same command goes out whatever the live modes are. */
async function wheelModesCase(page, url) {
  for (const modes of [{ alt: true }, { mouse: true }, { alt: true, mouse: true }, { alt: false, mouse: false }]) {
    const g = await open(page, url, modes);
    await wheel(page, g.pt(2, 2, 2), { deltaY: g.ch * 2, deltaMode: 0 });
    assert.deepEqual(
      await wheels(page),
      [{ pane: 2, rows: 2, x: g.grid(2, 2)[0], y: g.grid(2, 2)[1], mods: 0 }],
      `the wheel is a wheel command with modes ${JSON.stringify(modes)}`,
    );
    assert.deepEqual(await callsOf(page, "scroll"), [], "never scroll");
  }
}

/** A real wheel from the engine's input path reaches the core as a wheel command too. */
async function wheelRealCase(page, url) {
  const g = await open(page, url);
  await page.mouse.move(...g.pt(1, 3, 1));
  await page.mouse.wheel(0, 120);
  await page.waitForFunction(() => window.__MOCK__.callsOf("wheel").length >= 1, undefined, { timeout: 5000 });
  const down = (await wheels(page)).at(-1);
  assert.equal(down.pane, 1);
  assert.ok(down.rows > 0, "a positive delta is towards newer output");
  assert.deepEqual(await callsOf(page, "scroll"), []);
}

/** A press, a release and a hover while the program tracks the mouse are reported and never selected. */
async function reportCase(page, url) {
  const g = await openTracking(page, url);
  const [x, y] = g.pt(2, 3, 2);
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.up();
  await frames(page);

  const [gx, gy] = g.grid(3, 2);
  assert.deepEqual(await buttonCalls(page), [
    { pane: 2, action: "press", button: "left", x: gx, y: gy, mods: 0 },
    { pane: 2, action: "release", button: "left", x: gx, y: gy, mods: 0 },
  ], "press and release go to the program");
  assert.deepEqual(await selects(page), [], "a reported gesture is not a selection");
  assert.deepEqual(await callsOf(page, "focus_pane"), [], "the focused pane needs no focus_pane");

  // The payload keys and the strings are the golden's.
  const sent = (await mouseCalls(page))[0];
  assert.deepEqual(Object.keys(sent).sort(), [...golden.commands.mouse].sort(), "the mouse payload has the golden's keys");
  for (const a of await mouseCalls(page)) {
    assert.ok(golden.mouse.actions.includes(a.action), "the action is a golden string");
    assert.ok(golden.mouse.buttons.includes(a.button), "the button is a golden string");
    assert.ok(Number.isInteger(a.x) && a.x >= 0 && a.x < COLS * g.cw, "x is an integer inside the grid");
    assert.ok(Number.isInteger(a.y) && a.y >= 0 && a.y < ROWS * g.ch, "y is an integer inside the grid");
  }

  // Modifiers other than Shift travel as bits.
  await fresh(page);
  await pointer(page, "pointerdown", [x, y], { button: 0, buttons: 1, ctrlKey: true, altKey: true });
  await pointer(page, "pointerup", [x, y], { button: 0, buttons: 0, ctrlKey: true, altKey: true });
  assert.deepEqual((await buttonCalls(page)).map((a) => [a.action, a.mods]), [["press", 6], ["release", 6]], "ctrl and alt are bits");
}

/** Left, middle and right are reported as such; a release carries the button of its press. */
async function buttonsCase(page, url) {
  const g = await openTracking(page, url);
  const at = g.pt(2, 6, 1);
  for (const [button, name] of [[0, "left"], [1, "middle"], [2, "right"]]) {
    await reset(page);
    await pointer(page, "pointerdown", at, { button, buttons: 1 << button });
    await pointer(page, "pointerup", at, { button, buttons: 0 });
    assert.deepEqual((await buttonCalls(page)).map((a) => [a.action, a.button]), [["press", name], ["release", name]], `the ${name} button`);
    assert.deepEqual(await selects(page), [], "no selection for any button");
  }
}

/** Motion is one command per animation frame; the last position wins; hover is button none, a drag the held button. */
async function motionCase(page, url) {
  const g = await openTracking(page, url);

  // Hover: 30 moves inside one task are one command with the last position.
  const hover = Array.from({ length: 30 }, (_, i) => g.pt(2, 1 + (i % 6), i % 3));
  hover.push(g.pt(2, 9, 2));
  await page.evaluate(
    ([points, pid]) => {
      const c = document.getElementById("term");
      for (const [x, y] of points) {
        c.dispatchEvent(new PointerEvent("pointermove", { clientX: x, clientY: y, pointerId: pid, buttons: 0, bubbles: true }));
      }
    },
    [hover, 7],
  );
  await frames(page);
  assert.deepEqual(await motions(page), [{ pane: 2, action: "motion", button: "none", x: g.grid(9, 2)[0], y: g.grid(9, 2)[1], mods: 0 }],
    "31 hover moves in one task send one motion");

  // A drag: 20 moves with a frame between each give at most 20 and end at the final position.
  await reset(page);
  const down = g.pt(2, 1, 1);
  await pointer(page, "pointerdown", down, { button: 0, buttons: 1 });
  for (let i = 0; i < 20; i++) {
    await pointer(page, "pointermove", g.pt(2, 2 + (i % 10), i % 4), { buttons: 1 });
    await frames(page, 1);
  }
  const drag = await motions(page);
  assert.ok(drag.length >= 1 && drag.length <= 20, "20 spaced moves gave " + drag.length + " motions");
  assert.ok(drag.every((a) => a.button === "left"), "a drag moves with the held button");
  assert.deepEqual(drag.at(-1), { pane: 2, action: "motion", button: "left", x: g.grid(11, 3)[0], y: g.grid(11, 3)[1], mods: 0 },
    "the last motion is the final move");
  await pointer(page, "pointerup", g.pt(2, 11, 3), { button: 0, buttons: 0 });

  // Five moves in one task during a drag: one motion, at the last position.
  await reset(page);
  await pointer(page, "pointerdown", down, { button: 0, buttons: 1 });
  await page.evaluate(
    ([points]) => {
      const c = document.getElementById("term");
      for (const [x, y] of points) {
        c.dispatchEvent(new PointerEvent("pointermove", { clientX: x, clientY: y, pointerId: 7, buttons: 1, bubbles: true }));
      }
    },
    [[g.pt(2, 2, 1), g.pt(2, 3, 1), g.pt(2, 4, 2), g.pt(2, 5, 2), g.pt(2, 6, 3)]],
  );
  await frames(page);
  assert.deepEqual((await motions(page)).map((a) => [a.x, a.y]), [g.grid(6, 3)], "five moves in one task send one motion");

  // The release carries the release position and no motion is sent after it.
  await pointer(page, "pointerup", g.pt(2, 7, 3), { button: 0, buttons: 0 });
  const total = (await mouseCalls(page)).length;
  assert.deepEqual((await buttonCalls(page)).map((a) => [a.action, a.x, a.y]), [["press", ...g.grid(1, 1)], ["release", ...g.grid(7, 3)]]);
  await frames(page);
  await page.waitForTimeout(40);
  assert.equal((await mouseCalls(page)).length, total, "nothing is sent after the release");

  // Far outside the grid the position is clamped, not extrapolated.
  await reset(page);
  await pointer(page, "pointerdown", down, { button: 0, buttons: 1 });
  await pointer(page, "pointermove", [g.pt(2, 0, 0)[0] + 4000, g.pt(2, 0, 0)[1] + 4000], { buttons: 1 });
  await frames(page);
  const far = (await motions(page)).at(-1);
  assert.deepEqual([far.x, far.y], [COLS * g.cw - 1, ROWS * g.ch - 1], "clamped to the last device pixel of the grid");
  await pointer(page, "pointerup", down, { button: 0, buttons: 0 });
}

/** Shift, or a program that does not track the mouse, leaves the gesture to the selection exactly as before. */
async function selectionCase(page, url) {
  // Shift held: the program tracks the mouse but the user selects.
  let g = await openTracking(page, url);
  const at = g.pt(2, 4, 1);
  await pointer(page, "pointerdown", at, { button: 0, buttons: 1, shiftKey: true });
  await pointer(page, "pointerup", at, { button: 0, buttons: 0, shiftKey: true });
  await frames(page);
  assert.deepEqual((await selects(page)).map((o) => o.kind), ["begin", "clear"], "Shift selects");
  assert.deepEqual(await buttonCalls(page), [], "Shift sends no press or release");
  assert.deepEqual(await motions(page), [], "Shift sends no motion either");

  // The program does not track the mouse: selection, and no report of any kind.
  g = await openTracking(page, url, false);
  const [x, y] = g.pt(2, 4, 1);
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(...g.pt(2, 8, 2), { steps: 4 });
  await page.mouse.up();
  await frames(page);
  const ops = await selects(page);
  assert.equal(ops[0].kind, "begin", "a press begins a selection");
  assert.ok(ops.some((o) => o.kind === "extend"), "a drag extends it");
  assert.deepEqual(await mouseCalls(page), [], "nothing is reported to a program that did not ask");
}

/** A cancelled, lost or blurred pointer sends exactly one release and nothing afterwards. */
async function cancelCase(page, url) {
  const g = await openTracking(page, url);
  const at = g.pt(2, 5, 2);
  const events = {
    pointercancel: (p) => pointer(p, "pointercancel", at, { button: 0, buttons: 0 }),
    lostpointercapture: (p) => pointer(p, "lostpointercapture", at, { button: 0, buttons: 0 }),
    blur: (p) => p.evaluate(() => window.dispatchEvent(new Event("blur"))),
  };
  for (const [name, fire] of Object.entries(events)) {
    await reset(page);
    await pointer(page, "pointerdown", at, { button: 0, buttons: 1 });
    await fire(page);
    await fire(page); // a second one finds nothing held
    await pointer(page, "pointerup", at, { button: 0, buttons: 0 });
    await frames(page);
    assert.deepEqual(
      (await buttonCalls(page)).map((a) => [a.action, a.button]),
      [["press", "left"], ["release", "left"]],
      `${name}: one release for the held button and none afterwards`,
    );
    // The pane is usable again: the next press is reported.
    await pointer(page, "pointerdown", at, { button: 0, buttons: 1 });
    await pointer(page, "pointerup", at, { button: 0, buttons: 0 });
    assert.deepEqual((await buttonCalls(page)).map((a) => a.action), ["press", "release", "press", "release"], `${name}: a new gesture works`);
  }

  // Blur with no button down sends nothing.
  await reset(page);
  await page.evaluate(() => window.dispatchEvent(new Event("blur")));
  assert.deepEqual(await mouseCalls(page), [], "blur with nothing held sends nothing");
}

/** The first click on an unfocused pane only focuses it; the next one is reported. */
async function unfocusedCase(page, url) {
  const g = await openTracking(page, url);
  await page.evaluate(() => {
    window.__MOCK__.on("focus_pane", (a) => {
      const v = window.__MOCK__.view();
      v.focus = a.pane;
      return v;
    });
  });
  const at = g.pt(1, 2, 0);
  await pointer(page, "pointerdown", at, { button: 0, buttons: 1 });
  await pointer(page, "pointermove", g.pt(1, 4, 1), { buttons: 1 });
  await pointer(page, "pointerup", g.pt(1, 4, 1), { button: 0, buttons: 0 });
  await frames(page);
  assert.deepEqual(await callsOf(page, "focus_pane"), [{ pane: 1 }], "the click focuses the pane");
  assert.deepEqual(await mouseCalls(page), [], "the focusing click is not reported, nor are its moves");
  assert.deepEqual(await selects(page), [], "and it selects nothing");

  await pointer(page, "pointerdown", at, { button: 0, buttons: 1 });
  await pointer(page, "pointerup", at, { button: 0, buttons: 0 });
  assert.deepEqual((await buttonCalls(page)).map((a) => [a.pane, a.action]), [[1, "press"], [1, "release"]], "the next press is reported");

  // With Shift the unfocused pane is selected into as before: focus, then begin.
  const h = await openTracking(page, url);
  await pointer(page, "pointerdown", h.pt(1, 2, 0), { button: 0, buttons: 1, shiftKey: true });
  await pointer(page, "pointerup", h.pt(1, 2, 0), { button: 0, buttons: 0, shiftKey: true });
  await frames(page);
  assert.deepEqual(await callsOf(page, "focus_pane"), [{ pane: 1 }], "Shift on an unfocused pane focuses it");
  assert.deepEqual((await selects(page)).map((o) => [o.pane, o.kind]), [[1, "begin"], [1, "clear"]], "and selects as before");
  assert.deepEqual(await mouseCalls(page), [], "without a report");
}

/** The browser's context menu stays away while the program tracks the mouse and is left alone otherwise. */
async function contextMenuCase(page, url) {
  const menu = (p, at) =>
    p.evaluate(([x, y]) => {
      const e = new MouseEvent("contextmenu", { clientX: x, clientY: y, button: 2, bubbles: true, cancelable: true });
      document.getElementById("term").dispatchEvent(e);
      return e.defaultPrevented;
    }, at);

  let g = await openTracking(page, url, true);
  assert.equal(await menu(page, g.pt(2, 3, 1)), true, "the context menu is prevented over a tracking pane");
  g = await openTracking(page, url, false);
  assert.equal(await menu(page, g.pt(2, 3, 1)), false, "and left alone over a pane that does not track");
}

/** A pushed handler that consumes the event prevents both the report and the selection. */
async function handlerCase(page, url) {
  for (const mouse of [true, false]) {
    const g = await openTracking(page, url, mouse);
    const at = g.pt(2, 4, 1);
    await page.evaluate(async () => {
      const { pushPointerHandler } = await import(new URL("pointer.js", location.href).href);
      window.__eat = true;
      window.__pop = pushPointerHandler(() => window.__eat);
    });
    await page.mouse.move(...at);
    await page.mouse.down();
    await page.mouse.up();
    await frames(page);
    assert.deepEqual(await buttonCalls(page), [], `a consumed click is not reported (tracking ${mouse})`);
    assert.deepEqual(await selects(page), [], `a consumed click selects nothing (tracking ${mouse})`);

    // A release that a handler takes still reaches the program: no button stays stuck.
    await page.evaluate(() => { window.__eat = false; });
    await page.mouse.down();
    await page.evaluate(() => { window.__eat = true; });
    await page.mouse.up();
    await frames(page);
    if (mouse) {
      assert.deepEqual((await buttonCalls(page)).map((a) => a.action), ["press", "release"], "a consumed release is still sent");
    }
    await page.evaluate(() => window.__pop());
  }
}

/** The decision of a gesture is fixed at its press: a mode change in between never splits a gesture. */
async function gestureModeCase(page, url) {
  const g = await openTracking(page, url, true);
  const down = g.pt(2, 2, 1);
  await pointer(page, "pointerdown", down, { button: 0, buttons: 1 });
  await setModes(page, 2, { mouse: false });
  await pointer(page, "pointermove", g.pt(2, 5, 2), { buttons: 1 });
  await pointer(page, "pointerup", g.pt(2, 5, 2), { button: 0, buttons: 0 });
  await frames(page);
  assert.deepEqual((await buttonCalls(page)).map((a) => a.action), ["press", "release"], "a reported press is released as reported");
  assert.deepEqual(await selects(page), [], "and never selects");

  await fresh(page);
  await pointer(page, "pointerdown", down, { button: 0, buttons: 1 });
  await setModes(page, 2, { mouse: true });
  await pointer(page, "pointermove", g.pt(2, 5, 2), { buttons: 1 });
  await pointer(page, "pointerup", g.pt(2, 5, 2), { button: 0, buttons: 0 });
  await frames(page);
  assert.deepEqual(await mouseCalls(page), [], "a selection that began is not reported when the program starts tracking");
  assert.deepEqual((await selects(page)).map((o) => o.kind), ["begin", "extend"], "it stays a selection");
}
