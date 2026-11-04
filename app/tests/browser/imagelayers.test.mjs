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
    window.__I = { make, dispose, draw, whole, glError, pixels, at, bitmap, place, frame, CELL_W, CELL_H, WHITE };
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
}
