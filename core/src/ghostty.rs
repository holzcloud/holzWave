use std::cell::RefCell;
use std::rc::Rc;

use libghostty_vt::key::{self, Action, Key, OptionAsAlt};
use libghostty_vt::render::{CellIterator, CursorVisualStyle, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::Underline;
use libghostty_vt::terminal::ScrollViewport;
use libghostty_vt::{RenderState, Terminal};

use crate::engine::{
    Cell, Color, Cursor, CursorStyle, KeyAction, KeyEvent, Mods, PasteOutcome, Screen, Scrollbar,
    TerminalEngine, Width,
};
use crate::{Error, Result};

fn eng<E: std::fmt::Debug>(e: E) -> Error {
    Error::Engine(format!("{e:?}"))
}

fn color(c: libghostty_vt::style::RgbColor) -> Color {
    Color { r: c.r, g: c.g, b: c.b }
}

/// The libghostty key for a W3C `KeyboardEvent.code`. Codes it does not know are an unidentified key, which the
/// encoder still encodes from the event's text.
fn key_from_code(code: &str) -> Key {
    match code {
        "ArrowUp" => Key::ArrowUp,
        "ArrowDown" => Key::ArrowDown,
        "ArrowLeft" => Key::ArrowLeft,
        "ArrowRight" => Key::ArrowRight,
        "Enter" => Key::Enter,
        "KeyA" => Key::A,
        "KeyB" => Key::B,
        "KeyC" => Key::C,
        "KeyD" => Key::D,
        "KeyE" => Key::E,
        "KeyF" => Key::F,
        "KeyG" => Key::G,
        "KeyH" => Key::H,
        "KeyI" => Key::I,
        "KeyJ" => Key::J,
        "KeyK" => Key::K,
        "KeyL" => Key::L,
        "KeyM" => Key::M,
        "KeyN" => Key::N,
        "KeyO" => Key::O,
        "KeyP" => Key::P,
        "KeyQ" => Key::Q,
        "KeyR" => Key::R,
        "KeyS" => Key::S,
        "KeyT" => Key::T,
        "KeyU" => Key::U,
        "KeyV" => Key::V,
        "KeyW" => Key::W,
        "KeyX" => Key::X,
        "KeyY" => Key::Y,
        "KeyZ" => Key::Z,
        _ => Key::Unidentified,
    }
}

fn key_mods(m: Mods) -> key::Mods {
    let mut out = key::Mods::empty();
    if m.contains(Mods::SHIFT) {
        out |= key::Mods::SHIFT;
    }
    if m.contains(Mods::ALT) {
        out |= key::Mods::ALT;
    }
    if m.contains(Mods::CTRL) {
        out |= key::Mods::CTRL;
    }
    if m.contains(Mods::SUPER) {
        out |= key::Mods::SUPER;
    }
    out
}

/// [`TerminalEngine`] backed by `libghostty-vt`.
pub struct GhosttyEngine {
    terminal: Terminal<'static, 'static>,
    render: RenderState<'static>,
    rows_iter: RowIterator<'static>,
    cells_iter: CellIterator<'static>,
    /// Bytes the terminal wants written to the PTY: query replies and paste output.
    replies: Rc<RefCell<Vec<u8>>>,
    encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
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
            encoder: key::Encoder::new().map_err(eng)?,
            key_event: key::Event::new().map_err(eng)?,
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

    fn encode_key(&mut self, event: &KeyEvent) -> Result<Vec<u8>> {
        let action = match event.action {
            KeyAction::Press => Action::Press,
            KeyAction::Repeat => Action::Repeat,
            KeyAction::Release => Action::Release,
        };
        self.key_event
            .set_action(action)
            .set_key(key_from_code(&event.code))
            .set_mods(key_mods(event.mods))
            .set_consumed_mods(key_mods(event.consumed))
            .set_composing(event.composing)
            .set_unshifted_codepoint(event.unshifted.unwrap_or('\0'))
            .set_utf8(event.text.as_deref());

        // The terminal's live modes (DECCKM, keypad, Kitty flags, modifyOtherKeys) are read at encode time.
        // `set_options_from_terminal` resets option-as-alt to false, so the setting goes in afterwards.
        let option_as_alt = if self.option_as_alt { OptionAsAlt::True } else { OptionAsAlt::False };
        let mut out = Vec::with_capacity(16);
        self.encoder
            .set_options_from_terminal(&self.terminal)
            .set_macos_option_as_alt(option_as_alt)
            .encode_to_vec(&self.key_event, &mut out)
            .map_err(eng)?;
        Ok(out)
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
