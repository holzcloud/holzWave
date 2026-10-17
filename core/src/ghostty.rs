use std::cell::{Cell as StdCell, RefCell};
use std::rc::Rc;

use libghostty_vt::key::{self, Action, Key, OptionAsAlt};
use libghostty_vt::mouse::{self, EncoderSize};
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
    Cell, Color, CopyMotion, CopyOp, CopyState, Cursor, CursorStyle, EngineEvent, EngineTheme, KeyAction, KeyEvent,
    MAX_SEARCH_BYTES, MatchSpan, Modes, Mods, MouseAction, MouseButton, MouseEvent, PasteOutcome, RowUpdate, Screen,
    Scrollbar, SearchOp, SearchState, SelectMode, SelectOp, Span, TerminalEngine, Update, WheelEvent, WheelOutcome,
    Width, sanitize_program_text,
};
use crate::textread::{Cut, MAX_READ_BYTES, TextBuilder, TextRead, TextRegion, rows_within_budget};
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

/// The value of a frame decoration, or what stands in for it when it cannot be read.
///
/// A decoration (match boxes, the copy cursor) is not worth the frame: an error is written to stderr, so it leaves a
/// trace, and the frame goes out without it. `fallback` runs only for an error.
fn degraded<T>(what: &str, result: Result<T>, fallback: impl FnOnce() -> T) -> T {
    match result {
        Ok(value) => value,
        Err(e) => {
            eprintln!("holzwave: {what}: {e}");
            fallback()
        }
    }
}

/// The last `max_bytes` bytes of `text`, moved forward to the next character boundary.
///
/// The result is valid UTF-8 and may be a few bytes shorter than the bound. The tail is kept on purpose: the end of a
/// command's output holds the error or the result a reader needs most.
fn cap_tail(text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let mut start = text.len() - max_bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_owned()
}

/// The most code points of one cell's cluster that a read takes. A cell with more (a program can pile combining marks
/// onto one cell without end) reads as its primary code point, so one hostile cell costs a few bytes, not kilobytes.
const MAX_CLUSTER_CHARS: usize = 32;

/// What one cell contributes to a read.
enum CellText {
    /// The spacer half of a wide character: its head cell has the text.
    Skip,
    /// Nothing to show.
    Blank,
    Text(String),
}

/// The text of the cell at `at`.
///
/// A cluster that does not fit [`MAX_CLUSTER_CHARS`], or whose size the library does not tell, falls back to the
/// cell's primary code point.
fn cell_text(at: &GridRef<'_>) -> Result<CellText> {
    let cell = at.cell().map_err(eng)?;
    if matches!(cell.wide().map_err(eng)?, CellWide::SpacerTail | CellWide::SpacerHead) {
        return Ok(CellText::Skip);
    }
    if !cell.has_text().map_err(eng)? {
        return Ok(CellText::Blank);
    }
    let mut small = ['\0'; 16];
    let text: String = match at.graphemes(&mut small) {
        Ok(n) => small[..n.min(small.len())].iter().collect(),
        Err(VtError::OutOfSpace { required }) if (1..=MAX_CLUSTER_CHARS).contains(&required) => {
            let mut big = vec!['\0'; required];
            match at.graphemes(&mut big) {
                Ok(n) => big[..n.min(big.len())].iter().collect(),
                Err(VtError::OutOfSpace { .. }) => primary_char(&cell)?,
                Err(e) => return Err(eng(e)),
            }
        }
        Err(VtError::OutOfSpace { .. }) => primary_char(&cell)?,
        Err(e) => return Err(eng(e)),
    };
    Ok(if text.is_empty() { CellText::Blank } else { CellText::Text(text) })
}

/// The primary code point of a cell as text; empty when it is not a valid character.
fn primary_char(cell: &libghostty_vt::screen::Cell) -> Result<String> {
    Ok(char::from_u32(cell.codepoint().map_err(eng)?).map(String::from).unwrap_or_default())
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

/// The most wheel reports or arrow-key sequences one wheel event produces (T-02-93). A flood of rows from a fast
/// trackpad becomes a bounded burst; the page sends the rest as further events.
const MAX_WHEEL_REPORTS: u32 = 10;

/// Bits of [`GhosttyEngine::buttons`].
const BUTTON_LEFT: u8 = 1;
const BUTTON_MIDDLE: u8 = 2;
const BUTTON_RIGHT: u8 = 4;

/// A pointer coordinate in device pixels brought into `0..extent`: a value that is not finite or negative becomes 0
/// and one past the end becomes the last pixel, so the encoder only ever sees a position on the grid (T-02-94).
fn clamp_px(value: f32, extent: u32) -> f32 {
    if !value.is_finite() || value < 0.0 {
        return 0.0;
    }
    value.min(extent.max(1).saturating_sub(1) as f32)
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

/// The most cells a word motion looks at before it gives up and leaves the cursor where it is.
const MAX_COPY_SCAN: usize = 1 << 16;

/// A cell in screen coordinates: the column and the row counted from the top of the scrollback.
type ScreenCell = (u16, u32);

/// The cell after `at` in reading order, `None` at the very last cell.
fn next_cell(at: ScreenCell, cols: u16, last_row: u32) -> Option<ScreenCell> {
    let (x, y) = at;
    if x.saturating_add(1) < cols {
        Some((x + 1, y))
    } else if y < last_row {
        Some((0, y + 1))
    } else {
        None
    }
}

/// The cell before `at` in reading order, `None` at the very first cell.
fn prev_cell(at: ScreenCell, cols: u16) -> Option<ScreenCell> {
    let (x, y) = at;
    if x > 0 {
        Some((x - 1, y))
    } else if y > 0 {
        Some((cols.saturating_sub(1), y - 1))
    } else {
        None
    }
}

/// Copy mode: the keyboard cursor is the end of the terminal's own selection, which the library keeps attached to
/// the text, so output and scrolling never move it off what it points at.
#[derive(Clone, Copy, Debug)]
struct CopyModeState {
    /// Motions extend the selection from the anchor; otherwise the selection is the one cell under the cursor.
    selecting: bool,
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
    /// `Some` while copy mode is active.
    copy: Option<CopyModeState>,
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
    mouse_encoder: mouse::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    /// The cell size in device pixels as the last `resize` told it, `(0, 0)` while unknown.
    cell_px: (u32, u32),
    /// The buttons held down, as `BUTTON_*` bits. Kept here so that button-event tracking and the any-button state
    /// of the encoder are right without the caller supplying them, and so that a lost release cannot outlive the
    /// program's own interest in the mouse (T-02-95).
    buttons: u8,
    /// The cell of the last mouse report, for dropping a motion that stays inside one cell. Kept here and not in the
    /// library's encoder: that one forgets it whenever the options are set from the terminal, which has to happen
    /// for every event because the program may change its modes between any two of them.
    last_cell: Option<(u32, u32)>,
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
            mouse_encoder: mouse::Encoder::new().map_err(eng)?,
            mouse_event: mouse::Event::new().map_err(eng)?,
            cell_px: (0, 0),
            buttons: 0,
            last_cell: None,
            option_as_alt: false,
            force_full: true,
            last_size: None,
            sel_cache: Vec::new(),
            events,
            search: None,
            match_buf: MatchBuffer::new(),
            search_memo: None,
            copy: None,
        })
    }

    /// The output of the command at `screen_row`, as plain text, at most `max_bytes` bytes (the tail is kept, cut on
    /// a character boundary).
    ///
    /// `screen_row` counts from the top of the scrollback and the column is always 0, so the answer does not depend
    /// on how far the user has scrolled. The region is whatever the program's OSC 133 marks declare: the text is
    /// untrusted program output and may be forged or hidden by the program. Nothing is sanitised or redacted here;
    /// that belongs to the context gate of the caller. Soft-wrapped lines are joined and trailing blanks trimmed.
    /// `None` when `max_bytes` is 0, the row is past the end, or the row is not part of any command's output. The
    /// user's selection is neither read nor changed. The call belongs to the engine's own thread (the engine is not
    /// `Send`); the library allocation is bounded by the scrollback limit and the returned string by `max_bytes`.
    pub fn output_at(&self, screen_row: u32, max_bytes: usize) -> Result<Option<String>> {
        if max_bytes == 0 {
            return Ok(None);
        }
        let total = self.terminal.total_rows().map_err(eng)?;
        if screen_row as usize >= total {
            return Ok(None);
        }
        let at = self.screen_ref((0, screen_row))?;
        // A snapshot: it is not installed as the terminal's selection.
        let Some(sel) = self.terminal.select_output(at).map_err(eng)? else {
            return Ok(None);
        };
        let options = FormatOptions::new().with_selection(&sel).with_unwrap(true).with_trim(true);
        let Some(bytes) = self.terminal.format_selection_alloc(None, options).map_err(eng)? else {
            return Ok(None);
        };
        let text = String::from_utf8_lossy(bytes.as_ref());
        let text = text.trim_end_matches('\n');
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some(cap_tail(text.to_owned(), max_bytes)))
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

    /// The copy cursor on the viewport; `None` outside copy mode and when the cursor is scrolled out of sight.
    fn copy_cursor_in_viewport(&self) -> Result<Option<(u16, u16)>> {
        if self.copy.is_none() {
            return Ok(None);
        }
        let Some(selection) = self.terminal.selection().map_err(eng)? else { return Ok(None) };
        let rows = self.terminal.rows().map_err(eng)?;
        Ok(match viewport_edge(&self.terminal, &selection.end(), rows)? {
            Edge::In(x, y) => Some((x, y)),
            Edge::Above | Edge::Below => None,
        })
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

    /// The grid reference of a screen cell.
    fn screen_ref(&self, at: ScreenCell) -> Result<GridRef<'_>> {
        self.terminal.grid_ref(Point::Screen(PointCoordinate { x: at.0, y: at.1 })).map_err(eng)
    }

    /// A reference converted to screen coordinates; `None` when the library cannot place it.
    fn screen_point(&self, at: &GridRef<'_>) -> Result<Option<ScreenCell>> {
        let point = self.terminal.point_from_grid_ref(at, PointSpace::Screen).map_err(eng)?;
        Ok(point.map(|p| (p.x, p.y)))
    }

    /// The last row of the screen, counting the scrollback.
    fn last_screen_row(&self) -> Result<u32> {
        let total = self.terminal.total_rows().map_err(eng)?;
        Ok(u32::try_from(total.saturating_sub(1)).unwrap_or(u32::MAX))
    }

    /// Whether a cell shows nothing: never written, or a blank. A spacer half of a wide character counts as blank,
    /// its head cell is the one with text.
    fn cell_is_blank(&self, at: ScreenCell) -> Result<bool> {
        let cell = self.screen_ref(at)?.cell().map_err(eng)?;
        if !cell.has_text().map_err(eng)? {
            return Ok(true);
        }
        Ok(char::from_u32(cell.codepoint().map_err(eng)?).is_none_or(char::is_whitespace))
    }

    /// The text of the screen cells from `from` to `to` as a bounded read.
    ///
    /// A linear read runs from `from` to `to` in reading order, a rectangle takes the columns between the two corners
    /// on every row. Only `grid_ref` reads are used: the selection, the scroll position and the render state are not
    /// touched, so the next frame is what it would have been without the read. Rows past the row and cell budget are
    /// left out (the tail of the range is kept for [`Cut::KeepTail`], the head for [`Cut::KeepHead`]) and the result
    /// says so.
    fn read_cells(
        &self,
        from: ScreenCell,
        to: ScreenCell,
        rectangle: bool,
        cut: Cut,
        max_bytes: usize,
    ) -> Result<Option<TextRead>> {
        let cols = self.terminal.cols().map_err(eng)?;
        let to = (to.0, to.1.min(self.last_screen_row()?));
        if cols == 0 || from.1 > to.1 {
            return Ok(None);
        }
        let last_col = cols - 1;
        let (mut first_row, mut last_row) = (from.1, to.1);
        let (mut first_col, mut end_col) = (from.0.min(last_col), to.0.min(last_col));
        let (rect_left, rect_right) = (first_col.min(end_col), first_col.max(end_col));
        let wanted = to.1 - from.1 + 1;
        let allowed = rows_within_budget(cols, wanted);
        let mut builder = TextBuilder::new(cut);
        if allowed < wanted {
            builder.mark_truncated();
            match cut {
                Cut::KeepTail => {
                    first_row = to.1 + 1 - allowed;
                    first_col = 0;
                }
                Cut::KeepHead => {
                    last_row = from.1 + allowed - 1;
                    end_col = last_col;
                }
            }
        }
        for y in first_row..=last_row {
            let (x0, x1) = if rectangle {
                (rect_left, rect_right)
            } else {
                (if y == first_row { first_col } else { 0 }, if y == last_row { end_col } else { last_col })
            };
            for x in x0..=x1 {
                match cell_text(&self.screen_ref((x, y))?)? {
                    CellText::Skip => {}
                    CellText::Blank => builder.blank(),
                    CellText::Text(text) => builder.cell(&text, false),
                }
            }
            // The last row ends the text; a rectangle never joins rows.
            let wrapped = !rectangle && y != last_row && self.screen_ref((0, y))?.row().map_err(eng)?.is_wrapped().map_err(eng)?;
            builder.end_row(wrapped);
        }
        Ok(builder.finish(max_bytes))
    }

    /// The region as text. `max_bytes` is already clamped by the caller.
    fn read_region(&self, region: TextRegion, max_bytes: usize) -> Result<Option<TextRead>> {
        match region {
            TextRegion::Screen => {
                let bar = self.terminal.scrollbar().map_err(eng)?;
                if bar.len == 0 {
                    return Ok(None);
                }
                let top = u32::try_from(bar.offset).unwrap_or(u32::MAX);
                let bottom = u32::try_from(bar.offset + bar.len - 1).unwrap_or(u32::MAX);
                let last_col = self.terminal.cols().map_err(eng)?.saturating_sub(1);
                self.read_cells((0, top), (last_col, bottom), false, Cut::KeepTail, max_bytes)
            }
        }
    }

    /// The copy cursor in screen coordinates: the end of the terminal's selection.
    fn copy_cursor_cell(&self) -> Result<Option<ScreenCell>> {
        let Some(selection) = self.terminal.selection().map_err(eng)? else { return Ok(None) };
        self.screen_point(&selection.end())
    }

    /// Install the copy selection from `anchor` to `cursor` (both on the screen). The cursor is the end that moves.
    fn install_copy_selection(&self, anchor: ScreenCell, cursor: ScreenCell) -> Result<()> {
        let selection = Selection::new(self.screen_ref(anchor)?, self.screen_ref(cursor)?, false);
        self.terminal.set_selection(Some(&selection)).map_err(eng)?;
        Ok(())
    }

    /// Scroll the viewport the least that puts screen row `y` on it; nothing when it already is.
    fn reveal_row(&mut self, y: u32) -> Result<()> {
        let bar = self.terminal.scrollbar().map_err(eng)?;
        let y = u64::from(y);
        let top = if y < bar.offset {
            y
        } else if y >= bar.offset + bar.len {
            y + 1 - bar.len
        } else {
            return Ok(());
        };
        self.terminal.scroll_viewport(ScrollViewport::Row(usize::try_from(top).unwrap_or(usize::MAX)));
        Ok(())
    }

    /// Start copy mode (or start it again after the library dropped the selection): a one-cell selection on the
    /// terminal's cursor, or on the first column of the last row when the cursor is hidden.
    fn copy_enter(&mut self) -> Result<()> {
        let rows = self.terminal.rows().map_err(eng)?;
        let at = if self.terminal.is_cursor_visible().map_err(eng)? {
            PointCoordinate {
                x: self.terminal.cursor_x().map_err(eng)?,
                y: u32::from(self.terminal.cursor_y().map_err(eng)?),
            }
        } else {
            PointCoordinate { x: 0, y: u32::from(rows.saturating_sub(1)) }
        };
        let start = self.terminal.grid_ref(Point::Active(at)).map_err(eng)?;
        let Some(cell) = self.screen_point(&start)? else {
            return Err(Error::Engine("the cursor is not on the screen".into()));
        };
        self.install_copy_selection(cell, cell)?;
        self.copy = Some(CopyModeState { selecting: false });
        self.reveal_row(cell.1)
    }

    /// The word under `at`, as its first and last cell (the library's own notion of a word, which also covers soft
    /// wraps). A run of blanks or of separators is a "word" of its own; a cell that was never written has none.
    fn word_at(&self, at: ScreenCell) -> Result<Option<(ScreenCell, ScreenCell)>> {
        let Some(word) = self.terminal.select_word(SelectWordOptions::new(self.screen_ref(at)?)).map_err(eng)? else {
            return Ok(None);
        };
        Ok(self.screen_point(&word.start())?.zip(self.screen_point(&word.end())?))
    }

    /// The first (`last` false) or last non-blank cell of a row.
    fn row_edge(&self, y: u32, last: bool, cols: u16) -> Result<Option<u16>> {
        let mut found = None;
        for x in 0..cols {
            if !self.cell_is_blank((x, y))? {
                found = Some(x);
                if !last {
                    break;
                }
            }
        }
        Ok(found)
    }

    /// Where a motion takes the cursor from `from`. Always a cell inside the screen.
    fn copy_target(&self, from: ScreenCell, motion: CopyMotion) -> Result<ScreenCell> {
        let cols = self.terminal.cols().map_err(eng)?;
        let rows = u32::from(self.terminal.rows().map_err(eng)?);
        let last_row = self.last_screen_row()?;
        let last_col = cols.saturating_sub(1);
        // A column or row from before a resize is brought back into the grid first.
        let (x, y) = (from.0.min(last_col), from.1.min(last_row));
        Ok(match motion {
            CopyMotion::Left => (x.saturating_sub(1), y),
            CopyMotion::Right => (x.saturating_add(1).min(last_col), y),
            CopyMotion::Up => (x, y.saturating_sub(1)),
            CopyMotion::Down => (x, y.saturating_add(1).min(last_row)),
            CopyMotion::LineStart => (self.row_edge(y, false, cols)?.unwrap_or(0), y),
            CopyMotion::LineEnd => (self.row_edge(y, true, cols)?.unwrap_or(0), y),
            CopyMotion::Top => (0, 0),
            CopyMotion::Bottom => (0, last_row),
            CopyMotion::PageUp => (x, y.saturating_sub(rows)),
            CopyMotion::PageDown => (x, y.saturating_add(rows).min(last_row)),
            CopyMotion::WordForward => {
                // Past the end of the word under the cursor, then past blanks (and over line ends) to the first
                // non-blank cell. Nothing after it: the cursor stays.
                let mut at = self.word_at((x, y))?.map_or((x, y), |(_, end)| end);
                let mut found = None;
                for _ in 0..MAX_COPY_SCAN {
                    let Some(next) = next_cell(at, cols, last_row) else { break };
                    at = next;
                    if !self.cell_is_blank(at)? {
                        found = Some(at);
                        break;
                    }
                }
                found.unwrap_or((x, y))
            }
            CopyMotion::WordBack => {
                // Back over blanks to the last cell of the previous word, then to that word's first cell. From the
                // middle of a word the cell before is in the same word, so that is this word's start.
                let Some(mut at) = prev_cell((x, y), cols) else { return Ok((x, y)) };
                let mut steps = 0;
                while self.cell_is_blank(at)? {
                    match prev_cell(at, cols) {
                        Some(before) => at = before,
                        None => return Ok((0, 0)),
                    }
                    steps += 1;
                    if steps > MAX_COPY_SCAN {
                        return Ok((x, y));
                    }
                }
                self.word_at(at)?.map_or(at, |(start, _)| start)
            }
        })
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

    /// Move the viewport for a wheel event. No cap: a viewport move costs the same for any distance.
    fn scroll_locally(&mut self, rows: i32) -> WheelOutcome {
        self.terminal.scroll_viewport(ScrollViewport::Delta(rows as isize));
        WheelOutcome::Scrolled
    }

    /// Encode one pointer event against the terminal's live mouse modes.
    ///
    /// The position is clamped onto the grid first. An unknown cell size is a documented case, not an error: the
    /// encoder insists on a non-zero cell, so it gets a 1 x 1 pixel cell and the position is cell (0, 0).
    ///
    /// Nothing is held while the program does not track the mouse: a press that arrives then is not remembered, and
    /// whatever was remembered is dropped, so a program that turns tracking on later never sees a phantom drag.
    fn encode_pointer(&mut self, event: &MouseEvent) -> Result<Vec<u8>> {
        if !self.terminal.is_mouse_tracking().map_err(eng)? {
            self.release_buttons();
            return Ok(Vec::new());
        }
        let bit = match event.button {
            MouseButton::None => 0,
            MouseButton::Left => BUTTON_LEFT,
            MouseButton::Middle => BUTTON_MIDDLE,
            MouseButton::Right => BUTTON_RIGHT,
        };
        match event.action {
            MouseAction::Press => self.buttons |= bit,
            MouseAction::Release => self.buttons &= !bit,
            MouseAction::Motion => {}
        }
        // A motion says which button is held, or says nothing: then it is the one the engine saw pressed.
        let held = match event.button {
            MouseButton::None => match self.buttons {
                b if b & BUTTON_LEFT != 0 => Some(mouse::Button::Left),
                b if b & BUTTON_MIDDLE != 0 => Some(mouse::Button::Middle),
                b if b & BUTTON_RIGHT != 0 => Some(mouse::Button::Right),
                _ => None,
            },
            MouseButton::Left => Some(mouse::Button::Left),
            MouseButton::Middle => Some(mouse::Button::Middle),
            MouseButton::Right => Some(mouse::Button::Right),
        };
        let action = match event.action {
            MouseAction::Press => mouse::Action::Press,
            MouseAction::Release => mouse::Action::Release,
            MouseAction::Motion => mouse::Action::Motion,
        };
        self.encode_pointer_at(action, held, event.mods, event.x, event.y)
    }

    /// Forget every held button and the last reported cell.
    fn release_buttons(&mut self) {
        self.buttons = 0;
        self.last_cell = None;
    }

    /// Run the mouse encoder for one pointer event: clamp the position, describe the grid, encode.
    fn encode_pointer_at(
        &mut self,
        action: mouse::Action,
        button: Option<mouse::Button>,
        mods: Mods,
        x: f32,
        y: f32,
    ) -> Result<Vec<u8>> {
        let cols = u32::from(self.terminal.cols().map_err(eng)?);
        let rows = u32::from(self.terminal.rows().map_err(eng)?);
        let known = self.cell_px.0 > 0 && self.cell_px.1 > 0;
        let (cell_width, cell_height) = if known { self.cell_px } else { (1, 1) };
        let size = EncoderSize {
            screen_width: cols.saturating_mul(cell_width),
            screen_height: rows.saturating_mul(cell_height),
            cell_width,
            cell_height,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        };
        let (px, py) =
            if known { (clamp_px(x, size.screen_width), clamp_px(y, size.screen_height)) } else { (0.0, 0.0) };

        // A motion inside the cell of the previous report is not news, except in SGR-Pixels, which reports pixels.
        let cell = ((px as u32) / cell_width, (py as u32) / cell_height);
        if action == mouse::Action::Motion
            && self.last_cell == Some(cell)
            && !self.terminal.mode(Mode::SGR_PIXELS_MOUSE).map_err(eng)?
        {
            return Ok(Vec::new());
        }

        self.mouse_event
            .set_action(action)
            .set_button(button)
            .set_mods(key_mods(mods))
            .set_position(mouse::Position { x: px, y: py });
        let mut out = Vec::with_capacity(32);
        // The options of the terminal first: they reset what the calls after them set.
        self.mouse_encoder
            .set_options_from_terminal(&self.terminal)
            .set_size(size)
            .set_any_button_pressed(self.buttons != 0)
            .encode_to_vec(&self.mouse_event, &mut out)
            .map_err(eng)?;
        if !out.is_empty() {
            self.last_cell = Some(cell);
        }
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
        self.terminal.resize(cols, rows, cell_px.0, cell_px.1).map_err(eng)?;
        self.cell_px = cell_px;
        // The cells moved under the pointer: the last reported cell means nothing now, and a drag that was in
        // progress is over as far as the program is concerned.
        self.release_buttons();
        Ok(())
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

    fn mouse(&mut self, event: &MouseEvent) -> Result<Vec<u8>> {
        self.encode_pointer(event)
    }

    fn wheel(&mut self, event: &WheelEvent) -> Result<WheelOutcome> {
        if event.rows == 0 {
            return Ok(WheelOutcome::Nothing);
        }
        // `unsigned_abs`: the most negative i32 has no positive twin.
        let count = event.rows.unsigned_abs().min(MAX_WHEEL_REPORTS);
        let up = event.rows < 0;

        // Shift is the escape hatch of every terminal: the wheel moves this terminal's own scrollback, whatever the
        // program asked for.
        if event.mods.contains(Mods::SHIFT) {
            return Ok(self.scroll_locally(event.rows));
        }

        if self.terminal.is_mouse_tracking().map_err(eng)? {
            // Wheel up is button four, wheel down button five, one press per row. A wheel report is not a click: it
            // holds no button and does not count as the last reported cell for motion.
            let button = if up { mouse::Button::Four } else { mouse::Button::Five };
            let last_cell = self.last_cell;
            let mut bytes = Vec::new();
            let mut result = Ok(());
            for _ in 0..count {
                match self.encode_pointer_at(mouse::Action::Press, Some(button), event.mods, event.x, event.y) {
                    Ok(report) => bytes.extend_from_slice(&report),
                    Err(e) => {
                        result = Err(e);
                        break;
                    }
                }
            }
            self.last_cell = last_cell;
            result?;
            // A tracking mode with no wheel in it (X10): the program decided, nothing is scrolled behind its back.
            return Ok(if bytes.is_empty() { WheelOutcome::Nothing } else { WheelOutcome::Bytes(bytes) });
        }

        let alternate = self.terminal.active_screen().map_err(eng)? == VtScreen::Alternate;
        if alternate && self.terminal.mode(Mode::ALT_SCROLL).map_err(eng)? {
            // The program has no scrollback to offer, so the wheel is the arrow keys, which also work in programs
            // that never heard of the mouse. They are encoded like typed ones: application cursor mode applies.
            let key = if up { Key::ArrowUp } else { Key::ArrowDown };
            let mut bytes = Vec::new();
            for _ in 0..count {
                let arrow = self.encode_event(
                    Action::Press,
                    key,
                    key_mods(event.mods),
                    key::Mods::empty(),
                    false,
                    None,
                    None,
                )?;
                bytes.extend_from_slice(&arrow);
            }
            return Ok(WheelOutcome::Bytes(bytes));
        }

        Ok(self.scroll_locally(event.rows))
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
        // Match boxes and the copy cursor are decorations of the frame: when one cannot be read the frame still
        // goes out without it, instead of the whole pane going without frames for as long as the state is bad.
        let search_view = self.search_view();
        // Without the view the counts are still read when the library lets them be, so the bar keeps its numbers.
        let (matches, search) =
            degraded("search view", search_view, || (Vec::new(), self.search_state().unwrap_or(None)));
        let copy_cursor = degraded("copy cursor", self.copy_cursor_in_viewport(), || None);
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
            copy_cursor,
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

    fn copy_mode(&mut self, op: CopyOp) -> Result<CopyState> {
        let mut yanked = None;
        match op {
            CopyOp::Enter => {
                // Entering again keeps the cursor where it is, unless the library dropped the selection meanwhile.
                if self.copy.is_none() || self.copy_cursor_cell()?.is_none() {
                    self.copy_enter()?;
                }
            }
            CopyOp::Exit => {
                // Only a selection copy mode made is cleared: outside copy mode the selection is the pointer's.
                if self.copy.take().is_some() {
                    self.terminal.set_selection(None).map_err(eng)?;
                }
            }
            CopyOp::Yank => {
                if self.copy.take().is_some() {
                    yanked = self.selection_text()?;
                    self.terminal.set_selection(None).map_err(eng)?;
                }
            }
            CopyOp::ToggleSelection => {
                if let Some(state) = self.copy {
                    // Turning it on anchors at the cursor, which is already the one selected cell; turning it off
                    // collapses to the cursor cell.
                    let selecting = !state.selecting;
                    match self.copy_cursor_cell()? {
                        Some(cursor) => {
                            if !selecting {
                                self.install_copy_selection(cursor, cursor)?;
                            }
                            self.copy = Some(CopyModeState { selecting });
                        }
                        None => self.copy_enter()?,
                    }
                }
            }
            CopyOp::Move(motion) => {
                if let Some(state) = self.copy {
                    match self.copy_cursor_cell()? {
                        // The selection is gone (the other screen was shown, or the program cleared it): start again
                        // at the terminal's cursor instead of guessing.
                        None => self.copy_enter()?,
                        Some(from) => {
                            let to = self.copy_target(from, motion)?;
                            let anchor = if state.selecting {
                                let selection = self.terminal.selection().map_err(eng)?;
                                match selection {
                                    Some(selection) => self.screen_point(&selection.start())?.unwrap_or(to),
                                    None => to,
                                }
                            } else {
                                to
                            };
                            self.install_copy_selection(anchor, to)?;
                            self.reveal_row(to.1)?;
                        }
                    }
                }
            }
        }
        Ok(CopyState {
            active: self.copy.is_some(),
            selecting: self.copy.is_some_and(|state| state.selecting),
            yanked,
        })
    }

    fn read_text(&mut self, region: TextRegion, max_bytes: usize) -> Result<Option<TextRead>> {
        if max_bytes == 0 {
            return Ok(None);
        }
        self.read_region(region, max_bytes.min(MAX_READ_BYTES))
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

    #[test]
    fn cap_tail_keeps_the_tail_on_a_character_boundary() {
        assert_eq!(cap_tail("abc".into(), 10), "abc");
        assert_eq!(cap_tail("abc".into(), 3), "abc");
        assert_eq!(cap_tail("abcdef".into(), 2), "ef");
        // Bytes of "aéb": a, 0xC3, 0xA9, b. A bound of 2 starts inside the é and drops it whole.
        assert_eq!(cap_tail("aéb".into(), 2), "b");
        assert_eq!(cap_tail("aéb".into(), 3), "éb");
        assert_eq!(cap_tail("abc".into(), 0), "");
    }

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
    fn the_pinned_library_has_alternate_scroll_on_by_default() {
        // The wheel policy relies on it: less and vim get arrow keys from the wheel without any setup by the program
        // (D-24). A library bump that changes the default fails here instead of silently breaking the wheel.
        let mut term = Terminal::new(10, 3).expect("terminal");
        assert!(term.mode(Mode::ALT_SCROLL).expect("mode"), "alternate scroll is off in a new terminal");
        term.vt_write(b"\x1b[?1007l");
        assert!(!term.mode(Mode::ALT_SCROLL).expect("mode"));
        // A reset (RIS) returns to the default.
        term.vt_write(b"\x1bc");
        assert!(term.mode(Mode::ALT_SCROLL).expect("mode"), "RIS must restore the default");
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

    #[test]
    fn a_decoration_that_cannot_be_read_degrades_to_its_stand_in_instead_of_failing_the_frame() {
        // An error in the search view or the copy cursor must cost the match boxes or the cursor, never the frame
        // (WR-02): the stand-in is used, and only for an error.
        let spans: Result<Vec<u16>> = Err(Error::Engine("search broke".into()));
        assert_eq!(degraded("search view", spans, || vec![9]), vec![9]);

        let mut stand_in_runs = 0;
        let read: Result<Option<(u16, u16)>> = Ok(Some((3, 4)));
        let got = degraded("copy cursor", read, || {
            stand_in_runs += 1;
            None
        });
        assert_eq!(got, Some((3, 4)), "a value that was read is used as it is");
        assert_eq!(stand_in_runs, 0, "the stand-in is not built when nothing failed");
    }
}
