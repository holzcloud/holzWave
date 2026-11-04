import {
  BOLD, CURSOR_COLOR, Frame, INVERSE, ITALIC, ImageDraw, LINK, Metrics, PaneFx, PxRect, Renderer, UNDERLINE, css, dotSpans, font,
  gridCells, linkDot, rowExtras, visibleCursor,
} from "./frame.js";

/** The cursor bar and underline are this many device pixels thick, the hollow outline one. Same as the WebGL shader. */
const CUR_THICK = 2;

/** What the cell (x, y) of a frame holds: its character and style, and the cells a wide character covers. */
function cellAt(f: Frame, y: number, x: number): { ch: string; flags: number; first: number; span: number } {
  let col = 0;
  let prev: string | null = null;
  for (const [text, runCols, , , flags] of f.lines[y] ?? []) {
    const end = col + runCols;
    const chars = Array.from(text);
    if (x < end) {
      const i = x - col;
      const ch = chars[i] ?? " ";
      if (ch === "\0") {
        // The second half of a wide character: the character itself is the cell before.
        const before = i > 0 ? chars[i - 1] : prev;
        return { ch: before ?? " ", flags, first: x - 1, span: 2 };
      }
      const wide = chars[i + 1] === "\0";
      return { ch, flags, first: x, span: wide ? 2 : 1 };
    }
    prev = chars[chars.length - 1] ?? " ";
    col = end;
  }
  return { ch: " ", flags: 0, first: x, span: 1 };
}

/** The simple renderer: one `fillText` per glyph on a 2D canvas. */
export class Canvas2DRenderer implements Renderer {
  private ctx: CanvasRenderingContext2D;
  private m: Metrics = { cellW: 8, cellH: 17, dpr: 1, width: 0, height: 0 };
  private cursorColor = CURSOR_COLOR;
  /** The colour of the dotted line of a link when it is not the cell's own (forced colours), or null. */
  private linkColor: number | null = null;

  constructor(private canvas: HTMLCanvasElement) {
    this.ctx = canvas.getContext("2d", { alpha: false })!;
  }

  configure(m: Metrics) {
    this.m = m;
  }

  setCursorColor(rgb: number) {
    this.cursorColor = rgb & 0xffffff;
  }

  setLinkColor(rgb: number | null) {
    this.linkColor = rgb === null ? null : rgb & 0xffffff;
  }

  /**
   * The dotted line of a link over the cells `col` to `end` (exclusive) of row `y`, in `color` or the colour set for links: dots and
   * gaps of one CSS pixel, the line one CSS pixel high, in the place of the underline. Everything is laid out in whole device pixels
   * counted from the grid's corner, the same numbers the WebGL shader uses, so the two renderers draw the same dots.
   */
  private paintDots(y: number, col: number, end: number, color: number) {
    const { ctx, m } = this;
    const dot = linkDot(m.dpr);
    const cw = Math.max(1, Math.round(m.cellW * m.dpr));
    const ch = Math.max(1, Math.round(m.cellH * m.dpr));
    const top = y * ch + ch - 2 * dot;
    ctx.fillStyle = css(this.linkColor ?? color);
    ctx.beginPath();
    for (const [from, to] of dotSpans(col * cw, end * cw, dot)) ctx.rect(from / m.dpr, top / m.dpr, (to - from) / m.dpr, dot / m.dpr);
    ctx.fill();
  }

  begin(bg: number) {
    const { ctx, m } = this;
    ctx.setTransform(m.dpr, 0, 0, m.dpr, 0, 0);
    ctx.fillStyle = css(bg);
    ctx.fillRect(0, 0, m.width, m.height);
  }

  drawPane(f: Frame, r: PxRect, fx?: PaneFx) {
    const { ctx, m } = this;
    const { cellW, cellH } = m;
    ctx.save();
    ctx.beginPath();
    ctx.rect(r.x, r.y, r.w, r.h);
    ctx.clip();
    ctx.translate(r.x, r.y);
    ctx.textBaseline = "alphabetic";
    ctx.fillStyle = css(f.bg);
    ctx.fillRect(0, 0, r.w, r.h);

    // A frame whose size is not a sane grid draws as its background only.
    if (gridCells(f) === null) {
      ctx.restore();
      return;
    }

    // A slide moves the grid by whole device pixels (the glyphs stay on the pixel grid); what it pushes past the
    // grid's own box is clipped there. The frames under it fill the rows the frame itself leaves bare.
    const offset = this.devicePx(fx?.offsetY);
    const under = (fx?.under ?? []).filter((u) => u.frame.cols === f.cols && u.frame.rows === f.rows);
    if (offset !== 0 || under.length > 0) {
      ctx.beginPath();
      ctx.rect(0, 0, Math.min(r.w, f.cols * cellW), Math.min(r.h, f.rows * cellH));
      ctx.clip();
    }
    for (const u of under) this.paintGrid(u.frame, this.devicePx(u.offsetY));
    if (under.length > 0) {
      // The frame's blank cells are not painted (the pane's background was): cover what the layers drew where it is.
      ctx.fillStyle = css(f.bg);
      ctx.fillRect(0, offset, f.cols * cellW, f.rows * cellH);
    }
    this.paintGrid(f, offset);

    const grid = fx?.ghosts;
    if (grid && grid.length > 0) {
      ctx.fillStyle = css(this.cursorColor);
      for (const { x, y, alpha } of grid) {
        if (!Number.isInteger(x) || !Number.isInteger(y) || x < 0 || y < 0 || x >= f.cols || y >= f.rows) continue;
        if (!(alpha > 0)) continue;
        ctx.globalAlpha = Math.min(1, alpha);
        ctx.fillRect(x * cellW, y * cellH, cellW, cellH);
      }
      ctx.globalAlpha = 1;
    }
    ctx.restore();
  }

  /** CSS pixels (or nothing) as the CSS pixels of whole device pixels; anything that is not a finite number is none. */
  private devicePx(px: number | undefined): number {
    return px !== undefined && Number.isFinite(px) ? Math.round(px * this.m.dpr) / this.m.dpr : 0;
  }

  /**
   * The pictures of one layer, clipped to the grid's box (never over the pane's padding or header). The context is
   * translated by `offset`, so the clip is moved back: a slide shifts the pictures with the text but not the box.
   * The edges are whole device pixels, so a picture that ends at a cell edge ends on a pixel edge.
   */
  private paintImages(f: Frame, images: ImageDraw[], layer: number, offset: number) {
    const { ctx, m } = this;
    ctx.save();
    ctx.beginPath();
    ctx.rect(0, -offset, f.cols * m.cellW, f.rows * m.cellH);
    ctx.clip();
    const snap = (v: number) => Math.round(v * m.dpr) / m.dpr;
    for (const im of images) {
      if (im.layer !== layer) continue;
      const x0 = snap(im.dx), y0 = snap(im.dy);
      ctx.drawImage(im.bitmap, im.sx, im.sy, im.sw, im.sh, x0, y0, snap(im.dx + im.dw) - x0, snap(im.dy + im.dh) - y0);
    }
    ctx.restore();
  }

  /** The cell backgrounds the program set, alone: the pass that lets a picture lie between them and the text. */
  private paintBackgrounds(f: Frame) {
    const { ctx, m } = this;
    const lines = Math.min(f.rows, f.lines.length);
    for (let y = 0; y < lines; y++) {
      let col = 0;
      for (const [, runCols, fg0, bg0, flags] of f.lines[y]!) {
        if (col >= f.cols) break;
        const end = Math.min(f.cols, col + runCols);
        const bg = flags & INVERSE ? fg0 : bg0;
        if (bg !== f.bg) {
          ctx.fillStyle = css(bg);
          ctx.fillRect(col * m.cellW, y * m.cellH, (end - col) * m.cellW, m.cellH);
        }
        col = end;
      }
    }
  }

  /**
   * One frame's grid and cursor on the pane the context is translated to, `offset` CSS pixels lower than its place.
   * The pane's background is already painted; the context's clip is the caller's. Pictures come in three layers
   * (docs/design.md 6.31): below the cell backgrounds, between them and the text, above the text; a frame without
   * pictures takes the one pass it always took.
   */
  private paintGrid(f: Frame, offset: number) {
    const { ctx, m } = this;
    const { cellW, cellH } = m;
    ctx.save();
    if (offset !== 0) ctx.translate(0, offset);
    const { cols } = f;
    const baseline = Math.round(cellH * 0.78);
    let lastFont = -1;
    const lines = Math.min(f.rows, f.lines.length);
    const pictures = f.images !== undefined && f.images.length > 0 ? f.images : null;
    // With a picture under the text the backgrounds are painted first, alone, so the picture can lie between them.
    const split = pictures !== null && pictures.some((im) => im.layer < 2);
    if (pictures && split) {
      this.paintImages(f, pictures, 0, offset);
      this.paintBackgrounds(f);
      this.paintImages(f, pictures, 1, offset);
    }
    for (let y = 0; y < lines; y++) {
      const line = f.lines[y]!;
      const extras = rowExtras(f, y);
      let col = 0;
      for (const [text, runCols, fg0, bg0, flags] of line) {
        if (col >= cols) break;
        // A run's text has one code point per cell; nothing is drawn past the run's cells or the grid.
        const end = Math.min(cols, col + runCols);
        let fg = fg0, bg = bg0;
        if (flags & INVERSE) [fg, bg] = [bg, fg];
        const x = col * cellW;
        if (!split && bg !== f.bg) {
          ctx.fillStyle = css(bg);
          ctx.fillRect(x, y * cellH, (end - col) * cellW, cellH);
        }
        if (text.trim() !== "" || flags & UNDERLINE || extras) {
          ctx.fillStyle = css(fg);
          const style = flags & (BOLD | ITALIC);
          if (style !== lastFont) {
            ctx.font = font(style);
            lastFont = style;
          }
          const chars = Array.from(text);
          for (let i = 0, c = col; i < chars.length && c < end; i++, c++) {
            const ch = chars[i]!;
            const tail = extras?.get(c);
            if (ch === "\0" || (ch === " " && tail === undefined)) continue;
            // The cell's first code point plus the rest of its cluster, drawn as one glyph in its cell(s).
            const wide = chars[i + 1] === "\0";
            const glyph = tail === undefined ? ch : ch + tail;
            if (tail === undefined && !wide) {
              ctx.fillText(glyph, c * cellW, y * cellH + baseline);
            } else {
              // A cluster or a wide character may be wider than its cells: it never paints into the next one.
              ctx.save();
              ctx.beginPath();
              ctx.rect(c * cellW, y * cellH, (wide ? 2 : 1) * cellW, cellH);
              ctx.clip();
              ctx.fillText(glyph, c * cellW, y * cellH + baseline);
              ctx.restore();
            }
          }
          if (flags & UNDERLINE) ctx.fillRect(x, y * cellH + cellH - 2, (end - col) * cellW, 1);
        }
        // The resting cue of a link, also over blank cells (a link that hides itself is marked too); the program's own underline stays alone.
        if (flags & LINK && !(flags & UNDERLINE)) this.paintDots(y, col, end, fg);
        col = end;
      }
    }

    if (pictures) this.paintImages(f, pictures, 2, offset);

    const cursor = visibleCursor(f);
    if (cursor) {
      const at = cellAt(f, cursor.y, cursor.x);
      const x = at.first * cellW, y = cursor.y * cellH, w = at.span * cellW;
      const t = 1 / m.dpr; // one device pixel in CSS pixels
      ctx.fillStyle = css(this.cursorColor);
      switch (cursor.style) {
        case "block": {
          // A cursor-colour cell with its glyph redrawn in the pane background, so the character stays readable.
          ctx.fillRect(x, y, w, cellH);
          if (at.ch !== " " && at.ch !== "\0") {
            const tail = rowExtras(f, cursor.y)?.get(at.first);
            ctx.fillStyle = css(f.bg);
            ctx.font = font(at.flags & (BOLD | ITALIC));
            ctx.fillText(tail === undefined ? at.ch : at.ch + tail, x, y + baseline);
            if (at.flags & UNDERLINE) ctx.fillRect(x, y + cellH - 2, w, 1);
          }
          if (at.flags & LINK && !(at.flags & UNDERLINE)) this.paintDots(cursor.y, at.first, at.first + at.span, f.bg);
          break;
        }
        case "bar":
          ctx.fillRect(x, y, CUR_THICK * t, cellH);
          break;
        case "underline":
          ctx.fillRect(x, y + cellH - CUR_THICK * t, w, CUR_THICK * t);
          break;
        case "hollow":
          ctx.fillRect(x, y, w, t);
          ctx.fillRect(x, y + cellH - t, w, t);
          ctx.fillRect(x, y, t, cellH);
          ctx.fillRect(x + w - t, y, t, cellH);
          break;
      }
    }
    ctx.restore();
  }

  sync() {
    this.ctx.getImageData(0, 0, 1, 1);
  }
}
