use std::path::PathBuf;

use crate::platform::Os;
use crate::textread::{TextRead, TextRegion};
use crate::{Error, Result};

/// An RGB colour, already resolved against the palette.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// How a cell takes part in the grid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Width {
    /// One column.
    #[default]
    Narrow,
    /// The first of two columns (CJK, emoji). The next cell is a [`Width::Spacer`].
    Wide,
    /// The second column of a wide character, or the filler at the end of a
    /// soft-wrapped line. Not drawn.
    Spacer,
}

/// One cell of the visible screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    /// The grapheme cluster shown in the cell; empty for a blank cell.
    pub text: String,
    pub width: Width,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

/// The shape of the text cursor (DECSCUSR).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorStyle {
    /// A filled cell.
    #[default]
    Block,
    /// A thin vertical bar at the left edge of the cell.
    Bar,
    /// A line under the cell.
    Underline,
    /// An outlined cell; the engine reports it for an unfocused block cursor.
    Hollow,
}

/// The text cursor inside the viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    /// Whether the program wants the cursor shown (DECTCEM).
    pub visible: bool,
    pub style: CursorStyle,
    /// Whether the program asked for a blinking cursor.
    pub blinking: bool,
}

/// Where the viewport sits in the scrollable area, in rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scrollbar {
    /// Rows in total: scrollback plus the screen.
    pub total: u64,
    /// Row at the top of the viewport.
    pub offset: u64,
    /// Rows in the viewport.
    pub len: u64,
}

impl Scrollbar {
    /// Whether the viewport is at the bottom, which is where new output appears.
    pub fn at_bottom(&self) -> bool {
        self.offset + self.len >= self.total
    }
}

/// An owned, engine-independent copy of the visible screen.
///
/// The renderer never sees an engine's own types, so an engine can be swapped
/// without touching the UI.
#[derive(Clone, Debug, Default)]
pub struct Screen {
    pub cols: u16,
    pub rows: u16,
    pub cursor: Option<Cursor>,
    pub scrollbar: Scrollbar,
    pub default_fg: Color,
    pub default_bg: Color,
    /// Row-major, `rows * cols` cells.
    pub cells: Vec<Cell>,
}

impl Screen {
    /// The screen as plain text, one line per row, trailing blanks trimmed.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for row in self.cells.chunks(self.cols.max(1) as usize) {
            let line: String = row
                .iter()
                .filter(|c| c.width != Width::Spacer)
                .map(|c| if c.text.is_empty() { " " } else { c.text.as_str() })
                .collect();
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }
}

/// Modifier keys held while a key event happens, as a bit set.
///
/// The bit values are part of the contract with the webview: SHIFT 1, ALT 2, CTRL 4, SUPER 8 (the Command key on
/// macOS, the Windows key elsewhere). Only the low four bits mean anything; [`Mods::from_bits`] drops the rest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Mods(pub u8);

impl Mods {
    /// No modifier.
    pub const NONE: Mods = Mods(0);
    pub const SHIFT: Mods = Mods(1);
    pub const ALT: Mods = Mods(2);
    pub const CTRL: Mods = Mods(4);
    pub const SUPER: Mods = Mods(8);

    /// Build a set from raw bits, ignoring everything but the four known modifiers.
    pub const fn from_bits(bits: u8) -> Mods {
        Mods(bits & 0x0f)
    }

    /// The raw bits.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Whether every modifier of `other` is in this set.
    pub const fn contains(self, other: Mods) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether no modifier is set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// This set without the modifiers of `other`.
    pub const fn without(self, other: Mods) -> Mods {
        Mods(self.0 & !other.0)
    }
}

impl std::ops::BitOr for Mods {
    type Output = Mods;

    fn bitor(self, rhs: Mods) -> Mods {
        Mods(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Mods {
    fn bitor_assign(&mut self, rhs: Mods) {
        self.0 |= rhs.0;
    }
}

/// What happened to a key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyAction {
    #[default]
    Press,
    /// The key is held down and the operating system repeats it.
    Repeat,
    Release,
}

/// A key event in the terms of the engine: the physical key, the text the layout produced for it and what was held.
///
/// The webview never builds one directly. It sends [`DomKey`] facts and the core converts them with
/// [`KeyEvent::from_dom`], so the conversion rules are unit-testable without a window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyEvent {
    /// The W3C `KeyboardEvent.code` of the physical key, for example `ArrowUp` or `KeyA`. An unknown code is
    /// treated as an unidentified key.
    pub code: String,
    /// The text the key produced on the current layout, before any Ctrl or Meta transformation; `None` when the
    /// key produces no text (arrows, Enter, dead keys, function keys).
    pub text: Option<String>,
    /// The character of the key without Shift on a US layout, when known. Kitty reports use it.
    pub unshifted: Option<char>,
    pub mods: Mods,
    /// Modifiers that were already used up to produce `text` (Shift for a capital letter, Option on macOS for a
    /// composed character).
    pub consumed: Mods,
    pub action: KeyAction,
    /// The key is part of an IME composition; legacy encoding sends nothing for it.
    pub composing: bool,
}

/// The facts a webview sends for one `keydown` or `keyup`: `KeyboardEvent.code`, `.key` and the flags.
///
/// Nothing here is interpreted by the webview. [`KeyEvent::from_dom`] applies the rules.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DomKey {
    pub code: String,
    pub key: String,
    pub mods: Mods,
    pub repeat: bool,
    pub release: bool,
    pub composing: bool,
}

impl KeyEvent {
    /// Build the engine's key event from the facts the webview sent.
    ///
    /// The rules are pure and platform-independent except for one: on macOS the Option key composes text, so a held
    /// Alt that produced text is a consumed modifier unless `option_as_alt` is on. Ctrl+[, Ctrl+I and Ctrl+M get no
    /// special case: they are whatever the engine encodes (D-13).
    pub fn from_dom(dom: &DomKey, option_as_alt: bool) -> KeyEvent {
        KeyEvent::from_dom_on(dom, option_as_alt, Os::current())
    }

    /// [`from_dom`](Self::from_dom) for the rules of another platform, so that every platform's rule is testable on
    /// any host.
    pub fn from_dom_on(dom: &DomKey, option_as_alt: bool, os: Os) -> KeyEvent {
        let action = if dom.release {
            KeyAction::Release
        } else if dom.repeat {
            KeyAction::Repeat
        } else {
            KeyAction::Press
        };
        // A release never carries text: the engine reports it from the key alone.
        let text = if dom.release { None } else { key_text(&dom.key) };
        let unshifted = us_unshifted(&dom.code).or_else(|| text.as_deref().and_then(lowercase_char));

        let mut mods = dom.mods;
        let mut consumed = Mods::NONE;
        if text.is_some() {
            if dom.mods.contains(Mods::SHIFT) {
                consumed |= Mods::SHIFT;
            }
            // On macOS Option composes characters; unless it is meant as Alt, the composition used it up.
            if os == Os::Macos && !option_as_alt && dom.mods.contains(Mods::ALT) {
                consumed |= Mods::ALT;
            }
            // On Windows AltGr arrives as Ctrl+Alt. The page drops both when the browser says AltGraph; this is the
            // backstop for a webview that does not. Only text the layout composed counts: the lower-cased text is
            // not the US character of the physical key (or the key has none). Ctrl+Alt+C on a US layout types `c`
            // on the C key and stays a chord. The two modifiers are removed rather than marked consumed: the
            // encoder's Ctrl-sequence rule reads every held modifier, so a consumed Ctrl would still turn `@` into NUL.
            let altgr = Mods::CTRL | Mods::ALT;
            if os == Os::Windows && mods.contains(altgr) {
                let typed = text.as_deref().and_then(lowercase_char);
                let us = us_unshifted(&dom.code);
                if us.is_none() || us != typed {
                    mods = mods.without(altgr);
                }
            }
        }

        KeyEvent {
            code: dom.code.clone(),
            text,
            unshifted,
            mods,
            consumed,
            action,
            composing: dom.composing,
        }
    }
}

/// The text of a `KeyboardEvent.key` when it is a single printable character.
///
/// Exactly one code point (counted as code points, not UTF-16 units: an emoji is one), not a C0 control or DEL, and
/// not in the macOS function-key private-use range U+F700..=U+F8FF, which the library refuses as text. Everything
/// else (`Enter`, `Dead`, `F5`, the empty string) is a key without text and is encoded from the key itself.
fn key_text(key: &str) -> Option<String> {
    let mut chars = key.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let code = u32::from(c);
    if code <= 0x1f || code == 0x7f || (0xf700..=0xf8ff).contains(&code) {
        return None;
    }
    Some(key.to_owned())
}

/// The character a physical key types without Shift on a US layout.
///
/// The DOM has no layout-independent value, so the US layout is the reference, as it is for Kitty's alternate key
/// reports. Keys outside the table return `None` and the caller falls back to the lowercased text.
fn us_unshifted(code: &str) -> Option<char> {
    if let Some(rest) = code.strip_prefix("Key") {
        let mut it = rest.chars();
        return match (it.next(), it.next()) {
            (Some(c), None) if c.is_ascii_uppercase() => Some(c.to_ascii_lowercase()),
            _ => None,
        };
    }
    if let Some(rest) = code.strip_prefix("Digit") {
        let mut it = rest.chars();
        return match (it.next(), it.next()) {
            (Some(c), None) if c.is_ascii_digit() => Some(c),
            _ => None,
        };
    }
    Some(match code {
        "Minus" => '-',
        "Equal" => '=',
        "BracketLeft" => '[',
        "BracketRight" => ']',
        "Backslash" => '\\',
        "Semicolon" => ';',
        "Quote" => '\'',
        "Backquote" => '`',
        "Comma" => ',',
        "Period" => '.',
        "Slash" => '/',
        "Space" => ' ',
        _ => return None,
    })
}

/// The lower-case form of a single character, when it is again a single character.
fn lowercase_char(text: &str) -> Option<char> {
    let mut lower = text.chars().next()?.to_lowercase();
    let c = lower.next()?;
    if lower.next().is_some() { None } else { Some(c) }
}

/// What a mouse button or the pointer did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseAction {
    #[default]
    Press,
    Release,
    /// The pointer moved. The button field says which button is held, `MouseButton::None` for none.
    Motion,
}

/// A mouse button. The wheel is not one of them: it arrives as a [`WheelEvent`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseButton {
    /// No button: a motion with nothing held.
    #[default]
    None,
    Left,
    Middle,
    Right,
}

/// A mouse event as the webview saw it.
///
/// `x` and `y` are **device pixels** relative to the origin of the pane's grid (the top left corner of the first
/// cell), not cells: the engine converts with the cell size it learned from [`TerminalEngine::resize`], so the
/// SGR-Pixels format and sub-cell positions are possible. A position that is not finite, negative or outside the grid
/// is clamped into it; when the cell size is unknown (`(0, 0)`) the position is treated as cell (0, 0).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MouseEvent {
    pub action: MouseAction,
    pub button: MouseButton,
    pub mods: Mods,
    pub x: f32,
    pub y: f32,
}

/// One wheel event: `rows` is how many rows it moved, negative towards older output (up), positive towards newer
/// output (down). What it means is decided by [`TerminalEngine::wheel`] from the live terminal modes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WheelEvent {
    pub rows: i32,
    pub mods: Mods,
    /// Device pixels relative to the grid origin, as in [`MouseEvent`].
    pub x: f32,
    pub y: f32,
}

/// What a wheel event turned into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WheelOutcome {
    /// The program is to receive these bytes (wheel reports, or arrow keys in the alternate screen).
    Bytes(Vec<u8>),
    /// The viewport was scrolled locally; the screen changed.
    Scrolled,
    /// Nothing happened: no rows, or the program tracks the mouse in a format that has no wheel.
    Nothing,
}

/// What a paste did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteOutcome {
    /// The bytes are in [`TerminalEngine::take_replies`].
    Written,
    /// There was nothing to paste.
    Empty,
    /// The text could inject commands (a newline without bracketed paste, or the end marker with it) and nothing
    /// was written. The caller asks the user and repeats the paste with `allow_unsafe` set; the engine never
    /// decides that itself.
    Rejected,
}

/// Terminal modes the UI needs to know about.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modes {
    /// The alternate screen is active (full-screen programs).
    pub alt_screen: bool,
    /// The program asked for bracketed paste (mode 2004).
    pub bracketed_paste: bool,
    /// Some mouse tracking mode is on.
    pub mouse_tracking: bool,
}

/// A horizontal run of selected cells on one viewport row; `x0` and `x1` are inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub y: u16,
    pub x0: u16,
    pub x1: u16,
}

/// A search match on one viewport row; `x0` and `x1` are inclusive. `current` marks the match that `Next` and
/// `Prev` moved to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchSpan {
    pub y: u16,
    pub x0: u16,
    pub x1: u16,
    pub current: bool,
}

/// One changed viewport row with all its cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowUpdate {
    pub y: u16,
    pub cells: Vec<Cell>,
}

/// What changed since the previous [`TerminalEngine::update`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Update {
    /// Every viewport row is in `changed`.
    pub full: bool,
    /// The consumer must replace its whole mirror: the grid size or the theme changed, or it asked for a fresh
    /// start with [`TerminalEngine::request_full`].
    pub reset: bool,
    pub cols: u16,
    pub rows: u16,
    pub cursor: Option<Cursor>,
    pub scrollbar: Scrollbar,
    pub default_fg: Color,
    pub default_bg: Color,
    pub modes: Modes,
    pub changed: Vec<RowUpdate>,
    /// The complete current selection, not only the part on changed rows.
    pub selection: Vec<Span>,
    /// All search matches inside the viewport.
    pub matches: Vec<MatchSpan>,
    pub search: Option<SearchState>,
    /// Where the copy-mode cursor is, in viewport coordinates, when copy mode is active.
    pub copy_cursor: Option<(u16, u16)>,
}

/// The colours a theme gives the terminal. The palette entries 16 to 255 are derived from `bg`, `fg` and `ansi`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EngineTheme {
    pub fg: Color,
    pub bg: Color,
    pub cursor: Color,
    pub ansi: [Color; 16],
}

/// Something the program told the terminal that the shell should show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineEvent {
    /// The window title changed (OSC 0 or 2).
    Title(String),
    /// The working directory changed (OSC 7), as the raw value the program sent.
    Pwd(String),
    /// The program rang the bell (BEL). Runs of bells are folded into one event; the pane lets one through per
    /// [`crate::attention::GAP`].
    Bell,
}

/// The most bytes a title or working directory from a program may have (T-02-35).
pub const MAX_EVENT_BYTES: usize = 4096;

/// Make text that a program controls safe to hand to the UI: control characters (C0, DEL and C1) are removed and the
/// result is cut to [`MAX_EVENT_BYTES`] on a character boundary.
pub fn sanitize_program_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_EVENT_BYTES));
    for c in text.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > MAX_EVENT_BYTES {
            break;
        }
        out.push(c);
    }
    out
}

/// The value of one hex digit.
fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decode `%XX` escapes. `None` for a truncated or non-hex escape and for bytes that are not UTF-8 afterwards.
pub(crate) fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = hex_value(*bytes.get(i + 1)?)?;
            let lo = hex_value(*bytes.get(i + 2)?)?;
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The host and the absolute path of an OSC 7 value (`file://host/path`, percent-encoded).
///
/// `None` unless the scheme is `file`, the path is absolute and has no `..` segment after decoding, and nothing in
/// the value is a control character. The result names a working directory for a new shell, so it must never be able
/// to point somewhere the program did not already have access to by walking up with `..` (T-02-35). The path is
/// returned as written otherwise: `?` and `#` are part of a name, not a query or a fragment.
pub fn parse_osc7(value: &str) -> Option<(String, PathBuf)> {
    if value.len() > MAX_EVENT_BYTES || value.chars().any(char::is_control) {
        return None;
    }
    let scheme = value.get(..7)?;
    if !scheme.eq_ignore_ascii_case("file://") {
        return None;
    }
    let rest = &value[7..];
    let slash = rest.find('/')?;
    let (host, path) = rest.split_at(slash);
    let path = percent_decode(path)?;
    // After decoding, so that %2e%2e and %2F cannot smuggle a segment past the check.
    if path.chars().any(char::is_control) || path.split('/').any(|segment| segment == "..") {
        return None;
    }
    Some((host.to_owned(), PathBuf::from(path)))
}

/// How far a pointer selection grows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectMode {
    Cell,
    Word,
    Line,
}

/// A change to the selection. Coordinates are viewport cells; the engine keeps the selection attached to the text,
/// so it survives output and scrolling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectOp {
    Begin { x: u16, y: u16, mode: SelectMode },
    Extend { x: u16, y: u16 },
    /// Select everything: scrollback and screen.
    All,
    Clear,
}

/// A change to the search state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchOp {
    /// Start searching for this text (the previous search is replaced).
    Set(String),
    Next,
    Prev,
    Clear,
}

/// The most bytes a search needle may have (T-02-46): a longer one is refused and an earlier search stays.
pub const MAX_SEARCH_BYTES: usize = 1024;

/// The result of a search.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchState {
    /// Matches in the whole scrollback.
    pub total: usize,
    /// The place of the current match in reading order, when there is one: 0 is the oldest match and `total - 1`
    /// the newest, so a find bar shows `current + 1` of `total`. A search starts on the newest match, `Prev` goes up
    /// (towards 0) and `Next` goes down, both wrap.
    pub current: Option<usize>,
}

/// A cursor movement in copy mode.
///
/// The cursor moves over the whole screen, scrollback included, and the viewport follows it. Every motion stops at
/// the edges: nothing wraps and nothing goes past the first or the last cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyMotion {
    /// One cell left or right.
    Left,
    Right,
    /// One row up or down, blank rows included.
    Up,
    Down,
    /// The first and the last non-blank cell of the row (column 0 on a blank row).
    LineStart,
    LineEnd,
    /// The first cell of the scrollback and column 0 of the last row.
    Top,
    Bottom,
    /// One screen height up or down, in the same column.
    PageUp,
    PageDown,
    /// The first character of the next word, over blanks and line ends; the first character of this word, or of the
    /// previous one when already on a word start. A word is the library's own: what a double click selects.
    WordForward,
    WordBack,
}

/// An operation of the keyboard-driven copy mode.
///
/// The cursor is the moving end of the terminal's own selection, so it is tracked: output and scrolling leave it on
/// the text it points at. Without a selection in progress the selection is the one cell under the cursor. Every
/// operation but `Enter` does nothing outside copy mode, and none of them can fail on an out-of-range position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyOp {
    /// Start at the terminal's cursor (column 0 of the last row when it is hidden), replacing any selection.
    Enter,
    /// Leave without yanking and clear the selection.
    Exit,
    Move(CopyMotion),
    /// Anchor a selection at the cursor, or collapse the selection to the cursor cell.
    ToggleSelection,
    /// Leave copy mode and hand back the selected text in [`CopyState::yanked`] (soft wraps joined, trailing blanks
    /// trimmed); `None` when the selection holds no text.
    Yank,
}

/// The state of copy mode after an operation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CopyState {
    pub active: bool,
    pub selecting: bool,
    pub yanked: Option<String>,
}

/// The boundary between holzWave and a terminal emulation library.
///
/// `libghostty-vt` is young and its API moves, so nothing outside the engine
/// implementation may name its types. An engine is **not** required to be
/// `Send`: `libghostty-vt` objects are not, so an engine lives on one thread
/// and is driven over channels.
///
/// Everything that must go back to the PTY comes out of one place, [`TerminalEngine::take_replies`]: the engine's
/// answers to queries from the program (cursor position, device attributes, size reports) and the bytes of a
/// paste. The caller writes them to the PTY in order.
///
/// Capabilities that arrive with later work have default bodies that return [`Error::Unsupported`]; an engine
/// overrides what it can do.
pub trait TerminalEngine {
    /// Feed bytes that came from the PTY (or any VT source).
    fn feed(&mut self, bytes: &[u8]);

    /// Change the grid size. `cell_px` is the cell size in pixels, used for
    /// image protocols and size reports; pass `(0, 0)` if unknown.
    fn resize(&mut self, cols: u16, rows: u16, cell_px: (u32, u32)) -> Result<()>;

    /// The current grid size as `(cols, rows)`.
    fn size(&self) -> Result<(u16, u16)>;

    /// Scroll the viewport by `rows`; negative is towards older output.
    fn scroll(&mut self, rows: isize);

    /// Jump back to the live screen at the bottom.
    fn scroll_to_bottom(&mut self);

    /// Copy the visible screen out of the engine.
    fn screen(&mut self) -> Result<Screen>;

    /// Treat the macOS Option key as Alt (an ESC prefix) instead of a composing key. Off by default: a Swiss or
    /// German layout types `@ | [ ] { }` with Option.
    fn set_option_as_alt(&mut self, on: bool);

    /// Encode a key event using the terminal's current modes (application cursor keys, Kitty keyboard protocol,
    /// modifyOtherKeys). The result may be empty: key releases and composing keys send nothing in legacy mode.
    fn encode_key(&mut self, event: &KeyEvent) -> Result<Vec<u8>>;

    /// Encode committed text (an IME commit, a dead-key result) as input. It is not a paste and is never bracketed,
    /// and it is not sanitised either: it is what the user typed. Clipboard content goes through
    /// [`TerminalEngine::paste`].
    fn encode_text(&mut self, text: &str) -> Result<Vec<u8>>;

    /// Paste `text` the way the program asked for it: sanitised always, bracketed when mode 2004 is on. The bytes
    /// are queued for [`TerminalEngine::take_replies`]. Text that could inject commands is [`PasteOutcome::Rejected`]
    /// unless `allow_unsafe` is set.
    fn paste(&mut self, text: &str, allow_unsafe: bool) -> Result<PasteOutcome>;

    /// Signal that something that is not text was pasted (an image on the clipboard): an empty bracketed paste, the
    /// start and the end of a bracketed paste with nothing between, when the program turned on mode 2004, and nothing
    /// otherwise ([`PasteOutcome::Empty`]). Programs such as Claude Code read this as the cue to fetch the clipboard
    /// content themselves; a program that did not ask for bracketed paste sees nothing. The bytes are queued for
    /// [`TerminalEngine::take_replies`] like those of `paste`. [`TerminalEngine::paste`] of an empty text is a different
    /// thing and writes nothing in either mode. An engine without bracketed paste is correct with the default.
    fn paste_cue(&mut self) -> Result<PasteOutcome> {
        Ok(PasteOutcome::Empty)
    }

    /// Encode a mouse event the way the program asked for it (tracking mode and output format from the live terminal
    /// modes). The result is empty when the program does not track the mouse, and also for events the mode does not
    /// report (motion in normal mode, motion with no button in button mode).
    ///
    /// Positions are device pixels relative to the grid origin, see [`MouseEvent`]. An engine that reports nothing is
    /// correct for an engine without mouse support, which is what the default is.
    fn mouse(&mut self, _event: &MouseEvent) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }

    /// Decide what a wheel event means right now. Policy, in order: Shift held scrolls the viewport; a program that
    /// tracks the mouse gets wheel-button reports; the alternate screen with alternate scroll on gets arrow keys
    /// (application cursor mode honoured); anything else scrolls the viewport. The default scrolls.
    fn wheel(&mut self, event: &WheelEvent) -> Result<WheelOutcome> {
        self.scroll(event.rows as isize);
        Ok(WheelOutcome::Scrolled)
    }

    /// Take every byte the engine wants written to the PTY.
    fn take_replies(&mut self) -> Vec<u8>;

    /// What changed since the previous call.
    fn update(&mut self) -> Result<Update> {
        Err(Error::Unsupported("update".into()))
    }

    /// Make the next [`TerminalEngine::update`] report every row.
    fn request_full(&mut self) {}

    /// Set the default colours and palette.
    fn set_theme(&mut self, _theme: &EngineTheme) -> Result<()> {
        Err(Error::Unsupported("set_theme".into()))
    }

    /// Change the selection.
    fn select(&mut self, _op: SelectOp) -> Result<()> {
        Err(Error::Unsupported("select".into()))
    }

    /// The selected text, with soft wraps joined and trailing blanks trimmed.
    fn selection_text(&mut self) -> Result<Option<String>> {
        Err(Error::Unsupported("selection_text".into()))
    }

    /// Read a region as plain text, at most `max_bytes` bytes. `Ok(None)` when there is nothing to read.
    ///
    /// The user's selection, the scroll position and the pending frame are never touched. The text is what the grid
    /// holds: untrusted program output, neither sanitised nor redacted, and the cells the user could not read are
    /// reported as byte ranges in the result. The call belongs to the engine's own thread.
    ///
    /// Measured (release build): a 200x50 screen reads in about 0.3 ms and a read of the whole budget (2000 rows of
    /// 250 styled cells, 500 000 cells) in 20 to 35 ms.
    fn read_text(&mut self, _region: TextRegion, _max_bytes: usize) -> Result<Option<TextRead>> {
        Err(Error::Unsupported("read_text".into()))
    }

    /// Take the events the program caused (title, working directory, bell) since the previous call.
    fn take_events(&mut self) -> Vec<EngineEvent> {
        Vec::new()
    }

    /// Change the search and return its state.
    fn search(&mut self, _op: SearchOp) -> Result<SearchState> {
        Err(Error::Unsupported("search".into()))
    }

    /// Run a copy-mode operation and return its state.
    fn copy_mode(&mut self, _op: CopyOp) -> Result<CopyState> {
        Err(Error::Unsupported("copy_mode".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dom(code: &str, key: &str, mods: Mods) -> DomKey {
        DomKey { code: code.into(), key: key.into(), mods, ..DomKey::default() }
    }

    fn event(code: &str, key: &str, mods: Mods) -> KeyEvent {
        KeyEvent::from_dom_on(&dom(code, key, mods), false, Os::Linux)
    }

    #[test]
    fn a_printable_key_carries_its_text() {
        assert_eq!(event("KeyA", "a", Mods::NONE).text.as_deref(), Some("a"));
        assert_eq!(event("KeyA", "A", Mods::SHIFT).text.as_deref(), Some("A"));
        assert_eq!(event("Space", " ", Mods::NONE).text.as_deref(), Some(" "));
        assert_eq!(event("Digit7", "|", Mods::ALT).text.as_deref(), Some("|"));
    }

    #[test]
    fn named_and_dead_keys_carry_no_text() {
        assert_eq!(event("Enter", "Enter", Mods::NONE).text, None);
        assert_eq!(event("Tab", "Tab", Mods::NONE).text, None);
        assert_eq!(event("Backquote", "Dead", Mods::NONE).text, None);
        assert_eq!(event("ArrowUp", "ArrowUp", Mods::NONE).text, None);
        assert_eq!(event("F5", "F5", Mods::NONE).text, None);
        assert_eq!(event("KeyA", "", Mods::NONE).text, None);
    }

    #[test]
    fn exactly_one_code_point_counts_not_utf16_units() {
        // One astral code point is two UTF-16 units: it is still one character.
        assert_eq!(event("", "\u{1F600}", Mods::NONE).text.as_deref(), Some("\u{1F600}"));
        // Two code points are not one key.
        assert_eq!(event("", "ab", Mods::NONE).text, None);
        assert_eq!(event("", "e\u{0301}", Mods::NONE).text, None);
    }

    #[test]
    fn control_characters_and_function_key_private_use_carry_no_text() {
        assert_eq!(event("KeyA", "\u{0001}", Mods::CTRL).text, None);
        assert_eq!(event("Backspace", "\u{007F}", Mods::NONE).text, None);
        assert_eq!(event("ArrowUp", "\u{F700}", Mods::NONE).text, None);
        assert_eq!(event("F1", "\u{F8FF}", Mods::NONE).text, None);
        // The edges: U+0020 and U+0080 and U+F6FF and U+F900 are all ordinary text.
        assert!(event("Space", "\u{0020}", Mods::NONE).text.is_some());
        assert!(event("", "\u{0080}", Mods::NONE).text.is_some());
        assert!(event("", "\u{F6FF}", Mods::NONE).text.is_some());
        assert!(event("", "\u{F900}", Mods::NONE).text.is_some());
    }

    #[test]
    fn release_events_never_carry_text() {
        let d = DomKey { release: true, ..dom("KeyA", "a", Mods::NONE) };
        let e = KeyEvent::from_dom_on(&d, false, Os::Linux);
        assert_eq!(e.action, KeyAction::Release);
        assert_eq!(e.text, None);
        assert_eq!(e.consumed, Mods::NONE);
    }

    #[test]
    fn action_follows_release_then_repeat() {
        let press = KeyEvent::from_dom_on(&dom("KeyA", "a", Mods::NONE), false, Os::Linux);
        assert_eq!(press.action, KeyAction::Press);
        let repeat = DomKey { repeat: true, ..dom("KeyA", "a", Mods::NONE) };
        assert_eq!(KeyEvent::from_dom_on(&repeat, false, Os::Linux).action, KeyAction::Repeat);
        let both = DomKey { repeat: true, release: true, ..dom("KeyA", "a", Mods::NONE) };
        assert_eq!(KeyEvent::from_dom_on(&both, false, Os::Linux).action, KeyAction::Release);
    }

    #[test]
    fn composing_is_copied() {
        let d = DomKey { composing: true, ..dom("KeyA", "a", Mods::NONE) };
        assert!(KeyEvent::from_dom_on(&d, false, Os::Linux).composing);
        assert!(!event("KeyA", "a", Mods::NONE).composing);
    }

    #[test]
    fn unshifted_comes_from_the_us_layout_of_the_physical_key() {
        for (code, key, expected) in [
            ("KeyA", "A", 'a'),
            ("KeyZ", "z", 'z'),
            ("Digit0", ")", '0'),
            ("Digit7", "/", '7'),
            ("Minus", "_", '-'),
            ("Equal", "+", '='),
            ("BracketLeft", "{", '['),
            ("BracketRight", "}", ']'),
            ("Backslash", "|", '\\'),
            ("Semicolon", ":", ';'),
            ("Quote", "\"", '\''),
            ("Backquote", "~", '`'),
            ("Comma", "<", ','),
            ("Period", ">", '.'),
            ("Slash", "?", '/'),
            ("Space", " ", ' '),
        ] {
            assert_eq!(event(code, key, Mods::SHIFT).unshifted, Some(expected), "{code}");
        }
    }

    #[test]
    fn unshifted_falls_back_to_the_lowercased_text_then_to_none() {
        // A key outside the US table: the numpad, or a layout-specific key.
        assert_eq!(event("Numpad5", "5", Mods::NONE).unshifted, Some('5'));
        assert_eq!(event("IntlBackslash", "<", Mods::NONE).unshifted, Some('<'));
        assert_eq!(event("", "\u{00C4}", Mods::SHIFT).unshifted, Some('\u{00E4}'));
        // A character whose lower case is more than one code point is not guessed.
        assert_eq!(event("", "\u{0130}", Mods::NONE).unshifted, None);
        // No text and no US entry.
        assert_eq!(event("Enter", "Enter", Mods::NONE).unshifted, None);
        assert_eq!(event("ArrowUp", "ArrowUp", Mods::NONE).unshifted, None);
    }

    #[test]
    fn consumed_holds_shift_when_shift_produced_text() {
        assert_eq!(event("KeyA", "A", Mods::SHIFT).consumed, Mods::SHIFT);
        // Shift on a key without text consumed nothing.
        assert_eq!(event("Tab", "Tab", Mods::SHIFT).consumed, Mods::NONE);
        assert_eq!(event("ArrowUp", "ArrowUp", Mods::SHIFT).consumed, Mods::NONE);
        // Ctrl and Super are never consumed.
        assert_eq!(event("KeyC", "c", Mods::CTRL).consumed, Mods::NONE);
        assert_eq!(event("KeyC", "C", Mods::CTRL | Mods::SHIFT).consumed, Mods::SHIFT);
        assert_eq!(event("KeyC", "c", Mods::SUPER).consumed, Mods::NONE);
    }

    #[test]
    fn consumed_holds_alt_only_on_macos_when_option_composes_text() {
        let d = dom("KeyF", "\u{0192}", Mods::ALT);
        assert_eq!(KeyEvent::from_dom_on(&d, false, Os::Macos).consumed, Mods::ALT);
        // option-as-alt on: Option is Alt, nothing was composed away.
        assert_eq!(KeyEvent::from_dom_on(&d, true, Os::Macos).consumed, Mods::NONE);
        // Not macOS: Alt is Alt.
        assert_eq!(KeyEvent::from_dom_on(&d, false, Os::Linux).consumed, Mods::NONE);
        // No text: nothing to consume.
        let arrow = dom("ArrowLeft", "ArrowLeft", Mods::ALT);
        assert_eq!(KeyEvent::from_dom_on(&arrow, false, Os::Macos).consumed, Mods::NONE);
        // Alt not held: nothing to consume even on macOS.
        let plain = dom("KeyF", "f", Mods::NONE);
        assert_eq!(KeyEvent::from_dom_on(&plain, false, Os::Macos).consumed, Mods::NONE);
    }

    #[test]
    fn windows_drops_ctrl_and_alt_when_the_layout_composed_the_text() {
        // AltGr arrives as Ctrl+Alt: Swiss `|` on Digit7, German `@` on KeyQ, `@` on Digit2 of a US-based layout, `€`
        // on KeyE. The text differs from the US character of the physical key, so the layout used both up.
        let both = Mods::CTRL | Mods::ALT;
        for (code, key) in [("Digit2", "@"), ("Digit7", "|"), ("KeyE", "\u{20AC}"), ("KeyQ", "@"), ("Digit8", "{")] {
            let e = KeyEvent::from_dom_on(&dom(code, key, both), false, Os::Windows);
            assert_eq!(e.mods, Mods::NONE, "{code} {key}");
            assert_eq!(e.text.as_deref(), Some(key));
        }
        // A code with no US character and text present: the layout put something there.
        let d = dom("IntlBackslash", "\\", both);
        assert_eq!(KeyEvent::from_dom_on(&d, false, Os::Windows).mods, Mods::NONE);
        // Shift stays next to them, and stays consumed as before.
        let d = dom("Digit7", "\\", both | Mods::SHIFT);
        let e = KeyEvent::from_dom_on(&d, false, Os::Windows);
        assert_eq!((e.mods, e.consumed), (Mods::SHIFT, Mods::SHIFT));
    }

    #[test]
    fn windows_keeps_a_real_ctrl_alt_chord_on_a_us_layout() {
        let both = Mods::CTRL | Mods::ALT;
        // Ctrl+Alt+C types `c` on the C key, and Shift on top types `C`: both are the US character of the key.
        assert_eq!(KeyEvent::from_dom_on(&dom("KeyC", "c", both), false, Os::Windows).mods, both);
        let shifted = KeyEvent::from_dom_on(&dom("KeyC", "C", both | Mods::SHIFT), false, Os::Windows);
        assert_eq!((shifted.mods, shifted.consumed), (both | Mods::SHIFT, Mods::SHIFT));
        // Only one of the two held, or no text: nothing is dropped.
        assert_eq!(KeyEvent::from_dom_on(&dom("Digit7", "|", Mods::CTRL), false, Os::Windows).mods, Mods::CTRL);
        assert_eq!(KeyEvent::from_dom_on(&dom("Digit7", "|", Mods::ALT), false, Os::Windows).mods, Mods::ALT);
        assert_eq!(KeyEvent::from_dom_on(&dom("ArrowLeft", "ArrowLeft", both), false, Os::Windows).mods, both);
    }

    #[test]
    fn macos_and_linux_keep_every_ctrl_alt_event_as_it_was() {
        let both = Mods::CTRL | Mods::ALT;
        for (code, key) in [("Digit2", "@"), ("Digit7", "|"), ("KeyC", "c"), ("IntlBackslash", "\\")] {
            let d = dom(code, key, both);
            assert_eq!(KeyEvent::from_dom_on(&d, true, Os::Macos).mods, both, "{code} on macOS with option as alt");
            assert_eq!(KeyEvent::from_dom_on(&d, false, Os::Linux).mods, both, "{code} on Linux");
            assert_eq!(KeyEvent::from_dom_on(&d, false, Os::Linux).consumed, Mods::NONE, "{code} on Linux");
        }
        // macOS without option-as-alt still consumes Alt only, as before.
        let d = dom("Digit7", "|", both);
        assert_eq!(KeyEvent::from_dom_on(&d, false, Os::Macos).consumed, Mods::ALT);
    }

    #[test]
    fn ctrl_bracket_ctrl_i_and_ctrl_m_are_not_special_cased() {
        // D-13: the conversion is the same as for any other key; the engine decides what it encodes.
        for (code, key) in [("BracketLeft", "["), ("KeyI", "i"), ("KeyM", "m")] {
            let e = event(code, key, Mods::CTRL);
            assert_eq!(e.code, code);
            assert_eq!(e.text.as_deref(), Some(key));
            assert_eq!(e.mods, Mods::CTRL);
            assert_eq!(e.consumed, Mods::NONE);
        }
    }

    #[test]
    fn hostile_strings_are_handled_without_surprises() {
        let huge = "x".repeat(1 << 20);
        let e = event(&huge, &huge, Mods::NONE);
        assert_eq!(e.text, None);
        assert_eq!(e.unshifted, None);
    }

    #[test]
    fn mods_are_a_bit_set_with_fixed_values() {
        assert_eq!((Mods::SHIFT.0, Mods::ALT.0, Mods::CTRL.0, Mods::SUPER.0), (1, 2, 4, 8));
        let both = Mods::SHIFT | Mods::CTRL;
        assert_eq!(both.bits(), 5);
        assert!(both.contains(Mods::SHIFT) && both.contains(Mods::CTRL));
        assert!(!both.contains(Mods::ALT));
        assert!(Mods::NONE.is_empty() && !both.is_empty());
        assert_eq!(Mods::from_bits(0xff), Mods(15));
        let mut m = Mods::NONE;
        m |= Mods::SUPER;
        assert_eq!(m, Mods::SUPER);
    }
}
