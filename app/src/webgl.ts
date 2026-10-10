import {
  BOLD, CURSOR_COLOR, CursorShape, FONT_PX, Frame, INVERSE, ITALIC, Metrics, PxRect, Renderer, UNDERLINE, font, gridCells,
  rowExtras, visibleCursor,
} from "./frame.js";

// WebGL2 renderer: one instanced draw per frame. Every cell is an instance that carries its
// glyph slot in a texture atlas plus foreground and background colour; the fragment shader mixes them.
//
// Three rules keep a frame right:
//  - exactly one instance per cell, row-major, so the buffer always fits and the cursor is a change to the
//    instance of its cell, never an addition;
//  - the atlas is never reset in the middle of building a pane (a reset invalidates the slots already written
//    into the instance array); an overflow aborts the build, resets once and builds the pane again;
//  - a run's text has one code point per cell, the rest of a grapheme cluster comes in `Frame.extras`, so the
//    column advance is one per code point and a cluster can never shift the cells after it.

const ATLAS = 2048;
const WORDS = 6; // col, row (f32) and slot, fg, bg, flags (u32) per instance

// Instance flag bits. Bit 4 is the text underline; the others are the parts of a bar, underline or hollow cursor.
const F_UNDERLINE = 4;
const F_CUR_BAR = 8; // left edge, CUR_THICK device px wide
const F_CUR_UNDER = 16; // bottom edge, CUR_THICK device px high
const F_CUR_TOP = 32; // 1 device px lines of the hollow outline
const F_CUR_BOTTOM = 64;
const F_CUR_LEFT = 128;
const F_CUR_RIGHT = 256;
const CUR_THICK = 2;

const VERT = `#version 300 es
layout(location = 0) in vec2 aCell;
layout(location = 1) in uint aSlot;
layout(location = 2) in uint aFg;
layout(location = 3) in uint aBg;
layout(location = 4) in uint aFlags;
uniform vec2 uCell;      // cell size in device px
uniform vec2 uViewport;  // canvas size in device px
uniform vec2 uSlot;      // atlas slot size in px
uniform float uAtlasPx;  // atlas edge in px
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
  vUV = (origin + corner * uCell) / uAtlasPx;
  vFg = rgb(aFg);
  vBg = rgb(aBg);
  vCorner = corner;
  vFlags = aFlags;
}`;

const FRAG = `#version 300 es
precision highp float;
uniform sampler2D uAtlas;
uniform vec2 uCell;
uniform vec3 uCursor;
in vec2 vUV;
in vec3 vFg;
in vec3 vBg;
in vec2 vCorner;
flat in uint vFlags;
out vec4 outColor;
bool has(uint bit) { return (vFlags & bit) != 0u; }
void main() {
  float a = texture(uAtlas, vUV).a;
  vec3 c = mix(vBg, vFg, a);
  vec2 p = vCorner * uCell; // position inside the cell in device px; pixel centres are at n + 0.5
  if (has(${F_UNDERLINE}u) && p.y > uCell.y - 2.0) c = vFg;
  // Cursor shapes other than the block, which swaps the cell's colours on the CPU.
  if ((has(${F_CUR_BAR}u) && p.x < ${CUR_THICK}.0)
      || (has(${F_CUR_UNDER}u) && p.y > uCell.y - ${CUR_THICK}.0)
      || (has(${F_CUR_TOP}u) && p.y < 1.0)
      || (has(${F_CUR_BOTTOM}u) && p.y > uCell.y - 1.0)
      || (has(${F_CUR_LEFT}u) && p.x < 1.0)
      || (has(${F_CUR_RIGHT}u) && p.x > uCell.x - 1.0)) c = uCursor;
  outColor = vec4(c, 1.0);
}`;

function compile(gl: WebGL2RenderingContext, type: number, src: string) {
  const s = gl.createShader(type)!;
  gl.shaderSource(s, src);
  gl.compileShader(s);
  if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s) ?? "shader");
  return s;
}

export interface WebGLOptions {
  /** Edge of the glyph atlas in px (default 2048, at most the GPU's limit). Tests use a tiny one to force resets. */
  atlasSize?: number;
}

/** A glyph that has a slot but is not in the texture yet. */
interface Pending {
  slot: number;
  text: string;
  style: number;
  cells: number;
}

export class WebGLRenderer implements Renderer {
  private gl: WebGL2RenderingContext;
  private prog: WebGLProgram;
  private vao: WebGLVertexArrayObject;
  private buf: WebGLBuffer;
  private atlasTex: WebGLTexture;
  private atlasPx: number;
  private uniforms: Record<string, WebGLUniformLocation | null> = {};

  private m: Metrics = { cellW: 8, cellH: 17, dpr: 1, width: 0, height: 0 };
  private cw = 8; // device px
  private ch = 17;
  private atlasCols = 1;
  private atlasSlots = 1;
  private nextSlot = 1; // slot 0 stays blank
  private slots = new Map<string, number>();
  private pending: Pending[] = [];
  private strip = document.createElement("canvas");
  private sctx: CanvasRenderingContext2D;

  private words = new ArrayBuffer(0);
  private f32 = new Float32Array(0);
  private u32 = new Uint32Array(0);
  private capacity = 0;

  constructor(private canvas: HTMLCanvasElement, options: WebGLOptions = {}) {
    const gl = canvas.getContext("webgl2", { alpha: false, antialias: false, powerPreference: "high-performance" });
    if (!gl) throw new Error("WebGL2 is not available");
    this.gl = gl;
    const limit = Number(gl.getParameter(gl.MAX_TEXTURE_SIZE)) || ATLAS;
    const wanted = Math.floor(options.atlasSize ?? ATLAS);
    this.atlasPx = Math.max(1, Math.min(limit, Number.isFinite(wanted) ? wanted : ATLAS));

    const prog = gl.createProgram()!;
    gl.attachShader(prog, compile(gl, gl.VERTEX_SHADER, VERT));
    gl.attachShader(prog, compile(gl, gl.FRAGMENT_SHADER, FRAG));
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog) ?? "link");
    this.prog = prog;
    for (const n of ["uCell", "uViewport", "uSlot", "uAtlasPx", "uAtlasCols", "uAtlas", "uCursor"]) {
      this.uniforms[n] = gl.getUniformLocation(prog, n);
    }

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
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, this.atlasPx, this.atlasPx, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);

    this.sctx = this.strip.getContext("2d", { willReadFrequently: false })!;
  }

  configure(m: Metrics) {
    this.m = m;
    this.cw = Math.max(1, Math.round(m.cellW * m.dpr));
    this.ch = Math.max(1, Math.round(m.cellH * m.dpr));
    this.atlasCols = Math.floor(this.atlasPx / this.cw);
    this.atlasSlots = this.atlasCols * Math.floor(this.atlasPx / this.ch);
    this.slots.clear();
    this.pending.length = 0;
    this.nextSlot = 1;
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.atlasTex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, this.atlasPx, this.atlasPx, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
  }

  /**
   * The atlas slot for a glyph, or -1 when the atlas is full. A new glyph is only given its slot here; its pixels
   * are rasterised and uploaded together with the frame's other new glyphs by `flushGlyphs`.
   * Wide glyphs take two adjacent slots.
   */
  private slot(text: string, style: number, wide: boolean): number {
    const key = `${style}:${wide ? "w" : "n"}:${text}`;
    const hit = this.slots.get(key);
    if (hit !== undefined) return hit;

    const need = wide ? 2 : 1;
    if (this.atlasCols < need) return -1;
    if (wide && this.nextSlot % this.atlasCols === this.atlasCols - 1) this.nextSlot++; // keep both halves in one row
    if (this.nextSlot + need > this.atlasSlots) return -1;
    const slot = this.nextSlot;
    this.nextSlot += need;
    this.slots.set(key, slot);
    this.pending.push({ slot, text, style, cells: need });
    return slot;
  }

  /**
   * Rasterise the glyphs that got a slot since the last call and upload them. Slots are handed out in order, so
   * the glyphs of one atlas row are a contiguous run: one strip canvas and one upload per row instead of one
   * per glyph (a cold 160x50 screen is thousands of glyphs; per-glyph uploads cost 0.1 to 0.4 ms each).
   */
  private flushGlyphs() {
    const list = this.pending;
    if (list.length === 0) return;
    const { gl, sctx: c, strip, cw, ch, atlasCols } = this;
    const px = Math.round(FONT_PX * this.m.dpr);
    const baseline = Math.round(ch * 0.78);
    gl.bindTexture(gl.TEXTURE_2D, this.atlasTex);
    for (let i = 0; i < list.length; ) {
      const row = Math.floor(list[i]!.slot / atlasCols);
      let j = i;
      while (j + 1 < list.length && Math.floor(list[j + 1]!.slot / atlasCols) === row) j++;
      const first = list[i]!, last = list[j]!;
      const x0 = (first.slot % atlasCols) * cw;
      const x1 = ((last.slot % atlasCols) + last.cells) * cw;
      strip.width = x1 - x0; // resizing clears the strip
      strip.height = ch;
      c.textBaseline = "alphabetic";
      c.fillStyle = "#fff";
      let lastStyle = -1;
      for (let k = i; k <= j; k++) {
        const g = list[k]!;
        if (g.style !== lastStyle) {
          c.font = font(g.style, px);
          lastStyle = g.style;
        }
        const x = (g.slot % atlasCols) * cw - x0;
        // A glyph may not paint into its neighbour's slot (overhang of italics, wide fallback glyphs).
        c.save();
        c.beginPath();
        c.rect(x, 0, g.cells * cw, ch);
        c.clip();
        c.fillText(g.text, x, baseline);
        c.restore();
      }
      gl.texSubImage2D(gl.TEXTURE_2D, 0, x0, row * ch, gl.RGBA, gl.UNSIGNED_BYTE, strip);
      i = j + 1;
    }
    list.length = 0;
  }

  /**
   * Forget every glyph. Only between two builds of a pane: the slots a half-built instance array refers to are
   * invalid afterwards. The texture is not cleared: a slot is fully overwritten when its glyph is rasterised, and
   * slot 0 (the blank one) is never written.
   */
  private resetAtlas() {
    this.slots.clear();
    this.pending.length = 0;
    this.nextSlot = 1;
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

    const cursor = visibleCursor(f);
    if (!this.build(f, cursor, true)) {
      // The atlas filled up in the middle of the pane. Reset it once and build the pane again from scratch: a
      // pane has at most cols * rows distinct glyphs, so it normally fits; what does not is drawn blank.
      this.resetAtlas();
      this.build(f, cursor, false);
    }
    this.flushGlyphs();

    gl.useProgram(this.prog);
    gl.uniform2f(this.uniforms.uCell!, this.cw, this.ch);
    gl.uniform2f(this.uniforms.uViewport!, vw, vh);
    gl.uniform2f(this.uniforms.uSlot!, this.cw, this.ch);
    gl.uniform1f(this.uniforms.uAtlasPx!, this.atlasPx);
    gl.uniform1ui(this.uniforms.uAtlasCols!, this.atlasCols);
    gl.uniform3f(
      this.uniforms.uCursor!,
      ((CURSOR_COLOR >> 16) & 255) / 255, ((CURSOR_COLOR >> 8) & 255) / 255, (CURSOR_COLOR & 255) / 255,
    );
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
   * Fill the instance array: exactly one instance per cell. Returns false when the atlas is full and
   * `mayAbort` is set (the caller resets the atlas and builds again); without it a glyph that finds no slot is
   * drawn blank and the build always completes.
   */
  private build(f: Frame, cursor: CursorShape | null, mayAbort: boolean): boolean {
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
      const extras = rowExtras(f, y);
      let col = 0;
      let wideSlot = -1; // the slot of the wide glyph whose spacer is expected next
      for (let ri = 0; ri < line.length; ri++) {
        const [text, runCols, fg0, bg0, flags] = line[ri]!;
        let fg = fg0, bg = bg0;
        if (flags & INVERSE) [fg, bg] = [bg, fg];
        const style = flags & (BOLD | ITALIC);
        const underline = flags & UNDERLINE ? F_UNDERLINE : 0;
        const chars = Array.from(text);
        const end = Math.min(cols, col + runCols);
        const next = line[ri + 1]?.[0];
        for (let i = 0, c = col; c < end; i++, c++) {
          const ch = chars[i];
          if (ch === undefined) {
            put(c, y, 0, fg, bg, underline); // the run claims more cells than its text has: blank, run colours
            wideSlot = -1;
            continue;
          }
          if (ch === "\0") {
            // The second cell of a wide character: the glyph's second slot (blank when nothing precedes it).
            put(c, y, wideSlot >= 0 ? wideSlot + 1 : 0, fg, bg, underline);
            if (cursor && y === cursor.y && c === cursor.x) cursorSpacer = true;
            wideSlot = -1;
            continue;
          }
          const wide = chars[i + 1] === "\0" || (i === chars.length - 1 && next !== undefined && next.startsWith("\0"));
          const tail = extras?.get(c);
          let slot = 0;
          if (ch !== " " || tail !== undefined) {
            slot = this.slot(tail === undefined ? ch : ch + tail, style, wide);
            if (slot < 0) {
              if (mayAbort) return false;
              slot = 0;
            }
          }
          put(c, y, slot, fg, bg, underline);
          wideSlot = wide && slot > 0 ? slot : -1;
          if (wide && cursor && y === cursor.y && c === cursor.x) cursorWide = true;
        }
        col = end;
      }
    }

    if (cursor) {
      // A wide character is covered as a whole, from either of its halves.
      const first = cursorSpacer ? cursor.x - 1 : cursor.x;
      const span = cursorWide || cursorSpacer ? 2 : 1;
      for (let k = 0; k < span; k++) {
        const o = (cursor.y * cols + first + k) * WORDS;
        switch (cursor.style) {
          case "block":
            // The cell takes the cursor colour and its glyph the pane background, so the character stays readable.
            u32[o + 3] = f.bg;
            u32[o + 4] = CURSOR_COLOR;
            break;
          case "bar":
            if (k === 0) u32[o + 5] = u32[o + 5]! | F_CUR_BAR;
            break;
          case "underline":
            u32[o + 5] = u32[o + 5]! | F_CUR_UNDER;
            break;
          case "hollow":
            u32[o + 5] = u32[o + 5]! | F_CUR_TOP | F_CUR_BOTTOM | (k === 0 ? F_CUR_LEFT : 0) | (k === span - 1 ? F_CUR_RIGHT : 0);
            break;
        }
      }
    }
    return true;
  }

  sync() {
    const px = new Uint8Array(4);
    this.gl.readPixels(0, 0, 1, 1, this.gl.RGBA, this.gl.UNSIGNED_BYTE, px);
  }
}
