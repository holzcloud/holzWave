// Renderer pixel tests: both renderers (WebGL2 and Canvas 2D) drawn on fresh canvases and read back.
// The page is only a host: the renderer modules are imported into it and driven directly with hand-made frames,
// so the pixels are the renderers' own and no core is involved.
import assert from "node:assert/strict";

/** Install `window.__R` in the page: renderer construction, drawing, pixel reads and frame builders. */
async function installHarness(page) {
  await page.evaluate(async () => {
    const { WebGLRenderer } = await import("./webgl.js");
    const { Canvas2DRenderer } = await import("./canvas2d.js");
    const CELL_W = 8, CELL_H = 16, DPR = 1;
    const WHITE = 0xffffff;

    /** One renderer on its own canvas, sized to `cols` x `rows` cells. */
    const make = (kind, cols, rows, options) => {
      const canvas = document.createElement("canvas");
      const m = { cellW: CELL_W, cellH: CELL_H, dpr: DPR, width: cols * CELL_W, height: rows * CELL_H };
      canvas.width = m.width;
      canvas.height = m.height;
      document.body.append(canvas);
      const r = kind === "webgl" ? new WebGLRenderer(canvas, options) : new Canvas2DRenderer(canvas);
      r.configure(m);
      if (kind === "canvas") {
        // Chromium moves a 2D canvas from GPU to CPU rasterisation after a few read-backs, and the two differ by
        // one level in some channels. Do the switch now so every image a test compares comes from the same path.
        const g = canvas.getContext("2d");
        for (let i = 0; i < 64; i++) {
          g.fillRect(0, 0, 1, 1);
          g.getImageData(0, 0, 1, 1);
        }
      }
      return { kind, canvas, r, m, cols, rows };
    };
    const dispose = (h) => {
      if (h.kind === "webgl") h.canvas.getContext("webgl2")?.getExtension("WEBGL_lose_context")?.loseContext();
      h.canvas.remove();
    };
    const draw = (h, frame) => {
      h.r.begin(0x000000);
      h.r.drawPane(frame, { x: 0, y: 0, w: h.m.width, h: h.m.height });
    };
    /** The GL error flag after the last draw (always 0 for Canvas 2D). */
    const glError = (h) => (h.kind === "webgl" ? h.canvas.getContext("webgl2").getError() : 0);
    /** The canvas as top-down RGBA bytes. */
    const pixels = (h) => {
      const w = h.canvas.width, ht = h.canvas.height;
      if (h.kind === "canvas") return h.canvas.getContext("2d").getImageData(0, 0, w, ht).data;
      const gl = h.canvas.getContext("webgl2");
      const raw = new Uint8Array(w * ht * 4);
      gl.readPixels(0, 0, w, ht, gl.RGBA, gl.UNSIGNED_BYTE, raw);
      const out = new Uint8ClampedArray(raw.length);
      for (let y = 0; y < ht; y++) out.set(raw.subarray((ht - 1 - y) * w * 4, (ht - y) * w * 4), y * w * 4);
      return out;
    };
    const at = (h, img, x, y) => {
      const i = (y * h.canvas.width + x) * 4;
      return [img[i], img[i + 1], img[i + 2]];
    };
    const same = (a, b) => a[0] === b[0] && a[1] === b[1] && a[2] === b[2];
    const pack = (rgb) => (rgb[0] << 16) | (rgb[1] << 8) | rgb[2];

    /** Every cell covered by one run with a non-default background; the pane background stays black. */
    const covered = (cols, rows, runBg, text = "x") => ({
      cols, rows, cursor: null, focused: true, fg: WHITE, bg: 0x000000,
      lines: Array.from({ length: rows }, () => [[text.repeat(cols), cols, WHITE, runBg, 0]]),
    });
    const block = (x, y) => ({ x, y, style: "block", blink: false });

    /** Pixels of one cell (cx, cy) as [x, y, [r, g, b]] triples. */
    const cell = (h, img, cx, cy) => {
      const out = [];
      for (let y = 0; y < CELL_H; y++) {
        for (let x = 0; x < CELL_W; x++) out.push([x, y, at(h, img, cx * CELL_W + x, cy * CELL_H + y)]);
      }
      return out;
    };
    const corner = (h, img, cx, cy) => at(h, img, cx * CELL_W, cy * CELL_H);

    /** How many pixels of two images differ, over the whole canvas or the cells [c0, c1) of row 0 only. */
    const diff = (a, b, from = 0, to = a.length / 4) => {
      let n = 0;
      for (let i = from; i < to; i++) {
        const j = i * 4;
        if (a[j] !== b[j] || a[j + 1] !== b[j + 1] || a[j + 2] !== b[j + 2]) n++;
      }
      return n;
    };
    /** Differing pixels between two same-size images inside the cell columns [c0, c1) of the pixel rows [y0, y1). */
    const diffCols = (h, a, b, c0, c1, y0 = 0, y1 = h.canvas.height) => {
      let n = 0;
      for (let y = y0; y < y1; y++) {
        for (let x = c0 * CELL_W; x < c1 * CELL_W; x++) {
          const i = (y * h.canvas.width + x) * 4;
          if (a[i] !== b[i] || a[i + 1] !== b[i + 1] || a[i + 2] !== b[i + 2]) n++;
        }
      }
      return n;
    };
    /** Pixels in the cell columns [c0, c1) that are not the colour `bg`. */
    const ink = (h, img, c0, c1, bg) => {
      let n = 0;
      for (let y = 0; y < h.canvas.height; y++) {
        for (let x = c0 * CELL_W; x < c1 * CELL_W; x++) if (!same(at(h, img, x, y), bg)) n++;
      }
      return n;
    };
    /** A frame of one run per row: `text` covers `cols` cells. */
    const row = (text, cols, extras, bg = 0x000000) => ({
      cols, rows: 1, cursor: null, focused: true, fg: WHITE, bg,
      lines: [[[text, cols, WHITE, bg, 0]]], ...(extras ? { extras: [extras] } : {}),
    });
    /** The code point of the n-th distinct test glyph (the probe's mapping). */
    const glyph = (n) => String.fromCodePoint(0x4e00 + n);

    window.__R = {
      make, dispose, draw, glError, pixels, at, same, pack, covered, block, cell, corner, diff, diffCols, ink, row, glyph,
      CELL_W, CELL_H,
    };
  });
}

/** Run `fn(kind)` in the page once per renderer and return `{ webgl, canvas }`. `fn` reaches the harness as `window.__R`. */
async function bothRenderers(page, fn) {
  const out = {};
  for (const kind of ["webgl", "canvas"]) out[kind] = await page.evaluate(fn, kind);
  return out;
}

export default async function ({ page, url }) {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await installHarness(page);

  // ---- the cursor on a fully covered grid -------------------------------------------------------------
  // 40x6, every cell covered by a run with a non-default background, cursor at (3, 1). The block swaps glyph and
  // cell colours in place, so nothing is added to the instance buffer and the glyph stays readable.
  const covered = await bothRenderers(page, (kind) => {
    const R = window.__R;
    const RUN_BG = 0x112233, ROSE = 0xff8fb8;
    const h = R.make(kind, 40, 6);
    const base = R.covered(40, 6, RUN_BG);
    R.draw(h, base);
    const plain = R.pixels(h);
    R.draw(h, { ...base, cursor: R.block(3, 1) });
    const err = R.glError(h);
    const cur = R.pixels(h);

    const rose = [0xff, 0x8f, 0xb8], runBg = [0x11, 0x22, 0x33];
    let roseInPlain = 0, outsideDiff = 0, quietNotRose = 0, inkPixels = 0, inkKeptReadable = 0;
    for (let y = 0; y < h.canvas.height; y++) {
      for (let x = 0; x < h.canvas.width; x++) {
        const p = R.at(h, plain, x, y), c = R.at(h, cur, x, y);
        if (R.same(p, rose)) roseInPlain++;
        const inCursorCell = x >= 3 * 8 && x < 4 * 8 && y >= 1 * 16 && y < 2 * 16;
        if (!inCursorCell) {
          if (!R.same(p, c)) outsideDiff++;
        } else if (R.same(p, runBg)) {
          if (!R.same(c, rose)) quietNotRose++; // a pixel without glyph ink must be exactly the cursor colour
        } else if (p[1] > 0x80) {
          inkPixels++; // strong glyph ink: under the block it must not be the plain cursor colour
          if (!R.same(c, rose)) inkKeptReadable++;
        }
      }
    }
    const corners = [];
    for (let cy = 0; cy < 6; cy++) for (let cx = 0; cx < 40; cx++) corners.push(R.pack(R.corner(h, cur, cx, cy)));
    R.dispose(h);
    return { err, roseInPlain, outsideDiff, quietNotRose, inkPixels, inkKeptReadable, corners, RUN_BG, ROSE };
  });
  for (const kind of ["webgl", "canvas"]) {
    const r = covered[kind];
    assert.equal(r.err, 0, `${kind}: gl.getError() after a covered-grid draw with the cursor`);
    assert.equal(r.roseInPlain, 0, `${kind}: a frame without a cursor shows the cursor colour`);
    assert.equal(r.outsideDiff, 0, `${kind}: pixels outside the cursor cell changed when the cursor was added`);
    assert.equal(r.quietNotRose, 0, `${kind}: empty pixels of the cursor cell are not the cursor colour`);
    assert.ok(r.inkPixels > 0, `${kind}: the test glyph left no ink (test bug)`);
    assert.ok(r.inkKeptReadable > 0, `${kind}: the glyph under the block cursor is not readable`);
    const cursorIndex = 1 * 40 + 3;
    r.corners.forEach((c, i) => {
      const want = i === cursorIndex ? 0xff8fb8 : 0x112233;
      assert.equal(c, want, `${kind}: corner of cell ${i % 40},${Math.floor(i / 40)} is #${c.toString(16)}`);
    });
  }
  assert.deepEqual(covered.webgl.corners, covered.canvas.corners, "the two renderers disagree on cell colours");

  // ---- a block cursor on an empty cell is a solid cell; every position works ------------------------------
  const empty = await bothRenderers(page, (kind) => {
    const R = window.__R;
    const spaces = {
      cols: 40, rows: 6, cursor: null, focused: true, fg: 0xffffff, bg: 0x000000,
      lines: Array.from({ length: 6 }, () => [[" ".repeat(40), 40, 0xffffff, 0x112233, 0]]),
    };
    const h = R.make(kind, 40, 6);
    const res = [];
    for (const [x, y] of [[3, 1], [0, 0], [39, 0], [0, 5], [39, 5]]) {
      R.draw(h, { ...spaces, cursor: R.block(x, y) });
      const err = R.glError(h);
      const img = R.pixels(h);
      let inside = 0, wrongInside = 0, wrongOutside = 0;
      for (const [cx, cy, rgb] of R.cell(h, img, x, y)) {
        inside++;
        if (!R.same(rgb, [0xff, 0x8f, 0xb8])) wrongInside++;
      }
      for (let py = 0; py < h.canvas.height; py++) {
        for (let px = 0; px < h.canvas.width; px++) {
          const inCell = px >= x * 8 && px < (x + 1) * 8 && py >= y * 16 && py < (y + 1) * 16;
          if (!inCell && !R.same(R.at(h, img, px, py), [0x11, 0x22, 0x33])) wrongOutside++;
        }
      }
      res.push({ x, y, err, inside, wrongInside, wrongOutside });
    }
    // The same frame without a cursor shows no cursor colour anywhere.
    R.draw(h, spaces);
    const img = R.pixels(h);
    let rose = 0;
    for (let py = 0; py < h.canvas.height; py++) {
      for (let px = 0; px < h.canvas.width; px++) if (R.same(R.at(h, img, px, py), [0xff, 0x8f, 0xb8])) rose++;
    }
    R.dispose(h);
    return { res, rose };
  });
  for (const kind of ["webgl", "canvas"]) {
    assert.equal(empty[kind].rose, 0, `${kind}: a cursor-free frame shows the cursor colour`);
    for (const r of empty[kind].res) {
      const where = `${kind}: cursor at ${r.x},${r.y}`;
      assert.equal(r.err, 0, `${where}: gl.getError()`);
      assert.equal(r.inside, 128, `${where}: cell size`);
      assert.equal(r.wrongInside, 0, `${where}: the cursor cell is not solid cursor colour`);
      assert.equal(r.wrongOutside, 0, `${where}: pixels outside the cursor cell are not the run background`);
    }
  }

  // ---- a wide cell is covered as a whole, from either of its halves -----------------------------------
  const wide = await bothRenderers(page, (kind) => {
    const R = window.__R;
    const f = {
      cols: 10, rows: 1, cursor: null, focused: true, fg: 0xffffff, bg: 0x000000,
      lines: [[["a日\0b", 4, 0xffffff, 0x112233, 0], ["      ", 6, 0xffffff, 0x112233, 0]]],
    };
    const h = R.make(kind, 10, 1);
    const out = {};
    for (const x of [1, 2]) {
      R.draw(h, { ...f, cursor: R.block(x, 0) });
      const img = R.pixels(h);
      // A dense wide glyph can cover most of a cell, so a cell counts as covered when any of it is the cursor colour.
      const roseCount = (c) => R.cell(h, img, c, 0).filter(([, , rgb]) => R.same(rgb, [0xff, 0x8f, 0xb8])).length;
      out[x] = { err: R.glError(h), cells: Array.from({ length: 5 }, (_, c) => roseCount(c) > 0) };
    }
    R.dispose(h);
    return out;
  });
  for (const kind of ["webgl", "canvas"]) {
    for (const x of ["1", "2"]) {
      const r = wide[kind][x];
      const want = [false, true, true, false, false];
      assert.equal(r.err, 0, `${kind}: wide cursor at ${x}: gl.getError()`);
      assert.deepEqual(r.cells, want, `${kind}: cursor at ${x} on a wide character must cover both cells and no other`);
    }
  }

  // ---- extras on every cell of a covered grid do not break the draw ---------------------------------------
  const extras = await bothRenderers(page, (kind) => {
    const R = window.__R;
    const f = R.covered(40, 6, 0x112233);
    f.cursor = R.block(3, 1);
    f.extras = Array.from({ length: 6 }, () => Array.from({ length: 40 }, (_, c) => [c, "̈"]));
    const h = R.make(kind, 40, 6);
    R.draw(h, f);
    const err = R.glError(h);
    const img = R.pixels(h);
    const cornerOfCursor = R.pack(R.corner(h, img, 3, 1));
    R.dispose(h);
    return { err, cornerOfCursor };
  });
  for (const kind of ["webgl", "canvas"]) {
    assert.equal(extras[kind].err, 0, `${kind}: gl.getError() with extras on every cell`);
    assert.equal(extras[kind].cornerOfCursor, 0xff8fb8, `${kind}: cursor cell with extras`);
  }

  // ---- hostile sizes and cursors never allocate or throw ---------------------------------------------------
  const hostile = await bothRenderers(page, (kind) => {
    const R = window.__R;
    const h = R.make(kind, 4, 2);
    const base = R.covered(4, 2, 0x112233);
    const tries = [
      { ...base, cols: 1e6, rows: 1e6 },
      { ...base, cols: NaN },
      { ...base, rows: -3 },
      { ...base, cols: 2.5 },
      { ...base, cursor: R.block(4, 0) },
      { ...base, cursor: R.block(-1, 0) },
      { ...base, cursor: R.block(0, 2) },
      { ...base, cursor: R.block(NaN, 0) },
    ];
    const res = [];
    for (const f of tries) {
      let threw = null;
      try {
        R.draw(h, f);
      } catch (e) {
        threw = String(e);
      }
      res.push({ threw, err: R.glError(h) });
    }
    // Still usable afterwards, and an out-of-range cursor drew no cursor colour.
    R.draw(h, { ...base, cursor: R.block(1, 1) });
    const img = R.pixels(h);
    const ok = R.pack(R.corner(h, img, 1, 1));
    R.dispose(h);
    return { res, ok };
  });
  for (const kind of ["webgl", "canvas"]) {
    hostile[kind].res.forEach((r, i) => {
      assert.equal(r.threw, null, `${kind}: hostile frame ${i} threw ${r.threw}`);
      assert.equal(r.err, 0, `${kind}: hostile frame ${i} left a GL error`);
    });
    assert.equal(hostile[kind].ok, 0xff8fb8, `${kind}: the renderer is unusable after hostile frames`);
  }

  // ---- the atlas never resets in the middle of a frame ------------------------------------------------------
  // RESEARCH Pitfall 2: five 160x50 frames of unique glyphs on one renderer, the last frame's first row re-using
  // glyphs of frame 0, must equal that frame on a fresh renderer. Three frames fit and always did.
  const atlas = await page.evaluate(() => {
    const R = window.__R;
    const cols = 160, rows = 50;
    const frameK = (k) => {
      const lines = [];
      for (let y = 0; y < rows; y++) {
        const kk = y === 0 && k > 0 ? 0 : k;
        let s = "";
        for (let x = 0; x < cols; x++) s += R.glyph(kk * cols * rows + y * cols + x);
        lines.push([[s, cols, 0xffffff, 0x000000, 0]]);
      }
      return { cols, rows, cursor: null, focused: true, fg: 0xffffff, bg: 0x000000, lines };
    };
    const out = {};
    for (const frames of [3, 5]) {
      const long = R.make("webgl", cols, rows);
      let last;
      for (let k = 0; k < frames; k++) {
        last = frameK(k);
        R.draw(long, last);
      }
      const a = R.pixels(long);
      const errLong = R.glError(long);
      const fresh = R.make("webgl", cols, rows);
      R.draw(fresh, last);
      const b = R.pixels(fresh);
      out[frames] = { differing: R.diff(a, b), total: a.length / 4, errLong, errFresh: R.glError(fresh) };
      R.dispose(long);
      R.dispose(fresh);
    }
    return out;
  });
  for (const frames of ["3", "5"]) {
    assert.equal(atlas[frames].errLong + atlas[frames].errFresh, 0, `${frames} frames: GL error`);
    assert.equal(atlas[frames].differing, 0, `${frames} frames: pixels differ from a fresh renderer`);
  }

  // The same with a tiny atlas, so every frame resets it: a pane never sees half of a reset, and a pane with more
  // distinct glyphs than the atlas holds draws the rest as blanks instead of looping.
  const small = await page.evaluate(() => {
    const R = window.__R;
    const cols = 20, rows = 8; // 160 cells; an atlas of 128 x 128 px holds 16 x 8 = 128 slots, one of them blank
    const mk = (offsets) => {
      const lines = [];
      for (let y = 0; y < rows; y++) {
        let s = "";
        for (let x = 0; x < cols; x++) s += R.glyph(offsets(y, x));
        lines.push([[s, cols, 0xffffff, 0x000000, 0]]);
      }
      return { cols, rows, cursor: null, focused: true, fg: 0xffffff, bg: 0x000000, lines };
    };
    const half = (y, x) => y * cols + x; // 160 distinct glyphs
    const fewer = (base, n = 90) => (y, x) => base + ((y * cols + x) % n); // n distinct glyphs
    const out = {};

    // (a) distinct sets that overflow mid-frame, the next frame re-using glyphs of the first: the last frame has
    // 40 + 80 = 120 distinct glyphs, which fits after the reset
    const tiny = R.make("webgl", cols, rows, { atlasSize: 128 });
    const frames = [
      mk(fewer(0)), mk(fewer(1000)), mk(fewer(2000)),
      mk((y, x) => (y < 2 ? fewer(0)(y, x) : fewer(3000, 80)(y, x))),
    ];
    for (const f of frames) R.draw(tiny, f);
    const a = R.pixels(tiny);
    const big = R.make("webgl", cols, rows);
    R.draw(big, frames[3]);
    out.mixed = { differing: R.diff(a, R.pixels(big)), err: R.glError(tiny) };

    // (b) more distinct glyphs than the atlas holds: terminates, no error, the first glyphs are right, the rest blank
    R.draw(tiny, mk(half));
    const t = R.pixels(tiny);
    const errB = R.glError(tiny);
    R.draw(big, mk(half));
    const g = R.pixels(big);
    const slots = 127; // 128 slots, slot 0 stays blank
    const w = tiny.canvas.width;
    const firstRows = Math.floor(slots / cols); // complete rows that must be right
    const wrongFirst = R.diffCols(tiny, t, g, 0, cols, 0, firstRows * 16);
    let inkAfter = 0;
    for (let y = (firstRows + 1) * 16; y < rows * 16; y++) {
      for (let x = 0; x < w; x++) if (!R.same(R.at(tiny, t, x, y), [0, 0, 0])) inkAfter++;
    }
    out.overfull = { err: errB, wrongFirst, inkAfter, firstRows };
    R.dispose(tiny);
    R.dispose(big);
    return out;
  });
  assert.equal(small.mixed.err, 0, "tiny atlas: GL error");
  assert.equal(small.mixed.differing, 0, "tiny atlas: a frame after resets differs from a fresh renderer");
  assert.equal(small.overfull.err, 0, "overfull atlas: GL error");
  assert.equal(small.overfull.wrongFirst, 0, `overfull atlas: the first ${small.overfull.firstRows} rows are not right`);
  assert.equal(small.overfull.inkAfter, 0, "overfull atlas: glyphs beyond the atlas are not blank");

  // ---- a cluster is one cell: the cells after it stay aligned ------------------------------------------------
  const align = await bothRenderers(page, (kind) => {
    const R = window.__R;
    const out = {};
    const draw = (f) => {
      const h = R.make(kind, f.cols, f.rows);
      R.draw(h, f);
      const img = R.pixels(h);
      const err = R.glError(h);
      return { h, img, err };
    };
    // "a", "o" + U+0308, "b" in three cells; the precomposed form is the reference for the cells around it.
    const cluster = draw(R.row("aob      ", 9, [[1, "\u0308"]]));
    const plain = draw(R.row("aob      ", 9));
    const nfc = draw(R.row("a\u00f6b      ", 9));
    out.err = cluster.err + plain.err + nfc.err;
    out.leftOfCluster = R.diffCols(cluster.h, cluster.img, nfc.img, 0, 1);
    out.rightOfCluster = R.diffCols(cluster.h, cluster.img, nfc.img, 2, 9);
    out.clusterVsPlain = R.diffCols(cluster.h, cluster.img, plain.img, 1, 2);
    out.clusterInk = R.ink(cluster.h, cluster.img, 1, 2, [0, 0, 0]);
    out.plainInk = R.ink(plain.h, plain.img, 1, 2, [0, 0, 0]);
    for (const x of [cluster, plain, nfc]) R.dispose(x.h);

    // A ZWJ family and a flag, each a base code point plus its tail in extras, take their cell(s) and no more.
    const sample = (base, tail) => {
      const withTail = draw(R.row(`${base}\0b      `, 9, [[0, tail]]));
      const withoutTail = draw(R.row(`${base}\0b      `, 9));
      const blank = draw(R.row("  b      ", 9));
      const r = {
        err: withTail.err + withoutTail.err + blank.err,
        right: R.diffCols(withTail.h, withTail.img, blank.img, 2, 9),
        tailChangesGlyph: R.diffCols(withTail.h, withTail.img, withoutTail.img, 0, 2),
        ink: R.ink(withTail.h, withTail.img, 0, 2, [0, 0, 0]),
      };
      for (const x of [withTail, withoutTail, blank]) R.dispose(x.h);
      return r;
    };
    out.family = sample("\u{1F468}", "\u200D\u{1F469}\u200D\u{1F467}");
    out.flag = sample("\u{1F1E9}", "\u{1F1EA}");
    return out;
  });
  for (const kind of ["webgl", "canvas"]) {
    const a = align[kind];
    assert.equal(a.err, 0, `${kind}: GL error`);
    assert.equal(a.leftOfCluster, 0, `${kind}: the cell before a cluster changed`);
    assert.equal(a.rightOfCluster, 0, `${kind}: cells right of a combining cluster are not aligned`);
    assert.ok(a.clusterInk > 0, `${kind}: the cluster cell has no ink`);
    assert.ok(a.clusterVsPlain > 0 && a.clusterInk > a.plainInk, `${kind}: the combining mark was not drawn`);
    for (const name of ["family", "flag"]) {
      const c = a[name];
      assert.equal(c.err, 0, `${kind}: ${name}: GL error`);
      assert.equal(c.right, 0, `${kind}: ${name}: cells after the cluster are not unaffected`);
      assert.ok(c.ink > 0, `${kind}: ${name}: no ink`);
      assert.ok(c.tailChangesGlyph > 0, `${kind}: ${name}: the tail was not drawn with its base`);
    }
  }

  // ---- cursor shapes ----------------------------------------------------------------------------------------
  // Bar: the leftmost 2 device px of the cell. Underline: the bottom 2. Hollow: a 1 px border, interior unchanged.
  // An unfocused pane shows its cursor hollow whatever its style. Each is exactly that region and nothing else.
  const shapes = await bothRenderers(page, (kind) => {
    const R = window.__R;
    const base = R.covered(40, 6, 0x112233);
    const h = R.make(kind, 40, 6);
    R.draw(h, base);
    const plain = R.pixels(h);
    const out = {};
    const region = {
      bar: (x, y) => x < 2,
      underline: (x, y) => y >= 14,
      hollow: (x, y) => x === 0 || x === 7 || y === 0 || y === 15,
    };
    const check = (name, frame, inRegion) => {
      R.draw(h, frame);
      const err = R.glError(h);
      const img = R.pixels(h);
      let wrongIn = 0, wrongOut = 0, outsideCell = 0;
      for (let y = 0; y < h.canvas.height; y++) {
        for (let x = 0; x < h.canvas.width; x++) {
          const p = R.at(h, plain, x, y), c = R.at(h, img, x, y);
          const cx = x - 3 * 8, cy = y - 1 * 16;
          const inCell = cx >= 0 && cx < 8 && cy >= 0 && cy < 16;
          if (!inCell) {
            if (!R.same(p, c)) outsideCell++;
          } else if (inRegion(cx, cy)) {
            if (!R.same(c, [0xff, 0x8f, 0xb8])) wrongIn++;
          } else if (!R.same(p, c)) wrongOut++;
        }
      }
      out[name] = { err, wrongIn, wrongOut, outsideCell };
    };
    const cur = (style) => ({ x: 3, y: 1, style, blink: false });
    check("bar", { ...base, cursor: cur("bar") }, region.bar);
    check("underline", { ...base, cursor: cur("underline") }, region.underline);
    check("hollow", { ...base, cursor: cur("hollow") }, region.hollow);
    check("unfocusedBlock", { ...base, focused: false, cursor: cur("block") }, region.hollow);
    check("unfocusedBar", { ...base, focused: false, cursor: cur("bar") }, region.hollow);

    // A wide cell: the bar is one cell's width, the underline spans both, the hollow border goes around both.
    const wideBase = {
      cols: 10, rows: 1, cursor: null, focused: true, fg: 0xffffff, bg: 0x000000,
      // a, a wide character in cells 1 and 2, b, then blanks
      lines: [[["a\u65e5\0b", 4, 0xffffff, 0x112233, 0], ["      ", 6, 0xffffff, 0x112233, 0]]],
    };
    const hw = R.make(kind, 10, 1);
    R.draw(hw, wideBase);
    const wplain = R.pixels(hw);
    const wideCheck = (style, inRegion) => {
      R.draw(hw, { ...wideBase, cursor: { x: 1, y: 0, style, blink: false } });
      const img = R.pixels(hw);
      let wrongIn = 0, wrongOut = 0;
      for (let y = 0; y < 16; y++) {
        for (let x = 0; x < 80; x++) {
          const cx = x - 8; // relative to the wide character's first cell, 16 px wide
          const inCell = cx >= 0 && cx < 16;
          const want = inCell && inRegion(cx, y);
          const c = R.at(hw, img, x, y), p = R.at(hw, wplain, x, y);
          if (want) {
            if (!R.same(c, [0xff, 0x8f, 0xb8])) wrongIn++;
          } else if (!R.same(c, p)) wrongOut++;
        }
      }
      return { wrongIn, wrongOut, err: R.glError(hw) };
    };
    out.wideBar = wideCheck("bar", (x, y) => x < 2);
    out.wideUnderline = wideCheck("underline", (x, y) => y >= 14);
    out.wideHollow = wideCheck("hollow", (x, y) => x === 0 || x === 15 || y === 0 || y === 15);
    R.dispose(h);
    R.dispose(hw);
    return out;
  });
  for (const kind of ["webgl", "canvas"]) {
    for (const [name, r] of Object.entries(shapes[kind])) {
      const where = `${kind}: ${name}`;
      assert.equal(r.err, 0, `${where}: GL error`);
      assert.equal(r.wrongIn ?? 0, 0, `${where}: part of the shape is not the cursor colour`);
      assert.equal(r.wrongOut ?? 0, 0, `${where}: the shape covers pixels it should not`);
      assert.equal(r.outsideCell ?? 0, 0, `${where}: pixels outside the cursor cell changed`);
    }
  }

  // ---- the wire frame becomes the renderers' frame in exactly one place --------------------------------------
  const wire = await page.evaluate(async () => {
    const { applyFrame, toRenderFrame } = await import("./frames.js");
    const msg = {
      pane: 1, seq: 1, full: true, cols: 2, rows: 1, fg: 1, bg: 2,
      cursor: { x: 1, y: 0, style: "bar", blink: true }, scrollbar: [0, 1, 1], selection: [], matches: [],
      search: null, copy_cursor: null, modes: { alt: false, bracketed_paste: false, mouse: false },
      changed: [{ y: 0, runs: [["ab", 2, 1, 2, 0]] }],
    };
    const mirror = applyFrame(undefined, msg).mirror;
    return {
      focused: toRenderFrame(mirror, true),
      unfocused: toRenderFrame(mirror, false),
      none: toRenderFrame(applyFrame(undefined, { ...msg, cursor: null }).mirror, true),
    };
  });
  assert.deepEqual(wire.focused.cursor, { x: 1, y: 0, style: "bar", blink: true });
  assert.equal(wire.focused.focused, true);
  assert.equal(wire.unfocused.focused, false);
  assert.deepEqual(wire.unfocused.cursor, { x: 1, y: 0, style: "bar", blink: true }, "an unfocused pane keeps its cursor");
  assert.equal(wire.none.cursor, null);
  assert.deepEqual(wire.focused.lines, [[["ab", 2, 1, 2, 0]]]);
  assert.equal("scrollbar" in wire.focused, false, "the scrollbar is not part of the renderer frame");
}
