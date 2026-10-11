import {
  BOLD, CURSOR_COLOR, Frame, INVERSE, ITALIC, Metrics, PxRect, Renderer, UNDERLINE, css, font, gridCells, rowExtras,
  visibleCursor,
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

  constructor(private canvas: HTMLCanvasElement) {
    this.ctx = canvas.getContext("2d", { alpha: false })!;
  }

  configure(m: Metrics) {
    this.m = m;
  }

  setCursorColor(rgb: number) {
    this.cursorColor = rgb & 0xffffff;
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
      const extras = rowExtras(f, y);
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
        col = end;
      }
    }

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
