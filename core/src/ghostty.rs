use std::cell::RefCell;
use std::rc::Rc;

use libghostty_vt::render::{CellIterator, CursorVisualStyle, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::Underline;
use libghostty_vt::terminal::ScrollViewport;
use libghostty_vt::{RenderState, Terminal};

use crate::engine::{
    Cell, Color, Cursor, CursorStyle, KeyEvent, PasteOutcome, Screen, Scrollbar, TerminalEngine, Width,
};
use crate::{Error, Result};

fn eng<E: std::fmt::Debug>(e: E) -> Error {
    Error::Engine(format!("{e:?}"))
}

fn color(c: libghostty_vt::style::RgbColor) -> Color {
    Color { r: c.r, g: c.g, b: c.b }
}

/// [`TerminalEngine`] backed by `libghostty-vt`.
pub struct GhosttyEngine {
    terminal: Terminal<'static, 'static>,
    render: RenderState<'static>,
    rows_iter: RowIterator<'static>,
    cells_iter: CellIterator<'static>,
    /// Bytes the terminal wants written to the PTY: query replies and paste output.
    replies: Rc<RefCell<Vec<u8>>>,
    option_as_alt: bool,
}

impl GhosttyEngine {
    pub fn new(cols: u16, rows: u16, max_scrollback: usize) -> Result<Self> {
        let mut terminal = Terminal::new(cols, rows).map_err(eng)?;
        terminal.set_scrollback_max_lines(Some(max_scrollback)).map_err(eng)?;
        let replies: Rc<RefCell<Vec<u8>>> = Rc::default();
        {
            let sink = replies.clone();
            terminal
                .on_pty_write(move |_t, data| sink.borrow_mut().extend_from_slice(data))
                .map_err(eng)?;
        }
        Ok(Self {
            terminal,
            render: RenderState::new().map_err(eng)?,
            rows_iter: RowIterator::new().map_err(eng)?,
            cells_iter: CellIterator::new().map_err(eng)?,
            replies,
            option_as_alt: false,
        })
    }
}

impl TerminalEngine for GhosttyEngine {
    fn feed(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
    }

    fn resize(&mut self, cols: u16, rows: u16, cell_px: (u32, u32)) -> Result<()> {
        self.terminal.resize(cols, rows, cell_px.0, cell_px.1).map_err(eng)
    }

    fn size(&self) -> Result<(u16, u16)> {
        Ok((self.terminal.cols().map_err(eng)?, self.terminal.rows().map_err(eng)?))
    }

    fn scroll(&mut self, rows: isize) {
        self.terminal.scroll_viewport(ScrollViewport::Delta(rows));
    }

    fn scroll_to_bottom(&mut self) {
        self.terminal.scroll_viewport(ScrollViewport::Bottom);
    }

    fn screen(&mut self) -> Result<Screen> {
        let sb = self.terminal.scrollbar().map_err(eng)?;
        let snapshot = self.render.update(&self.terminal).map_err(eng)?;
        let colors = snapshot.colors().map_err(eng)?;
        let (cols, rows) = (snapshot.cols().map_err(eng)?, snapshot.rows().map_err(eng)?);

        let cursor = {
            let c = snapshot.cursor().map_err(eng)?;
            c.viewport.map(|v| Cursor {
                x: v.x,
                y: v.y,
                visible: c.visible,
                style: match c.visual_style {
                    CursorVisualStyle::Bar => CursorStyle::Bar,
                    CursorVisualStyle::Underline => CursorStyle::Underline,
                    CursorVisualStyle::BlockHollow => CursorStyle::Hollow,
                    _ => CursorStyle::Block,
                },
                blinking: c.blinking,
            })
        };

        let mut cells = Vec::with_capacity(cols as usize * rows as usize);
        let mut row_iter = self.rows_iter.update(&snapshot).map_err(eng)?;
        while let Some(row) = row_iter.next() {
            let mut cell_iter = self.cells_iter.update(row).map_err(eng)?;
            while let Some(cell) = cell_iter.next() {
                let mut text = String::new();
                cell.graphemes_utf8(&mut text).map_err(eng)?;
                let style = cell.style().map_err(eng)?;
                let width = match cell.raw_cell().map_err(eng)?.wide().map_err(eng)? {
                    CellWide::Narrow => Width::Narrow,
                    CellWide::Wide => Width::Wide,
                    CellWide::SpacerTail | CellWide::SpacerHead => Width::Spacer,
                };
                cells.push(Cell {
                    text,
                    width,
                    fg: color(cell.fg_color().map_err(eng)?.unwrap_or(colors.foreground)),
                    bg: color(cell.bg_color().map_err(eng)?.unwrap_or(colors.background)),
                    bold: style.bold,
                    italic: style.italic,
                    underline: style.underline != Underline::None,
                    inverse: style.inverse,
                });
            }
        }

        Ok(Screen {
            cols,
            rows,
            cursor,
            scrollbar: Scrollbar { total: sb.total, offset: sb.offset, len: sb.len },
            default_fg: color(colors.foreground),
            default_bg: color(colors.background),
            cells,
        })
    }

    fn set_option_as_alt(&mut self, on: bool) {
        self.option_as_alt = on;
    }

    fn encode_key(&mut self, _event: &KeyEvent) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }

    fn encode_text(&mut self, _text: &str) -> Result<Vec<u8>> {
        Err(Error::Unsupported("encode_text".into()))
    }

    fn paste(&mut self, _text: &str, _allow_unsafe: bool) -> Result<PasteOutcome> {
        Err(Error::Unsupported("paste".into()))
    }

    fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut *self.replies.borrow_mut())
    }
}
