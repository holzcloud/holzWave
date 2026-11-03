//! The frame protocol between the core and the webview: how an engine update becomes a frame message with changed
//! rows, a `full` flag and sequence numbers.
//!
//! # A frame
//!
//! One JSON object per pane and change: `pane`, `seq`, `full`, `cols`, `rows`, `fg`, `bg` (default colours as
//! `0xRRGGBB`), `cursor` (`null` when hidden), `scrollbar` (`[offset, total, len]`), `selection`, `matches`,
//! `search`, `copy_cursor`, `modes` and `changed`, the rows that differ from the previous frame, and, only when
//! something changed, `images`.
//!
//! # Images
//!
//! `images` is the list of [`PlacementMsg`] (one array of sixteen numbers per image, never pixels) and is in a frame
//! exactly when the list differs from the one the encoder last sent for this pane, and on a full frame that has any.
//! An empty list in a frame that is not full means every image is gone; a full frame replaces everything, so the key
//! missing from it means no images. A frame without the key leaves the page's list as it is, and is byte for byte
//! what a frame was before images existed. A change of the list alone is a frame. At most
//! [`crate::engine::MAX_PLACEMENTS`] entries. The pixels are pulled in pieces of at most [`IMAGE_CHUNK_BYTES`] by
//! `(image id, generation)`, see [`image_data_bytes`].
//!
//! # Rows: one code point per cell
//!
//! A row is a list of runs `[text, cols, fg, bg, flags]`, each covering `cols` cells of one style, and an optional
//! `extras` list. The rule that makes a row renderable without measuring text:
//!
//! - every cell contributes **exactly one code point** to its run's text: the first code point of its grapheme
//!   cluster, a space for an empty cell, `\0` for the second column of a wide character;
//! - the code points after the first (a combining mark, the rest of a ZWJ sequence, the second regional indicator of
//!   a flag, a skin tone) travel in `extras` as `[column, tail]`;
//! - so `text.chars().count() == cols` for every run, a row never has more cells than the grid is wide, and a
//!   renderer that advances one cell per code point stays aligned (RESEARCH Pitfall 1 and 2b).
//!
//! Trailing blanks in the default background without flags are not sent; the consumer pads the row to `cols`.
//!
//! # Which frames are sent
//!
//! The engine reports what libghostty marked dirty, and libghostty calls every scroll a full redraw (RESEARCH
//! Pitfall 7). The [`FrameEncoder`] therefore remembers the rows it last sent and sends only rows whose encoding
//! differs. `full` is true only on the first frame, after a size change, after a theme change and after
//! [`FrameEncoder::request_full`] (webview reconnect, or a gap in `seq`): the consumer then replaces everything.
//! A frame with no changed rows is still sent when the header changed (cursor visibility and shape arrive from the
//! engine as a clean snapshot), and nothing is sent, and no sequence number used, when neither changed.
//!
//! `seq` starts at 1 per pane and goes up by exactly one per emitted frame. A consumer that sees a gap asks for a full
//! frame; it never patches over a gap.
//!
//! The golden sequence in `app/tests/fixtures/frames.json` is written by the `fixtures` test in `core/tests/wire.rs`
//! (`UPDATE_FIXTURES=1`) and applied step by step by the frontend patcher test, so both languages are proven against
//! the same bytes.

use serde::Serialize;

use crate::engine::{
    Cell, Color, CursorStyle, ImageChunk, ImageLayer, ImagePlacement, MAX_IMAGE_CHUNK_BYTES, MAX_PLACEMENTS, Update, Width,
};

/// Style flag bits of a [`Run`].
pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 2;
pub const UNDERLINE: u8 = 4;
pub const INVERSE: u8 = 8;

/// `[text, cols, fg, bg, flags]`: `cols` cells of one style. `text` holds one code point per cell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Run(pub String, pub u16, pub u32, pub u32, pub u8);

/// One image on the screen, as the page needs it: sixteen numbers in a fixed order, never the pixels (those are asked
/// for by `(image id, generation)`, see [`image_data_bytes`]).
///
/// In order: image id, generation, layer (0 below the cell backgrounds, 1 below the text, 2 above the text), z, column,
/// row, grid columns, grid rows, x offset, y offset (pixels inside the first cell), pixel width, pixel height, source x,
/// source y, source width, source height (image pixels). Column and row are negative when the image starts above or
/// left of the viewport; the page clips.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlacementMsg(
    pub u32,
    pub u64,
    pub u8,
    pub i32,
    pub i32,
    pub i32,
    pub u32,
    pub u32,
    pub u32,
    pub u32,
    pub u32,
    pub u32,
    pub u32,
    pub u32,
    pub u32,
    pub u32,
);

impl PlacementMsg {
    fn of(p: &ImagePlacement) -> PlacementMsg {
        let layer = match p.layer {
            ImageLayer::BelowBg => 0,
            ImageLayer::BelowText => 1,
            ImageLayer::AboveText => 2,
        };
        PlacementMsg(
            p.image_id,
            p.generation,
            layer,
            p.z,
            p.col,
            p.row,
            p.cols,
            p.rows,
            p.offset_x,
            p.offset_y,
            p.width,
            p.height,
            p.src_x,
            p.src_y,
            p.src_w,
            p.src_h,
        )
    }
}

/// The most bytes of pixels one answer of the image read carries: the page asks again with the next offset. Straight
/// RGBA, so 512 KiB is 131072 pixels, and a 4000 by 4000 picture takes 122 reads.
pub const IMAGE_CHUNK_BYTES: usize = 512 * 1024;
const _: () = assert!(IMAGE_CHUNK_BYTES <= MAX_IMAGE_CHUNK_BYTES);

/// The length of the header of an answer to the image read.
pub const IMAGE_DATA_HEADER_BYTES: usize = 16;

/// The raw answer to an image read: four big-endian `u32` (width, height, total length in bytes, offset of the first
/// byte that follows) and then the bytes, straight RGBA. The chunk is cut to [`IMAGE_CHUNK_BYTES`] here again, so no
/// caller can make an answer larger than that; a size that does not fit a `u32` is not representable on the wire and
/// answers `None` (the engine's image limit makes that unreachable, the check is the proof).
pub fn image_data_bytes(chunk: &ImageChunk) -> Option<Vec<u8>> {
    let width = u32::try_from(chunk.width).ok()?;
    let height = u32::try_from(chunk.height).ok()?;
    let total = u32::try_from(chunk.total).ok()?;
    let offset = u32::try_from(chunk.offset).ok()?;
    let bytes = &chunk.bytes[..chunk.bytes.len().min(IMAGE_CHUNK_BYTES)];
    let mut out = Vec::with_capacity(IMAGE_DATA_HEADER_BYTES + bytes.len());
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&offset.to_be_bytes());
    out.extend_from_slice(bytes);
    Some(out)
}

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
    /// The images on the screen: `Some` only when the list changed since the last frame of this pane (an empty list
    /// means they are all gone) and on a full frame that has images; absent from the JSON otherwise. A full frame
    /// replaces everything, so absent on a full frame means no images.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<PlacementMsg>>,
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
        let flags = (u8::from(cell.bold) * BOLD)
            | (u8::from(cell.italic) * ITALIC)
            | (u8::from(cell.underline) * UNDERLINE)
            | (u8::from(cell.inverse) * INVERSE);
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

/// Everything in a frame except the rows. Two consecutive frames with the same header and no changed rows carry no
/// information, so the encoder does not emit the second one.
#[derive(Clone, Debug, PartialEq)]
struct Header {
    cols: u16,
    rows: u16,
    fg: u32,
    bg: u32,
    cursor: Option<CursorMsg>,
    scrollbar: (u64, u64, u64),
    selection: Vec<(u16, u16, u16)>,
    matches: Vec<(u16, u16, u16, bool)>,
    search: Option<SearchMsg>,
    copy_cursor: Option<(u16, u16)>,
    modes: ModesMsg,
    /// The images on the screen now (not only the change), so that the equality rule sees a change of the list.
    images: Vec<PlacementMsg>,
}

impl Header {
    fn of(update: &Update, images: Vec<PlacementMsg>) -> Header {
        Header {
            cols: update.cols,
            rows: update.rows,
            fg: rgb(update.default_fg),
            bg: rgb(update.default_bg),
            // A hidden cursor is not on the wire: the webview draws a cursor exactly when there is one.
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
            images,
        }
    }

    /// `images` is the list to put in the frame, already decided by the encoder (`None`: the page keeps its own).
    fn into_frame(
        self,
        pane: u64,
        seq: u64,
        full: bool,
        images: Option<Vec<PlacementMsg>>,
        changed: Vec<RowMsg>,
    ) -> FrameMsg {
        FrameMsg {
            pane,
            seq,
            full,
            cols: self.cols,
            rows: self.rows,
            fg: self.fg,
            bg: self.bg,
            cursor: self.cursor,
            scrollbar: self.scrollbar,
            selection: self.selection,
            matches: self.matches,
            search: self.search,
            copy_cursor: self.copy_cursor,
            modes: self.modes,
            images,
            changed,
        }
    }
}

fn blank_row(y: u16) -> RowMsg {
    RowMsg { y, runs: Vec::new(), extras: Vec::new() }
}

/// Turns the updates of one engine into the frames of one pane.
///
/// What the webview holds is exactly what the encoder last sent, so the encoder keeps the rows it sent and the
/// header it sent, and compares every incoming row with them. libghostty reports a full redraw for every scroll
/// (RESEARCH Pitfall 7); the comparison is what turns that back into "only the rows that differ" (D-11).
///
/// A frame says `full` only when the consumer must replace everything: the first frame, a size change, a theme change
/// (the engine's `reset`) and a [`FrameEncoder::request_full`] (webview reconnect or a gap in the sequence numbers,
/// D-09). The encoder is created together with its engine; one created later should be paired with
/// `engine.request_full()` so that its first frame has every row.
#[derive(Debug)]
pub struct FrameEncoder {
    pane: u64,
    seq: u64,
    /// The next frame is full, whatever the update says.
    request: bool,
    /// The header of the last frame sent; `None` before the first.
    header: Option<Header>,
    /// The rows of the last frame sent, by `y`.
    rows: Vec<RowMsg>,
    /// The images the engine last reported, converted: what the screen shows now. The engine says `None` for "no
    /// change", so the encoder is the one that remembers the list.
    images: Vec<PlacementMsg>,
}

impl FrameEncoder {
    pub fn new(pane: u64) -> Self {
        Self { pane, seq: 0, request: false, header: None, rows: Vec::new(), images: Vec::new() }
    }

    /// Make the next frame full: every row, resent from what was sent before. The webview asks for this when it
    /// reconnects or notices a gap in the sequence numbers.
    pub fn request_full(&mut self) {
        self.request = true;
    }

    /// Encode one engine update. `None` when there is nothing to tell the webview: no row differs from the last
    /// frame and the header is the same. A frame that is not emitted does not use up a sequence number.
    pub fn encode(&mut self, update: Update) -> Option<FrameMsg> {
        if let Some(list) = &update.images {
            // Converted only when the engine says the list changed; the engine already cuts at the same maximum.
            self.images.clear();
            self.images.extend(list.iter().take(MAX_PLACEMENTS).map(PlacementMsg::of));
        }
        let header = Header::of(&update, self.images.clone());
        let size_changed =
            self.header.as_ref().is_none_or(|h| h.cols != header.cols || h.rows != header.rows);
        let full = update.reset || self.request || size_changed;
        let (cols, rows) = (usize::from(update.cols), usize::from(update.rows));

        let mut changed: Vec<RowMsg> = Vec::new();
        if full {
            // Start from what the webview holds when the grid is the same (an encoder-side request), else from
            // blank rows; the update's rows replace theirs. An engine update after a reset carries every row.
            let mut all = if size_changed { Vec::new() } else { std::mem::take(&mut self.rows) };
            all.truncate(rows);
            while all.len() < rows {
                all.push(blank_row(all.len() as u16));
            }
            for r in &update.changed {
                if let Some(slot) = all.get_mut(usize::from(r.y)) {
                    *slot = encode_row(r.y, &r.cells, cols, header.bg);
                }
            }
            changed.clone_from(&all);
            self.rows = all;
        } else {
            for r in &update.changed {
                let Some(held) = self.rows.get_mut(usize::from(r.y)) else { continue };
                let row = encode_row(r.y, &r.cells, cols, header.bg);
                if *held != row {
                    *held = row.clone();
                    changed.push(row);
                }
            }
        }

        if !full && changed.is_empty() && self.header.as_ref() == Some(&header) {
            return None;
        }
        // What the page must be told: on a full frame it replaces everything, so a non-empty list is sent and an empty
        // one is the absence of the key; otherwise the list goes out exactly when it differs from the last one sent.
        let images = if full {
            (!header.images.is_empty()).then(|| header.images.clone())
        } else {
            let sent = self.header.as_ref().map(|h| h.images.as_slice()).unwrap_or_default();
            (sent != header.images.as_slice()).then(|| header.images.clone())
        };
        self.request = false;
        self.header = Some(header.clone());
        self.seq += 1;
        Some(header.into_frame(self.pane, self.seq, full, images, changed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(bytes: Vec<u8>, total: u64, offset: u64) -> ImageChunk {
        ImageChunk { width: 3, height: 2, generation: 7, total, offset, bytes }
    }

    #[test]
    fn the_raw_answer_is_four_big_endian_numbers_and_then_the_bytes() {
        let raw = image_data_bytes(&chunk(vec![9, 8, 7], 24, 0x0102_0304)).expect("fits");
        assert_eq!(&raw[..16], [0, 0, 0, 3, 0, 0, 0, 2, 0, 0, 0, 24, 1, 2, 3, 4]);
        assert_eq!(&raw[16..], [9, 8, 7]);
        assert_eq!(raw.len(), IMAGE_DATA_HEADER_BYTES + 3);
    }

    #[test]
    fn an_offset_at_the_end_has_the_header_and_no_bytes() {
        let raw = image_data_bytes(&chunk(Vec::new(), 24, 24)).expect("fits");
        assert_eq!(raw.len(), IMAGE_DATA_HEADER_BYTES);
    }

    #[test]
    fn the_answer_never_carries_more_than_one_chunk() {
        let raw = image_data_bytes(&chunk(vec![1; IMAGE_CHUNK_BYTES + 100], 1 << 20, 0)).expect("fits");
        assert_eq!(raw.len(), IMAGE_DATA_HEADER_BYTES + IMAGE_CHUNK_BYTES);
    }

    #[test]
    fn a_number_that_does_not_fit_the_wire_is_no_answer() {
        assert!(image_data_bytes(&chunk(Vec::new(), u64::from(u32::MAX) + 1, 0)).is_none());
        assert!(image_data_bytes(&chunk(Vec::new(), 0, u64::from(u32::MAX) + 1)).is_none());
    }
}
