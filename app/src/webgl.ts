import {
  BOLD, CURSOR_COLOR, CursorShape, Frame, GhostCell, INVERSE, ITALIC, ImageDraw, Metrics, PaneFx, PxRect, Renderer, UNDERLINE, font,
  getFontPx, gridCells, rowExtras, visibleCursor,
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
//
// The effects of DESK-28 add to that and never change it: a scroll slide is one more uniform (`uOffsetY`, the grid
// drawn lower by whole device pixels, clipped to the grid's own box), the frames under a slide are the same one-
// instance-per-cell draw with their own offset, and the cursor's ghosts are separate one-instance draws that blend
// over the finished grid. A draw without `PaneFx` is exactly what it was.
//
// Pictures (Kitty graphics, docs/design.md 6.31) come in three layers: below the cell backgrounds, between them and the
// text, above the text. A frame without a picture, or with pictures only above the text, keeps the one instanced draw.
// A frame with a picture in one of the lower layers draws its grid in two steps from the same instance buffer (`uPass`
// 1: the backgrounds that are not the pane colour; 2: the glyphs and underlines, blended), with the pictures between the
// steps. The cursor never goes through the grid in such a frame: it is drawn last, over every picture, as the canvas
// renderer does. A picture is a texture of its own, made from its `ImageBitmap` the first time it is drawn and deleted
// when the bitmap was closed, was not drawn for IDLE_DRAWS draws, or the renderer lets go of it.

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

/** A picture's texture is deleted when this many draws (`begin` calls) have passed without it being drawn. */
const IDLE_DRAWS = 120;

const VERT = `#version 300 es
layout(location = 0) in vec2 aCell;
layout(location = 1) in uint aSlot;
layout(location = 2) in uint aFg;
layout(location = 3) in uint aBg;
layout(location = 4) in uint aFlags;
uniform vec2 uCell;      // cell size in device px
uniform vec2 uViewport;  // canvas size in device px
uniform float uOffsetY;  // the grid is drawn this many device px lower (scroll slide), 0 otherwise
uniform vec2 uSlot;      // atlas slot size in px
uniform float uAtlasPx;  // atlas edge in px
uniform uint uAtlasCols;
out vec2 vUV;
out vec3 vFg;
out vec3 vBg;
out vec2 vCorner;
flat out uint vFlags;
flat out uint vBgRaw;
vec3 rgb(uint c) { return vec3(float((c >> 16u) & 255u), float((c >> 8u) & 255u), float(c & 255u)) / 255.0; }
void main() {
  vec2 corner = vec2(float(gl_VertexID & 1), float(gl_VertexID >> 1));
  vec2 px = (aCell + corner) * uCell + vec2(0.0, uOffsetY);
  gl_Position = vec4(px.x / uViewport.x * 2.0 - 1.0, 1.0 - px.y / uViewport.y * 2.0, 0.0, 1.0);
  vec2 origin = vec2(float(aSlot % uAtlasCols), float(aSlot / uAtlasCols)) * uSlot;
  vUV = (origin + corner * uCell) / uAtlasPx;
  vFg = rgb(aFg);
  vBg = rgb(aBg);
  vCorner = corner;
  vFlags = aFlags;
  vBgRaw = aBg;
}`;

const FRAG = `#version 300 es
precision highp float;
uniform sampler2D uAtlas;
uniform vec2 uCell;
uniform vec3 uCursor;
uniform float uAlpha;    // 1, except for a cursor ghost, which is blended
uniform uint uPass;      // 0 the whole cell, 1 backgrounds only, 2 glyphs and underlines only, 3 cursor shapes only
uniform uint uDefaultBg; // the frame's own background, packed like the instances: pass 1 leaves those cells bare
in vec2 vUV;
in vec3 vFg;
in vec3 vBg;
in vec2 vCorner;
flat in uint vFlags;
flat in uint vBgRaw;
out vec4 outColor;
bool has(uint bit) { return (vFlags & bit) != 0u; }
void main() {
  vec2 p = vCorner * uCell; // position inside the cell in device px; pixel centres are at n + 0.5
  bool underline = has(${F_UNDERLINE}u) && p.y > uCell.y - 2.0;
  // Cursor shapes other than the block, which swaps the cell's colours on the CPU.
  bool shape = (has(${F_CUR_BAR}u) && p.x < ${CUR_THICK}.0)
      || (has(${F_CUR_UNDER}u) && p.y > uCell.y - ${CUR_THICK}.0)
      || (has(${F_CUR_TOP}u) && p.y < 1.0)
      || (has(${F_CUR_BOTTOM}u) && p.y > uCell.y - 1.0)
      || (has(${F_CUR_LEFT}u) && p.x < 1.0)
      || (has(${F_CUR_RIGHT}u) && p.x > uCell.x - 1.0);
  if (uPass == 1u) {
    if (vBgRaw == uDefaultBg) discard;
    outColor = vec4(vBg, 1.0);
    return;
  }
  if (uPass == 3u) {
    if (!shape) discard;
    outColor = vec4(uCursor, 1.0);
    return;
  }
  float a = texture(uAtlas, vUV).a;
  if (uPass == 2u) {
    float cover = underline ? 1.0 : a;
    if (cover <= 0.0) discard;
    outColor = vec4(vFg, cover);
    return;
  }
  vec3 c = mix(vBg, vFg, a);
  if (underline) c = vFg;
  if (shape) c = uCursor;
  outColor = vec4(c, uAlpha);
}`;

// The picture program: one rectangle per draw from uniforms (no buffer). `uDst` is the rectangle in device px from the
// pane's top-left corner, `uSrc` the part of the texture in 0..1 and `uClamp` that part shrunk by half a texel, so
// the linear filter never reads a texel outside the source rectangle. The texture holds the pixels as the browser
// keeps an `ImageBitmap` (premultiplied alpha, the first row at the top), so the output is premultiplied too.
const PIC_VERT = `#version 300 es
uniform vec2 uViewport;
uniform vec4 uDst;
uniform vec4 uSrc;
out vec2 vUV;
void main() {
  vec2 corner = vec2(float(gl_VertexID & 1), float(gl_VertexID >> 1));
  vec2 px = mix(uDst.xy, uDst.zw, corner);
  gl_Position = vec4(px.x / uViewport.x * 2.0 - 1.0, 1.0 - px.y / uViewport.y * 2.0, 0.0, 1.0);
  vUV = mix(uSrc.xy, uSrc.zw, corner);
}`;

const PIC_FRAG = `#version 300 es
precision highp float;
uniform sampler2D uImage;
uniform vec4 uClamp;
in vec2 vUV;
out vec4 outColor;
void main() {
  outColor = texture(uImage, clamp(vUV, uClamp.xy, uClamp.zw));
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
  /** The GPU context was lost and is back and the renderer rebuilt its resources: the caller draws again. */
  onRestored?: () => void;
}

/** A glyph that has a slot but is not in the texture yet. */
interface Pending {
  slot: number;
  text: string;
  style: number;
  cells: number;
}

/** A picture ready to draw: the layer, the bitmap and its rectangles (device px from the pane's corner, and 0..1). */
interface PicDraw {
  layer: number;
  bitmap: ImageBitmap;
  x0: number;
  y0: number;
  x1: number;
  y1: number;
  u0: number;
  v0: number;
  u1: number;
  v1: number;
  cu0: number;
  cv0: number;
  cu1: number;
  cv1: number;
}

/** The pictures of one frame that can be drawn, and which layers have one. */
interface PicSet {
  list: PicDraw[];
  has: [boolean, boolean, boolean];
}

/** A picture's texture, whose draw it was last used in, and what it holds in memory. */
interface PicTex {
  tex: WebGLTexture;
  bytes: number;
  used: number;
}

export class WebGLRenderer implements Renderer {
  private gl: WebGL2RenderingContext;
  // The GPU objects: all of them are gone with a lost context and are made again by `setup`.
  private prog!: WebGLProgram;
  private vao!: WebGLVertexArrayObject;
  private buf!: WebGLBuffer;
  private atlasTex!: WebGLTexture;
  private atlasPx: number;
  private uniforms: Record<string, WebGLUniformLocation | null> = {};
  /** The context is lost (a GPU reset, sleep and wake, a GPU switch): nothing is drawn until it is back. */
  private lost = false;

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
  private cursorColor = CURSOR_COLOR;
  // The cursor's instances of a frame with pictures, kept out of the instance buffer so they can be drawn last.
  private curWords = new ArrayBuffer(2 * WORDS * 4);
  private curF32 = new Float32Array(this.curWords);
  private curU32 = new Uint32Array(this.curWords);
  private curCount = 0;
  private curBlock = false;

  // Pictures: the program (made the first time one is drawn), one texture per bitmap, and the draw counter.
  private picProg: WebGLProgram | null = null;
  private picUniforms: Record<string, WebGLUniformLocation | null> = {};
  private picBroken = false;
  private pictures = new Map<ImageBitmap, PicTex>();
  private picBytes = 0;
  private maxTexture = 2048;
  private stamp = 0;

  constructor(private canvas: HTMLCanvasElement, private options: WebGLOptions = {}) {
    const gl = canvas.getContext("webgl2", { alpha: false, antialias: false, powerPreference: "high-performance" });
    if (!gl) throw new Error("WebGL2 is not available");
    this.gl = gl;
    const limit = Number(gl.getParameter(gl.MAX_TEXTURE_SIZE)) || ATLAS;
    this.maxTexture = limit;
    const wanted = Math.floor(options.atlasSize ?? ATLAS);
    this.atlasPx = Math.max(1, Math.min(limit, Number.isFinite(wanted) ? wanted : ATLAS));
    this.setup();
    this.sctx = this.strip.getContext("2d", { willReadFrequently: false })!;
    // Without `preventDefault` on the loss the browser never restores the context.
    canvas.addEventListener("webglcontextlost", (e) => {
      e.preventDefault();
      this.lost = true;
      this.dropPictures(false);
    });
    canvas.addEventListener("webglcontextrestored", () => this.restore());
  }

  /** Build the GPU objects: the program, the instance buffer and the atlas texture. Throws when they cannot be. */
  private setup() {
    const gl = this.gl;
    const prog = gl.createProgram()!;
    gl.attachShader(prog, compile(gl, gl.VERTEX_SHADER, VERT));
    gl.attachShader(prog, compile(gl, gl.FRAGMENT_SHADER, FRAG));
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog) ?? "link");
    this.prog = prog;
    for (const n of [
      "uCell", "uViewport", "uSlot", "uAtlasPx", "uAtlasCols", "uAtlas", "uCursor", "uOffsetY", "uAlpha", "uPass", "uDefaultBg",
    ]) {
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
  }

  /** Whether the GPU context is gone: drawing then is a no-op, and so is every call that would reach the GPU. */
  private get dead(): boolean {
    return this.lost || this.gl.isContextLost();
  }

  /** The context is back: every GPU object is new, so make them again, with the glyph atlas empty. */
  private restore() {
    this.dropPictures(false); // every object of the old context is gone with it
    try {
      this.setup();
    } catch (e) {
      // The same shaders compiled before; if they do not now the context is not usable and nothing is drawn. A
      // Canvas 2D fallback is not possible here: a canvas that had a WebGL context never gives out a 2D one, and
      // the page's listeners are on this element.
      console.error("holzwave: the WebGL context came back unusable", e);
      return;
    }
    this.lost = false;
    this.configure(this.m);
    this.options.onRestored?.();
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
    if (this.dead) return; // `restore` configures again with these metrics
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.atlasTex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, this.atlasPx, this.atlasPx, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
  }

  setCursorColor(rgb: number) {
    this.cursorColor = rgb & 0xffffff;
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
    const px = Math.round(getFontPx() * this.m.dpr);
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
    this.stamp++;
    if (this.dead) return;
    const gl = this.gl;
    gl.disable(gl.SCISSOR_TEST);
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.clearColor(((bg >> 16) & 255) / 255, ((bg >> 8) & 255) / 255, (bg & 255) / 255, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
  }

  drawPane(f: Frame, r: PxRect, fx?: PaneFx) {
    if (this.dead) {
      if (this.pictures.size > 0) this.dropPictures(false);
      return;
    }
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

    // A slide moves the grid by whole device pixels, so its glyphs stay on the pixel grid. What it pushes past the
    // grid's own box is clipped there: the strip of the pane below the last row stays the pane's background.
    const offset = this.devicePx(fx?.offsetY);
    const under = (fx?.under ?? []).filter((u) => u.frame.cols === f.cols && u.frame.rows === f.rows);
    const gh = Math.min(vh, f.rows * this.ch), gw = Math.min(vw, f.cols * this.cw);
    if (offset !== 0 || under.length > 0) gl.scissor(vx, vy + vh - gh, gw, gh);
    for (const u of under) this.drawGrid(u.frame, cells, vw, vh, this.devicePx(u.offsetY), null);

    // Pictures are drawn inside the grid's box only, never over the pane's padding or header. The frames under a slide
    // have none (a picture slides with the frame it belongs to).
    const pics = f.images !== undefined && f.images.length > 0 ? this.preparePictures(f.images, gw, gh, offset) : null;
    if (pics) {
      gl.scissor(vx, vy + vh - gh, gw, gh);
      if (under.length > 0 && (pics.has[0] || pics.has[1])) {
        // The lower layers leave the frame's blank cells bare: cover what the frames under it drew where it is.
        const y0 = Math.max(0, offset), y1 = Math.min(gh, offset + f.rows * this.ch);
        if (y1 > y0) {
          gl.scissor(vx, vy + vh - y1, gw, y1 - y0);
          gl.clear(gl.COLOR_BUFFER_BIT);
          gl.scissor(vx, vy + vh - gh, gw, gh);
        }
      }
    }
    this.drawGrid(f, cells, vw, vh, offset, pics);
    if (pics) gl.scissor(vx, vy, vw, vh);
    if (fx?.ghosts && fx.ghosts.length > 0) this.drawGhosts(fx.ghosts, f);
    if (this.pictures.size > 0) this.purgePictures();
  }

  /** CSS pixels (or nothing) as whole device pixels; anything that is not a finite number is no offset. */
  private devicePx(css: number | undefined): number {
    return css !== undefined && Number.isFinite(css) ? Math.round(css * this.m.dpr) : 0;
  }

  /**
   * The pictures of a frame that can be drawn, with their rectangles in device pixels from the pane's corner (the edges
   * rounded to whole device pixels like the canvas renderer's) and, `offset` lower, the slide. A picture is left out when
   * its bitmap is closed or too large for a texture, a number is not finite, its source rectangle is empty once cut to the
   * bitmap, or its rectangle lies wholly outside the grid box `gw` by `gh`. Null when none is left.
   */
  private preparePictures(images: readonly ImageDraw[], gw: number, gh: number, offset: number): PicSet | null {
    const dpr = this.m.dpr, max = this.maxTexture;
    let set: PicSet | null = null;
    for (const im of images) {
      const bmp = im.bitmap;
      if (im.layer !== 0 && im.layer !== 1 && im.layer !== 2) continue;
      const W = bmp?.width, H = bmp?.height;
      if (!(W > 0) || !(H > 0) || W > max || H > max) continue; // a closed bitmap reads 0
      if (!Number.isFinite(im.dx + im.dy + im.dw + im.dh + im.sx + im.sy + im.sw + im.sh)) continue;
      if (!(im.sw > 0) || !(im.sh > 0)) continue;
      let x0 = Math.round(im.dx * dpr), x1 = Math.round((im.dx + im.dw) * dpr);
      let y0 = Math.round(im.dy * dpr) + offset, y1 = Math.round((im.dy + im.dh) * dpr) + offset;
      // The source is cut to the bitmap; the destination shrinks by the same share.
      const cx0 = Math.max(im.sx, 0), cx1 = Math.min(im.sx + im.sw, W);
      const cy0 = Math.max(im.sy, 0), cy1 = Math.min(im.sy + im.sh, H);
      if (!(cx1 > cx0) || !(cy1 > cy0)) continue;
      if (cx0 !== im.sx || cx1 !== im.sx + im.sw) {
        const k = (x1 - x0) / im.sw;
        x1 -= (im.sx + im.sw - cx1) * k;
        x0 += (cx0 - im.sx) * k;
      }
      if (cy0 !== im.sy || cy1 !== im.sy + im.sh) {
        const k = (y1 - y0) / im.sh;
        y1 -= (im.sy + im.sh - cy1) * k;
        y0 += (cy0 - im.sy) * k;
      }
      if (!(x1 > x0) || !(y1 > y0) || x1 <= 0 || y1 <= 0 || x0 >= gw || y0 >= gh) continue;
      set ??= { list: [], has: [false, false, false] };
      set.has[im.layer] = true;
      set.list.push({
        layer: im.layer, bitmap: bmp, x0, y0, x1, y1,
        u0: cx0 / W, v0: cy0 / H, u1: cx1 / W, v1: cy1 / H,
        cu0: Math.min(cx0 + 0.5, (cx0 + cx1) / 2) / W, cv0: Math.min(cy0 + 0.5, (cy0 + cy1) / 2) / H,
        cu1: Math.max(cx1 - 0.5, (cx0 + cx1) / 2) / W, cv1: Math.max(cy1 - 0.5, (cy0 + cy1) / 2) / H,
      });
    }
    return set;
  }

  /** Make the picture program the first time a picture is drawn. False when it cannot be made: pictures are then off. */
  private picProgram(): boolean {
    if (this.picProg) return true;
    if (this.picBroken) return false;
    const gl = this.gl;
    try {
      const prog = gl.createProgram()!;
      gl.attachShader(prog, compile(gl, gl.VERTEX_SHADER, PIC_VERT));
      gl.attachShader(prog, compile(gl, gl.FRAGMENT_SHADER, PIC_FRAG));
      gl.linkProgram(prog);
      if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog) ?? "link");
      for (const n of ["uViewport", "uDst", "uSrc", "uClamp", "uImage"]) this.picUniforms[n] = gl.getUniformLocation(prog, n);
      this.picProg = prog;
      return true;
    } catch (e) {
      this.picBroken = true;
      console.error("holzwave: the picture program could not be made, pictures are not drawn in WebGL", e);
      return false;
    }
  }

  /** The texture of a bitmap, made and uploaded the first time and marked as used in this draw. Null when it cannot be. */
  private textureOf(bmp: ImageBitmap): WebGLTexture | null {
    const hit = this.pictures.get(bmp);
    if (hit) {
      hit.used = this.stamp;
      return hit.tex;
    }
    const gl = this.gl;
    const tex = gl.createTexture();
    if (!tex) return null;
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    try {
      // The pixel-store flags do not apply to an ImageBitmap: both engines keep it premultiplied and upright.
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, bmp);
    } catch {
      gl.deleteTexture(tex); // the bitmap was closed or detached since it was checked
      return null;
    }
    const bytes = bmp.width * bmp.height * 4;
    this.pictures.set(bmp, { tex, bytes, used: this.stamp });
    this.picBytes += bytes;
    return tex;
  }

  /** Draw the pictures of one layer, in the order of the list, into the viewport and the scissor that are set. */
  private drawPictures(set: PicSet, layer: number, vw: number, vh: number) {
    if (!set.has[layer as 0 | 1 | 2] || !this.picProgram()) return;
    const gl = this.gl;
    gl.useProgram(this.picProg);
    gl.bindVertexArray(null);
    gl.activeTexture(gl.TEXTURE1); // unit 0 keeps the glyph atlas
    gl.uniform1i(this.picUniforms.uImage!, 1);
    gl.uniform2f(this.picUniforms.uViewport!, vw, vh);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA); // the texture is premultiplied
    for (const p of set.list) {
      if (p.layer !== layer) continue;
      if (p.bitmap.width === 0) continue; // closed since the frame was made
      const tex = this.textureOf(p.bitmap);
      if (!tex) continue;
      gl.bindTexture(gl.TEXTURE_2D, tex);
      gl.uniform4f(this.picUniforms.uDst!, p.x0, p.y0, p.x1, p.y1);
      gl.uniform4f(this.picUniforms.uSrc!, p.u0, p.v0, p.u1, p.v1);
      gl.uniform4f(this.picUniforms.uClamp!, p.cu0, p.cv0, p.cu1, p.cv1);
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    }
    gl.disable(gl.BLEND);
    gl.activeTexture(gl.TEXTURE0);
    // Back to the state `drawGrid` and `drawGhosts` rely on.
    gl.useProgram(this.prog);
    gl.bindVertexArray(this.vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buf);
  }

  /** Delete the textures of bitmaps that were closed and of pictures not drawn for `IDLE_DRAWS` draws. */
  private purgePictures() {
    const gl = this.gl;
    for (const [bmp, t] of this.pictures) {
      if (bmp.width !== 0 && this.stamp - t.used < IDLE_DRAWS) continue;
      gl.deleteTexture(t.tex);
      this.picBytes -= t.bytes;
      this.pictures.delete(bmp);
    }
  }

  /** Forget every picture texture; they are deleted only while the context is alive (`free`). The program goes too. */
  private dropPictures(free: boolean) {
    if (free && !this.dead) {
      for (const t of this.pictures.values()) this.gl.deleteTexture(t.tex);
      if (this.picProg) this.gl.deleteProgram(this.picProg);
    }
    this.pictures.clear();
    this.picBytes = 0;
    this.picProg = null;
  }

  /** Let go of everything the pictures hold on the GPU. The renderer stays usable: the next picture makes them again. */
  dispose() {
    this.dropPictures(true);
  }

  /** How many picture textures are alive (the glyph atlas is not one). For tests and diagnostics. */
  get pictureTextures(): number {
    return this.pictures.size;
  }

  /** The memory the picture textures hold, in bytes (width times height times 4 each). For tests and diagnostics. */
  get pictureBytes(): number {
    return this.picBytes;
  }

  /**
   * Build and draw one frame's grid into the viewport that is set, `offset` device pixels lower than its place. With
   * `pics` (a frame that has a picture to draw) the grid goes in the steps the layers need and the cursor comes last.
   */
  private drawGrid(f: Frame, cells: number, vw: number, vh: number, offset: number, pics: PicSet | null) {
    const gl = this.gl;
    if (cells > this.capacity) {
      this.capacity = cells;
      this.words = new ArrayBuffer(cells * WORDS * 4);
      this.f32 = new Float32Array(this.words);
      this.u32 = new Uint32Array(this.words);
    }

    const cursor = visibleCursor(f);
    if (!this.build(f, cursor, true, pics !== null)) {
      // The atlas filled up in the middle of the pane. Reset it once and build the pane again from scratch: a
      // pane has at most cols * rows distinct glyphs, so it normally fits; what does not is drawn blank.
      this.resetAtlas();
      this.build(f, cursor, false, pics !== null);
    }
    this.flushGlyphs();

    gl.useProgram(this.prog);
    gl.uniform2f(this.uniforms.uCell!, this.cw, this.ch);
    gl.uniform2f(this.uniforms.uViewport!, vw, vh);
    gl.uniform2f(this.uniforms.uSlot!, this.cw, this.ch);
    gl.uniform1f(this.uniforms.uAtlasPx!, this.atlasPx);
    gl.uniform1ui(this.uniforms.uAtlasCols!, this.atlasCols);
    gl.uniform1f(this.uniforms.uOffsetY!, offset);
    gl.uniform1f(this.uniforms.uAlpha!, 1);
    gl.uniform1ui(this.uniforms.uPass!, 0);
    gl.uniform1ui(this.uniforms.uDefaultBg!, f.bg >>> 0);
    gl.uniform3f(
      this.uniforms.uCursor!,
      ((this.cursorColor >> 16) & 255) / 255, ((this.cursorColor >> 8) & 255) / 255, (this.cursorColor & 255) / 255,
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
    if (!pics) {
      gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, n);
      return;
    }

    // Layer 0 lies under the cell backgrounds, layer 1 between them and the text, layer 2 over the text. A frame with
    // nothing under the text keeps the one whole-cell draw (the cursor is in a draw of its own either way).
    this.drawPictures(pics, 0, vw, vh);
    if (pics.has[0] || pics.has[1]) {
      gl.uniform1ui(this.uniforms.uPass!, 1);
      gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, n);
      this.drawPictures(pics, 1, vw, vh);
      gl.uniform1ui(this.uniforms.uPass!, 2);
      gl.enable(gl.BLEND);
      gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
      gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, n);
      gl.disable(gl.BLEND);
    } else {
      gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, n);
    }
    this.drawPictures(pics, 2, vw, vh);

    // The cursor over every picture. The block is a whole cell (its colour, its glyph in the pane colour); the other
    // shapes are drawn alone, so the cell under them stays as the layers made it.
    if (this.curCount > 0) {
      gl.uniform1ui(this.uniforms.uPass!, this.curBlock ? 0 : 3);
      gl.bufferData(gl.ARRAY_BUFFER, this.curF32.subarray(0, this.curCount * WORDS), gl.STREAM_DRAW);
      gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, this.curCount);
    }
    gl.uniform1ui(this.uniforms.uPass!, 0);
  }

  /**
   * The cursor's ghosts: one blended block per ghost, in the cursor colour, over the grid that was just drawn. Each is
   * its own one-instance draw (at most six), with the opacity as a uniform; a ghost off the grid is skipped. The
   * program and its uniforms are the ones `drawGrid` left, only the offset and the opacity change.
   */
  private drawGhosts(ghosts: readonly GhostCell[], f: Frame) {
    const gl = this.gl;
    gl.uniform1f(this.uniforms.uOffsetY!, 0);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
    for (const g of ghosts) {
      const { x, y, alpha } = g;
      if (!Number.isInteger(x) || !Number.isInteger(y) || x < 0 || y < 0 || x >= f.cols || y >= f.rows) continue;
      if (!(alpha > 0)) continue;
      this.f32[0] = x;
      this.f32[1] = y;
      this.u32[2] = 0; // the blank slot: the cell is its background colour
      this.u32[3] = this.cursorColor;
      this.u32[4] = this.cursorColor;
      this.u32[5] = 0;
      gl.uniform1f(this.uniforms.uAlpha!, Math.min(1, alpha));
      gl.bufferData(gl.ARRAY_BUFFER, this.f32.subarray(0, WORDS), gl.STREAM_DRAW);
      gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, 1);
    }
    gl.disable(gl.BLEND);
    gl.uniform1f(this.uniforms.uAlpha!, 1);
  }

  /**
   * Fill the instance array: exactly one instance per cell. Returns false when the atlas is full and
   * `mayAbort` is set (the caller resets the atlas and builds again); without it a glyph that finds no slot is
   * drawn blank and the build always completes. With `aside` the cursor's instances are not put into the array but into
   * `curWords` (`curCount` of them), so the caller can draw them after the pictures.
   */
  private build(f: Frame, cursor: CursorShape | null, mayAbort: boolean, aside: boolean): boolean {
    const { f32, u32 } = this;
    this.curCount = 0;
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
      this.curBlock = cursor.style === "block";
      for (let k = 0; k < span; k++) {
        const at = (cursor.y * cols + first + k) * WORDS;
        // Aside, the cell is copied and the copy changed; the array keeps the cell as the layers draw it.
        let o = at;
        let tu = u32;
        if (aside) {
          o = k * WORDS;
          tu = this.curU32;
          for (let w = 0; w < WORDS; w++) tu[o + w] = u32[at + w]!;
          this.curCount = k + 1;
        }
        switch (cursor.style) {
          case "block":
            // The cell takes the cursor colour and its glyph the pane background, so the character stays readable.
            tu[o + 3] = f.bg;
            tu[o + 4] = this.cursorColor;
            break;
          case "bar":
            if (k === 0) tu[o + 5] = tu[o + 5]! | F_CUR_BAR;
            break;
          case "underline":
            tu[o + 5] = tu[o + 5]! | F_CUR_UNDER;
            break;
          case "hollow":
            tu[o + 5] = tu[o + 5]! | F_CUR_TOP | F_CUR_BOTTOM | (k === 0 ? F_CUR_LEFT : 0) | (k === span - 1 ? F_CUR_RIGHT : 0);
            break;
        }
      }
    }
    return true;
  }

  sync() {
    if (this.dead) return;
    const px = new Uint8Array(4);
    this.gl.readPixels(0, 0, 1, 1, this.gl.RGBA, this.gl.UNSIGNED_BYTE, px);
  }
}
