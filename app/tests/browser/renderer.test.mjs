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

    window.__R = { make, dispose, draw, glError, pixels, at, same, pack, covered, block, cell, corner, CELL_W, CELL_H };
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
      out[x] = { err: R.glError(h), cells: Array.from({ length: 5 }, (_, c) => R.pack(R.corner(h, img, c, 0))) };
    }
    R.dispose(h);
    return out;
  });
  for (const kind of ["webgl", "canvas"]) {
    for (const x of ["1", "2"]) {
      const r = wide[kind][x];
      const want = [0x112233, 0xff8fb8, 0xff8fb8, 0x112233, 0x112233];
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

  // ---- the legacy payload is converted in exactly one place ------------------------------------------------
  const legacy = await page.evaluate(async () => {
    const { fromLegacyFrame } = await import("./scene.js");
    const lines = [[["ab", 2, 1, 2, 0]]];
    const f = { cols: 2, rows: 1, cursor: [1, 0], fg: 1, bg: 2, lines, scrollbar: [0, 0, 1] };
    return {
      focused: fromLegacyFrame(f, true),
      unfocused: fromLegacyFrame(f, false),
      none: fromLegacyFrame({ ...f, cursor: null }, true),
    };
  });
  assert.deepEqual(legacy.focused.cursor, { x: 1, y: 0, style: "block", blink: false });
  assert.equal(legacy.focused.focused, true);
  assert.equal(legacy.unfocused.focused, false);
  assert.deepEqual(legacy.unfocused.cursor, { x: 1, y: 0, style: "block", blink: false }, "an unfocused pane keeps its cursor");
  assert.equal(legacy.none.cursor, null);
  assert.deepEqual(legacy.focused.lines, [[["ab", 2, 1, 2, 0]]]);
  assert.equal("scrollbar" in legacy.focused, false, "the legacy scrollbar is not part of the renderer frame");
}
