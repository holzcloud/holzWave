//! The frame protocol between the core and the webview: how an engine update becomes a frame message with changed
//! rows, a `full` flag and sequence numbers.

use serde::Serialize;

use crate::engine::{Cell, Color, CursorStyle, Update, Width};

/// Style flag bits of a [`Run`].
pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 2;
pub const UNDERLINE: u8 = 4;
pub const INVERSE: u8 = 8;

/// `[text, cols, fg, bg, flags]`: `cols` cells of one style. `text` holds one code point per cell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Run(pub String, pub u16, pub u32, pub u32, pub u8);

/// One row of a frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RowMsg {
    pub y: u16,
    pub runs: Vec<Run>,
    /// `(column, tail)`: code points of a grapheme cluster after its first one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extras: Vec<(u16, String)>,
}

/// The cursor shape on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
    Hollow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CursorMsg {
    pub x: u16,
    pub y: u16,
    pub style: CursorShape,
    pub blink: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ModesMsg {
    pub alt: bool,
    pub bracketed_paste: bool,
    pub mouse: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SearchMsg {
    pub total: usize,
    pub current: Option<usize>,
}

/// One frame for one pane.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FrameMsg {
    pub pane: u64,
    pub seq: u64,
    pub full: bool,
    pub cols: u16,
    pub rows: u16,
    pub fg: u32,
    pub bg: u32,
    pub cursor: Option<CursorMsg>,
    /// `[offset, total, len]`.
    pub scrollbar: (u64, u64, u64),
    /// `[y, x0, x1]`, inclusive.
    pub selection: Vec<(u16, u16, u16)>,
    /// `[y, x0, x1, current]`, inclusive.
    pub matches: Vec<(u16, u16, u16, bool)>,
    pub search: Option<SearchMsg>,
    pub copy_cursor: Option<(u16, u16)>,
    pub modes: ModesMsg,
    pub changed: Vec<RowMsg>,
}

fn rgb(c: Color) -> u32 {
    u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
}

/// Encode one row: one code point per cell, run-length encoded by style, trailing default-style blanks trimmed.
///
/// A cell contributes the first code point of its grapheme cluster (a space when empty, `\0` for the spacer half of
/// a wide character); the code points after the first go to the row's `extras` as `(column, tail)`.
fn encode_row(y: u16, cells: &[Cell], cols: usize, default_bg: u32) -> RowMsg {
    let mut runs: Vec<Run> = Vec::new();
    let mut extras: Vec<(u16, String)> = Vec::new();
    for (x, cell) in cells.iter().take(cols).enumerate() {
        let flags = u8::from(cell.bold) * BOLD
            | u8::from(cell.italic) * ITALIC
            | u8::from(cell.underline) * UNDERLINE
            | u8::from(cell.inverse) * INVERSE;
        let (fg, bg) = (rgb(cell.fg), rgb(cell.bg));
        let mut chars = cell.text.chars();
        let first = match cell.width {
            Width::Spacer => '\0',
            _ => match chars.next() {
                // NUL is the spacer marker on the wire; a cell must never carry one of its own.
                None | Some('\0') => ' ',
                Some(c) => c,
            },
        };
        if cell.width != Width::Spacer {
            let tail: String = chars.collect();
            if !tail.is_empty() {
                extras.push((x as u16, tail));
            }
        }
        match runs.last_mut() {
            Some(r) if r.2 == fg && r.3 == bg && r.4 == flags => {
                r.0.push(first);
                r.1 += 1;
            }
            _ => runs.push(Run(first.to_string(), 1, fg, bg, flags)),
        }
    }
    // Trailing blanks in the default style carry no information, unless a cluster tail sits on one.
    let mut width: usize = runs.iter().map(|r| usize::from(r.1)).sum();
    while let Some(last) = runs.last() {
        let start = width - usize::from(last.1);
        let blank = last.0.chars().all(|c| c == ' ') && last.3 == default_bg && last.4 == 0;
        if !blank || extras.iter().any(|(x, _)| usize::from(*x) >= start) {
            break;
        }
        width = start;
        runs.pop();
    }
    RowMsg { y, runs, extras }
}

/// Turns engine updates into frames for one pane.
#[derive(Debug)]
pub struct FrameEncoder {
    pane: u64,
    seq: u64,
    first: bool,
}

impl FrameEncoder {
    pub fn new(pane: u64) -> Self {
        Self { pane, seq: 0, first: true }
    }

    pub fn request_full(&mut self) {}

    /// Encode one engine update. `None` when there is nothing to tell the webview.
    pub fn encode(&mut self, update: Update) -> Option<FrameMsg> {
        let full = self.first || update.reset;
        let default_bg = rgb(update.default_bg);
        let cols = usize::from(update.cols);
        let changed: Vec<RowMsg> =
            update.changed.iter().map(|r| encode_row(r.y, &r.cells, cols, default_bg)).collect();
        if !full && changed.is_empty() {
            return None;
        }
        self.first = false;
        self.seq += 1;
        Some(FrameMsg {
            pane: self.pane,
            seq: self.seq,
            full,
            cols: update.cols,
            rows: update.rows,
            fg: rgb(update.default_fg),
            bg: default_bg,
            cursor: update.cursor.filter(|c| c.visible).map(|c| CursorMsg {
                x: c.x,
                y: c.y,
                style: match c.style {
                    CursorStyle::Block => CursorShape::Block,
                    CursorStyle::Bar => CursorShape::Bar,
                    CursorStyle::Underline => CursorShape::Underline,
                    CursorStyle::Hollow => CursorShape::Hollow,
                },
                blink: c.blinking,
            }),
            scrollbar: (update.scrollbar.offset, update.scrollbar.total, update.scrollbar.len),
            selection: update.selection.iter().map(|s| (s.y, s.x0, s.x1)).collect(),
            matches: update.matches.iter().map(|m| (m.y, m.x0, m.x1, m.current)).collect(),
            search: update.search.map(|s| SearchMsg { total: s.total, current: s.current }),
            copy_cursor: update.copy_cursor,
            modes: ModesMsg {
                alt: update.modes.alt_screen,
                bracketed_paste: update.modes.bracketed_paste,
                mouse: update.modes.mouse_tracking,
            },
            changed,
        })
    }
}
