import {
  BOLD, CURSOR_COLOR, CursorShape, FONT_PX, Frame, INVERSE, ITALIC, Metrics, PxRect, Renderer, UNDERLINE, font, gridCells,
  visibleCursor,
} from "./frame.js";

// WebGL2 renderer: one instanced draw per frame. Every cell is an instance that carries its
// glyph slot in a texture atlas plus foreground and background colour; the fragment shader mixes them.

const ATLAS = 2048;
const WORDS = 6; // col, row (f32) and slot, fg, bg, flags (u32) per instance

const VERT = `#version 300 es
layout(location = 0) in vec2 aCell;
layout(location = 1) in uint aSlot;
layout(location = 2) in uint aFg;
layout(location = 3) in uint aBg;
layout(location = 4) in uint aFlags;
uniform vec2 uCell;      // cell size in device px
uniform vec2 uViewport;  // canvas size in device px
uniform vec2 uSlot;      // atlas slot size in px
uniform uint uAtlasCols;
out vec2 vUV;
out vec3 vFg;
out vec3 vBg;
out vec2 vCorner;
flat out uint vFlags;
vec3 rgb(uint c) { return vec3(float((c >> 16u) & 255u), float((c >> 8u) & 255u), float(c & 255u)) / 255.0; }
void main() {
  vec2 corner = vec2(float(gl_VertexID & 1), float(gl_VertexID >> 1));
  vec2 px = (aCell + corner) * uCell;
  gl_Position = vec4(px.x / uViewport.x * 2.0 - 1.0, 1.0 - px.y / uViewport.y * 2.0, 0.0, 1.0);
  vec2 origin = vec2(float(aSlot % uAtlasCols), float(aSlot / uAtlasCols)) * uSlot;
  vUV = (origin + corner * uCell) / ${ATLAS}.0;
  vFg = rgb(aFg);
  vBg = rgb(aBg);
  vCorner = corner;
  vFlags = aFlags;
}`;

const FRAG = `#version 300 es
precision highp float;
uniform sampler2D uAtlas;
uniform vec2 uCell;
in vec2 vUV;
in vec3 vFg;
in vec3 vBg;
in vec2 vCorner;
flat in uint vFlags;
out vec4 outColor;
void main() {
  float a = texture(uAtlas, vUV).a;
  vec3 c = mix(vBg, vFg, a);
  if ((vFlags & 4u) != 0u && vCorner.y > 1.0 - 2.0 / uCell.y) c = vFg;
  outColor = vec4(c, 1.0);
}`;

function compile(gl: WebGL2RenderingContext, type: number, src: string) {
  const s = gl.createShader(type)!;
  gl.shaderSource(s, src);
  gl.compileShader(s);
  if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s) ?? "shader");
  return s;
}

export class WebGLRenderer implements Renderer {
  private gl: WebGL2RenderingContext;
  private prog: WebGLProgram;
  private vao: WebGLVertexArrayObject;
  private buf: WebGLBuffer;
  private atlasTex: WebGLTexture;
  private uniforms: Record<string, WebGLUniformLocation | null> = {};

  private m: Metrics = { cellW: 8, cellH: 17, dpr: 1, width: 0, height: 0 };
  private cw = 8; // device px
  private ch = 17;
  private atlasCols = 1;
  private atlasSlots = 1;
  private nextSlot = 1; // slot 0 stays blank
  private slots = new Map<string, number>();
  private scratch = document.createElement("canvas");
  private sctx: CanvasRenderingContext2D;

  private words = new ArrayBuffer(0);
  private f32 = new Float32Array(0);
  private u32 = new Uint32Array(0);
  private capacity = 0;

  constructor(private canvas: HTMLCanvasElement) {
    const gl = canvas.getContext("webgl2", { alpha: false, antialias: false, powerPreference: "high-performance" });
    if (!gl) throw new Error("WebGL2 is not available");
    this.gl = gl;

    const prog = gl.createProgram()!;
    gl.attachShader(prog, compile(gl, gl.VERTEX_SHADER, VERT));
    gl.attachShader(prog, compile(gl, gl.FRAGMENT_SHADER, FRAG));
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog) ?? "link");
    this.prog = prog;
    for (const n of ["uCell", "uViewport", "uSlot", "uAtlasCols", "uAtlas"]) this.uniforms[n] = gl.getUniformLocation(prog, n);

    this.vao = gl.createVertexArray()!;
    this.buf = gl.createBuffer()!;
    gl.bindVertexArray(this.vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buf);
    const stride = WORDS * 4;
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, stride, 0);
    gl.vertexAttribDivisor(0, 1);
    for (let i = 1; i <= 4; i++) {
      gl.enableVertexAttribArray(i);
      gl.vertexAttribIPointer(i, 1, gl.UNSIGNED_INT, stride, (i + 1) * 4);
      gl.vertexAttribDivisor(i, 1);
    }

    this.atlasTex = gl.createTexture()!;
    gl.bindTexture(gl.TEXTURE_2D, this.atlasTex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, ATLAS, ATLAS, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);

    this.sctx = this.scratch.getContext("2d", { willReadFrequently: false })!;
  }

  configure(m: Metrics) {
    this.m = m;
    this.cw = Math.max(1, Math.round(m.cellW * m.dpr));
    this.ch = Math.max(1, Math.round(m.cellH * m.dpr));
    this.atlasCols = Math.floor(ATLAS / this.cw);
    this.atlasSlots = this.atlasCols * Math.floor(ATLAS / this.ch);
    this.slots.clear();
    this.nextSlot = 1;
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.atlasTex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, ATLAS, ATLAS, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
  }

  /** The atlas slot for a glyph, rasterising it on first use. Wide glyphs take two adjacent slots. */
  private slot(ch: string, style: number, wide: boolean): number {
    const key = `${style}:${wide ? "w" : "n"}:${ch}`;
    const hit = this.slots.get(key);
    if (hit !== undefined) return hit;

    const need = wide ? 2 : 1;
    if (wide && this.nextSlot % this.atlasCols === this.atlasCols - 1) this.nextSlot++; // keep both halves in one row
    if (this.nextSlot + need > this.atlasSlots) {
      // Atlas full: start over. Cheap and rare; the next frame re-rasterises what it needs.
      this.slots.clear();
      this.nextSlot = 1;
    }
    const slot = this.nextSlot;
    this.nextSlot += need;

    const w = this.cw * need;
    this.scratch.width = w;
    this.scratch.height = this.ch;
    const c = this.sctx;
    c.clearRect(0, 0, w, this.ch);
    c.font = font(style, Math.round(FONT_PX * this.m.dpr));
    c.textBaseline = "alphabetic";
    c.fillStyle = "#fff";
    c.fillText(ch, 0, Math.round(this.ch * 0.78));

    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.atlasTex);
    gl.texSubImage2D(
      gl.TEXTURE_2D, 0,
      (slot % this.atlasCols) * this.cw, Math.floor(slot / this.atlasCols) * this.ch,
      gl.RGBA, gl.UNSIGNED_BYTE, this.scratch,
    );
    this.slots.set(key, slot);
    return slot;
  }

  begin(bg: number) {
    const gl = this.gl;
    gl.disable(gl.SCISSOR_TEST);
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.clearColor(((bg >> 16) & 255) / 255, ((bg >> 8) & 255) / 255, (bg & 255) / 255, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
  }

  drawPane(f: Frame, r: PxRect) {
    const gl = this.gl;
    // The pane's rectangle in device pixels; the origin of a GL viewport is the bottom-left corner.
    const dpr = this.m.dpr;
    const vx = Math.round(r.x * dpr), vw = Math.round(r.w * dpr), vh = Math.round(r.h * dpr);
    const vy = this.canvas.height - Math.round(r.y * dpr) - vh;
    gl.viewport(vx, vy, vw, vh);
    gl.enable(gl.SCISSOR_TEST);
    gl.scissor(vx, vy, vw, vh);
    // The grid covers whole cells only; the rest of the rectangle is the pane's background.
    gl.clearColor(((f.bg >> 16) & 255) / 255, ((f.bg >> 8) & 255) / 255, (f.bg & 255) / 255, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);

    // A frame whose size is not a sane grid draws as its background only (the buffers are sized from it).
    const cells = gridCells(f);
    if (cells === null) return;
    if (cells > this.capacity) {
      this.capacity = cells;
      this.words = new ArrayBuffer(cells * WORDS * 4);
      this.f32 = new Float32Array(this.words);
      this.u32 = new Uint32Array(this.words);
    }
    this.build(f, visibleCursor(f));

    gl.useProgram(this.prog);
    gl.uniform2f(this.uniforms.uCell!, this.cw, this.ch);
    gl.uniform2f(this.uniforms.uViewport!, vw, vh);
    gl.uniform2f(this.uniforms.uSlot!, this.cw, this.ch);
    gl.uniform1ui(this.uniforms.uAtlasCols!, this.atlasCols);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.atlasTex);
    gl.uniform1i(this.uniforms.uAtlas!, 0);

    gl.bindVertexArray(this.vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buf);
    // One instance per cell, in row-major order, so the instance of cell (x, y) is y * cols + x; never more
    // than the buffer holds.
    const n = Math.min(cells, this.capacity);
    gl.bufferData(gl.ARRAY_BUFFER, this.f32.subarray(0, n * WORDS), gl.STREAM_DRAW);
    gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, n);
  }

  /**
   * Fill the instance array: exactly one instance per cell. A run's text has one code point per cell, so the
   * column advance is one per code point and the cursor is a change to the instance of its cell, never an
   * extra instance.
   */
  private build(f: Frame, cursor: CursorShape | null) {
    const { f32, u32 } = this;
    const { cols, rows } = f;
    const put = (c: number, y: number, slot: number, fg: number, bg: number, flags: number) => {
      const o = (y * cols + c) * WORDS;
      f32[o] = c;
      f32[o + 1] = y;
      u32[o + 2] = slot;
      u32[o + 3] = fg;
      u32[o + 4] = bg;
      u32[o + 5] = flags;
    };
    // Every cell starts as a blank in the default colours; the runs overwrite what they cover.
    for (let y = 0; y < rows; y++) for (let c = 0; c < cols; c++) put(c, y, 0, f.fg, f.bg, 0);

    // What the cursor's cell turned out to be: the first of a wide pair, or the spacer of one.
    let cursorWide = false, cursorSpacer = false;
    const lines = Math.min(rows, f.lines.length);
    for (let y = 0; y < lines; y++) {
      const line = f.lines[y]!;
      let col = 0;
      let wideSlot = -1; // the slot of the wide glyph whose spacer is expected at `col`
      for (let ri = 0; ri < line.length; ri++) {
        const [text, runCols, fg0, bg0, flags] = line[ri]!;
        let fg = fg0, bg = bg0;
        if (flags & INVERSE) [fg, bg] = [bg, fg];
        const style = flags & (BOLD | ITALIC);
        const underline = flags & UNDERLINE;
        const chars = Array.from(text);
        const end = Math.min(cols, col + runCols);
        const next = line[ri + 1]?.[0];
        for (let i = 0, c = col; c < end; i++, c++) {
          const ch = chars[i];
          if (ch === undefined) {
            put(c, y, 0, fg, bg, underline); // the run claims more cells than its text has: blank, run colours
            continue;
          }
          if (ch === "\0") {
            // The second cell of a wide character: the glyph's second slot (or blank when nothing precedes it).
            put(c, y, wideSlot >= 0 ? wideSlot + 1 : 0, fg, bg, underline);
            if (cursor && y === cursor.y && c === cursor.x) cursorSpacer = true;
            wideSlot = -1;
            continue;
          }
          const wide = chars[i + 1] === "\0" || (i === chars.length - 1 && next !== undefined && next.startsWith("\0"));
          const slot = ch === " " ? 0 : this.slot(ch, style, wide);
          put(c, y, slot, fg, bg, underline);
          wideSlot = wide ? slot : -1;
          if (wide && cursor && y === cursor.y && c === cursor.x) cursorWide = true;
        }
        col = end;
      }
    }

    if (cursor) {
      // Block: the cell takes the cursor colour and its glyph the pane background, so the character stays
      // readable. A wide character is covered as a whole, from either of its halves.
      const first = cursorSpacer ? cursor.x - 1 : cursor.x;
      const span = cursorWide || cursorSpacer ? 2 : 1;
      for (let k = 0; k < span; k++) {
        const o = (cursor.y * cols + first + k) * WORDS;
        u32[o + 3] = f.bg;
        u32[o + 4] = CURSOR_COLOR;
      }
    }
  }

  sync() {
    const px = new Uint8Array(4);
    this.gl.readPixels(0, 0, 1, 1, this.gl.RGBA, this.gl.UNSIGNED_BYTE, px);
  }
}
