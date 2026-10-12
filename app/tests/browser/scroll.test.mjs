// Smooth scrolling and the cursor trail in the real page (DESK-28): the two settings reach the page and follow reduced
// motion live, a scroll slides from where the old rows were and settles on the plain frame, a jumping cursor leaves at
// most six ghosts, and no animation frame loop runs while nothing animates. The page's clock is frozen
// (`performance.now` reads `window.__t`) so a slide can be looked at at an exact moment. The browser stub plays the core.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, emit } from "./lib.mjs";

export default async function ({ page, url, errors }) {
  page.setDefaultTimeout(5000);
  await preferenceCase(page, url);
  await slideCase(page, url);
  await offCase(page, url);
  await trailCase(page, url);
  await idleCase(page, url, "?renderer=canvas");
  await idleCase(page, url, "?renderer=webgl");
  assert.deepEqual(errors, [], "the page reported errors");
}

const golden = JSON.parse(readFileSync(join(APP_DIR, "tests/fixtures/protocol.json"), "utf8"));
const view = (settings = {}) => {
  const v = structuredClone(golden.settings_view);
  Object.assign(v.settings, settings);
  return v;
};

const BG = 0x14091a, FG = 0xf6efe6, CURSOR = 0xff8fb8;
const COLS = 60, ROWS = 4;

/** Row colours: the old picture, and the one after a scroll up by one row (a new row at the top, the rest moved down). */
const OLD = [0x112233, 0x223344, 0x334455, 0x445566];
const NEW = [0x556677, 0x112233, 0x223344, 0x334455];

/** A frame of `pane`: row y is one run of `colours[y]`; `seq` 1 is full, later ones carry all rows. */
const frame = (pane, seq, colours, scrollbar, cursor = null) => ({
  pane, seq, full: seq === 1, cols: COLS, rows: ROWS, fg: FG, bg: BG, cursor,
  scrollbar, selection: [], matches: [], search: null, copy_cursor: null,
  modes: { alt: false, bracketed_paste: false, mouse: false },
  changed: colours.map((c, y) => ({ y, runs: [[" ".repeat(COLS), COLS, FG, c, 0]] })),
});

/** The page against the stub with the clock frozen, a counter for animation frames and the settings view given. */
async function open(page, url, settings, query = "?renderer=canvas") {
  await page.addInitScript(() => {
    window.__t = 1000;
    performance.now = () => window.__t;
    window.__raf = 0;
    if (!window.__rafCounted) {
      window.__rafCounted = true; // the init script of an earlier `open` must not be wrapped a second time
      const raf = window.requestAnimationFrame.bind(window);
      window.requestAnimationFrame = (fn) => {
        window.__raf++;
        return raf(fn);
      };
    }
  });
  const v = view(settings);
  await page.addInitScript((value) => {
    window.__MOCK__.on("get_settings", () => structuredClone(value));
    window.__MOCK__.on("reload_settings", () => structuredClone(value));
  }, v);
  await page.goto(`${url}index.html${query}`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.evaluate(async () => {
    window.__state = (await import(new URL("state.js", location.href).href)).state;
    window.__a11y = await import(new URL("a11y.js", location.href).href);
  });
}

const send = (page, f) => page.evaluate((x) => window.__MOCK__.emit("frame", x), f);
const prefs = (page) => page.evaluate(() => {
  const p = window.__a11y.preferences();
  return { scroll: p.smoothScroll, trail: p.cursorTrail, reduced: p.motionReduced };
});
const rafs = (page) => page.evaluate(() => window.__raf);
/** Move the page's clock and let a few animation frames run. */
const tick = async (page, ms) => {
  await page.evaluate((d) => (window.__t += d), ms);
  await page.waitForTimeout(60);
};

/** The colour at the middle of every cell of the focused pane's grid as the canvas shows it now: `[y][x]`. */
const cells = (page) =>
  page.evaluate(async () => {
    const { state } = await import(new URL("state.js", location.href).href);
    const { gridRect } = await import(new URL("geometry.js", location.href).href);
    const entry = state.view.panes.find(([id]) => id === state.view.focus);
    const g = gridRect(state.metrics, entry[1], state.headerPx);
    const m = state.metrics;
    const ctx = document.getElementById("term").getContext("2d");
    const out = [];
    for (let cy = 0; cy < 4; cy++) {
      const row = [];
      for (let cx = 0; cx < 60; cx++) {
        const px = Math.floor((g.x + (cx + 0.5) * m.cellW) * m.dpr);
        const py = Math.floor((g.y + (cy + 0.5) * m.cellH) * m.dpr);
        const [r, gr, b] = ctx.getImageData(px, py, 1, 1).data;
        row.push((r << 16) | (gr << 8) | b);
      }
      out.push(row);
    }
    return out;
  });
/** One column of the grid, top to bottom. */
const column = async (page, x = 5) => (await cells(page)).map((row) => row[x]);

/** The old picture on the focused pane (2), drawn and settled. */
async function showOld(page, cursor = null) {
  await send(page, frame(2, 1, OLD, [10, 100, ROWS], cursor));
  await page.waitForTimeout(60);
  assert.deepEqual(await column(page), OLD, "the first frame is drawn as it is");
}

// ---- the settings and reduced motion --------------------------------------------------------------------

/** The two settings reach the page; reduced motion (OS or the Motion setting) switches them off, live. */
async function preferenceCase(page, url) {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await open(page, url, {});
  assert.deepEqual(await prefs(page), { scroll: false, trail: false, reduced: false }, "off by default");

  await emit(page, "settings", view({ smooth_scroll: true }));
  assert.deepEqual(await prefs(page), { scroll: true, trail: false, reduced: false });
  await emit(page, "settings", view({ smooth_scroll: true, cursor_trail: true }));
  assert.deepEqual(await prefs(page), { scroll: true, trail: true, reduced: false });

  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.waitForFunction(() => window.__a11y.preferences().motionReduced);
  assert.deepEqual(await prefs(page), { scroll: false, trail: false, reduced: true }, "the OS asks to reduce: both off, live");
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.waitForFunction(() => window.__a11y.preferences().smoothScroll);
  assert.deepEqual(await prefs(page), { scroll: true, trail: true, reduced: false }, "and back on without a reload");

  await emit(page, "settings", view({ smooth_scroll: true, cursor_trail: true, motion: "reduce" }));
  assert.deepEqual(await prefs(page), { scroll: false, trail: false, reduced: true }, "the app's own motion setting");
  await page.emulateMedia({ reducedMotion: "reduce" });
  await emit(page, "settings", view({ smooth_scroll: true, cursor_trail: true, motion: "full" }));
  assert.deepEqual(await prefs(page), { scroll: true, trail: true, reduced: false }, "full motion beats the OS");
  await emit(page, "settings", view({ smooth_scroll: false, cursor_trail: true, motion: "full" }));
  assert.deepEqual(await prefs(page), { scroll: false, trail: true, reduced: false }, "each effect has its own setting");
}

// ---- smooth scrolling ----------------------------------------------------------------------------------

/** A scroll slides from where the old rows were, draws at once, and settles on the plain frame. */
async function slideCase(page, url) {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await open(page, url, { smooth_scroll: true });
  await showOld(page);

  // One row up in the history. The frame is drawn the moment it arrives, shifted so that the picture is the old one.
  await send(page, frame(2, 2, NEW, [9, 100, ROWS]));
  await page.waitForTimeout(60);
  assert.deepEqual(await column(page), OLD, "at the start of the slide the picture is still the old one, with no gap");

  await tick(page, 20);
  const mid = await column(page);
  assert.notDeepEqual(mid, NEW, "mid-slide the rows are not yet in place");

  await tick(page, 130); // 150 ms: past the 120 ms the slide may take
  assert.deepEqual(await column(page), NEW, "the slide ends on the plain frame");
  const settled = await rafs(page);
  await page.waitForTimeout(400);
  assert.equal(await rafs(page), settled, "nothing animates any more: no animation frame is asked for");

  // A frame that is not a scroll is never slid: the output is drawn where it is.
  await send(page, { ...frame(2, 3, OLD, [9, 101, ROWS]), full: false });
  await page.waitForTimeout(60);
  assert.deepEqual(await column(page), OLD, "output is drawn at once and in place");

  // Turning the setting off in the middle of a slide drops it at once.
  await send(page, frame(2, 4, NEW, [8, 101, ROWS]));
  await page.waitForTimeout(60);
  await emit(page, "settings", view({ smooth_scroll: false }));
  await page.waitForTimeout(60);
  assert.deepEqual(await column(page), NEW, "the plain frame replaces the slide when the setting goes off");
}

/** With the setting off, or under reduced motion, the frame is drawn in place at once and nothing animates. */
async function offCase(page, url) {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await open(page, url, { smooth_scroll: false });
  await showOld(page);
  let before = await rafs(page);
  await send(page, frame(2, 2, NEW, [9, 100, ROWS]));
  await page.waitForTimeout(60);
  assert.deepEqual(await column(page), NEW, "off: the new rows are in place at once");
  assert.ok((await rafs(page)) - before <= 3, "off: one draw, no loop");

  await page.emulateMedia({ reducedMotion: "reduce" });
  await open(page, url, { smooth_scroll: true });
  await showOld(page);
  before = await rafs(page);
  await send(page, frame(2, 2, NEW, [9, 100, ROWS]));
  await page.waitForTimeout(60);
  assert.deepEqual(await column(page), NEW, "reduced motion: the setting is on and nothing slides");
  assert.ok((await rafs(page)) - before <= 3, "reduced motion: no loop");
}

// ---- the cursor trail ----------------------------------------------------------------------------------

/** The cells of the focused pane (every column of every row) that are neither the background nor `except`. */
async function inked(page, except) {
  const out = [];
  (await cells(page)).forEach((row, y) =>
    row.forEach((c, x) => {
      if (c !== BG && c !== except) out.push([x, y, c]);
    }),
  );
  return out;
}

/** A cursor that jumps leaves at most six fading ghosts; none for a step, a hidden cursor or reduced motion. */
async function trailCase(page, url) {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await open(page, url, { cursor_trail: true });
  const still = { x: 2, y: 1, style: "block", blink: false };
  const grid = [BG, BG, BG, BG];
  await send(page, frame(2, 1, grid, [0, ROWS, ROWS], still));
  await page.waitForTimeout(60);
  assert.deepEqual(await inked(page, CURSOR), [], "the cursor alone: no ghost");

  // A step of one cell is typing, not a jump.
  await send(page, frame(2, 2, grid, [0, ROWS, ROWS], { ...still, x: 3 }));
  await page.waitForTimeout(60);
  assert.deepEqual(await inked(page, CURSOR), [], "one cell: no trail");

  // A jump across the line: ghosts, at most six, on the way and not on the target.
  await send(page, frame(2, 3, grid, [0, ROWS, ROWS], { ...still, x: 40, y: 2 }));
  await page.waitForTimeout(60);
  const ghosts = await inked(page, CURSOR);
  assert.ok(ghosts.length >= 1 && ghosts.length <= 6, `${ghosts.length} ghost cells`);
  assert.ok(ghosts.some(([x, y]) => x === 3 && y === 1), "the first ghost is the cursor's old cell");
  assert.ok(ghosts.every(([x]) => x < 40), "no ghost on or past the target");
  assert.equal((await cells(page))[2][40], CURSOR, "the cursor itself is solid at its new place");

  // How far the ghosts are from the background, summed over the channels: it only falls while they fade.
  const strength = (list) => list.reduce((n, [, , c]) => n + ((c >> 16) & 255) + ((c >> 8) & 255) + (c & 255) - 0x14 - 0x09 - 0x1a, 0);
  await tick(page, 60);
  assert.ok(strength(await inked(page, CURSOR)) < strength(ghosts), "the ghosts fade");
  await tick(page, 200);
  assert.deepEqual(await inked(page, CURSOR), [], "and are gone after the trail's time");
  const settled = await rafs(page);
  await page.waitForTimeout(400);
  assert.equal(await rafs(page), settled, "no animation frame is asked for once the trail is gone");

  // A program that hides the cursor right after a jump: no trail to a cursor that is not there.
  await send(page, frame(2, 4, grid, [0, ROWS, ROWS], { ...still, x: 5, y: 0 }));
  await send(page, frame(2, 5, grid, [0, ROWS, ROWS], null));
  await page.waitForTimeout(60);
  assert.deepEqual(await inked(page, CURSOR), [], "a hidden cursor leaves nothing");

  // Reduced motion: the setting is on and the jump leaves no ghost.
  await page.emulateMedia({ reducedMotion: "reduce" });
  await open(page, url, { cursor_trail: true });
  await send(page, frame(2, 1, grid, [0, ROWS, ROWS], still));
  await send(page, frame(2, 2, grid, [0, ROWS, ROWS], { ...still, x: 40, y: 2 }));
  await page.waitForTimeout(60);
  assert.deepEqual(await inked(page, CURSOR), [], "reduced motion: no trail");
}

// ---- idle --------------------------------------------------------------------------------------------

/** Both renderers: with both effects on, an idle page asks for no animation frame, and a scroll's loop ends by itself. */
async function idleCase(page, url, query) {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await open(page, url, { smooth_scroll: true, cursor_trail: true }, query);
  const still = { x: 2, y: 1, style: "block", blink: false };
  await send(page, frame(2, 1, OLD, [10, 100, ROWS], still));
  await page.waitForTimeout(100);
  const idle = await rafs(page);
  await page.waitForTimeout(500);
  assert.equal(await rafs(page), idle, `${query}: an idle page runs no animation frame loop`);

  await send(page, frame(2, 2, NEW, [9, 100, ROWS], { ...still, x: 30, y: 3 }));
  await page.waitForTimeout(100);
  assert.ok((await rafs(page)) > idle + 1, `${query}: the scroll and the jump animate`);
  await tick(page, 300);
  await page.waitForTimeout(100);
  const done = await rafs(page);
  await page.waitForTimeout(500);
  assert.equal(await rafs(page), done, `${query}: the loop stopped by itself when the effects ended`);
}
