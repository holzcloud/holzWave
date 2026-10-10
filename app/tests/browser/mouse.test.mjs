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

export default async function ({ page, url, errors }) {
  await wheelCase(page, url);
  await wheelRemainderCase(page, url);
  await wheelModesCase(page, url);
  await wheelRealCase(page, url);
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
