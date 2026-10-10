use std::cell::{Cell as StdCell, RefCell};
use std::rc::Rc;

use libghostty_vt::key::{self, Action, Key, OptionAsAlt};
use libghostty_vt::paste::Options as PasteOptions;
use libghostty_vt::render::{
    CellIterator, Colors, CursorVisualStyle, Dirty, RowIteration, RowIterator, Snapshot,
};
use libghostty_vt::screen::{CellWide, GridRef, Screen as VtScreen};
use libghostty_vt::search::{MatchBuffer, Scroll, Search};
use libghostty_vt::selection::{FormatOptions, SelectLineOptions, SelectWordOptions, Selection};
use libghostty_vt::style::{Palette, PaletteIndex, RgbColor, Underline};
use libghostty_vt::terminal::{
    ClipboardMime, Mode, Point, PointCoordinate, PointSpace, ScrollViewport, SizeReportSize,
};
use libghostty_vt::{Error as VtError, RenderState, Terminal};

use crate::engine::{
    Cell, Color, Cursor, CursorStyle, EngineEvent, EngineTheme, KeyAction, KeyEvent, MAX_SEARCH_BYTES, MatchSpan,
    Modes, Mods, PasteOutcome, RowUpdate, Screen, Scrollbar, SearchOp, SearchState, SelectMode, SelectOp, Span,
    TerminalEngine, Update, Width, sanitize_program_text,
};
use crate::{Error, Result};

/// The most bytes of program-controlled replies that may wait for the PTY at once.
///
/// A program can ask for a reply with a few bytes of query; without a bound a flood of queries grows the buffer as
/// fast as the program can write. User-controlled bytes (a paste) are not subject to it.
const REPLY_CAP: usize = 64 * 1024;

/// The most events (titles, working directories) that wait for `take_events` at once. A program can change its title
/// as fast as it can write; only the latest few matter, so the oldest are dropped.
const MAX_PENDING_EVENTS: usize = 64;

/// Queue an event from a program: dropped when it repeats the previous one, the oldest dropped when the queue is full.
fn push_event(queue: &Rc<RefCell<Vec<EngineEvent>>>, event: EngineEvent) {
    let mut queue = queue.borrow_mut();
    if queue.last() == Some(&event) {
        return;
    }
    if queue.len() >= MAX_PENDING_EVENTS {
        queue.remove(0);
    }
    queue.push(event);
}

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

/// Where a cell is in relation to the viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    /// Scrolled out of sight above the first row.
    Above,
    /// On the viewport at this column and row.
    In(u16, u16),
    /// Scrolled out of sight below the last row.
    Below,
}

/// The spans, as `(y, x0, x1)` with both ends inclusive, that a match covers on the viewport rows.
///
/// A start above the viewport is clipped to the top left cell and an end below it to the bottom right one; a match
/// that lies wholly outside gives nothing. The first row runs from `start.x` to the last column, the rows between are
/// whole, and the last row runs to `end.x`, so a match that wraps is one span per row. Everything is clamped into the
/// grid, whatever the library reports.
fn match_spans(start: Edge, end: Edge, cols: u16, rows: u16) -> Vec<(u16, u16, u16)> {
    let last_col = cols.saturating_sub(1);
    let last_row = rows.saturating_sub(1);
    let (mut x0, mut y0) = match start {
        Edge::Above => (0, 0),
        Edge::In(x, y) => (x.min(last_col), y.min(last_row)),
        Edge::Below => return Vec::new(),
    };
    let (mut x1, mut y1) = match end {
        Edge::Below => (last_col, last_row),
        Edge::In(x, y) => (x.min(last_col), y.min(last_row)),
        Edge::Above => return Vec::new(),
    };
    if (y0, x0) > (y1, x1) {
        // Endpoints keep their direction; a span runs from the earlier cell to the later one. Clipped ends are never
        // reversed, so this is only about two cells on the viewport.
        if !matches!((start, end), (Edge::In(..), Edge::In(..))) {
            return Vec::new();
        }
        (x0, y0, x1, y1) = (x1, y1, x0, y0);
    }
    if y0 == y1 {
        return vec![(y0, x0, x1)];
    }
    let mut spans = Vec::with_capacity(usize::from(y1 - y0) + 1);
    spans.push((y0, x0, last_col));
    spans.extend((y0 + 1..y1).map(|y| (y, 0, last_col)));
    spans.push((y1, 0, x1));
    spans
}

/// Where a grid reference is in relation to a viewport of `rows` rows.
///
/// The library answers `None` for a cell above the viewport and a row number past the last one for a cell below it.
fn viewport_edge(term: &Terminal<'_, '_>, at: &GridRef<'_>, rows: u16) -> Result<Edge> {
    Ok(match term.point_from_grid_ref(at, PointSpace::Viewport).map_err(eng)? {
        None => Edge::Above,
        Some(p) if p.y < u32::from(rows) => Edge::In(p.x, p.y as u16),
        Some(_) => Edge::Below,
    })
}

/// The most `select_next` and `select_prev` steps spent putting a lost selection back (see [`restore_selection`]).
const MAX_RESTORE_STEPS: usize = 4096;

/// Put the selected match back after the library dropped it.
///
/// A resize clears the library's selected match although the matches themselves survive, and a find bar that shows
/// "- of 10" while the window is dragged is not what a search should do. `memo` is the place in reading order the
/// engine last saw; it is re-selected by stepping from whichever end of the list is nearer (`select_prev` from no
/// selection lands on the oldest match, `select_next` on the newest). A match list so long that this would take more
/// than [`MAX_RESTORE_STEPS`] steps is left alone: the current match stays unset until the user moves.
fn restore_selection(search: &mut Search<'_, '_>, terminal: &mut Terminal<'_, '_>, memo: Option<usize>) -> Result<()> {
    let Some(place) = memo else { return Ok(()) };
    if search.selected_index().map_err(eng)?.is_some() {
        return Ok(());
    }
    let total = search.total_matches().map_err(eng)?;
    if total == 0 {
        return Ok(());
    }
    let place = place.min(total - 1);
    let (from_oldest, from_newest) = (place + 1, total - place);
    if from_oldest.min(from_newest) > MAX_RESTORE_STEPS {
        return Ok(());
    }
    if from_oldest <= from_newest {
        for _ in 0..from_oldest {
            search.select_prev(terminal).map_err(eng)?;
        }
    } else {
        for _ in 0..from_newest {
            search.select_next(terminal).map_err(eng)?;
        }
    }
    Ok(())
}

/// [`TerminalEngine`] backed by `libghostty-vt`.
pub struct GhosttyEngine {
    terminal: Terminal<'static, 'static>,
    /// The active search, from a non-empty needle until `Clear`. It borrows nothing: the terminal is passed to every
    /// call that needs it, and either may be dropped first.
    search: Option<Search<'static, 'static>>,
    /// Storage for the matches of one update, reused so a frame does not allocate.
    match_buf: MatchBuffer,
    /// The place in reading order of the current match as last seen, so that it can be put back when the library
    /// drops its selection (a resize does).
    search_memo: Option<usize>,
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
    /// The grid size at the previous `update`, so that a size change resets the consumer even when libghostty
    /// reports nothing.
    last_size: Option<(u16, u16)>,
    /// The selected column range `(x0, x1)` of every viewport row, as libghostty last reported it for that row.
    ///
    /// A partial pass visits only dirty rows, so the cache is what makes `Update.selection` complete: a row that did
    /// not change keeps its range. Setting or clearing a selection, scrolling and resizing are full passes that
    /// refresh every row.
    sel_cache: Vec<Option<(u16, u16)>>,
    /// Titles and working directories the program set since the last `take_events`, already sanitised.
    events: Rc<RefCell<Vec<EngineEvent>>>,
}

impl GhosttyEngine {
    pub fn new(cols: u16, rows: u16, max_scrollback: usize) -> Result<Self> {
        let mut terminal = Terminal::new(cols, rows).map_err(eng)?;
        terminal.set_scrollback_max_lines(Some(max_scrollback)).map_err(eng)?;
        // Grapheme clustering (mode 2027) is off in a fresh libghostty terminal: every code point of a ZWJ family or a
        // flag gets a cell of its own, so the layout disagrees with Unicode. It is on from the start here, as the
        // default that a reset (RIS) returns to; a program can still switch it off and DECRQM reports the truth
        // (DESK-27).
        terminal.set_default_mode(Mode::GRAPHEME_CLUSTER, true).map_err(eng)?;
        let replies: Rc<RefCell<Vec<u8>>> = Rc::default();
        let reply_cap: Rc<StdCell<Option<usize>>> = Rc::default();
        let events: Rc<RefCell<Vec<EngineEvent>>> = Rc::default();
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
            // Title (OSC 0 and 2) and working directory (OSC 7): program-controlled strings, sanitised here at the
            // boundary (length cap, no control characters) before anything else sees them. Same rule as above: plain
            // pushes only, a panic in a callback aborts the process.
            let queue = events.clone();
            terminal
                .on_title_changed(move |t| {
                    if let Ok(title) = t.title() {
                        push_event(&queue, EngineEvent::Title(sanitize_program_text(title)));
                    }
                })
                .map_err(eng)?;
            let queue = events.clone();
            terminal
                .on_pwd_changed(move |t| {
                    if let Ok(pwd) = t.pwd() {
                        push_event(&queue, EngineEvent::Pwd(sanitize_program_text(pwd)));
                    }
                })
                .map_err(eng)?;
            // Deliberately not installed: on_bell (nothing in the shell reacts to it yet), on_clipboard_read and
            // on_clipboard_write (OSC 52 stays inert, and the read callback would turn on Kitty paste events), and
            // title reports stay disabled (a program could put text into its own input with them).
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
            last_size: None,
            sel_cache: Vec::new(),
            events,
            search: None,
            match_buf: MatchBuffer::new(),
            search_memo: None,
        })
    }
}

impl GhosttyEngine {
    /// The viewport point of a cell, with both coordinates clamped into the grid.
    ///
    /// Coordinates come from the webview and are never trusted; libghostty is only called with points inside the
    /// grid (T-02-37).
    fn clamped_point(&self, x: u16, y: u16) -> Result<Point> {
        let cols = self.terminal.cols().map_err(eng)?;
        let rows = self.terminal.rows().map_err(eng)?;
        Ok(Point::Viewport(PointCoordinate {
            x: x.min(cols.saturating_sub(1)),
            y: u32::from(y.min(rows.saturating_sub(1))),
        }))
    }

    /// The state of the active search, `None` without one. Reads what the library last saw: callers that need it
    /// current have run the search first.
    fn search_state(&mut self) -> Result<Option<SearchState>> {
        let Some(search) = &self.search else { return Ok(None) };
        let total = search.total_matches().map_err(eng)?;
        // The library counts from the newest match; the engine's contract counts in reading order.
        let current = search.selected_index().map_err(eng)?.and_then(|newest_first| total.checked_sub(newest_first + 1));
        if current.is_some() {
            self.search_memo = current;
        }
        Ok(Some(SearchState { total, current }))
    }

    /// Catch the search up with the terminal and read the matches that are on the viewport.
    ///
    /// Done before the render state is read, so the terminal borrows never overlap. The library lists matches page
    /// by page, so the list can reach past the viewport; every match is converted to viewport rows and clipped.
    fn search_view(&mut self) -> Result<(Vec<MatchSpan>, Option<SearchState>)> {
        let Some(search) = self.search.as_mut() else { return Ok((Vec::new(), None)) };
        let cols = self.terminal.cols().map_err(eng)?;
        let rows = self.terminal.rows().map_err(eng)?;
        // Output, resizes and screen switches reach the search only through a feed; `run` feeds and ticks until the
        // search has caught up with the terminal.
        search.run(&mut self.terminal).map_err(eng)?;
        restore_selection(search, &mut self.terminal, self.search_memo)?;
        let total = search.total_matches().map_err(eng)?;
        let current = search.selected_index().map_err(eng)?.and_then(|newest_first| total.checked_sub(newest_first + 1));
        if current.is_some() {
            self.search_memo = current;
        }

        let snapshot = search.snapshot(&mut self.terminal).map_err(eng)?;
        let term = snapshot.terminal();
        let selected = match snapshot.selected_match().map_err(eng)? {
            Some(sel) => Some((viewport_edge(term, &sel.start(), rows)?, viewport_edge(term, &sel.end(), rows)?)),
            None => None,
        };
        let mut spans = Vec::new();
        for found in snapshot.viewport_matches(&mut self.match_buf).map_err(eng)? {
            let edges = (viewport_edge(term, &found.start(), rows)?, viewport_edge(term, &found.end(), rows)?);
            let is_current = selected == Some(edges);
            spans.extend(
                match_spans(edges.0, edges.1, cols, rows)
                    .into_iter()
                    .map(|(y, x0, x1)| MatchSpan { y, x0, x1, current: is_current }),
            );
        }
        spans.sort_by_key(|m| (m.y, m.x0));
        Ok((spans, Some(SearchState { total, current })))
    }

    /// Scroll so that the selected match is visible: untouched when its first row is already on the viewport,
    /// centred on that row otherwise. Without a search or a selected match nothing happens.
    fn reveal_selected(&mut self) -> Result<()> {
        let Some(search) = self.search.as_mut() else { return Ok(()) };
        let rows_u16 = self.terminal.rows().map_err(eng)?;
        let rows = usize::from(rows_u16);
        let top = {
            let snapshot = search.snapshot(&mut self.terminal).map_err(eng)?;
            let Some(selected) = snapshot.selected_match().map_err(eng)? else { return Ok(()) };
            let term = snapshot.terminal();
            let start = selected.start();
            if matches!(viewport_edge(term, &start, rows_u16)?, Edge::In(..)) {
                return Ok(());
            }
            let Some(at) = term.point_from_grid_ref(&start, PointSpace::Screen).map_err(eng)? else {
                return Ok(());
            };
            (at.y as usize).saturating_sub(rows / 2)
        };
        // A match near the end must not be centred past the last page.
        let last_top = self.terminal.total_rows().map_err(eng)?.saturating_sub(rows);
        self.terminal.scroll_viewport(ScrollViewport::Row(top.min(last_top)));
        Ok(())
    }

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
        // The pinned libghostty revision trips its PageList integrity assertion (an abort of the whole process in a
        // debug build, an inconsistent viewport otherwise) when the row count grows while the viewport is scrolled
        // into history and the grown screen needs more rows than exist. Measured: it needs rows to grow, the view to
        // be scrolled back and too little content; shrinking and column changes never trigger it. Growing from the
        // live screen is safe, so a grow while scrolled back first returns to the live screen.
        if rows > self.terminal.rows().map_err(eng)? {
            let sb = self.terminal.scrollbar().map_err(eng)?;
            if sb.offset + sb.len < sb.total {
                self.terminal.scroll_viewport(ScrollViewport::Bottom);
            }
        }
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
        // The search is caught up first: it needs the terminal exclusively, which the render state below borrows.
        let (matches, search) = self.search_view()?;
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
        let reset = self.force_full || self.last_size != Some((cols, rows));
        let full = reset || snapshot.dirty().map_err(eng)? == Dirty::Full;
        let cursor = read_cursor(&snapshot)?;

        // The cache has one entry per viewport row. A size change is always a full pass, which refreshes every
        // entry, so starting from blanks loses nothing.
        if self.sel_cache.len() != usize::from(rows) {
            self.sel_cache.clear();
            self.sel_cache.resize(usize::from(rows), None);
        }

        let mut changed = Vec::new();
        {
            let mut row_iter = self.rows_iter.update(&snapshot).map_err(eng)?;
            if full {
                // A full pass: libghostty's per-row flags are conservative, every row is read.
                let mut y: u16 = 0;
                while let Some(row) = row_iter.next() {
                    let mut cells = Vec::with_capacity(cols as usize);
                    read_row(&mut self.cells_iter, row, &colors, &mut cells)?;
                    if let Some(slot) = self.sel_cache.get_mut(usize::from(y)) {
                        *slot = row.selection().map_err(eng)?.map(|s| (s.start_x, s.end_x));
                    }
                    changed.push(RowUpdate { y, cells });
                    y = y.saturating_add(1);
                }
            } else {
                while let Some((y, row)) = row_iter.next_dirty() {
                    let mut cells = Vec::with_capacity(cols as usize);
                    read_row(&mut self.cells_iter, row, &colors, &mut cells)?;
                    if let Some(slot) = self.sel_cache.get_mut(usize::from(y)) {
                        *slot = row.selection().map_err(eng)?.map(|s| (s.start_x, s.end_x));
                    }
                    changed.push(RowUpdate { y, cells });
                }
            }
        }
        // The complete list in viewport order, whatever part of it was visited: unvisited rows keep their range.
        let selection: Vec<Span> = self
            .sel_cache
            .iter()
            .enumerate()
            .filter_map(|(y, range)| {
                let (x0, x1) = (*range)?;
                Some(Span { y: y as u16, x0: x0.min(x1), x1: x1.min(cols.saturating_sub(1)) })
            })
            .collect();
        // Only after the rows were consumed: clean() clears the global and the per-row flags.
        snapshot.clean().map_err(eng)?;
        self.force_full = false;
        self.last_size = Some((cols, rows));

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
            selection,
            matches,
            search,
            copy_cursor: None,
        })
    }

    fn request_full(&mut self) {
        self.force_full = true;
    }

    fn set_theme(&mut self, theme: &EngineTheme) -> Result<()> {
        let rgb = |c: Color| RgbColor { r: c.r, g: c.g, b: c.b };
        // The sixteen colours of the theme go into a default palette, and the 6x6x6 cube and the grey ramp are
        // derived from background and foreground (interpolated, entries 0 to 15 are kept).
        let mut base = Palette::default();
        for (i, c) in theme.ansi.iter().enumerate() {
            base.set(PaletteIndex(i as u8), rgb(*c));
        }
        let palette = Palette::generate(Some(&base), None, rgb(theme.bg), rgb(theme.fg), true);
        self.terminal
            .set_default_fg_color(Some(rgb(theme.fg)))
            .map_err(eng)?
            .set_default_bg_color(Some(rgb(theme.bg)))
            .map_err(eng)?
            .set_default_cursor_color(Some(rgb(theme.cursor)))
            .map_err(eng)?
            .set_default_color_palette(Some(palette))
            .map_err(eng)?;
        // Everything is dirty after this; the consumer replaces its mirror.
        self.force_full = true;
        Ok(())
    }

    fn take_events(&mut self) -> Vec<EngineEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    fn select(&mut self, op: SelectOp) -> Result<()> {
        match op {
            SelectOp::Begin { x, y, mode } => {
                let point = self.clamped_point(x, y)?;
                let at = self.terminal.grid_ref(point).map_err(eng)?;
                // The derived selection of a word or a line; a point on nothing selectable (blank cells for a
                // word) falls back to the one cell under the pointer, so a drag that follows still has an anchor.
                let derived = match mode {
                    SelectMode::Cell => None,
                    SelectMode::Word => {
                        self.terminal.select_word(SelectWordOptions::new(at.clone())).map_err(eng)?
                    }
                    SelectMode::Line => {
                        self.terminal.select_line(SelectLineOptions::new(at.clone())).map_err(eng)?
                    }
                };
                let selection = derived.unwrap_or_else(|| Selection::new(at.clone(), at, false));
                self.terminal.set_selection(Some(&selection)).map_err(eng)?;
            }
            SelectOp::Extend { x, y } => {
                // The terminal's own tracked anchor is the start of its current selection; nothing is cached here.
                // Without a selection there is nothing to extend, which is not an error.
                let Some(current) = self.terminal.selection().map_err(eng)? else { return Ok(()) };
                let to = self.terminal.grid_ref(self.clamped_point(x, y)?).map_err(eng)?;
                let extended = Selection::new(current.start(), to, current.is_rectangle());
                self.terminal.set_selection(Some(&extended)).map_err(eng)?;
            }
            SelectOp::All => {
                let all = self.terminal.select_all().map_err(eng)?;
                self.terminal.set_selection(all.as_ref()).map_err(eng)?;
            }
            SelectOp::Clear => {
                self.terminal.set_selection(None).map_err(eng)?;
            }
        }
        Ok(())
    }

    fn search(&mut self, op: SearchOp) -> Result<SearchState> {
        match &op {
            SearchOp::Set(needle) => {
                if needle.len() > MAX_SEARCH_BYTES {
                    // Before anything is touched: the earlier search stays as it was.
                    return Err(Error::Engine(format!(
                        "search needle is {} bytes, the limit is {MAX_SEARCH_BYTES}",
                        needle.len()
                    )));
                }
                if needle.is_empty() {
                    self.search = None;
                    self.search_memo = None;
                    return Ok(SearchState::default());
                }
                // A fresh search every time: results of an equal needle would be kept by the library, but the
                // contract is that `Set` starts over on the newest match. The old search is replaced only when the
                // new one is ready, so a failure leaves it alone.
                let mut fresh = Search::new(&mut self.terminal).map_err(eng)?;
                // The engine scrolls itself (centred); the library would put the match on the nearest edge.
                fresh.set_scroll(Scroll::None).map_err(eng)?;
                fresh.set_needle(&mut self.terminal, needle.as_bytes()).map_err(eng)?;
                fresh.run(&mut self.terminal).map_err(eng)?;
                // The first step goes to the newest match, the one nearest the prompt.
                fresh.select_next(&mut self.terminal).map_err(eng)?;
                self.search = Some(fresh);
                self.search_memo = None;
                self.reveal_selected()?;
            }
            SearchOp::Next | SearchOp::Prev => {
                let Some(search) = self.search.as_mut() else { return Ok(SearchState::default()) };
                // The selection may have been dropped by a resize since the last update; the step must count from
                // where the user was.
                search.run(&mut self.terminal).map_err(eng)?;
                restore_selection(search, &mut self.terminal, self.search_memo)?;
                // The library orders from the newest match: its "next" is older, which is the engine's `Prev`.
                let moved = if op == SearchOp::Next {
                    search.select_prev(&mut self.terminal)
                } else {
                    search.select_next(&mut self.terminal)
                }
                .map_err(eng)?;
                if moved {
                    self.reveal_selected()?;
                }
            }
            SearchOp::Clear => {
                // Dropping the search releases the library's tracked state. The viewport stays where it is.
                self.search = None;
                self.search_memo = None;
                return Ok(SearchState::default());
            }
        }
        Ok(self.search_state()?.unwrap_or_default())
    }

    fn selection_text(&mut self) -> Result<Option<String>> {
        // Soft wraps are joined and trailing blanks trimmed, so the text is what the user would paste.
        let options = FormatOptions::new().with_unwrap(true).with_trim(true);
        let bytes = self.terminal.format_selection_alloc(None, options).map_err(eng)?;
        // A selection of nothing but blanks has no text to copy: that is no text, like no selection.
        Ok(bytes
            .map(|b| String::from_utf8_lossy(b.as_ref()).into_owned())
            .filter(|text| !text.is_empty()))
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

    /// The columns the cursor advances for `text`, summed over its grapheme clusters as the library measures them.
    fn library_width(text: &str) -> usize {
        let chars: Vec<char> = text.chars().collect();
        let mut rest = chars.as_slice();
        let mut total = 0;
        while !rest.is_empty() {
            let (consumed, width) = libghostty_vt::unicode::grapheme_width(rest);
            assert!(consumed > 0, "the library consumes at least one character of {text:?}");
            total += usize::from(width);
            rest = &rest[consumed..];
        }
        total
    }

    #[test]
    fn the_cursor_advances_by_the_librarys_grapheme_width() {
        // The terminal's own layout and the function a UI can call must agree (DESK-27). Each text is fed at the
        // start of an empty row.
        let cases = [
            "e\u{0301}",
            "\u{65E5}\u{672C}",
            "\u{1F600}",
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
            "\u{1F1E8}\u{1F1ED}",
            "\u{2764}\u{FE0F}",
            "\u{1F44D}\u{1F3FD}",
            "\u{1112}\u{1161}\u{11AB}",
            "\u{65E5}\u{0301}",
            "abc",
            "a\u{65E5}b\u{1F600}c",
        ];
        for text in cases {
            let mut engine = GhosttyEngine::new(40, 3, 100).expect("engine");
            engine.feed(text.as_bytes());
            let advance = engine.update().expect("update").cursor.expect("cursor").x;
            assert_eq!(usize::from(advance), library_width(text), "{text:?}");
        }
    }

    #[test]
    fn a_single_cluster_is_consumed_whole() {
        for text in ["\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}", "\u{1F1E8}\u{1F1ED}", "e\u{0301}"] {
            let chars: Vec<char> = text.chars().collect();
            let (consumed, _) = libghostty_vt::unicode::grapheme_width(&chars);
            assert_eq!(consumed, chars.len(), "{text:?} is one cluster");
        }
    }
}
