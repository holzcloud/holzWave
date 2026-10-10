use std::cell::{Cell as StdCell, RefCell};
use std::rc::Rc;

use libghostty_vt::key::{self, Action, Key, OptionAsAlt};
use libghostty_vt::paste::Options as PasteOptions;
use libghostty_vt::render::{
    CellIterator, Colors, CursorVisualStyle, Dirty, RowIteration, RowIterator, Snapshot,
};
use libghostty_vt::screen::{CellWide, Screen as VtScreen};
use libghostty_vt::style::Underline;
use libghostty_vt::terminal::{ClipboardMime, Mode, ScrollViewport, SizeReportSize};
use libghostty_vt::{Error as VtError, RenderState, Terminal};

use crate::engine::{
    Cell, Color, Cursor, CursorStyle, KeyAction, KeyEvent, Modes, Mods, PasteOutcome, RowUpdate, Screen,
    Scrollbar, TerminalEngine, Update, Width,
};
use crate::{Error, Result};

/// The most bytes of program-controlled replies that may wait for the PTY at once.
///
/// A program can ask for a reply with a few bytes of query; without a bound a flood of queries grows the buffer as
/// fast as the program can write. User-controlled bytes (a paste) are not subject to it.
const REPLY_CAP: usize = 64 * 1024;

fn eng<E: std::fmt::Debug>(e: E) -> Error {
    Error::Engine(format!("{e:?}"))
}

fn color(c: libghostty_vt::style::RgbColor) -> Color {
    Color { r: c.r, g: c.g, b: c.b }
}

/// The libghostty key for a W3C `KeyboardEvent.code`.
///
/// The variant names of `key::Key` are the W3C code names, except that the letters are `A` to `Z` for `KeyA` to
/// `KeyZ`. The library does not expose its own parser, so the table is written out here and a unit test walks the
/// whole enum to prove it complete. A code the table does not know (including any oversized or hostile string) is an
/// unidentified key, which the encoder still encodes from the event's text.
fn key_from_code(code: &str) -> Key {
    match code {
        "Backquote" => Key::Backquote,
        "Backslash" => Key::Backslash,
        "BracketLeft" => Key::BracketLeft,
        "BracketRight" => Key::BracketRight,
        "Comma" => Key::Comma,
        "Digit0" => Key::Digit0,
        "Digit1" => Key::Digit1,
        "Digit2" => Key::Digit2,
        "Digit3" => Key::Digit3,
        "Digit4" => Key::Digit4,
        "Digit5" => Key::Digit5,
        "Digit6" => Key::Digit6,
        "Digit7" => Key::Digit7,
        "Digit8" => Key::Digit8,
        "Digit9" => Key::Digit9,
        "Equal" => Key::Equal,
        "IntlBackslash" => Key::IntlBackslash,
        "IntlRo" => Key::IntlRo,
        "IntlYen" => Key::IntlYen,
        "Minus" => Key::Minus,
        "Period" => Key::Period,
        "Quote" => Key::Quote,
        "Semicolon" => Key::Semicolon,
        "Slash" => Key::Slash,
        "AltLeft" => Key::AltLeft,
        "AltRight" => Key::AltRight,
        "Backspace" => Key::Backspace,
        "CapsLock" => Key::CapsLock,
        "ContextMenu" => Key::ContextMenu,
        "ControlLeft" => Key::ControlLeft,
        "ControlRight" => Key::ControlRight,
        "Enter" => Key::Enter,
        "MetaLeft" => Key::MetaLeft,
        "MetaRight" => Key::MetaRight,
        "ShiftLeft" => Key::ShiftLeft,
        "ShiftRight" => Key::ShiftRight,
        "Space" => Key::Space,
        "Tab" => Key::Tab,
        "Convert" => Key::Convert,
        "KanaMode" => Key::KanaMode,
        "NonConvert" => Key::NonConvert,
        "Delete" => Key::Delete,
        "End" => Key::End,
        "Help" => Key::Help,
        "Home" => Key::Home,
        "Insert" => Key::Insert,
        "PageDown" => Key::PageDown,
        "PageUp" => Key::PageUp,
        "ArrowDown" => Key::ArrowDown,
        "ArrowLeft" => Key::ArrowLeft,
        "ArrowRight" => Key::ArrowRight,
        "ArrowUp" => Key::ArrowUp,
        "NumLock" => Key::NumLock,
        "Numpad0" => Key::Numpad0,
        "Numpad1" => Key::Numpad1,
        "Numpad2" => Key::Numpad2,
        "Numpad3" => Key::Numpad3,
        "Numpad4" => Key::Numpad4,
        "Numpad5" => Key::Numpad5,
        "Numpad6" => Key::Numpad6,
        "Numpad7" => Key::Numpad7,
        "Numpad8" => Key::Numpad8,
        "Numpad9" => Key::Numpad9,
        "NumpadAdd" => Key::NumpadAdd,
        "NumpadBackspace" => Key::NumpadBackspace,
        "NumpadClear" => Key::NumpadClear,
        "NumpadClearEntry" => Key::NumpadClearEntry,
        "NumpadComma" => Key::NumpadComma,
        "NumpadDecimal" => Key::NumpadDecimal,
        "NumpadDivide" => Key::NumpadDivide,
        "NumpadEnter" => Key::NumpadEnter,
        "NumpadEqual" => Key::NumpadEqual,
        "NumpadMemoryAdd" => Key::NumpadMemoryAdd,
        "NumpadMemoryClear" => Key::NumpadMemoryClear,
        "NumpadMemoryRecall" => Key::NumpadMemoryRecall,
        "NumpadMemoryStore" => Key::NumpadMemoryStore,
        "NumpadMemorySubtract" => Key::NumpadMemorySubtract,
        "NumpadMultiply" => Key::NumpadMultiply,
        "NumpadParenLeft" => Key::NumpadParenLeft,
        "NumpadParenRight" => Key::NumpadParenRight,
        "NumpadSubtract" => Key::NumpadSubtract,
        "NumpadSeparator" => Key::NumpadSeparator,
        "NumpadUp" => Key::NumpadUp,
        "NumpadDown" => Key::NumpadDown,
        "NumpadRight" => Key::NumpadRight,
        "NumpadLeft" => Key::NumpadLeft,
        "NumpadBegin" => Key::NumpadBegin,
        "NumpadHome" => Key::NumpadHome,
        "NumpadEnd" => Key::NumpadEnd,
        "NumpadInsert" => Key::NumpadInsert,
        "NumpadDelete" => Key::NumpadDelete,
        "NumpadPageUp" => Key::NumpadPageUp,
        "NumpadPageDown" => Key::NumpadPageDown,
        "Escape" => Key::Escape,
        "F1" => Key::F1,
        "F2" => Key::F2,
        "F3" => Key::F3,
        "F4" => Key::F4,
        "F5" => Key::F5,
        "F6" => Key::F6,
        "F7" => Key::F7,
        "F8" => Key::F8,
        "F9" => Key::F9,
        "F10" => Key::F10,
        "F11" => Key::F11,
        "F12" => Key::F12,
        "F13" => Key::F13,
        "F14" => Key::F14,
        "F15" => Key::F15,
        "F16" => Key::F16,
        "F17" => Key::F17,
        "F18" => Key::F18,
        "F19" => Key::F19,
        "F20" => Key::F20,
        "F21" => Key::F21,
        "F22" => Key::F22,
        "F23" => Key::F23,
        "F24" => Key::F24,
        "F25" => Key::F25,
        "Fn" => Key::Fn,
        "FnLock" => Key::FnLock,
        "PrintScreen" => Key::PrintScreen,
        "ScrollLock" => Key::ScrollLock,
        "Pause" => Key::Pause,
        "BrowserBack" => Key::BrowserBack,
        "BrowserFavorites" => Key::BrowserFavorites,
        "BrowserForward" => Key::BrowserForward,
        "BrowserHome" => Key::BrowserHome,
        "BrowserRefresh" => Key::BrowserRefresh,
        "BrowserSearch" => Key::BrowserSearch,
        "BrowserStop" => Key::BrowserStop,
        "Eject" => Key::Eject,
        "LaunchApp1" => Key::LaunchApp1,
        "LaunchApp2" => Key::LaunchApp2,
        "LaunchMail" => Key::LaunchMail,
        "MediaPlayPause" => Key::MediaPlayPause,
        "MediaSelect" => Key::MediaSelect,
        "MediaStop" => Key::MediaStop,
        "MediaTrackNext" => Key::MediaTrackNext,
        "MediaTrackPrevious" => Key::MediaTrackPrevious,
        "Power" => Key::Power,
        "Sleep" => Key::Sleep,
        "AudioVolumeDown" => Key::AudioVolumeDown,
        "AudioVolumeMute" => Key::AudioVolumeMute,
        "AudioVolumeUp" => Key::AudioVolumeUp,
        "WakeUp" => Key::WakeUp,
        "Copy" => Key::Copy,
        "Cut" => Key::Cut,
        "Paste" => Key::Paste,
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

/// The cursor of a snapshot in viewport terms; `None` when it is outside the viewport.
fn read_cursor(snapshot: &Snapshot<'static, '_>) -> Result<Option<Cursor>> {
    let c = snapshot.cursor().map_err(eng)?;
    Ok(c.viewport.map(|v| Cursor {
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
    }))
}

/// Read every cell of the row the iteration stands on, resolving colours against the snapshot's defaults.
///
/// The one place where a libghostty cell becomes a neutral [`Cell`]: `screen` and `update` share it, so the two can
/// never disagree about what a cell is.
fn read_row(
    cells_iter: &mut CellIterator<'static>,
    row: &RowIteration<'static, '_>,
    colors: &Colors,
    out: &mut Vec<Cell>,
) -> Result<()> {
    let mut cell_iter = cells_iter.update(row).map_err(eng)?;
    while let Some(cell) = cell_iter.next() {
        let mut text = String::new();
        cell.graphemes_utf8(&mut text).map_err(eng)?;
        let style = cell.style().map_err(eng)?;
        let width = match cell.raw_cell().map_err(eng)?.wide().map_err(eng)? {
            CellWide::Narrow => Width::Narrow,
            CellWide::Wide => Width::Wide,
            CellWide::SpacerTail | CellWide::SpacerHead => Width::Spacer,
        };
        out.push(Cell {
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
    Ok(())
}

/// [`TerminalEngine`] backed by `libghostty-vt`.
pub struct GhosttyEngine {
    terminal: Terminal<'static, 'static>,
    render: RenderState<'static>,
    rows_iter: RowIterator<'static>,
    cells_iter: CellIterator<'static>,
    /// Bytes the terminal wants written to the PTY: query replies and paste output.
    replies: Rc<RefCell<Vec<u8>>>,
    /// While `Some`, the reply buffer refuses whole replies that would grow it past the limit. Set around
    /// `vt_write` only, so what `paste` writes is never cut.
    reply_cap: Rc<StdCell<Option<usize>>>,
    encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    option_as_alt: bool,
    /// The next `update` reports every row and tells the consumer to start over: true at the start and after
    /// `request_full`.
    force_full: bool,
}

impl GhosttyEngine {
    pub fn new(cols: u16, rows: u16, max_scrollback: usize) -> Result<Self> {
        let mut terminal = Terminal::new(cols, rows).map_err(eng)?;
        terminal.set_scrollback_max_lines(Some(max_scrollback)).map_err(eng)?;
        let replies: Rc<RefCell<Vec<u8>>> = Rc::default();
        let reply_cap: Rc<StdCell<Option<usize>>> = Rc::default();
        {
            // Callbacks run synchronously inside `vt_write`, `resize` and `paste`, and a panic inside one aborts
            // the process: only plain pushes here, no unwrap, and a failed borrow cannot happen because the
            // engine is single-threaded and the buffer is not borrowed across a library call.
            let sink = replies.clone();
            let cap = reply_cap.clone();
            terminal
                .on_pty_write(move |_t, data| {
                    let mut buf = sink.borrow_mut();
                    match cap.get() {
                        // Whole replies only: a truncated sequence would corrupt what the program reads next.
                        Some(limit) if buf.len() + data.len() > limit => {}
                        _ => buf.extend_from_slice(data),
                    }
                })
                .map_err(eng)?;

            // XTWINOPS 14, 16 and 18 t: answered from the terminal's own geometry. Without this callback the
            // library stays silent.
            terminal
                .on_size(|t| {
                    let (cols, rows) = (t.cols().ok()?, t.rows().ok()?);
                    let (w, h) = (t.width_px().ok()?, t.height_px().ok()?);
                    Some(SizeReportSize {
                        rows,
                        columns: cols,
                        cell_width: if cols > 0 { w / u32::from(cols) } else { 0 },
                        cell_height: if rows > 0 { h / u32::from(rows) } else { 0 },
                    })
                })
                .map_err(eng)?;
            // Deliberately not installed: on_clipboard_read and on_clipboard_write (OSC 52 stays inert, and the
            // read callback would turn on Kitty paste events), and title reports stay disabled (a program could
            // put text into its own input with them).
        }
        Ok(Self {
            terminal,
            render: RenderState::new().map_err(eng)?,
            rows_iter: RowIterator::new().map_err(eng)?,
            cells_iter: CellIterator::new().map_err(eng)?,
            replies,
            reply_cap,
            encoder: key::Encoder::new().map_err(eng)?,
            key_event: key::Event::new().map_err(eng)?,
            option_as_alt: false,
            force_full: true,
        })
    }
}

impl GhosttyEngine {
    /// Fill the reusable key event and run the encoder against the terminal's current modes.
    #[allow(clippy::too_many_arguments)]
    fn encode_event(
        &mut self,
        action: Action,
        key: Key,
        mods: key::Mods,
        consumed: key::Mods,
        composing: bool,
        unshifted: Option<char>,
        text: Option<&str>,
    ) -> Result<Vec<u8>> {
        self.key_event
            .set_action(action)
            .set_key(key)
            .set_mods(mods)
            .set_consumed_mods(consumed)
            .set_composing(composing)
            .set_unshifted_codepoint(unshifted.unwrap_or('\0'))
            .set_utf8(text);

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
}

impl TerminalEngine for GhosttyEngine {
    fn feed(&mut self, bytes: &[u8]) {
        // Everything the terminal writes back while it parses program output is program-controlled: bound it.
        self.reply_cap.set(Some(REPLY_CAP));
        self.terminal.vt_write(bytes);
        self.reply_cap.set(None);
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

        let cursor = read_cursor(&snapshot)?;

        let mut cells = Vec::with_capacity(cols as usize * rows as usize);
        let mut row_iter = self.rows_iter.update(&snapshot).map_err(eng)?;
        while let Some(row) = row_iter.next() {
            read_row(&mut self.cells_iter, row, &colors, &mut cells)?;
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
        self.encode_event(
            action,
            key_from_code(&event.code),
            key_mods(event.mods),
            key_mods(event.consumed),
            event.composing,
            event.unshifted,
            event.text.as_deref(),
        )
    }

    fn encode_text(&mut self, text: &str) -> Result<Vec<u8>> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        // Committed text is an unidentified key whose text is the commit, so that the Kitty "associated text"
        // report works. It is input, not a paste: nothing is bracketed.
        self.encode_event(
            Action::Press,
            Key::Unidentified,
            key::Mods::empty(),
            key::Mods::empty(),
            false,
            None,
            Some(text),
        )
    }

    fn paste(&mut self, text: &str, allow_unsafe: bool) -> Result<PasteOutcome> {
        // The library decides: bracketed or not by mode 2004, control bytes replaced, and text that could inject
        // commands refused unless `allow_unsafe`. Nothing is written for an error. The output goes through the
        // same reply buffer as query replies and is not capped.
        let mimes = [ClipboardMime::new("text/plain")];
        let options = PasteOptions::new().with_allow_unsafe(allow_unsafe);
        match self.terminal.paste(options, &mimes, |_mime, out| out.write_all(text.as_bytes())) {
            Ok(true) => Ok(PasteOutcome::Written),
            Ok(false) => Ok(PasteOutcome::Empty),
            Err(VtError::Rejected) => Ok(PasteOutcome::Rejected),
            Err(e) => Err(eng(e)),
        }
    }

    fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut *self.replies.borrow_mut())
    }

    fn update(&mut self) -> Result<Update> {
        // The scrollbar is read at the same moment as the render-state update (render.rs docs).
        let sb = self.terminal.scrollbar().map_err(eng)?;
        let modes = Modes {
            alt_screen: self.terminal.active_screen().map_err(eng)? == VtScreen::Alternate,
            bracketed_paste: self.terminal.mode(Mode::BRACKETED_PASTE).map_err(eng)?,
            mouse_tracking: self.terminal.is_mouse_tracking().map_err(eng)?,
        };
        let snapshot = self.render.update(&self.terminal).map_err(eng)?;
        let colors = snapshot.colors().map_err(eng)?;
        let (cols, rows) = (snapshot.cols().map_err(eng)?, snapshot.rows().map_err(eng)?);
        let reset = self.force_full;
        let full = reset || snapshot.dirty().map_err(eng)? == Dirty::Full;
        let cursor = read_cursor(&snapshot)?;

        let mut changed = Vec::new();
        {
            let mut row_iter = self.rows_iter.update(&snapshot).map_err(eng)?;
            if full {
                // A full pass: libghostty's per-row flags are conservative, every row is read.
                let mut y: u16 = 0;
                while let Some(row) = row_iter.next() {
                    let mut cells = Vec::with_capacity(cols as usize);
                    read_row(&mut self.cells_iter, row, &colors, &mut cells)?;
                    changed.push(RowUpdate { y, cells });
                    y = y.saturating_add(1);
                }
            } else {
                while let Some((y, row)) = row_iter.next_dirty() {
                    let mut cells = Vec::with_capacity(cols as usize);
                    read_row(&mut self.cells_iter, row, &colors, &mut cells)?;
                    changed.push(RowUpdate { y, cells });
                }
            }
        }
        // Only after the rows were consumed: clean() clears the global and the per-row flags.
        snapshot.clean().map_err(eng)?;
        self.force_full = false;

        Ok(Update {
            full,
            reset,
            cols,
            rows,
            cursor,
            scrollbar: Scrollbar { total: sb.total, offset: sb.offset, len: sb.len },
            default_fg: color(colors.foreground),
            default_bg: color(colors.background),
            modes,
            changed,
            selection: Vec::new(),
            matches: Vec::new(),
            search: None,
            copy_cursor: None,
        })
    }

    fn request_full(&mut self) {
        self.force_full = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The W3C code of a libghostty key: its variant name, with the single letters `A` to `Z` standing for
    /// `KeyA` to `KeyZ`.
    fn w3c_code(key: Key) -> String {
        let name = format!("{key:?}");
        if name.len() == 1 && name.as_bytes()[0].is_ascii_uppercase() { format!("Key{name}") } else { name }
    }

    #[test]
    fn every_libghostty_key_has_its_w3c_code_mapped() {
        // Walk the enum by number until the library stops knowing values. The count pins the revision: a newer
        // libghostty with more keys fails here until the table in key_from_code is extended.
        let mut seen = 0;
        for i in 0.. {
            let Ok(key) = Key::try_from(i) else { break };
            let code = w3c_code(key);
            assert_eq!(key_from_code(&code), key, "code {code} (variant {i}) is not mapped");
            seen += 1;
        }
        assert_eq!(seen, 176, "the pinned libghostty revision has 176 key variants");
    }

    #[test]
    fn unknown_and_hostile_codes_are_unidentified() {
        for code in ["", "Bogus", "keya", "KeyAA", "Key", "Digit10", "F26", " ArrowUp"] {
            assert_eq!(key_from_code(code), Key::Unidentified, "{code:?}");
        }
        assert_eq!(key_from_code(&"K".repeat(1 << 20)), Key::Unidentified);
    }
}
