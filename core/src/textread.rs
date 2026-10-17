//! Reading a region of a terminal's text, the engine-neutral half.
//!
//! A read gives the text of a region of the grid (the rows the user sees, the user's selection, the last rows of the
//! buffer) as the grid holds it: overwritten text is gone and escape sequences are already consumed. The text is
//! **untrusted program output**. It is neither sanitised nor redacted here; that belongs to whoever hands it to a
//! reader that is not the user. What a read does report are the cells the user could not read (drawn with the conceal
//! attribute, or with a foreground that cannot be told from the background), as byte ranges of the text, so that the
//! caller can remove them before anything else.
//!
//! A read never touches the user's selection, the scroll position or the next frame. Nothing in this module names a
//! type of the engine library: the engine walks its own cells and feeds a [`TextBuilder`].

use std::fmt;
use std::ops::Range;

/// The most bytes a read ever returns, whatever the caller asks for. It is also the size at which a consumer should
/// cut a raw item before it scans it.
pub const MAX_READ_BYTES: usize = 512 * 1024;

/// The most rows a read walks.
pub const MAX_READ_ROWS: u16 = 2000;

/// The most cells a read walks. The row budget follows the width: a wide screen allows fewer rows
/// ([`rows_within_budget`]).
pub const MAX_READ_CELLS: usize = 1 << 19;

/// The most hidden ranges a read keeps. Past it the last range grows over everything up to the next hidden cell
/// instead of a hidden character being left uncovered.
pub const MAX_HIDDEN_RANGES: usize = 1024;

/// A region of a terminal's text.
///
/// Every region is read as the grid holds it and the result is untrusted program output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextRegion {
    /// The rows the viewport shows now, scrolled back or not: what the user sees.
    Screen,
}

/// The text of a region, with the parts of it that the user could not see.
///
/// The text is untrusted program output, exactly as the grid holds it: neither sanitised nor redacted. It is not
/// serialisable on purpose and its `Debug` form prints sizes only, so it cannot reach a log or a page by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct TextRead {
    /// The text: one line feed between rows, soft-wrapped lines joined without one, trailing blanks of every row and
    /// blank rows at both ends dropped.
    pub text: String,
    /// The byte ranges of `text` that were drawn hidden. Every range is non-empty, the list is sorted and the ranges
    /// do not overlap; each starts and ends on a character boundary. The text stays in place: removing it is the
    /// consumer's work.
    pub hidden: Vec<Range<usize>>,
    /// True when anything was cut: by the byte bound, the row cap or the cell cap.
    pub truncated: bool,
}

impl fmt::Debug for TextRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextRead")
            .field("bytes", &self.text.len())
            .field("hidden_ranges", &self.hidden.len())
            .field("truncated", &self.truncated)
            .finish()
    }
}

/// Which end of an over-long text survives a cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cut {
    /// Keep the start (a selection: what the user began with).
    KeepHead,
    /// Keep the end (the screen, the last rows: the newest text matters most).
    KeepTail,
}

/// Builds the text of a read cell by cell.
///
/// A cell whose text is empty or whitespace only is a blank. Blanks are held back and written as one space each only
/// when a later non-blank cell of the same line arrives, so trailing blanks never reach the text, and a blank that
/// ends a soft-wrapped row is kept when the line goes on in the next row.
pub struct TextBuilder {
    cut: Cut,
    text: String,
    hidden: Vec<Range<usize>>,
    /// Blanks seen since the last non-blank cell, not written yet.
    blanks: usize,
    truncated: bool,
}

impl fmt::Debug for TextBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextBuilder")
            .field("bytes", &self.text.len())
            .field("hidden_ranges", &self.hidden.len())
            .field("truncated", &self.truncated)
            .finish()
    }
}

impl TextBuilder {
    /// A builder that keeps the `cut` end of a text that is longer than the bound given to [`TextBuilder::finish`].
    pub fn new(cut: Cut) -> Self {
        Self { cut, text: String::new(), hidden: Vec::new(), blanks: 0, truncated: false }
    }

    /// The text of one cell. `hidden` is true when the user could not read it.
    pub fn cell(&mut self, grapheme: &str, hidden: bool) {
        if grapheme.chars().all(char::is_whitespace) {
            self.blank();
            return;
        }
        for _ in 0..std::mem::take(&mut self.blanks) {
            self.text.push(' ');
        }
        let start = self.text.len();
        self.text.push_str(grapheme);
        if hidden {
            self.cover(start..self.text.len());
        }
    }

    /// A cell with nothing to show.
    pub fn blank(&mut self) {
        self.blanks = self.blanks.saturating_add(1);
    }

    /// The end of a row. `wrapped` is true when the line goes on in the next row: no line feed is written and the
    /// held blanks stay held.
    pub fn end_row(&mut self, wrapped: bool) {
        if !wrapped {
            self.blanks = 0;
            self.text.push('\n');
        }
    }

    /// Note that rows were left out before the first one this builder saw.
    pub fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    /// Record the range of a hidden cell: merged into the previous range when they touch. Past the cap the last
    /// range grows to the end of this cell, so a hostile screen can cost visible text but never uncover hidden text.
    fn cover(&mut self, range: Range<usize>) {
        let full = self.hidden.len() >= MAX_HIDDEN_RANGES;
        match self.hidden.last_mut() {
            Some(last) if full || last.end == range.start => last.end = range.end,
            _ => self.hidden.push(range),
        }
    }

    /// The finished read: `None` when there is no text or `max_bytes` is 0, otherwise the text cut to `max_bytes` on
    /// a character boundary, with the ranges moved or clipped to follow it.
    pub fn finish(self, max_bytes: usize) -> Option<TextRead> {
        if max_bytes == 0 {
            return None;
        }
        let Self { cut, mut text, mut hidden, mut truncated, .. } = self;

        // Blank rows at both ends.
        let tail = text.trim_end_matches('\n').len();
        text.truncate(tail);
        let lead = text.len() - text.trim_start_matches('\n').len();
        if lead > 0 {
            text.drain(..lead);
            shift_ranges(&mut hidden, lead);
        }
        if text.is_empty() {
            return None;
        }

        if text.len() > max_bytes {
            truncated = true;
            match cut {
                Cut::KeepTail => {
                    let mut start = text.len() - max_bytes;
                    while !text.is_char_boundary(start) {
                        start += 1;
                    }
                    text.drain(..start);
                    shift_ranges(&mut hidden, start);
                }
                Cut::KeepHead => {
                    let mut end = max_bytes;
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    text.truncate(end);
                    let kept = text.trim_end_matches('\n').len();
                    text.truncate(kept);
                    hidden.retain_mut(|r| {
                        r.end = r.end.min(kept);
                        r.start < r.end
                    });
                }
            }
            if text.is_empty() {
                return None;
            }
        }
        Some(TextRead { text, hidden, truncated })
    }
}

/// Remove `by` bytes in front of every range: a range that ends inside them is gone, one that starts inside them
/// starts at 0.
fn shift_ranges(ranges: &mut Vec<Range<usize>>, by: usize) {
    ranges.retain_mut(|r| {
        if r.end <= by {
            return false;
        }
        r.start = r.start.saturating_sub(by);
        r.end -= by;
        true
    });
}

/// How many of `wanted` rows a read of a grid `cols` wide may walk: at most [`MAX_READ_ROWS`], at most
/// [`MAX_READ_CELLS`] cells, and at least one when any row is wanted. A width of 0 counts as 1.
pub fn rows_within_budget(cols: u16, wanted: u32) -> u32 {
    if wanted == 0 {
        return 0;
    }
    let by_cells = u32::try_from(MAX_READ_CELLS / usize::from(cols.max(1))).unwrap_or(u32::MAX);
    wanted.min(u32::from(MAX_READ_ROWS)).min(by_cells).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of cells: a space is a blank, anything else a visible cell.
    fn row(b: &mut TextBuilder, text: &str, wrapped: bool) {
        for c in text.chars() {
            if c == ' ' {
                b.blank();
            } else {
                b.cell(&c.to_string(), false);
            }
        }
        b.end_row(wrapped);
    }

    fn builder() -> TextBuilder {
        TextBuilder::new(Cut::KeepTail)
    }

    #[test]
    fn trailing_blanks_go_and_rows_join_with_one_newline() {
        let mut b = builder();
        row(&mut b, "ab  ", false);
        row(&mut b, "c", false);
        let read = b.finish(4096).expect("text");
        assert_eq!(read.text, "ab\nc");
        assert!(read.hidden.is_empty());
        assert!(!read.truncated);
    }

    #[test]
    fn a_wrapped_row_continues_the_line_and_keeps_its_blanks() {
        let mut b = builder();
        row(&mut b, "foo  ", true);
        row(&mut b, "bar", false);
        assert_eq!(b.finish(4096).expect("text").text, "foo  bar");
    }

    #[test]
    fn blank_rows_at_both_ends_go_and_inner_ones_stay() {
        let mut b = builder();
        row(&mut b, "", false);
        row(&mut b, "  ", false);
        row(&mut b, "x", false);
        row(&mut b, "", false);
        row(&mut b, "y", false);
        row(&mut b, "", false);
        row(&mut b, "", false);
        assert_eq!(b.finish(4096).expect("text").text, "x\n\ny");

        let mut b = builder();
        row(&mut b, "  z", false);
        assert_eq!(b.finish(4096).expect("text").text, "  z", "the indentation of the first kept row stays");
    }

    #[test]
    fn blank_text_and_a_zero_bound_are_no_text() {
        let mut b = builder();
        row(&mut b, "   ", false);
        row(&mut b, "", false);
        assert_eq!(b.finish(4096), None);

        let mut b = builder();
        b.cell("\u{a0}", false);
        b.cell(" ", false);
        b.end_row(false);
        assert_eq!(b.finish(4096), None, "cells of whitespace are blanks");

        let mut b = builder();
        row(&mut b, "abc", false);
        assert_eq!(b.finish(0), None);
    }

    #[test]
    fn hidden_cells_become_byte_ranges_that_merge_only_when_adjacent() {
        let mut b = builder();
        b.cell("a", true);
        b.cell("b", true);
        b.end_row(false);
        assert_eq!(b.finish(4096).expect("text").hidden, vec![0..2]);

        let mut b = builder();
        b.cell("a", true);
        b.blank();
        b.cell("b", true);
        b.end_row(false);
        let read = b.finish(4096).expect("text");
        assert_eq!(read.text, "a b");
        assert_eq!(read.hidden, vec![0..1, 2..3], "the blank between is visible");

        let mut b = builder();
        b.cell("\u{e9}", true);
        b.cell("x", true);
        b.end_row(false);
        let read = b.finish(4096).expect("text");
        assert_eq!(read.hidden, vec![0..3]);
        for r in &read.hidden {
            assert!(read.text.is_char_boundary(r.start) && read.text.is_char_boundary(r.end));
        }
    }

    #[test]
    fn ranges_follow_the_text_when_leading_rows_are_dropped() {
        let mut b = builder();
        row(&mut b, "", false);
        row(&mut b, "", false);
        for c in "SECRET".chars() {
            b.cell(&c.to_string(), true);
        }
        b.end_row(false);
        let read = b.finish(4096).expect("text");
        assert_eq!(read.text, "SECRET");
        assert_eq!(read.hidden, vec![0..6]);
    }

    /// Twenty characters, `a` and `\u{e9}` alternating: 30 bytes. Character `i` starts at `3 * (i / 2)` when it is an
    /// `a` and one byte later when it is an `\u{e9}`. The characters listed in `hide` are hidden.
    fn mixed(cut: Cut, hide: &[usize]) -> TextBuilder {
        let mut b = TextBuilder::new(cut);
        for i in 0..20 {
            b.cell(if i % 2 == 0 { "a" } else { "\u{e9}" }, hide.contains(&i));
        }
        b.end_row(false);
        b
    }

    #[test]
    fn a_tail_cut_lands_on_a_character_boundary_and_ranges_move_with_it() {
        let whole = mixed(Cut::KeepTail, &[]).finish(4096).expect("text").text;
        assert_eq!(whole.len(), 30);
        // Character 3 (bytes 4..6) lies in the removed part, character 18 (bytes 27..28) in the kept part.
        let read = mixed(Cut::KeepTail, &[3, 18]).finish(10).expect("text");
        assert!(read.text.len() <= 10);
        assert!(whole.ends_with(&read.text), "the end of the original survives");
        assert!(read.truncated);
        // Byte 20 is inside a two-byte character, so the cut moves forward to byte 21.
        assert_eq!(read.text.len(), 9);
        assert_eq!(read.hidden, vec![6..7], "the range inside the kept part moved by 21, the other is gone");
        assert_eq!(&read.text[read.hidden[0].clone()], "a");
    }

    #[test]
    fn a_head_cut_lands_on_a_character_boundary_and_ranges_are_clipped() {
        let whole = mixed(Cut::KeepHead, &[]).finish(4096).expect("text").text;
        // Characters 6 and 7 are bytes 9..12, which the cut at byte 10 splits; character 12 starts after it.
        let read = mixed(Cut::KeepHead, &[6, 7, 12]).finish(10).expect("text");
        assert!(read.text.len() <= 10);
        assert!(whole.starts_with(&read.text), "the start of the original survives");
        assert!(read.truncated);
        assert_eq!(read.hidden, vec![9..10], "the range across the cut is clipped, the one after it is gone");

        // A bound inside a two-byte character gives it up whole.
        let read = mixed(Cut::KeepHead, &[]).finish(11).expect("text");
        assert_eq!(read.text.len(), 10);

        // Line feeds left by the cut are trimmed.
        let mut b = TextBuilder::new(Cut::KeepHead);
        row(&mut b, "abc", false);
        row(&mut b, "def", false);
        let read = b.finish(4).expect("text");
        assert_eq!(read.text, "abc");
        assert!(read.truncated);
    }

    #[test]
    fn more_hidden_runs_than_the_cap_grow_the_last_range_instead_of_leaking() {
        let mut b = builder();
        for _ in 0..3000 {
            b.cell("H", true);
            b.cell("v", false);
        }
        b.end_row(false);
        let read = b.finish(1 << 20).expect("text");
        assert!(read.hidden.len() <= MAX_HIDDEN_RANGES, "{} ranges", read.hidden.len());
        for (at, _) in read.text.match_indices('H') {
            assert!(read.hidden.iter().any(|r| r.contains(&at)), "the hidden H at {at} lies in no range");
        }
        for pair in read.hidden.windows(2) {
            assert!(pair[0].end <= pair[1].start, "sorted and not overlapping");
        }
    }

    #[test]
    fn debug_prints_sizes_and_never_the_text() {
        const CANARY: &str = "QZXJWK";
        let mut b = builder();
        for c in CANARY.chars() {
            b.cell(&c.to_string(), true);
        }
        b.end_row(false);
        for shown in [format!("{b:?}"), format!("{b:#?}")] {
            assert!(!shown.contains(CANARY));
            assert!(shown.contains("bytes: 7"), "{shown}");
            for pair in CANARY.as_bytes().windows(2) {
                assert!(!shown.contains(std::str::from_utf8(pair).unwrap_or("?!")), "{shown}");
            }
        }
        let read = b.finish(100).expect("text");
        for shown in [format!("{read:?}"), format!("{read:#?}")] {
            assert!(!shown.contains(CANARY));
            assert!(shown.contains("bytes: 6"), "{shown}");
            for pair in CANARY.as_bytes().windows(2) {
                assert!(!shown.contains(std::str::from_utf8(pair).unwrap_or("?!")), "{shown}");
            }
        }
    }

    #[test]
    fn the_row_budget_follows_the_width_and_the_caps() {
        let rows = u32::from(MAX_READ_ROWS);
        assert_eq!(rows_within_budget(80, 5000), rows);
        assert_eq!(rows_within_budget(1000, 5000), (MAX_READ_CELLS / 1000) as u32);
        assert_eq!(rows_within_budget(0, 5000), rows.min(MAX_READ_CELLS as u32), "a width of 0 counts as 1");
        assert_eq!(rows_within_budget(80, 7), 7, "a count below the budget is unchanged");
        assert_eq!(rows_within_budget(u16::MAX, 1), 1);
        assert!(rows_within_budget(u16::MAX, 50) >= 1);
        assert_eq!(rows_within_budget(80, 0), 0);
    }
}
