//! The frame protocol between the core and the webview: how an engine update becomes a frame message with changed
//! rows, a `full` flag and sequence numbers.

use serde::Serialize;

use crate::engine::Update;

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

/// Turns engine updates into frames for one pane.
#[derive(Debug)]
pub struct FrameEncoder {
    pane: u64,
}

impl FrameEncoder {
    pub fn new(pane: u64) -> Self {
        Self { pane }
    }

    pub fn request_full(&mut self) {}

    pub fn encode(&mut self, _update: Update) -> Option<FrameMsg> {
        let _ = self.pane;
        None
    }
}
