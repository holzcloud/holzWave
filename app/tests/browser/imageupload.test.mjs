// Upload spike for Kitty images (phase 10): raw RGBA reaches both renderers under the page's real Content-Security-Policy,
// without `img-src`, and the blend of a half-transparent pixel can be read back.
//
// The page is only a host for the policy (the runner serves it with the CSP of tauri.conf.json and fails the test on any
// violation). Nothing here imports from app/src: the canvases are made off screen with the same context options as the
// renderers (`alpha: false`, no antialiasing) and driven with plain browser APIs, so the numbers say what the browser does,
// not what the page does.
//
//   texImage2D(Uint8Array)  -> WebGL2 canvas, blended over [16, 16, 16]
//   putImageData + drawImage -> Canvas 2D, blended over [16, 16, 16]
//   createImageBitmap(Blob)  -> attempted once and recorded; nothing depends on it
//
// The expected value of the half-transparent pixel [255, 255, 255, 128] over [16, 16, 16] is
// round(255 * 128 / 255 + 16 * (1 - 128 / 255)) = 136 in every channel; a result is accepted within 1 per channel.
import assert from "node:assert/strict";

export const backends = ["tauri"];
export const backendNote = "it exercises browser upload and blend APIs under the page's policy, not the page's bridge, so one backend is enough";

const EXPECTED = 136;
const TOLERANCE = 1;
/** The 2x2 texture, row by row from the top: red, green, blue, and a half-transparent white. Straight (not premultiplied) alpha. */
const TEXELS = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 128];
const BACKGROUND = [16, 16, 16];

/** A valid 1x1 PNG, built here (zlib and a CRC-32 of our own) so that `createImageBitmap` is given real image bytes. */
async function onePixelPng(rgba) {
  const { deflateSync } = await import("node:zlib");
  const table = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
  });
  const crc = (buf) => {
    let c = 0xffffffff;
    for (const b of buf) c = table[(c ^ b) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
  const chunk = (type, data) => {
    const out = Buffer.alloc(12 + data.length);
    out.writeUInt32BE(data.length, 0);
    out.write(type, 4, "latin1");
    data.copy(out, 8);
    out.writeUInt32BE(crc(out.subarray(4, 8 + data.length)), 8 + data.length);
    return out;
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(1, 0); // width
  ihdr.writeUInt32BE(1, 4); // height
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // colour type: RGBA
  const row = Buffer.concat([Buffer.from([0]), Buffer.from(rgba)]); // filter type 0, then the pixel
  return Array.from(
    Buffer.concat([
      Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
      chunk("IHDR", ihdr),
      chunk("IDAT", deflateSync(row)),
      chunk("IEND", Buffer.alloc(0)),
    ]),
  );
}

export default async function ({ engine, page, url, errors }) {
  await page.goto(`${url}index.html`);
  // Prove that the policy of the shipped app is in force: a data: image must violate img-src. The violation of this probe
  // is the expected one, so it is taken out of `errors` (and only it: the reports of this one call), which leaves only
  // the spike's own reports to fail the test.
  const probeFrom = errors.length;
  const probe = await page.evaluate(
    () =>
      new Promise((resolve) => {
        const img = new Image();
        document.addEventListener("securitypolicyviolation", (e) => resolve(e.violatedDirective), { once: true });
        img.onerror = () => setTimeout(() => resolve("no violation"), 300);
        img.onload = () => resolve("loaded");
        img.src = "data:image/png;base64,iVBORw0KGgo=";
      }),
  );
  assert.match(probe, /^img-src/, `the page must run under the real policy (a data: image must violate img-src), saw: ${probe}`);
  await page.waitForTimeout(100);
  const probeReports = errors.splice(probeFrom, errors.length - probeFrom);
  assert.ok(probeReports.every((e) => /img-src/.test(e)), `unexpected policy reports during the probe: ${probeReports.join("; ")}`);

  const result = await page.evaluate(
    ({ texels, background }) => {
      const out = { webgl: null, canvas2d: null, noWebgl: false };

      // ---- WebGL2 ------------------------------------------------------------------------------------------------
      const canvas = document.createElement("canvas");
      canvas.width = 4;
      canvas.height = 4;
      const gl = canvas.getContext("webgl2", { alpha: false, antialias: false });
      if (!gl) {
        out.noWebgl = true;
      } else {
        const compile = (type, src) => {
          const s = gl.createShader(type);
          gl.shaderSource(s, src);
          gl.compileShader(s);
          if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(`shader: ${gl.getShaderInfoLog(s)}`);
          return s;
        };
        const program = gl.createProgram();
        gl.attachShader(program, compile(gl.VERTEX_SHADER, `#version 300 es
          const vec2 P[6] = vec2[6](vec2(-1.,-1.), vec2(1.,-1.), vec2(-1.,1.), vec2(-1.,1.), vec2(1.,-1.), vec2(1.,1.));
          out vec2 uv;
          void main() {
            vec2 p = P[gl_VertexID];
            // The top of the canvas samples the first row of the data.
            uv = vec2((p.x + 1.) / 2., (1. - p.y) / 2.);
            gl_Position = vec4(p, 0., 1.);
          }`));
        gl.attachShader(program, compile(gl.FRAGMENT_SHADER, `#version 300 es
          precision mediump float;
          uniform sampler2D tex;
          in vec2 uv;
          out vec4 color;
          void main() { color = texture(tex, uv); }`));
        gl.linkProgram(program);
        if (!gl.getProgramParameter(program, gl.LINK_STATUS)) throw new Error(`link: ${gl.getProgramInfoLog(program)}`);
        gl.useProgram(program);
        gl.viewport(0, 0, 4, 4);
        gl.enable(gl.BLEND);

        const drawWith = ({ premultiplyOnUpload, src }) => {
          gl.clearColor(background[0] / 255, background[1] / 255, background[2] / 255, 1);
          gl.clear(gl.COLOR_BUFFER_BIT);
          const tex = gl.createTexture();
          gl.bindTexture(gl.TEXTURE_2D, tex);
          gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, premultiplyOnUpload);
          gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, 2, 2, 0, gl.RGBA, gl.UNSIGNED_BYTE, new Uint8Array(texels));
          gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
          gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
          gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
          gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
          gl.uniform1i(gl.getUniformLocation(program, "tex"), 0);
          gl.blendFunc(src, gl.ONE_MINUS_SRC_ALPHA);
          gl.drawArrays(gl.TRIANGLES, 0, 6);
          const raw = new Uint8Array(4 * 4 * 4);
          gl.readPixels(0, 0, 4, 4, gl.RGBA, gl.UNSIGNED_BYTE, raw);
          const error = gl.getError();
          gl.deleteTexture(tex);
          // Top-down pixel (x, y) is read at row 3 - y.
          const at = (x, y) => Array.from(raw.subarray(((3 - y) * 4 + x) * 4, ((3 - y) * 4 + x) * 4 + 3));
          return { error, red: at(0, 0), green: at(3, 0), blue: at(0, 3), half: at(3, 3), halfOtherCorner: at(2, 2) };
        };
        out.webgl = {
          straight: drawWith({ premultiplyOnUpload: false, src: gl.SRC_ALPHA }),
          premultiplied: drawWith({ premultiplyOnUpload: true, src: gl.ONE }),
          // The wrong pairing, to show that the readback can tell a fringe from a correct blend.
          wrong: drawWith({ premultiplyOnUpload: false, src: gl.ONE }),
        };
        gl.getExtension("WEBGL_lose_context")?.loseContext();
      }

      // ---- Canvas 2D ---------------------------------------------------------------------------------------------
      const source = document.createElement("canvas");
      source.width = 2;
      source.height = 2;
      source.getContext("2d").putImageData(new ImageData(new Uint8ClampedArray(texels), 2, 2), 0, 0);
      const target = document.createElement("canvas");
      target.width = 4;
      target.height = 4;
      const c2d = target.getContext("2d", { alpha: false });
      c2d.imageSmoothingEnabled = false;
      c2d.fillStyle = `rgb(${background.join(",")})`;
      c2d.fillRect(0, 0, 4, 4);
      c2d.drawImage(source, 0, 0, 4, 4);
      const px = (x, y) => Array.from(c2d.getImageData(x, y, 1, 1).data.subarray(0, 3));
      out.canvas2d = { red: px(0, 0), green: px(3, 0), blue: px(0, 3), half: px(3, 3), halfOtherCorner: px(2, 2) };
      return out;
    },
    { texels: TEXELS, background: BACKGROUND },
  );

  // ---- createImageBitmap(Blob): attempted once, recorded, never relied on ----------------------------------------
  const before = errors.length;
  const bitmap = await page.evaluate(async (png) => {
    try {
      const bmp = await createImageBitmap(new Blob([new Uint8Array(png)], { type: "image/png" }));
      const size = `${bmp.width}x${bmp.height}`;
      bmp.close();
      return { allowed: true, size };
    } catch (e) {
      return { allowed: false, name: e?.name ?? "Error", message: String(e?.message ?? e) };
    }
  }, await onePixelPng([10, 20, 30, 255]));
  await page.waitForTimeout(100); // a policy report arrives a moment after the call that caused it
  // Only the reports of this one attempt are taken out of `errors` and recorded: any other violation still fails the test.
  const bitmapReports = errors.splice(before, errors.length - before);

  if (result.noWebgl) {
    console.log(`SKIP: ${engine} has no WebGL2 context in this headless run; the WebGL2 half of the upload spike did not run`);
  }
  const fmt = (rgb) => (rgb ? rgb.join(",") : "n/a");
  const gl = result.webgl;
  console.log(
    `UPLOAD ${engine} webgl straight=${fmt(gl?.straight.half)} premultiplied=${fmt(gl?.premultiplied.half)} ` +
      `canvas2d=${fmt(result.canvas2d.half)} (wrong pairing, straight with ONE: ${fmt(gl?.wrong.half)}; expected ${EXPECTED} per channel)`,
  );
  console.log(
    bitmap.allowed && bitmapReports.length === 0
      ? `BITMAP allowed [${engine}] ${bitmap.size}`
      : `BITMAP blocked ${bitmap.name ?? "SecurityPolicyViolation"} [${engine}]${bitmapReports.length ? ` reports: ${bitmapReports.join(" | ")}` : ""}`,
  );

  // ---- assertions: a value read back from the engine, never a number the engine did not produce ----------------------
  const near = (rgb, want) => rgb.every((v, i) => Math.abs(v - want[i]) <= TOLERANCE);
  const half = [EXPECTED, EXPECTED, EXPECTED];
  if (gl) {
    for (const mode of ["straight", "premultiplied"]) {
      const r = gl[mode];
      assert.equal(r.error, 0, `${engine} webgl ${mode}: GL error ${r.error}`);
      assert.deepEqual(r.red, [255, 0, 0], `${engine} webgl ${mode}: opaque red`);
      assert.deepEqual(r.green, [0, 255, 0], `${engine} webgl ${mode}: opaque green`);
      assert.deepEqual(r.blue, [0, 0, 255], `${engine} webgl ${mode}: opaque blue`);
      assert.ok(near(r.half, half), `${engine} webgl ${mode}: half-transparent pixel ${fmt(r.half)}, expected ${EXPECTED} +-${TOLERANCE}`);
      assert.deepEqual(r.half, r.halfOtherCorner, `${engine} webgl ${mode}: the whole texel is one value`);
    }
    assert.ok(!near(gl.wrong.half, half), `${engine}: the control (straight alpha with ONE) must not look like a correct blend, got ${fmt(gl.wrong.half)}`);
  }
  const c = result.canvas2d;
  assert.deepEqual(c.red, [255, 0, 0], `${engine} canvas2d: opaque red`);
  assert.deepEqual(c.green, [0, 255, 0], `${engine} canvas2d: opaque green`);
  assert.deepEqual(c.blue, [0, 0, 255], `${engine} canvas2d: opaque blue`);
  assert.ok(near(c.half, half), `${engine} canvas2d: half-transparent pixel ${fmt(c.half)}, expected ${EXPECTED} +-${TOLERANCE}`);
  assert.deepEqual(c.half, c.halfOtherCorner, `${engine} canvas2d: the whole texel is one value`);
}
