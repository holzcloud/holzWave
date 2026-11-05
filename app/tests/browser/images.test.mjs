// Pictures in a pane with the canvas renderer (POL-01): a frame with placements makes the page pull the pixels with
// `image_data`, check them and draw them in the layer the program chose, clipped to the grid. The browser stub plays
// the core: it answers `image_data` with the 16-byte header and straight RGBA, as the real core does (raw bytes on
// the tauri backend, a binary message on the socket backend). The colours are read back from the canvas.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { APP_DIR, callsOf, currentBackend, emit } from "./lib.mjs";

const golden = JSON.parse(readFileSync(join(APP_DIR, "tests/fixtures/protocol.json"), "utf8"));
/** The settings view of the core, with `over` laid over its settings. */
const settingsView = (over = {}) => {
  const view = structuredClone(golden.settings_view);
  Object.assign(view.settings, over);
  return view;
};

const PANE_1_BG = 0x6b2fa0;
const PANE_2_BG = 0x2a6fdb;
const EXPLICIT = 0xaa3300; // a cell background the program set
const WHITE = 0xffffff;
const RED = 0xff0000;
const GREEN = 0x00ff00;
const BLUE = 0x0000ff;
const YELLOW = 0xffff00;
const PAD = 6; // CSS px between a pane's edge and its grid (geometry.ts)
const HEADER = 20; // CSS px of a pane's header strip at the base font size

const MODES = { alt: false, bracketed_paste: false, mouse: false };

/** A frame message as the core sends it (pane 1 is 20 by 4 cells). */
const frame = (seq, over = {}) => ({
  pane: 1, seq, full: false, cols: 20, rows: 4, fg: WHITE, bg: PANE_1_BG, cursor: null,
  scrollbar: [0, 4, 4], selection: [], matches: [], search: null, copy_cursor: null, modes: MODES, changed: [],
  ...over,
});
const fullFrame = (seq, over = {}) => frame(seq, { full: true, ...over });

/**
 * One placement as the core sends it. Sizes are in cells; `cw` and `ch` turn them into the pixels of the cell size the
 * core was told. `src` is the part of the picture shown, in the picture's own pixels.
 */
const placement = (c, { image, generation = 1, layer = 2, z = 0, col, row, cols, rows, src, dx = 0, dy = 0 }) => [
  image, generation, layer, z, col, row, cols, rows, dx, dy, cols * c.cw, rows * c.ch, src[0], src[1], src[2], src[3],
];

/** A picture of `w` by `h` pixels from a list of [r, g, b, a] pixels, row by row. */
const pic = (w, h, pixels) => ({ w, h, px: pixels.flat() });
const solid = (w, h, rgb) => pic(w, h, Array.from({ length: w * h }, () => [(rgb >> 16) & 255, (rgb >> 8) & 255, rgb & 255, 255]));
const rgb4 = (n) => [(n >> 16) & 255, (n >> 8) & 255, n & 255, 255];
/** A picture as wide as `colours.length` cells and one cell tall, one colour per cell, so it maps pixel for pixel. */
const strip = (c, colours) =>
  pic(colours.length * c.cw, c.ch, Array.from({ length: colours.length * c.cw * c.ch }, (_, i) => rgb4(colours[Math.floor((i % (colours.length * c.cw)) / c.cw)])));

const hex = (n) => `#${n.toString(16).padStart(6, "0")}`;

/** The renderer the page is opened with: `canvas` (the reference) or `webgl` (the colour cases are run with both). */
let renderer = "canvas";

async function open(page, url) {
  await page.goto(`${url}index.html?renderer=${renderer}`);
  await page.waitForSelector(".divider");
  const name = await page.evaluate(async () => (await import(new URL("state.js", location.href).href)).state.renderer);
  assert.equal(name, renderer === "webgl" ? "webgl2" : "canvas2d", "the page uses the renderer that was asked for");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  await page.waitForFunction(() => window.__MOCK__.callsOf("request_full").length >= 2);
  const start = (await callsOf(page, "start_pane"))[0];
  return { cw: start.cell_w, ch: start.cell_h }; // dpr 1: device px are CSS px
}

/**
 * Make the stub answer `image_data` for the pictures of `table` (keys `pane:image:generation`). A picture may carry
 * `total` or `offset` to put a lie into the header.
 */
function script(page, table) {
  return page.evaluate((t) => {
    window.__PICS__ = { ...(window.__PICS__ ?? {}), ...t };
    window.__MOCK__.on("image_data", (a) => {
      const p = window.__PICS__[`${a.pane}:${a.image}:${a.generation}`];
      if (!p) throw "no such image";
      const total = p.w * p.h * 4;
      const rgba = new Uint8Array(p.px);
      const from = Math.min(a.offset, total);
      const body = rgba.subarray(from, Math.min(from + 512 * 1024, total));
      const out = new Uint8Array(16 + body.length);
      const dv = new DataView(out.buffer);
      dv.setUint32(0, p.w);
      dv.setUint32(4, p.h);
      dv.setUint32(8, p.total ?? total);
      dv.setUint32(12, p.offset ?? from);
      out.set(body, 16);
      return out.buffer;
    });
  }, table);
}

/**
 * The colours (0xRRGGBB) of the canvas at `points` (CSS pixels), after the next draw. A WebGL canvas is cleared once the
 * browser has shown a frame, so the page is asked to draw again first: its draw is queued before this read in the same
 * animation frame, and the read sees the pixels it just drew.
 */
const read = (page, points) =>
  page.evaluate(
    async (pts) => {
      const { schedule } = await import(new URL("scene.js", location.href).href);
      schedule();
      await new Promise((done) => requestAnimationFrame(() => done()));
      const c = document.getElementById("term");
      const k = c.width / c.clientWidth;
      const gl = c.getContext("webgl2");
      if (gl) {
        const px = new Uint8Array(4);
        return pts.map(([x, y]) => {
          gl.readPixels(Math.floor(x * k), c.height - 1 - Math.floor(y * k), 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
          return (px[0] << 16) | (px[1] << 8) | px[2];
        });
      }
      const ctx = c.getContext("2d");
      return pts.map(([x, y]) => {
        const [r, g, b] = ctx.getImageData(Math.floor(x * k), Math.floor(y * k), 1, 1).data;
        return (r << 16) | (g << 8) | b;
      });
    },
    points,
  );

/** Deliver `events`, then read `points` until `ok(colours)` holds or two seconds pass; the colours of the last read. */
async function settle(page, events, points, ok) {
  await page.evaluate((evs) => {
    for (const [name, payload] of evs) window.__MOCK__.emit(name, payload);
  }, events);
  const deadline = Date.now() + 2000;
  let got = await read(page, points);
  while (!ok(got) && Date.now() < deadline) {
    await page.waitForTimeout(25);
    got = await read(page, points);
  }
  return got;
}

const colours = (got, want, label) =>
  assert.deepEqual(got.map(hex), want.map(hex), label);

/** How many `image_data` calls named this picture. */
const asked = async (page, image) => (await callsOf(page, "image_data")).filter((a) => a.image === image);

/** The page's picture store, read from the live module. */
const held = (page, pane) =>
  page.evaluate(async (id) => {
    const { images } = await import(new URL("images.js", location.href).href);
    return images.stats(id);
  }, pane);

export default async function ({ page, url, errors }) {
  // The cases that read colours back run with the WebGL renderer too: the pictures must look the same in both.
  for (renderer of ["webgl", "canvas"]) {
    await tracerCase(page, url);
    await layersCase(page, url);
    await clipCase(page, url);
    await hostileCase(page, url);
    await alphaCase(page, url);
  }
  await textOnlyCase(page, url);
  await memoryCase(page, url);
  await motionCase(page, url);
  await refusedCase(page, url);
  if (currentBackend() === "ws") await reconnectCase(page, url);
  if (currentBackend() === "ws") await failedPullReconnectCase(page, url);
  await failedPullClosedBitmapCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A frame with one placement: the page asks once, the picture appears at its cells, a new generation replaces it. */
async function tracerCase(page, url) {
  const c = await open(page, url);
  const cell = (col, row) => [PAD + (col + 0.5) * c.cw, HEADER + (row + 0.5) * c.ch];
  await script(page, {
    "1:7:1": strip(c, [RED, GREEN]),
    "1:7:2": strip(c, [BLUE, BLUE]),
  });
  const place = (generation) => placement(c, { image: 7, generation, col: 2, row: 1, cols: 2, rows: 1, src: [0, 0, 2 * c.cw, c.ch] });
  const points = [cell(2, 1), cell(3, 1), cell(1, 1), cell(4, 1)];

  let got = await settle(page, [["frame", fullFrame(1, { images: [place(1)] })]], points, (g) => g[0] === RED && g[1] === GREEN);
  colours(got, [RED, GREEN, PANE_1_BG, PANE_1_BG], "the picture is at its two cells and nowhere else");
  assert.deepEqual(await asked(page, 7), [{ pane: 1, image: 7, generation: 1, offset: 0 }], "one request, for the placement's generation");

  // A frame without the field keeps the picture and asks for nothing.
  got = await settle(page, [["frame", frame(2)]], points, (g) => g[0] === RED);
  colours(got, [RED, GREEN, PANE_1_BG, PANE_1_BG], "a frame without images keeps the picture");
  assert.equal((await asked(page, 7)).length, 1, "the same generation is not asked for twice");

  // A new generation is a new picture: asked for once, shown when it arrived.
  got = await settle(page, [["frame", frame(3, { images: [place(2)] })]], points, (g) => g[0] === BLUE && g[1] === BLUE);
  colours(got, [BLUE, BLUE, PANE_1_BG, PANE_1_BG], "generation 2 replaces generation 1");
  assert.deepEqual(
    (await asked(page, 7)).map((a) => [a.generation, a.offset]),
    [[1, 0], [2, 0]],
    "one request for each generation",
  );
  assert.equal((await held(page, 1)).held, 1, "the replaced bitmap is closed and no longer held");

  // An empty list removes the picture from the screen.
  got = await settle(page, [["frame", frame(4, { images: [] })]], points, (g) => g[0] === PANE_1_BG);
  colours(got, [PANE_1_BG, PANE_1_BG, PANE_1_BG, PANE_1_BG], "images: [] removes the picture");

  // A full frame sets the list again (the cached generation 2 is shown without a new request).
  got = await settle(page, [["frame", fullFrame(5, { images: [place(2)] })]], points, (g) => g[0] === BLUE);
  colours(got, [BLUE, BLUE, PANE_1_BG, PANE_1_BG], "a full frame sets the list");
  assert.equal((await asked(page, 7)).length, 2, "a picture the page still holds is not asked for again");

  // A pane that exits drops every bitmap it held.
  await emit(page, "pane-exit", { pane: 1, code: 0 });
  assert.deepEqual(await held(page, 1), { held: 0, bytes: 0, jobs: 0 }, "an exited pane holds nothing");
}

/** The three layers and the order the spec gives, by readback. */
async function layersCase(page, url) {
  const c = await open(page, url);
  const cell = (col, row) => [PAD + (col + 0.5) * c.cw, HEADER + (row + 0.5) * c.ch];
  await script(page, { "1:20:1": strip(c, [RED, GREEN, BLUE, YELLOW]) });
  // Row 1: column 2 has a cell background the program set, 3 and 5 are blank, 4 is a full block in white.
  const row = {
    y: 1,
    runs: [
      ["  ", 2, WHITE, PANE_1_BG, 0],
      [" ", 1, WHITE, EXPLICIT, 0],
      [" ", 1, WHITE, PANE_1_BG, 0],
      ["█", 1, WHITE, PANE_1_BG, 0],
      [" ", 1, WHITE, PANE_1_BG, 0],
      [" ".repeat(14), 14, WHITE, PANE_1_BG, 0],
    ],
  };
  const at = (layer) => [placement(c, { image: 20, layer, col: 2, row: 1, cols: 4, rows: 1, src: [0, 0, 4 * c.cw, c.ch] })];
  const points = [cell(2, 1), cell(3, 1), cell(4, 1), cell(5, 1)];

  // Wait for the first draw of the picture (layer 2 shows it everywhere), then look at each layer.
  let got = await settle(page, [["frame", fullFrame(1, { changed: [row], images: at(2) })]], points, (g) => g[3] === YELLOW);
  colours(got, [RED, GREEN, BLUE, YELLOW], "layer 2 covers the cell background and the block");

  got = await settle(page, [["frame", frame(2, { images: at(0) })]], points, (g) => g[0] === EXPLICIT);
  colours(got, [EXPLICIT, GREEN, WHITE, YELLOW], "layer 0: the cell background hides the picture, the blank cells show it, the block is drawn over it");

  got = await settle(page, [["frame", frame(3, { images: at(1) })]], points, (g) => g[0] === RED && g[2] === WHITE);
  colours(got, [RED, GREEN, WHITE, YELLOW], "layer 1: the picture is over the cell background and under the text");

  // Inside a layer the z value orders, ties go to the lower image id.
  await script(page, { "1:21:1": solid(1, 1, WHITE) });
  const both = [
    placement(c, { image: 21, layer: 2, z: 5, col: 2, row: 1, cols: 1, rows: 1, src: [0, 0, 1, 1] }),
    placement(c, { image: 20, layer: 2, z: 1, col: 2, row: 1, cols: 4, rows: 1, src: [0, 0, 4 * c.cw, c.ch] }),
  ];
  got = await settle(page, [["frame", frame(4, { images: both })]], points, (g) => g[0] === WHITE);
  colours(got, [WHITE, GREEN, BLUE, YELLOW], "a higher z is drawn later, whatever its place in the list");
  assert.equal((await asked(page, 20)).length, 1, "the layers changed without a new request");
}

/** A picture never paints outside the grid's box or over the header; one wholly outside is not even asked for. */
async function clipCase(page, url) {
  const c = await open(page, url);
  const cell = (col, row) => [PAD + (col + 0.5) * c.cw, HEADER + (row + 0.5) * c.ch];
  await script(page, {
    "1:30:1": strip(c, [RED, GREEN, BLUE, YELLOW]),
    "1:31:1": solid(1, 1, RED),
    "1:32:1": solid(1, 1, RED),
  });
  const list = [
    // Columns 18 to 21 of a grid that has 20 columns: 18 and 19 show, 20 and 21 are outside the grid's box.
    placement(c, { image: 30, col: 18, row: 2, cols: 4, rows: 1, src: [0, 0, 4 * c.cw, c.ch] }),
    // Rows -1 and 0: row -1 lies in the pane header, row 0 is the first grid row.
    placement(c, { image: 31, col: 5, row: -1, cols: 2, rows: 2, src: [0, 0, 1, 1] }),
    // Wholly outside the 20 columns.
    placement(c, { image: 32, col: 30, row: 1, cols: 2, rows: 1, src: [0, 0, 1, 1] }),
  ];
  const headerPoint = [PAD + 5.5 * c.cw, HEADER - c.ch / 2];
  const points = [cell(18, 2), cell(19, 2), cell(20, 2), cell(21, 2), cell(5, 0), headerPoint, cell(30, 1)];
  const got = await settle(page, [["frame", fullFrame(1, { images: list })]], points, (g) => g[0] === RED && g[4] === RED);
  colours(got, [RED, GREEN, PANE_1_BG, PANE_1_BG, RED, PANE_1_BG, PANE_1_BG], "clipped to the grid's box, never over the header");
  assert.equal((await asked(page, 32)).length, 0, "a picture wholly outside the grid is not requested");
  assert.equal((await asked(page, 30)).length, 1);
}

/** What the core sends is not trusted: bad placements are skipped, a lying header is refused once, a new generation is tried. */
async function hostileCase(page, url) {
  const c = await open(page, url);
  const cell = (col, row) => [PAD + (col + 0.5) * c.cw, HEADER + (row + 0.5) * c.ch];
  await script(page, {
    "1:40:1": { ...solid(1, 1, RED), total: 8 }, // says 8 bytes for a 1 by 1 picture
    "1:40:2": solid(1, 1, GREEN),
    "1:41:1": solid(1, 1, RED),
    "1:42:1": { ...solid(1, 1, RED), offset: 4 }, // answers another offset than the one asked
    "1:43:1": { ...solid(1, 1, RED), w: 16385, h: 1 }, // wider than any picture
    "1:44:1": solid(1, 1, BLUE),
  });
  const ok = (image, generation, col) => placement(c, { image, generation, col, row: 1, cols: 1, rows: 1, src: [0, 0, 1, 1] });
  const wrong = [
    [41, 1, 3, 0, 5, 1, 1, 1, 0, 0, c.cw, c.ch, 0, 0, 1, 1], // layer 3
    [41, 1.5, 2, 0, 6, 1, 1, 1, 0, 0, c.cw, c.ch, 0, 0, 1, 1], // a fraction
    [41, 1, 2, 0, 7, 1, 1, 1, 0, 0, c.cw, c.ch, 0, 0, 1], // fifteen numbers
    [41, 0, 2, 0, 8, 1, 1, 1, 0, 0, c.cw, c.ch, 0, 0, 1, 1], // generation 0
    [41, 1, 2, 0, 9, 1, 1, 1, 0, 0, 0, c.ch, 0, 0, 1, 1], // no width
    [41, 1, 2, 0, 10, 1, 1, 1, 0, 0, c.cw, c.ch, 0, 0, 20000, 1], // a part wider than any picture
    [41, 1, 2, 0, 11, 1, 1, 1, 0, 0, c.cw, c.ch, 0, 0, "1", 1], // text
    null,
    "x",
  ];
  const list = [...wrong, ok(40, 1, 12), ok(42, 1, 13), ok(43, 1, 14), ok(44, 1, 15)];
  const points = [cell(12, 1), cell(13, 1), cell(14, 1), cell(15, 1), cell(5, 1)];
  const got = await settle(page, [["frame", fullFrame(1, { images: list })]], points, (g) => g[3] === BLUE);
  colours(got, [PANE_1_BG, PANE_1_BG, PANE_1_BG, BLUE, PANE_1_BG], "only the sound picture is drawn");
  await page.waitForTimeout(150);
  assert.equal((await asked(page, 41)).length, 0, "an entry that fails the check is not requested");
  for (const image of [40, 42, 43, 44]) assert.equal((await asked(page, image)).length, 1, `picture ${image} is asked once`);

  // The same generation again does not retry the one that failed; the next generation is tried.
  await settle(page, [["frame", frame(2, { images: list })]], points, () => true);
  await page.waitForTimeout(100);
  assert.equal((await asked(page, 40)).length, 1, "a failed generation is not asked for again");
  const next = [ok(40, 2, 12)];
  const again = await settle(page, [["frame", frame(3, { images: next })]], [cell(12, 1)], (g) => g[0] === GREEN);
  colours(again, [GREEN], "a new generation of the failed picture is fetched and drawn");
  assert.equal((await asked(page, 40)).length, 2);
}

/** Half-transparent pixels blend as straight alpha: 50 % white over the pane colour. */
async function alphaCase(page, url) {
  const c = await open(page, url);
  const cell = (col, row) => [PAD + (col + 0.5) * c.cw, HEADER + (row + 0.5) * c.ch];
  await script(page, { "1:50:1": pic(1, 1, [[255, 255, 255, 128]]) });
  const list = [placement(c, { image: 50, col: 3, row: 1, cols: 1, rows: 1, src: [0, 0, 1, 1] })];
  const [got] = await settle(page, [["frame", fullFrame(1, { images: list })]], [cell(3, 1)], (g) => g[0] !== PANE_1_BG);
  const want = [(255 + 0x6b) / 2, (255 + 0x2f) / 2, (255 + 0xa0) / 2];
  const have = [(got >> 16) & 255, (got >> 8) & 255, got & 255];
  for (let i = 0; i < 3; i++) {
    assert.ok(Math.abs(have[i] - want[i]) <= 2, `channel ${i}: ${have[i]}, straight alpha gives about ${want[i]}`);
  }
}

/** A pane without pictures never touches drawImage and never asks for pixels. */
async function textOnlyCase(page, url) {
  await open(page, url);
  await script(page, {});
  await page.evaluate(() => {
    const proto = CanvasRenderingContext2D.prototype;
    const original = proto.drawImage;
    window.__DRAWS__ = 0;
    proto.drawImage = function (...args) {
      window.__DRAWS__++;
      return original.apply(this, args);
    };
  });
  const text = { y: 0, runs: [["hello world", 11, WHITE, PANE_1_BG, 0], [" ".repeat(9), 9, WHITE, PANE_1_BG, 0]] };
  for (let seq = 1; seq <= 5; seq++) {
    await settle(page, [["frame", seq === 1 ? fullFrame(1, { changed: [text] }) : frame(seq, { changed: [text] })]], [[PAD + 2, HEADER + 2]], () => true);
  }
  assert.equal(await page.evaluate(() => window.__DRAWS__), 0, "a text-only pane never calls drawImage");
  assert.equal((await callsOf(page, "image_data")).length, 0, "a text-only pane never asks for pixels");
  assert.deepEqual(await held(page, 1), { held: 0, bytes: 0, jobs: 0 });
}

/** `open` with the settings the core answers: the page reads `get_settings` at start and hears `settings` events. */
async function openWith(page, url, view) {
  await page.addInitScript((v) => {
    window.__MOCK__.on("get_settings", () => structuredClone(v));
    window.__MOCK__.on("reload_settings", () => structuredClone(v));
  }, view);
  return open(page, url);
}

/** The `enabled` of every `set_image_animation` call so far, oldest first. */
const reports = async (page) => (await callsOf(page, "set_image_animation")).map((a) => a.enabled);

/** Wait until `n` reports were made, then give a late extra one time to show itself, and answer them. */
async function reportsAre(page, n) {
  await page.waitForFunction((count) => window.__MOCK__.callsOf("set_image_animation").length >= count, n, { timeout: 5000 });
  await page.waitForTimeout(150);
  return reports(page);
}

/** The setting `image_memory_mib` 0 turns pictures off in the page; 64 turns them on with the next frame. */
async function memoryCase(page, url) {
  const c = await openWith(page, url, settingsView());
  const cell = (col, row) => [PAD + (col + 0.5) * c.cw, HEADER + (row + 0.5) * c.ch];
  await script(page, { "1:60:1": strip(c, [RED, GREEN]) });
  const list = [placement(c, { image: 60, col: 2, row: 1, cols: 2, rows: 1, src: [0, 0, 2 * c.cw, c.ch] })];
  const points = [cell(2, 1), cell(3, 1)];

  await emit(page, "settings", settingsView({ image_memory_mib: 0 }));
  let got = await settle(page, [["frame", fullFrame(1, { images: list })]], points, () => true);
  await page.waitForTimeout(150);
  colours(got, [PANE_1_BG, PANE_1_BG], "pictures are off: nothing is drawn");
  assert.equal((await asked(page, 60)).length, 0, "pictures are off: nothing is requested");
  assert.deepEqual(await held(page, 1), { held: 0, bytes: 0, jobs: 0 });

  await emit(page, "settings", settingsView({ image_memory_mib: 64 }));
  got = await settle(page, [["frame", frame(2)]], points, (g) => g[0] === RED && g[1] === GREEN);
  colours(got, [RED, GREEN], "with the setting back the next frame fetches and draws");
  assert.equal((await asked(page, 60)).length, 1);

  // Turned off again, what was held is let go of and the screen shows no picture.
  await emit(page, "settings", settingsView({ image_memory_mib: 0 }));
  got = await settle(page, [["frame", frame(3)]], points, (g) => g[0] === PANE_1_BG);
  colours(got, [PANE_1_BG, PANE_1_BG], "turned off while a picture is shown: it goes");
  assert.deepEqual(await held(page, 1), { held: 0, bytes: 0, jobs: 0 });
  await emit(page, "settings", settingsView());
}

/** The person's motion preference reaches the core once at start, on a change of the fact, and not for a change that leaves it. */
async function motionCase(page, url) {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await openWith(page, url, settingsView());
  assert.deepEqual(await reportsAre(page, 1), [false], "reduced motion at start: enabled is false, once");

  await page.emulateMedia({ reducedMotion: "no-preference" });
  assert.deepEqual(await reportsAre(page, 2), [false, true], "the OS preference changed while the page runs");

  await emit(page, "settings", settingsView({ motion: "reduce" }));
  assert.deepEqual(await reportsAre(page, 3), [false, true, false], "the setting motion = reduce");

  await emit(page, "settings", settingsView({ motion: "reduce", contrast: "high" }));
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.waitForTimeout(250);
  assert.deepEqual(await reports(page), [false, true, false], "a change that leaves the fact as it was sends nothing");

  await emit(page, "settings", settingsView({ motion: "full" }));
  assert.deepEqual(await reportsAre(page, 4), [false, true, false, true], "the setting motion = full beats the OS");

  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.waitForTimeout(250);
  assert.equal((await reports(page)).length, 4, "the OS changing under an explicit setting changes nothing");

  // Not reduced at start.
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await openWith(page, url, settingsView());
  assert.deepEqual(await reportsAre(page, 1), [true], "full motion at start: enabled is true");
  await page.emulateMedia({ reducedMotion: null });
}

/** A core that refuses `set_image_animation` (an older build) leaves the page working. */
async function refusedCase(page, url) {
  await page.addInitScript(() => {
    window.__MOCK__.on("set_image_animation", () => {
      throw "unknown command";
    });
  });
  const c = await open(page, url);
  assert.equal((await reports(page)).length >= 1, true, "the page asked");
  await script(page, { "1:61:1": strip(c, [RED]) });
  const list = [placement(c, { image: 61, col: 2, row: 1, cols: 1, rows: 1, src: [0, 0, c.cw, c.ch] })];
  const [got] = await settle(page, [["frame", fullFrame(1, { images: list })]], [[PAD + 2.5 * c.cw, HEADER + 1.5 * c.ch]], (g) => g[0] === RED);
  assert.equal(hex(got), hex(RED), "pictures still work, and no page error was reported");
  await page.addInitScript(() => window.__MOCK__.on("set_image_animation", null));
}

/** After a connection that was lost comes back, the page reports the fact again (a restarted worker has the default). */
async function reconnectCase(page, url) {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await open(page, url);
  assert.deepEqual(await reportsAre(page, 1), [false]);
  await page.evaluate(() => window.__WS_MOCK__.drop(1006));
  assert.deepEqual(await reportsAre(page, 2), [false, false], "after the reconnect the current fact is sent again");
  await page.emulateMedia({ reducedMotion: null });
}

/** Poll the canvas at `points` until `ok(colours)` holds or `ms` pass, with no frame delivered; the colours of the last read. */
async function until(page, points, ok, ms = 6000) {
  const deadline = Date.now() + ms;
  let got = await read(page, points);
  while (!ok(got) && Date.now() < deadline) {
    await page.waitForTimeout(40);
    got = await read(page, points);
  }
  return got;
}

/** Put a solid picture of `w` by `h` pixels into the stub's table, built in the page so a big one never travels through the test. */
const scriptSolidBig = (page, key, w, h, rgb) =>
  page.evaluate(([k, ww, hh, c]) => {
    const px = new Uint8Array(ww * hh * 4);
    for (let i = 0; i < ww * hh; i++) px.set([(c >> 16) & 255, (c >> 8) & 255, c & 255, 255], i * 4);
    window.__PICS__ = { ...(window.__PICS__ ?? {}), [k]: { w: ww, h: hh, px } };
  }, [key, w, h, rgb]);

/**
 * A pull that failed for a reason that can go away is asked for again, with no new frame: here the connection was lost
 * while the picture was asked for and comes back. A picture the core refused for a property of its own (a header that
 * lies) is not asked for again. Web backend only, as the case above: the desktop has no connection to lose; the other
 * trigger of the same rule (a bitmap closed in the pane) is the next case and runs on both backends.
 */
async function failedPullReconnectCase(page, url) {
  const c = await open(page, url);
  const cell = (col, row) => [PAD + (col + 0.5) * c.cw, HEADER + (row + 0.5) * c.ch];
  await script(page, {
    "1:81:1": { ...solid(1, 1, RED), total: 8 }, // refused: says 8 bytes for a 1 by 1 picture
    "1:82:1": solid(1, 1, GREEN), // the control: nothing wrong with it
  }); // picture 80 is not in the table yet: the stub throws, as a core that cannot answer does
  const at = (image, col) => placement(c, { image, col, row: 1, cols: 1, rows: 1, src: [0, 0, 1, 1] });
  const list = [at(80, 2), at(81, 4), at(82, 6)];
  const points = [cell(2, 1), cell(4, 1), cell(6, 1)];

  let got = await settle(page, [["frame", fullFrame(1, { images: list })]], points, (g) => g[2] === GREEN);
  colours(got, [PANE_1_BG, PANE_1_BG, GREEN], "the control is drawn, the two that failed are not");
  for (const [image, n] of [[80, 1], [81, 1], [82, 1]]) assert.equal((await asked(page, image)).length, n, `picture ${image} is asked once`);

  // Frames that repeat the same list are no reason to ask again, whatever failed.
  await settle(page, [["frame", frame(2, { images: list })]], points, () => true);
  await settle(page, [["frame", frame(3, { images: list })]], points, () => true);
  await page.waitForTimeout(150);
  assert.equal((await asked(page, 80)).length, 1, "a failed pull is not asked again by frames that repeat the list");
  assert.equal((await asked(page, 81)).length, 1);
  assert.deepEqual(await held(page, 1), { held: 1, bytes: 4, jobs: 0 }, "only the control is held");

  // The core can answer now; the connection is lost and comes back, and no frame is delivered.
  await script(page, { "1:80:1": solid(1, 1, RED) });
  const sockets = await page.evaluate(() => window.__WS_MOCK__.sockets.length);
  await page.evaluate(() => window.__WS_MOCK__.drop(1006));
  await page.waitForFunction((n) => window.__WS_MOCK__.sockets.length > n, sockets, { timeout: 5000 });
  got = await until(page, points, (g) => g[0] === RED);
  colours(got, [RED, PANE_1_BG, GREEN], "after the connection came back the picture whose pull failed is drawn, without a new frame");
  assert.equal((await asked(page, 80)).length, 2, "asked again once");
  assert.equal((await asked(page, 81)).length, 1, "a refused picture is not asked for again after the reconnect");
  assert.equal((await asked(page, 82)).length, 1, "a picture that is held is not asked for again");
  await page.waitForTimeout(150);
  assert.equal((await asked(page, 80)).length, 2, "and nothing loops");
}

/**
 * The other trigger of the retry rule: a bitmap closed in the pane. With a budget of 1 MiB picture A (600 KiB) is held
 * and then no longer placed; picture B is placed and its pull fails; picture C (600 KiB) is placed as well, its header
 * makes the page close A, and that frees memory, so B is asked for once more and, now answerable, is drawn. No frame
 * is sent for B after the one that placed it.
 */
async function failedPullClosedBitmapCase(page, url) {
  const c = await openWith(page, url, settingsView({ image_memory_mib: 1 }));
  const cell = (col, row) => [PAD + (col + 0.5) * c.cw, HEADER + (row + 0.5) * c.ch];
  await script(page, {});
  await scriptSolidBig(page, "1:90:1", 512, 300, RED);
  await scriptSolidBig(page, "1:92:1", 512, 300, BLUE);
  const at = (image, col, w, h) => placement(c, { image, col, row: 1, cols: 2, rows: 1, src: [0, 0, w, h] });
  const points = [cell(2, 1), cell(5, 1), cell(8, 1)];

  let got = await settle(page, [["frame", fullFrame(1, { images: [at(90, 2, 512, 300)] })]], points, (g) => g[0] === RED);
  colours(got, [RED, PANE_1_BG, PANE_1_BG], "picture A is drawn");
  assert.equal((await held(page, 1)).bytes, 512 * 300 * 4, "A is held");

  // A is not placed any more; B is, and cannot be had.
  const b = [at(91, 5, 1, 1)];
  got = await settle(page, [["frame", frame(2, { images: b })]], points, (g) => g[0] === PANE_1_BG);
  await settle(page, [["frame", frame(3, { images: b })]], points, () => true);
  await page.waitForTimeout(150);
  assert.equal((await asked(page, 91)).length, 1, "B is asked once and the frame that repeats the list asks nothing");
  colours(await read(page, points), [PANE_1_BG, PANE_1_BG, PANE_1_BG], "B failed: nothing drawn");
  assert.equal((await held(page, 1)).held, 1, "A is still held (nothing needed its room yet)");

  // The core can answer for B now. C is placed next to B: its header needs A's room.
  await script(page, { "1:91:1": solid(1, 1, GREEN) });
  const list = [at(91, 5, 1, 1), at(92, 8, 512, 300)];
  got = await settle(page, [["frame", frame(4, { images: list })]], points, (g) => g[1] === GREEN && g[2] === BLUE);
  colours(got, [PANE_1_BG, GREEN, BLUE], "A was closed for C, and B, which failed before, is asked again and drawn");
  assert.equal((await asked(page, 91)).length, 2, "B was asked once more");
  assert.deepEqual((await asked(page, 90)).map((a) => a.offset), [0, 512 * 1024], "A was pulled once, in two pieces, and not asked again");
  assert.deepEqual((await asked(page, 92)).map((a) => a.offset), [0, 512 * 1024], "C in two pieces");
}
