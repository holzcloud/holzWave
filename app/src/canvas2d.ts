import { BOLD, CURSOR_COLOR, Frame, INVERSE, ITALIC, Metrics, PxRect, Renderer, UNDERLINE, css, font } from "./frame.js";

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

    const baseline = Math.round(cellH * 0.78);
    let lastFont = -1;
    for (let y = 0; y < f.lines.length; y++) {
      const line = f.lines[y]!;
      let col = 0;
      for (const [text, cols, fg0, bg0, flags] of line) {
        let fg = fg0, bg = bg0;
        if (flags & INVERSE) [fg, bg] = [bg, fg];
        const x = col * cellW;
        if (bg !== f.bg) {
          ctx.fillStyle = css(bg);
          ctx.fillRect(x, y * cellH, cols * cellW, cellH);
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
            if (ch === "\0") { c += 1; continue; }
            if (ch !== " ") ctx.fillText(ch, c * cellW, y * cellH + baseline);
            c += 1;
          }
          if (flags & UNDERLINE) ctx.fillRect(x, y * cellH + cellH - 2, cols * cellW, 1);
        }
        col += cols;
      }
    }
    if (f.cursor && f.focused) {
      ctx.fillStyle = css(CURSOR_COLOR);
      ctx.globalAlpha = 0.85;
      ctx.fillRect(f.cursor.x * cellW, f.cursor.y * cellH, cellW, cellH);
      ctx.globalAlpha = 1;
    }
    ctx.restore();
  }

  sync() {
    this.ctx.getImageData(0, 0, 1, 1);
  }
}
