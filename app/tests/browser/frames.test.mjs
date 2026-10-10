// `frame` events patch a per-pane mirror and the renderer draws from it (DESK-11): a full frame replaces, a frame
// of changed rows patches, a gap in the sequence asks the core for a full frame exactly once, and a pane seen for
// the first time is started and then asked for a full frame. The legacy `screen` event has no listener any more.
import assert from "node:assert/strict";
import { callsOf, emit } from "./lib.mjs";

const PANE_1_BG = 0x6b2fa0;
const PANE_2_BG = 0x2a6fdb;
const GREEN = 0x00c040;
const PAD = 6; // CSS px between a pane's edge and its grid (geometry.ts)

const MODES = { alt: false, bracketed_paste: false, mouse: false };

/** A frame message as the core sends it. */
const frame = (pane, seq, over = {}) => ({
  pane, seq, full: false, cols: 20, rows: 4, fg: 0xffffff, bg: PANE_1_BG, cursor: null,
  scrollbar: [0, 4, 4], selection: [], matches: [], search: null, copy_cursor: null, modes: MODES, changed: [],
  ...over,
});
const fullFrame = (pane, seq, bg, changed = []) => frame(pane, seq, { full: true, bg, changed });
/** A row of `n` blank cells with the background `bg`. */
const stripe = (y, n, bg) => ({ y, runs: [[" ".repeat(n), n, 0xffffff, bg, 0]] });

async function open(page, url, query = "?renderer=canvas") {
  await page.goto(`${url}index.html${query}`);
  await page.waitForSelector(".divider");
  await page.waitForFunction(() => window.__MOCK__.callsOf("start_pane").length >= 2);
  const asked = await page
    .waitForFunction(() => window.__MOCK__.callsOf("request_full").length >= 2, undefined, { timeout: 5000 })
    .then(() => true, () => false);
  assert.ok(asked, "the page never sent request_full for the panes it started");
}

/**
 * Deliver `events` to the page, wait for the draw they cause and read the colours (0xRRGGBB) at `points`, all in
 * one page task: a WebGL canvas without preserveDrawingBuffer is only readable in the task of its draw. The
 * scene's own animation-frame callback is registered by the emit, so ours runs after it in the same frame.
 * Points are CSS pixels inside the canvas; works for either renderer.
 */
const paint = (page, events, points) =>
  page.evaluate(
    async ([evs, pts]) => {
      for (const [name, payload] of evs) window.__MOCK__.emit(name, payload);
      await new Promise((done) => requestAnimationFrame(() => done()));
      const c = document.getElementById("term");
      const k = c.width / c.clientWidth;
      const gl = c.getContext("webgl2");
      return pts.map(([x, y]) => {
        const px = Math.floor(x * k);
        const py = Math.floor(y * k);
        if (gl) {
          const out = new Uint8Array(4);
          gl.readPixels(px, gl.drawingBufferHeight - 1 - py, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, out);
          return (out[0] << 16) | (out[1] << 8) | out[2];
        }
        const [r, g, b] = c.getContext("2d").getImageData(px, py, 1, 1).data;
        return (r << 16) | (g << 8) | b;
      });
    },
    [events, points],
  );

const hex = (n) => `#${n.toString(16).padStart(6, "0")}`;

export default async function ({ page, url, errors }) {
  await firstSightCase(page, url);
  await drawCase(page, url, "?renderer=canvas");
  await drawCase(page, url, "");
  await gapCase(page, url);
  await legacyCase(page, url);
  assert.deepEqual(errors, [], "the page reported errors");
}

/** A pane seen for the first time is started with its size and cell size, then asked for a full frame. */
async function firstSightCase(page, url) {
  await open(page, url);
  const calls = await page.evaluate(() => window.__MOCK__.calls.map(([cmd, args]) => [cmd, args]));
  for (const pane of [1, 2]) {
    const start = calls.findIndex(([cmd, a]) => cmd === "start_pane" && a.pane === pane);
    const full = calls.findIndex(([cmd, a]) => cmd === "request_full" && a.pane === pane);
    assert.ok(start >= 0, `pane ${pane} was not started`);
    assert.ok(full > start, `pane ${pane}: request_full (${full}) must follow start_pane (${start})`);
    const a = calls[start][1];
    assert.ok(Number.isInteger(a.cell_w) && a.cell_w > 0, `cell_w is ${a.cell_w}`);
    assert.ok(Number.isInteger(a.cell_h) && a.cell_h > 0, `cell_h is ${a.cell_h}`);
    assert.ok(a.cols >= 2 && a.rows >= 1);
  }
  assert.equal((await callsOf(page, "request_full")).length, 2, "one request_full per pane at start-up");
}

/** Full frames paint the panes; a frame of changed rows patches only those rows. */
async function drawCase(page, url, query) {
  await open(page, url, query);
  const start = (await callsOf(page, "start_pane"))[0];
  const cw = start.cell_w; // dpr 1: device px are CSS px
  const ch = start.cell_h;
  const size = await page.evaluate(() => {
    const c = document.getElementById("term");
    return { w: c.clientWidth, h: c.clientHeight };
  });
  const p1 = (col, row) => [PAD + (col + 0.5) * cw, PAD + (row + 0.5) * ch];
  const p2 = (col, row) => [size.w / 2 + PAD + (col + 0.5) * cw, PAD + (row + 0.5) * ch];

  const at = (label, got, want) => assert.equal(hex(got), hex(want), `${query || "webgl"}: ${label}`);

  let [row0, row1, other] = await paint(
    page,
    [["frame", fullFrame(1, 1, PANE_1_BG, [stripe(0, 6, GREEN)])], ["frame", fullFrame(2, 1, PANE_2_BG)]],
    [p1(2, 0), p1(2, 1), p2(2, 0)],
  );
  at("row 0 of pane 1 is green", row0, GREEN);
  at("row 1 of pane 1 has the pane background", row1, PANE_1_BG);
  at("pane 2 has its own background", other, PANE_2_BG);

  // Only row 2 changes: row 0 must stay green (a patch, not a replacement).
  let [r0, r2] = await paint(page, [["frame", frame(1, 2, { changed: [stripe(2, 6, GREEN)] })]], [p1(2, 0), p1(2, 2)]);
  at("the patched row is green", r2, GREEN);
  at("an unchanged row was lost by the patch", r0, GREEN);

  // A full frame replaces: row 0 and row 2 are blank again.
  let [a, b, c] = await paint(
    page,
    [["frame", fullFrame(1, 3, PANE_1_BG, [stripe(1, 6, GREEN)])]],
    [p1(2, 1), p1(2, 0), p1(2, 2)],
  );
  at("the full frame's row is green", a, GREEN);
  at("a full frame must replace the rows it does not send", b, PANE_1_BG);
  at("a full frame must replace the rows it does not send (row 2)", c, PANE_1_BG);

  assert.equal((await callsOf(page, "request_full")).length, 2, "a gap-free sequence asks for nothing");
}

/** A skipped sequence number asks for a full frame once; frames are ignored until it arrives. */
async function gapCase(page, url) {
  await open(page, url);
  const fulls = async (pane) => (await callsOf(page, "request_full")).filter((a) => a.pane === pane).length;
  const base = await fulls(2);

  await emit(page, "frame", fullFrame(2, 1, PANE_2_BG));
  await emit(page, "frame", frame(2, 2, { bg: PANE_2_BG }));
  assert.equal(await fulls(2), base, "frames in sequence ask for nothing");

  await emit(page, "frame", frame(2, 4, { bg: PANE_2_BG })); // 3 is missing
  assert.equal(await fulls(2), base + 1, "a gap asks for a full frame");
  await emit(page, "frame", frame(2, 5, { bg: PANE_2_BG }));
  await emit(page, "frame", frame(2, 6, { bg: PANE_2_BG }));
  assert.equal(await fulls(2), base + 1, "frames after a gap must not ask again before the full frame arrives");
  assert.equal(await fulls(1), 1, "the other pane was not touched");

  await emit(page, "frame", fullFrame(2, 7, PANE_2_BG));
  await emit(page, "frame", frame(2, 8, { bg: PANE_2_BG }));
  assert.equal(await fulls(2), base + 1, "the full frame ends the gap and the sequence continues from it");

  await emit(page, "frame", frame(2, 10, { bg: PANE_2_BG })); // 9 is missing: a second gap asks again
  assert.equal(await fulls(2), base + 2, "a second gap asks again");
}

/** The `screen` event is gone: nothing listens to it. */
async function legacyCase(page, url) {
  await open(page, url);
  assert.equal(await emit(page, "screen", { pane: 1, frame: {} }), 0, "something still listens to the legacy screen event");
  assert.ok((await emit(page, "frame", fullFrame(1, 1, PANE_1_BG))) >= 1, "the frame event has a listener");
}
