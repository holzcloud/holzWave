// Pictures in the WebGL renderer against the canvas renderer (POL-01, D-01, D-03): both renderers are drawn on fresh
// canvases from the same hand-made frames, with pictures in the three layers, and the colours they read back are
// compared. The page is only a host: the renderer modules are imported into it and driven directly, so no core and no
// mock command is involved. The canvas renderer is the reference; the WebGL renderer must show the same.
import assert from "node:assert/strict";

/** Install `window.__I` in the page: renderers, bitmaps, frames, counters on the GL context, pixel reads. */
async function installHarness(page) {
  await page.evaluate(async () => {
    const { WebGLRenderer } = await import("./webgl.js");
    const { Canvas2DRenderer } = await import("./canvas2d.js");
    const CELL_W = 8, CELL_H = 16, DPR = 1;
    const WHITE = 0xffffff;

    /**
     * One renderer on its own canvas, sized to `cols` x `rows` cells plus `extra` pixels on the right and at the bottom
     * (the pane's padding). For WebGL the context's calls are counted before the renderer is built.
     */
    const make = (kind, cols, rows, extra = { w: 0, h: 0 }) => {
      const canvas = document.createElement("canvas");
      const m = { cellW: CELL_W, cellH: CELL_H, dpr: DPR, width: cols * CELL_W + extra.w, height: rows * CELL_H + extra.h };
      canvas.width = m.width;
      canvas.height = m.height;
      document.body.append(canvas);
      const count = { programs: 0, textures: 0, deleted: 0, instanced: 0, strips: 0, errors: 0 };
      if (kind === "webgl") {
        const gl = canvas.getContext("webgl2", { alpha: false, antialias: false, powerPreference: "high-performance" });
        const wrap = (name, field) => {
          const orig = gl[name].bind(gl);
          gl[name] = (...a) => {
            count[field]++;
            return orig(...a);
          };
        };
        wrap("createProgram", "programs");
        wrap("createTexture", "textures");
        wrap("deleteTexture", "deleted");
        wrap("drawArraysInstanced", "instanced");
        wrap("drawArrays", "strips");
      }
      const r = kind === "webgl" ? new WebGLRenderer(canvas) : new Canvas2DRenderer(canvas);
      r.configure(m);
      if (kind === "canvas") {
        // Chromium moves a 2D canvas from GPU to CPU rasterisation after a few read-backs; do the switch now so every
        // image a test compares comes from the same path.
        const g = canvas.getContext("2d");
        for (let i = 0; i < 64; i++) {
          g.fillRect(0, 0, 1, 1);
          g.getImageData(0, 0, 1, 1);
        }
      }
      return { kind, canvas, r, m, cols, rows, count };
    };
    const dispose = (h) => {
      if (h.kind === "webgl") h.canvas.getContext("webgl2")?.getExtension("WEBGL_lose_context")?.loseContext();
      h.canvas.remove();
    };
    /** The pane's rectangle: the whole canvas. */
    const whole = (h) => ({ x: 0, y: 0, w: h.m.width, h: h.m.height });
    const draw = (h, frame, fx) => {
      h.r.begin(0x000000);
      h.r.drawPane(frame, whole(h), fx);
    };
    /** The GL error flag after the last draw (always 0 for Canvas 2D); it also clears the flag. */
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
    /** A bitmap from straight RGBA pixels (`px` is a flat list of r, g, b, a), made the way the picture store makes one. */
    const bitmap = (w, ht, px) => createImageBitmap(new ImageData(new Uint8ClampedArray(px), w, ht));
    /** A picture placed at cell (col, row), `cols` by `rows` cells, showing the source rectangle (all of it by default). */
    const place = (bmp, { layer = 2, z = 0, id = 1, col, row, cols, rows, src }) => ({
      layer, z, id, bitmap: bmp,
      dx: col * CELL_W, dy: row * CELL_H, dw: cols * CELL_W, dh: rows * CELL_H,
      sx: src ? src[0] : 0, sy: src ? src[1] : 0, sw: src ? src[2] : bmp.width, sh: src ? src[3] : bmp.height,
    });
    /** A frame of one run per row; `rows` is a list of [text, bg] pairs, each covering all `cols` cells. */
    const frame = (cols, rows, images, over = {}) => ({
      cols, rows: rows.length, cursor: null, focused: true, fg: WHITE, bg: 0x000000,
      lines: rows.map(([text, bg = 0x000000]) => [[text.padEnd(cols, " "), cols, WHITE, bg, 0]]),
      ...(images ? { images } : {}),
      ...over,
    });
    /** The colours at a list of [x, y] pixels. */
    const probes = (h, img, list) => list.map(([x, y]) => at(h, img, x, y));
    /** The pixel at the centre of cell (col, row). */
    const mid = (col, row) => [col * CELL_W + CELL_W / 2, row * CELL_H + CELL_H / 2];
    /** A draw's instanced draw calls (the grid and the cursor), counted on the WebGL context; 0 for the canvas renderer. */
    const drawCalls = (h, frame, fx) => {
      const before = h.count.instanced;
      draw(h, frame, fx);
      return h.count.instanced - before;
    };
    /**
     * The 12 by 4 frame of the layer cases. Row 1: a full block at column 2, a blank cell with an explicit background at
     * column 4, blank cells with the default background elsewhere; the other rows are blank.
     */
    const EXPLICIT = 0xaa3300;
    const layered = (images, over = {}) => {
      const blank = (n, bg = 0x000000) => [" ".repeat(n), n, WHITE, bg, 0];
      return {
        cols: 12, rows: 4, cursor: null, focused: true, fg: WHITE, bg: 0x000000,
        lines: [[blank(12)], [["  \u2588 ", 4, WHITE, 0x000000, 0], blank(1, EXPLICIT), blank(7)], [blank(12)], [blank(12)]],
        images, ...over,
      };
    };
    window.__I = { make, dispose, draw, whole, glError, pixels, at, bitmap, place, frame, probes, mid, drawCalls, layered, EXPLICIT, CELL_W, CELL_H, WHITE };
  });
}

/** Run `fn(kind)` in the page once per renderer and return `{ webgl, canvas }`. `fn` reaches the harness as `window.__I`. */
async function bothRenderers(page, fn, arg) {
  const out = {};
  for (const kind of ["webgl", "canvas"]) out[kind] = await page.evaluate(fn, { kind, arg });
  return out;
}

const RED = [255, 0, 0], GREEN = [0, 255, 0], BLUE = [0, 0, 255], WHITE3 = [255, 255, 255], BLACK = [0, 0, 0];
const hex = (c) => `rgb(${c.join(",")})`;

export default async function ({ page, url }) {
  await page.goto(`${url}index.html?renderer=canvas`);
  await page.waitForSelector(".divider");
  await installHarness(page);

  // ---- the tracer: one picture above the text, equal in both renderers -----------------------------------
  // 12 by 4 cells with text; a 2 by 2 pixel picture (red, green / blue, white) at column 3, row 1, two cells wide and
  // two tall. Probes sit in the outer half texel of each quadrant, where bilinear scaling gives the pure colour.
  const tracer = await bothRenderers(page, async ({ kind }) => {
    const I = window.__I;
    const h = I.make(kind, 12, 4);
    const bmp = await I.bitmap(2, 2, [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]);
    const rows = [["hello world!"], ["abcdefghijkl"], ["mnopqrstuvwx"], ["yz0123456789"]];
    const without = I.frame(12, rows);
    const withPic = I.frame(12, rows, [I.place(bmp, { col: 3, row: 1, cols: 2, rows: 2 })]);
    I.draw(h, without);
    const plain = I.pixels(h);
    I.draw(h, withPic);
    const err = I.glError(h);
    const img = I.pixels(h);
    // The picture covers x 24..40, y 16..48. A glyph pixel outside it that is the text colour without the picture.
    let ink = null;
    for (let y = 0; y < 64 && !ink; y++) {
      for (let x = 0; x < 96 && !ink; x++) {
        const inside = x >= 24 && x < 40 && y >= 16 && y < 48;
        const p = I.at(h, plain, x, y);
        if (!inside && p[0] === 255 && p[1] === 255 && p[2] === 255) ink = [x, y];
      }
    }
    const probe = (x, y) => I.at(h, img, x, y);
    const out = {
      err,
      red: probe(26, 19), green: probe(37, 19), blue: probe(26, 44), white: probe(37, 44),
      ink, inkNow: ink ? probe(ink[0], ink[1]) : null,
      // Outside the picture nothing changed.
      outsideSame: (() => {
        let n = 0;
        for (let y = 0; y < 64; y++) {
          for (let x = 0; x < 96; x++) {
            if (x >= 24 && x < 40 && y >= 16 && y < 48) continue;
            const a = I.at(h, plain, x, y), b = probe(x, y);
            if (a[0] !== b[0] || a[1] !== b[1] || a[2] !== b[2]) n++;
          }
        }
        return n;
      })(),
    };
    I.dispose(h);
    return out;
  });
  for (const kind of ["webgl", "canvas"]) {
    const r = tracer[kind];
    assert.equal(r.err, 0, `${kind}: gl.getError() after a picture above the text`);
    assert.deepEqual(r.red, RED, `${kind}: top left quadrant`);
    assert.deepEqual(r.green, GREEN, `${kind}: top right quadrant`);
    assert.deepEqual(r.blue, BLUE, `${kind}: bottom left quadrant`);
    assert.deepEqual(r.white, WHITE3, `${kind}: bottom right quadrant`);
    assert.ok(r.ink, `${kind}: the frame has no text pixel outside the picture (test bug)`);
    assert.deepEqual(r.inkNow, WHITE3, `${kind}: the text outside the picture keeps its colour`);
    assert.equal(r.outsideSame, 0, `${kind}: pixels outside the picture changed`);
  }
  assert.deepEqual(tracer.webgl.red, tracer.canvas.red, "red quadrant differs between the renderers");

  // ---- a frame without pictures: no picture program, no texture ------------------------------------------
  const none = await bothRenderers(page, async ({ kind }) => {
    const I = window.__I;
    const h = I.make(kind, 12, 4);
    const built = { ...h.count };
    const rows = [["hello world!"], ["abcdefghijkl"], ["mnopqrstuvwx"], ["yz0123456789"]];
    I.draw(h, I.frame(12, rows));
    const a = Array.from(I.pixels(h));
    I.draw(h, I.frame(12, rows, []));
    const b = Array.from(I.pixels(h));
    const err = I.glError(h);
    const after = { ...h.count };
    let diff = 0;
    for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) diff++;
    I.dispose(h);
    return { built, after, diff, err };
  });
  assert.equal(none.webgl.err, 0, "webgl: gl.getError() after frames without pictures");
  assert.equal(none.webgl.after.programs, none.webgl.built.programs, "a frame without pictures compiled a program");
  assert.equal(none.webgl.after.textures, none.webgl.built.textures, "a frame without pictures made a texture");
  assert.equal(none.webgl.after.instanced - none.webgl.built.instanced, 2, "two text-only draws are two instanced draws");
  assert.equal(none.webgl.diff, 0, "images: [] draws other pixels than no images");
  assert.equal(none.canvas.diff, 0, "canvas: images: [] draws other pixels than no images");

  // ---- the source rectangle is honoured ------------------------------------------------------------------
  // A 4 by 1 picture, red red blue blue, drawn from its right half only: every probe reads blue. (Chromium's canvas lets
  // the filter read a little past the source rectangle, so the probes keep half a texel from its left edge.)
  const source = await bothRenderers(page, async ({ kind }) => {
    const I = window.__I;
    const h = I.make(kind, 12, 4);
    const bmp = await I.bitmap(4, 1, [255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 255, 255, 0, 0, 255, 255]);
    const rows = [[""], [""], [""], [""]];
    I.draw(h, I.frame(12, rows, [I.place(bmp, { col: 3, row: 1, cols: 2, rows: 2, src: [2, 0, 2, 1] })]));
    const err = I.glError(h);
    const img = I.pixels(h);
    const probes = [[33, 19], [37, 24], [34, 44], [37, 44]].map(([x, y]) => I.at(h, img, x, y));
    I.dispose(h);
    return { err, probes };
  });
  for (const kind of ["webgl", "canvas"]) {
    assert.equal(source[kind].err, 0, `${kind}: gl.getError() after a picture from a source rectangle`);
    for (const p of source[kind].probes) assert.deepEqual(p, BLUE, `${kind}: the right half of the bitmap is blue`);
  }

  // ---- a picture wholly outside the grid draws nothing; one that reaches past it is cut at the grid's box --------
  // The canvas is 20 pixels wider and 20 taller than the 12 by 4 grid (the pane's padding); a picture that starts
  // inside the grid and runs past its right and bottom edge shows only inside it.
  const clip = await bothRenderers(page, async ({ kind }) => {
    const I = window.__I;
    const h = I.make(kind, 12, 4, { w: 20, h: 20 });
    const bmp = await I.bitmap(1, 1, [0, 255, 0, 255]);
    const rows = [[""], [""], [""], [""]];
    const far = { layer: 2, z: 0, id: 9, bitmap: bmp, dx: 300, dy: 0, dw: 16, dh: 16, sx: 0, sy: 0, sw: 1, sh: 1 };
    I.draw(h, I.frame(12, rows, [far]));
    const errFar = I.glError(h);
    const farImg = I.pixels(h);
    let farGreen = 0;
    for (let i = 0; i < farImg.length; i += 4) if (farImg[i + 1] === 255) farGreen++;
    const over = { layer: 2, z: 0, id: 1, bitmap: bmp, dx: 80, dy: 48, dw: 40, dh: 40, sx: 0, sy: 0, sw: 1, sh: 1 };
    I.draw(h, I.frame(12, rows, [over]));
    const errOver = I.glError(h);
    const img = I.pixels(h);
    const w = h.canvas.width;
    const inside = I.at(h, img, 90, 55), edgeIn = I.at(h, img, 95, 63);
    const rightPad = I.at(h, img, 100, 55), bottomPad = I.at(h, img, 90, 70), corner = I.at(h, img, 110, 80);
    I.dispose(h);
    return { errFar, farGreen, errOver, inside, edgeIn, rightPad, bottomPad, corner, w };
  });
  for (const kind of ["webgl", "canvas"]) {
    const r = clip[kind];
    assert.equal(r.errFar, 0, `${kind}: gl.getError() after a picture wholly outside the grid`);
    assert.equal(r.farGreen, 0, `${kind}: a picture wholly outside the grid drew pixels`);
    assert.equal(r.errOver, 0, `${kind}: gl.getError() after a picture that reaches past the grid`);
    assert.deepEqual(r.inside, GREEN, `${kind}: the part of the picture inside the grid`);
    assert.deepEqual(r.edgeIn, GREEN, `${kind}: the last pixel of the grid is covered`);
    assert.deepEqual(r.rightPad, BLACK, `${kind}: the picture reached into the padding on the right`);
    assert.deepEqual(r.bottomPad, BLACK, `${kind}: the picture reached into the padding below`);
    assert.deepEqual(r.corner, BLACK, `${kind}: the picture reached into the corner of the padding`);
  }

  // ---- the three layers against cell backgrounds, text and each other ---------------------------------------
  // Row 1 holds a full block (white ink, cell 2), a cell with an explicit background (cell 4) and blank cells (the rest).
  // One opaque green picture covers cells 1 to 8 of the row. Layer 0 lies under the backgrounds, so the explicit cell
  // stays its colour and the blank ones show green; layer 1 covers the backgrounds but not the ink; layer 2 covers all.
  const EXPLICIT3 = [0xaa, 0x33, 0x00];
  const layers = await bothRenderers(page, async ({ kind }) => {
    const I = window.__I;
    const h = I.make(kind, 12, 4);
    const bmp = await I.bitmap(1, 1, [0, 255, 0, 255]);
    const out = {};
    I.draw(h, I.layered(undefined));
    out.plain = I.probes(h, I.pixels(h), [I.mid(2, 1), I.mid(4, 1), I.mid(6, 1)]);
    for (const layer of [0, 1, 2]) {
      I.draw(h, I.layered([I.place(bmp, { layer, col: 1, row: 1, cols: 8, rows: 1 })]));
      out[layer] = { err: I.glError(h), colours: I.probes(h, I.pixels(h), [I.mid(2, 1), I.mid(4, 1), I.mid(6, 1), I.mid(0, 1), I.mid(9, 1)]) };
    }
    I.dispose(h);
    return out;
  });
  for (const kind of ["webgl", "canvas"]) {
    const r = layers[kind];
    assert.deepEqual(r.plain, [WHITE3, EXPLICIT3, BLACK], `${kind}: the frame without a picture (block ink, explicit cell, blank cell)`);
    for (const layer of [0, 1, 2]) assert.equal(r[layer].err, 0, `${kind}: gl.getError() with a layer ${layer} picture`);
    // cells: block (2), explicit (4), blank (6), left of the picture (0), right of the picture (9)
    assert.deepEqual(r[0].colours, [WHITE3, EXPLICIT3, GREEN, BLACK, BLACK], `${kind}: a layer 0 picture is under the backgrounds`);
    assert.deepEqual(r[1].colours, [WHITE3, GREEN, GREEN, BLACK, BLACK], `${kind}: a layer 1 picture is over the backgrounds and under the ink`);
    assert.deepEqual(r[2].colours, [GREEN, GREEN, GREEN, BLACK, BLACK], `${kind}: a layer 2 picture is over the ink`);
  }
  assert.deepEqual(layers.webgl, layers.canvas, "the layers differ between the renderers");

  // ---- a half transparent picture in each layer ---------------------------------------------------------
  // Alpha 128 of 255. The renderers agree within 2 of 255 per channel; the blend is a green wash of about half.
  const alpha = await bothRenderers(page, async ({ kind }) => {
    const I = window.__I;
    const h = I.make(kind, 12, 4);
    const bmp = await I.bitmap(1, 1, [0, 255, 0, 128]);
    const out = {};
    for (const layer of [0, 1, 2]) {
      I.draw(h, I.layered([I.place(bmp, { layer, col: 1, row: 1, cols: 8, rows: 1 })]));
      out[layer] = { err: I.glError(h), colours: I.probes(h, I.pixels(h), [I.mid(2, 1), I.mid(4, 1), I.mid(6, 1)]) };
    }
    I.dispose(h);
    return out;
  });
  for (const layer of [0, 1, 2]) {
    assert.equal(alpha.webgl[layer].err, 0, `webgl: gl.getError() with a half transparent layer ${layer} picture`);
    alpha.webgl[layer].colours.forEach((c, i) => {
      const ref = alpha.canvas[layer].colours[i];
      for (let k = 0; k < 3; k++) {
        assert.ok(Math.abs(c[k] - ref[k]) <= 2, `layer ${layer}, probe ${i}: webgl ${hex(c)} against canvas ${hex(ref)}`);
      }
    });
  }
  // The wash is really there (not opaque, not absent): over black the blank cell reads about (0, 128, 0) in layers 0 to 2.
  for (const layer of [0, 1, 2]) {
    const blank = alpha.webgl[layer].colours[2];
    assert.ok(blank[1] > 110 && blank[1] < 146 && blank[0] <= 2 && blank[2] <= 2, `layer ${layer}: blank cell under alpha 128 reads ${hex(blank)}`);
  }

  // ---- the cursor, the underline and a ghost over a layer 1 picture ---------------------------------------
  // The cursor lies over every picture, as in the canvas renderer: a block is a solid cell, the other shapes are drawn
  // alone and the picture shows around them.
  const ROSE = [0xff, 0x8f, 0xb8];
  const cursors = await bothRenderers(page, async ({ kind }) => {
    const I = window.__I;
    const h = I.make(kind, 12, 4);
    const bmp = await I.bitmap(1, 1, [0, 255, 0, 255]);
    const cell = (col, row, x, y) => [col * 8 + x, row * 16 + y];
    // inside the cell: centre, left edge, right edge, top row, bottom row, the row above the bottom, left column x 4
    const around = (col, row) => [[4, 8], [0, 8], [7, 8], [4, 0], [4, 15], [4, 14], [1, 8], [4, 5]].map(([x, y]) => cell(col, row, x, y));
    const out = {};
    for (const layer of [1, 2]) {
      const pic = [I.place(bmp, { layer, col: 1, row: 1, cols: 8, rows: 1 })];
      for (const style of ["block", "bar", "underline", "hollow"]) {
        for (const at of [[6, 1], [4, 1]]) {
          I.draw(h, I.layered(pic, { cursor: { x: at[0], y: at[1], style, blink: false } }));
          out[`${layer} ${style} ${at}`] = { err: I.glError(h), colours: I.probes(h, I.pixels(h), around(at[0], at[1])) };
        }
      }
      I.draw(h, I.layered(pic, { focused: false, cursor: { x: 6, y: 1, style: "block", blink: false } }));
      out[`${layer} unfocused`] = { err: I.glError(h), colours: I.probes(h, I.pixels(h), around(6, 1)) };
    }
    // an underlined run of two blank cells (5 and 6 of row 1) under a layer 1 picture, and a ghost over it
    const under = I.layered([I.place(bmp, { layer: 1, col: 1, row: 1, cols: 8, rows: 1 })]);
    under.lines[1] = [["    ", 4, 0xffffff, 0, 0], ["  ", 2, 0xffffff, 0, 4], ["      ", 6, 0xffffff, 0, 0]];
    I.draw(h, under);
    out.underline = { err: I.glError(h), colours: I.probes(h, I.pixels(h), [cell(4, 1, 4, 14), cell(5, 1, 4, 14), cell(5, 1, 4, 8), cell(6, 1, 4, 14), cell(7, 1, 4, 14)]) };
    I.draw(h, I.layered([I.place(bmp, { layer: 1, col: 1, row: 1, cols: 8, rows: 1 })]), { ghosts: [{ x: 6, y: 1, alpha: 0.5 }] });
    out.ghost = { err: I.glError(h), colours: I.probes(h, I.pixels(h), [cell(6, 1, 4, 8), cell(7, 1, 4, 8)]) };
    I.dispose(h);
    return out;
  });
  const names = Object.keys(cursors.webgl);
  for (const name of names) {
    const w = cursors.webgl[name], c = cursors.canvas[name];
    assert.equal(w.err, 0, `webgl: gl.getError() for ${name}`);
    w.colours.forEach((p, i) => {
      for (let k = 0; k < 3; k++) assert.ok(Math.abs(p[k] - c.colours[i][k]) <= 2, `${name}, probe ${i}: webgl ${hex(p)} against canvas ${hex(c.colours[i])}`);
    });
  }
  for (const kind of ["webgl", "canvas"]) {
    const r = cursors[kind];
    for (const layer of [1, 2]) {
      // probes: centre, left edge, right edge, top row, bottom row, row above the bottom, x 1, y 5 (all of cell 6, a blank)
      assert.deepEqual(r[`${layer} block 6,1`].colours[0], ROSE, `${kind}: a block cursor over a layer ${layer} picture`);
      assert.deepEqual(r[`${layer} bar 6,1`].colours.slice(1, 2).concat(r[`${layer} bar 6,1`].colours.slice(6, 7)), [ROSE, ROSE], `${kind}: the bar is the cursor colour`);
      assert.deepEqual(r[`${layer} bar 6,1`].colours[0], GREEN, `${kind}: the picture shows beside the bar (layer ${layer})`);
      assert.deepEqual(r[`${layer} underline 6,1`].colours[4], ROSE, `${kind}: the underline cursor (layer ${layer})`);
      assert.deepEqual(r[`${layer} underline 6,1`].colours[0], GREEN, `${kind}: the picture shows above the underline cursor (layer ${layer})`);
      assert.deepEqual(r[`${layer} hollow 6,1`].colours[1], ROSE, `${kind}: the hollow cursor's edge (layer ${layer})`);
      assert.deepEqual(r[`${layer} hollow 6,1`].colours[0], GREEN, `${kind}: the picture shows inside the hollow cursor (layer ${layer})`);
      assert.deepEqual(r[`${layer} unfocused`].colours[1], ROSE, `${kind}: an unfocused cursor is hollow (layer ${layer})`);
      assert.deepEqual(r[`${layer} unfocused`].colours[0], GREEN, `${kind}: an unfocused cursor leaves the picture inside (layer ${layer})`);
    }
    // the underline of text over a layer 1 picture; probes: cells 4 and 5 (underlined) at the bottom, cell 5 at the
    // middle, cells 6 and 7 (no flag) at the bottom
    assert.deepEqual(r.underline.colours.slice(0, 2), [WHITE3, WHITE3], `${kind}: the underline of text over a layer 1 picture`);
    assert.deepEqual(r.underline.colours.slice(2), [GREEN, GREEN, GREEN], `${kind}: the picture shows above the underline and beside the run`);
  }
  // a ghost at alpha 0.5 over green: about half of each
  for (const kind of ["webgl", "canvas"]) {
    const g = cursors[kind].ghost.colours[0];
    assert.ok(Math.abs(g[0] - 0x80) <= 3 && Math.abs(g[1] - 0xc7) <= 3 && Math.abs(g[2] - 0x5c) <= 3, `${kind}: a ghost over a picture reads ${hex(g)}`);
    assert.deepEqual(cursors[kind].ghost.colours[1], GREEN, `${kind}: the cell next to the ghost`);
  }

  // ---- order: the list is the order inside a layer; layers interleave with the passes ---------------------------
  const order = await bothRenderers(page, async ({ kind }) => {
    const I = window.__I;
    const h = I.make(kind, 12, 4);
    const red = await I.bitmap(1, 1, [255, 0, 0, 255]);
    const green = await I.bitmap(1, 1, [0, 255, 0, 255]);
    const blue = await I.bitmap(1, 1, [0, 0, 255, 255]);
    const out = {};
    const probe = (cols) => I.probes(h, I.pixels(h), cols.map((c) => I.mid(c, 1)));
    // two pictures of one layer overlapping at cells 4 to 5: the later one in the list lies on top
    I.draw(h, I.layered([
      I.place(red, { layer: 1, z: 0, id: 1, col: 3, row: 1, cols: 3, rows: 1 }),
      I.place(blue, { layer: 1, z: 1, id: 2, col: 4, row: 1, cols: 3, rows: 1 }),
    ]));
    out.same = { err: I.glError(h), colours: probe([3, 4, 5, 6]) };
    I.draw(h, I.layered([
      I.place(blue, { layer: 1, z: 1, id: 2, col: 4, row: 1, cols: 3, rows: 1 }),
      I.place(red, { layer: 1, z: 0, id: 1, col: 3, row: 1, cols: 3, rows: 1 }),
    ]));
    out.reversed = { err: I.glError(h), colours: probe([3, 4, 5, 6]) };
    // pictures of different layers, listed in the "wrong" order, still interleave with the passes: layer 0 green over
    // cells 1 to 8, layer 1 red over cells 3 to 5 and layer 2 blue over cells 5 to 6; cell 4 is the explicit one
    I.draw(h, I.layered([
      I.place(blue, { layer: 2, id: 3, col: 5, row: 1, cols: 2, rows: 1 }),
      I.place(red, { layer: 1, id: 2, col: 3, row: 1, cols: 3, rows: 1 }),
      I.place(green, { layer: 0, id: 1, col: 1, row: 1, cols: 8, rows: 1 }),
    ]));
    out.layers = { err: I.glError(h), colours: probe([1, 2, 3, 4, 5, 6, 7]) };
    I.dispose(h);
    return out;
  });
  for (const kind of ["webgl", "canvas"]) {
    const r = order[kind];
    for (const name of ["same", "reversed", "layers"]) assert.equal(r[name].err, 0, `${kind}: gl.getError() in the ${name} order case`);
    assert.deepEqual(r.same.colours, [RED, BLUE, BLUE, BLUE], `${kind}: the later picture of a layer lies on top`);
    assert.deepEqual(r.reversed.colours, [RED, RED, RED, BLUE], `${kind}: the list order is the order inside a layer`);
    // 1 green (layer 0), 2 ink (white), 3 red (layer 1), 4 red (layer 1 over the explicit background), 5 blue, 6 blue, 7 green
    assert.deepEqual(r.layers.colours, [GREEN, WHITE3, RED, RED, BLUE, BLUE, GREEN], `${kind}: pictures of three layers`);
  }
  assert.deepEqual(order.webgl, order.canvas, "the order cases differ between the renderers");

  // ---- the number of instanced draws -------------------------------------------------------------------------
  // A frame without a picture, or with pictures above the text only, is one instanced draw; a picture below the text
  // makes two (backgrounds, then text); a visible cursor in a frame with pictures is one more (it comes last).
  const calls = await page.evaluate(async () => {
    const I = window.__I;
    const h = I.make("webgl", 12, 4);
    const bmp = await I.bitmap(1, 1, [0, 255, 0, 255]);
    const pic = (layer) => [I.place(bmp, { layer, col: 1, row: 1, cols: 4, rows: 1 })];
    const cursor = { x: 6, y: 1, style: "block", blink: false };
    const out = {
      none: I.drawCalls(h, I.layered(undefined)),
      noneCursor: I.drawCalls(h, I.layered(undefined, { cursor })),
      layer2: I.drawCalls(h, I.layered(pic(2))),
      layer2Cursor: I.drawCalls(h, I.layered(pic(2), { cursor })),
      layer1: I.drawCalls(h, I.layered(pic(1))),
      layer0: I.drawCalls(h, I.layered(pic(0))),
      layer0Cursor: I.drawCalls(h, I.layered(pic(0), { cursor })),
      outside: I.drawCalls(h, I.layered([I.place(bmp, { layer: 0, col: 40, row: 1, cols: 4, rows: 1 })])),
      err: I.glError(h),
    };
    I.dispose(h);
    return out;
  });
  assert.equal(calls.err, 0, "gl.getError() after the draw counting");
  assert.equal(calls.none, 1, "a frame without a picture is one instanced draw");
  assert.equal(calls.noneCursor, 1, "a cursor without pictures is part of that draw");
  assert.equal(calls.layer2, 1, "a frame with pictures above the text only is one instanced draw");
  assert.equal(calls.layer2Cursor, 2, "its cursor comes last, in a draw of its own");
  assert.equal(calls.layer1, 2, "a layer 1 picture makes the backgrounds and the text two draws");
  assert.equal(calls.layer0, 2, "a layer 0 picture makes the backgrounds and the text two draws");
  assert.equal(calls.layer0Cursor, 3, "the cursor comes last");
  assert.equal(calls.outside, 1, "a lower layer picture wholly outside the grid does not split the grid draw");

  // ---- a slide moves pictures with the grid, clipped to its box; blank cells hide the frames under it ----------
  const slide = await bothRenderers(page, async ({ kind }) => {
    const I = window.__I;
    const h = I.make(kind, 12, 4, { w: 16, h: 16 });
    const bmp = await I.bitmap(1, 1, [0, 255, 0, 255]);
    const out = {};
    // layer 2 over cells 1 to 4 of row 0 and of the last row (3); the grid slides down one row
    const pic = (layer) => [
      I.place(bmp, { layer, id: 1, col: 1, row: 0, cols: 4, rows: 1 }),
      I.place(bmp, { layer, id: 2, col: 1, row: 3, cols: 4, rows: 1 }),
    ];
    const at = (col, row, x = 4, y = 8) => [col * 8 + x, row * 16 + y];
    const list = [at(2, 0), at(2, 1), at(2, 3), at(2, 4), at(2, 4, 4, 2)];
    I.draw(h, I.layered(pic(2)), { offsetY: 16 });
    out.layer2 = { err: I.glError(h), colours: I.probes(h, I.pixels(h), list) };
    // the same with a frame under the slide (all cells blue) and a layer 1 picture: the frame's blank cells cover blue
    const blue = { cols: 12, rows: 4, cursor: null, focused: true, fg: 0xffffff, bg: 0x000000, lines: Array.from({ length: 4 }, () => [[" ".repeat(12), 12, 0xffffff, 0x0000ff, 0]]) };
    I.draw(h, I.layered(pic(1)), { offsetY: 16, under: [{ frame: blue, offsetY: 0 }] });
    out.under = { err: I.glError(h), colours: I.probes(h, I.pixels(h), [at(8, 0), at(8, 1), at(2, 1), at(8, 3), at(2, 4)]) };
    I.dispose(h);
    return out;
  });
  for (const kind of ["webgl", "canvas"]) {
    const r = slide[kind];
    assert.equal(r.layer2.err, 0, `${kind}: gl.getError() with a picture on a slide`);
    // row 0 is bare (black), row 1 shows the picture that was in row 0, row 3 shows the one from row 2 (none: black),
    // the picture of row 3 slid past the box into the padding: not drawn
    assert.deepEqual(r.layer2.colours, [BLACK, GREEN, BLACK, BLACK, BLACK], `${kind}: the pictures follow the slide and stop at the grid's box`);
    assert.equal(r.under.err, 0, `${kind}: gl.getError() with a picture on a slide and a frame under it`);
    // row 0: the frame under it (blue); row 1 cell 8: blank, black over the blue; row 1 cell 2: the layer 1 picture
    assert.deepEqual(r.under.colours.slice(0, 3), [BLUE, BLACK, GREEN], `${kind}: the frame under the slide only where the frame leaves bare`);
    assert.deepEqual(r.under.colours[4], BLACK, `${kind}: nothing in the padding`);
  }
  assert.deepEqual(slide.webgl, slide.canvas, "the slide cases differ between the renderers");
}
