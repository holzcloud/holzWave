import {
  BOLD, CURSOR_COLOR, Frame, INVERSE, ITALIC, Metrics, PxRect, Renderer, UNDERLINE, css, font, gridCells, visibleCursor,
} from "./frame.js";

/** What the cell (x, y) of a frame holds: its character and style, and the cells a wide character covers. */
function cellAt(f: Frame, y: number, x: number): { ch: string; flags: number; first: number; span: number } {
  let col = 0;
  let prev: { ch: string; flags: number } | null = null;
  for (const [text, runCols, , , flags] of f.lines[y] ?? []) {
    const end = col + runCols;
    if (x < end) {
      const chars = Array.from(text);
      const i = x - col;
      const ch = chars[i] ?? " ";
      if (ch === "\0") {
        // The second half of a wide character: the character itself is the cell before.
        const before = i > 0 ? chars[i - 1] : prev?.ch;
        return { ch: before ?? " ", flags, first: x - 1, span: 2 };
      }
      const wide = chars[i + 1] === "\0";
      return { ch, flags, first: x, span: wide ? 2 : 1 };
    }
    const chars = Array.from(text);
    prev = { ch: chars[chars.length - 1] ?? " ", flags };
    col = end;
  }
  return { ch: " ", flags: 0, first: x, span: 1 };
}

/** The simple renderer: one `fillText` per glyph on a 2D canvas. */
export class Canvas2DRenderer implements Renderer {
  private ctx: CanvasRenderingContext2D;
  private m: Metrics = { cellW: 8, cellH: 17, dpr: 1, width: 0, height: 0 };

  constructor(private canvas: HTMLCanvasElement) {
    this.ctx = canvas.getContext("2d", { alpha: false })!;
  }

  configure(m: Metrics) {
    this.m = m;
  }

  begin(bg: number) {
    const { ctx, m } = this;
    ctx.setTransform(m.dpr, 0, 0, m.dpr, 0, 0);
    ctx.fillStyle = css(bg);
    ctx.fillRect(0, 0, m.width, m.height);
  }

  drawPane(f: Frame, r: PxRect) {
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
    const { cols } = f;
    const baseline = Math.round(cellH * 0.78);
    let lastFont = -1;
    const lines = Math.min(f.rows, f.lines.length);
    for (let y = 0; y < lines; y++) {
      const line = f.lines[y]!;
      let col = 0;
      for (const [text, runCols, fg0, bg0, flags] of line) {
        if (col >= cols) break;
        // A run's text has one code point per cell; nothing is drawn past the run's cells or the grid.
        const end = Math.min(cols, col + runCols);
        let fg = fg0, bg = bg0;
        if (flags & INVERSE) [fg, bg] = [bg, fg];
        const x = col * cellW;
        if (bg !== f.bg) {
          ctx.fillStyle = css(bg);
          ctx.fillRect(x, y * cellH, (end - col) * cellW, cellH);
        }
        if (text.trim() !== "" || flags & UNDERLINE) {
          ctx.fillStyle = css(fg);
          const style = flags & (BOLD | ITALIC);
          if (style !== lastFont) {
            ctx.font = font(style);
            lastFont = style;
          }
          let c = col;
          for (const ch of text) {
            if (c >= end) break;
            if (ch !== "\0" && ch !== " ") ctx.fillText(ch, c * cellW, y * cellH + baseline);
            c += 1;
          }
          if (flags & UNDERLINE) ctx.fillRect(x, y * cellH + cellH - 2, (end - col) * cellW, 1);
        }
        col = end;
      }
    }

    const cursor = visibleCursor(f);
    if (cursor) {
      // Block: a cursor-colour cell with its glyph redrawn in the pane background, so the character stays readable.
      const at = cellAt(f, cursor.y, cursor.x);
      const x = at.first * cellW, y = cursor.y * cellH;
      ctx.fillStyle = css(CURSOR_COLOR);
      ctx.fillRect(x, y, at.span * cellW, cellH);
      if (at.ch !== " " && at.ch !== "\0") {
        ctx.fillStyle = css(f.bg);
        ctx.font = font(at.flags & (BOLD | ITALIC));
        ctx.fillText(at.ch, x, y + baseline);
        if (at.flags & UNDERLINE) ctx.fillRect(x, y + cellH - 2, at.span * cellW, 1);
      }
    }
    ctx.restore();
  }

  sync() {
    this.ctx.getImageData(0, 0, 1, 1);
  }
}
