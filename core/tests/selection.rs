//! The engine-owned selection (DESK-18): it is tracked, so it survives output, scrolling and resizing, it is
//! reported as spans in viewport coordinates, and it is made by one `SelectOp` API (cell range, word, line, all).
#![cfg(feature = "ghostty")]

mod common;

use common::engine;
use holzwave_core::TerminalEngine;
use holzwave_core::engine::{SelectMode, SelectOp, Span};
use holzwave_core::wire::{FrameEncoder, FrameMsg};

/// Apply an operation and assert that the engine accepted it.
fn op(e: &mut impl TerminalEngine, op: SelectOp) {
    let result = e.select(op);
    assert!(result.is_ok(), "select({op:?}) must be accepted, got {result:?}");
}

fn text(e: &mut impl TerminalEngine) -> Option<String> {
    let result = e.selection_text();
    assert!(result.is_ok(), "selection_text must work, got {result:?}");
    result.unwrap_or(None)
}

fn spans(e: &mut impl TerminalEngine) -> Vec<Span> {
    e.update().expect("update").selection
}

/// `row01`, `row02`, ... one per line, the last one without a line break (the cursor stays on it).
fn numbered_rows(e: &mut impl TerminalEngine, count: usize) {
    for n in 1..=count {
        let line = if n < count { format!("row{n:02}\r\n") } else { format!("row{n:02}") };
        e.feed(line.as_bytes());
    }
}

fn span(y: u16, x0: u16, x1: u16) -> Span {
    Span { y, x0, x1 }
}

/// 12 numbered rows in a 20x5 engine: the viewport shows `row08` to `row12`, so `row09` is on y 1 and `row10` on y 2.
fn twelve_rows() -> impl TerminalEngine {
    let mut e = engine(20, 5);
    numbered_rows(&mut e, 12);
    e
}

const BEGIN: SelectOp = SelectOp::Begin { x: 0, y: 1, mode: SelectMode::Cell };
const EXTEND: SelectOp = SelectOp::Extend { x: 3, y: 2 };

#[test]
fn a_cell_range_is_the_text_between_the_two_points_and_a_span_per_row() {
    let mut e = twelve_rows();
    op(&mut e, BEGIN);
    op(&mut e, EXTEND);

    // Two rows joined with one newline; the second row is cut after the fourth cell.
    assert_eq!(text(&mut e).as_deref(), Some("row09\nrow1"));
    // The first row is selected from the start to the end of the row, the second from the start to x 3.
    assert_eq!(spans(&mut e), vec![span(1, 0, 19), span(2, 0, 3)]);
}

#[test]
fn the_text_stays_the_same_after_output_and_the_spans_follow_the_scroll() {
    let mut e = twelve_rows();
    op(&mut e, BEGIN);
    op(&mut e, EXTEND);
    let before = spans(&mut e);
    assert_eq!(before, vec![span(1, 0, 19), span(2, 0, 3)]);

    // Eight more lines push the selected rows out of the viewport.
    for n in 13..=20 {
        e.feed(format!("\r\nrow{n:02}").as_bytes());
    }
    assert_eq!(text(&mut e).as_deref(), Some("row09\nrow1"), "output must not change what is selected");
    assert!(spans(&mut e).is_empty(), "the selected rows are not on the screen any more");

    // Scrolled back far enough, the same rows are visible again at the same places.
    e.scroll(-8);
    assert_eq!(text(&mut e).as_deref(), Some("row09\nrow1"), "scrolling must not change what is selected");
    assert_eq!(spans(&mut e), before);

    // Two rows further down only the second selected row is still on the screen, now on y 0.
    e.scroll(2);
    assert_eq!(spans(&mut e), vec![span(0, 0, 3)]);
}

#[test]
fn spans_never_leave_the_viewport_wherever_it_is_scrolled() {
    let mut e = twelve_rows();
    op(&mut e, SelectOp::All);
    for n in 13..=40 {
        e.feed(format!("\r\nrow{n:02}").as_bytes());
    }
    for delta in [-100, 7, 7, 7, 7, 7, 100, -3, -3] {
        e.scroll(delta);
        let update = e.update().expect("update");
        for s in &update.selection {
            assert!(s.y < update.rows, "span row {} outside {} rows: {:?}", s.y, update.rows, update.selection);
            assert!(s.x0 <= s.x1 && s.x1 < update.cols, "span columns of {s:?} outside {} cols", update.cols);
        }
        let ys: Vec<u16> = update.selection.iter().map(|s| s.y).collect();
        let mut sorted = ys.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(ys, sorted, "spans come once per row in viewport order");
    }
}

#[test]
fn a_word_selection_selects_the_word_under_the_point() {
    let mut e = engine(20, 5);
    e.feed(b"hello world foo");
    op(&mut e, SelectOp::Begin { x: 7, y: 0, mode: SelectMode::Word });

    assert_eq!(text(&mut e).as_deref(), Some("world"));
    assert_eq!(spans(&mut e), vec![span(0, 6, 10)]);
}

#[test]
fn a_line_selection_selects_the_whole_logical_line_and_unwraps_soft_wraps() {
    let mut e = engine(20, 5);
    // 28 characters wrap after column 20: one logical line on two rows, then a second logical line.
    e.feed(b"0123456789abcdefghijKLMNOPQR\r\nnext line");
    op(&mut e, SelectOp::Begin { x: 2, y: 1, mode: SelectMode::Line });

    assert_eq!(text(&mut e).as_deref(), Some("0123456789abcdefghijKLMNOPQR"));
    let selected = spans(&mut e);
    assert_eq!(selected.first().map(|s| s.y), Some(0), "the first wrapped row is part of the line: {selected:?}");
    assert_eq!(selected.last().map(|s| s.y), Some(1));
    assert!(selected.iter().all(|s| s.y <= 1), "the next logical line is not selected: {selected:?}");
}

#[test]
fn extending_without_a_begin_is_ignored_without_an_error() {
    let mut e = twelve_rows();
    op(&mut e, SelectOp::Extend { x: 3, y: 2 });

    assert_eq!(text(&mut e), None);
    assert!(spans(&mut e).is_empty());
}

#[test]
fn select_all_takes_scrollback_and_screen_and_clear_removes_it() {
    let mut e = twelve_rows();
    op(&mut e, SelectOp::All);

    let all = text(&mut e).expect("everything is selected");
    assert!(all.starts_with("row01\nrow02\n"), "starts in the scrollback: {all:?}");
    assert!(all.ends_with("row12"), "ends with the last row, trailing blanks trimmed: {all:?}");
    assert_eq!(spans(&mut e).len(), 5, "every row of the viewport carries a span");

    op(&mut e, SelectOp::Clear);
    assert_eq!(text(&mut e), None);
    assert!(spans(&mut e).is_empty());
}

#[test]
fn clear_without_a_selection_is_fine() {
    let mut e = twelve_rows();
    op(&mut e, SelectOp::Clear);
    assert_eq!(text(&mut e), None);
}

#[test]
fn a_second_begin_replaces_the_first_selection() {
    let mut e = twelve_rows();
    op(&mut e, BEGIN);
    op(&mut e, EXTEND);
    op(&mut e, SelectOp::Begin { x: 0, y: 4, mode: SelectMode::Cell });
    op(&mut e, SelectOp::Extend { x: 4, y: 4 });

    assert_eq!(text(&mut e).as_deref(), Some("row12"));
    assert_eq!(spans(&mut e), vec![span(4, 0, 4)]);
}

#[test]
fn setting_and_clearing_a_selection_changes_only_the_frame_header() {
    let mut e = twelve_rows();
    let mut enc = FrameEncoder::new(1);
    let first: FrameMsg = enc.encode(e.update().expect("update")).expect("first frame");
    assert!(first.full && first.selection.is_empty());

    op(&mut e, BEGIN);
    op(&mut e, EXTEND);
    let selected = enc.encode(e.update().expect("update")).expect("a new selection is a frame");
    assert!(!selected.full, "a selection is not a reset");
    assert!(selected.changed.is_empty(), "no row changed, only the header: {:?}", selected.changed);
    assert_eq!(selected.selection, vec![(1, 0, 19), (2, 0, 3)]);

    op(&mut e, SelectOp::Clear);
    let cleared = enc.encode(e.update().expect("update")).expect("clearing is a frame");
    assert!(!cleared.full);
    assert!(cleared.changed.is_empty());
    assert!(cleared.selection.is_empty());
}

#[test]
fn a_selection_survives_a_resize() {
    let mut e = twelve_rows();
    op(&mut e, BEGIN);
    op(&mut e, EXTEND);

    e.resize(30, 5, (0, 0)).expect("wider");
    assert_eq!(text(&mut e).as_deref(), Some("row09\nrow1"));
    e.resize(12, 4, (0, 0)).expect("smaller");
    assert_eq!(text(&mut e).as_deref(), Some("row09\nrow1"));
}

#[test]
fn coordinates_outside_the_grid_are_clamped_not_trusted() {
    let mut e = twelve_rows();
    op(&mut e, SelectOp::Begin { x: u16::MAX, y: u16::MAX, mode: SelectMode::Cell });
    op(&mut e, SelectOp::Extend { x: u16::MAX, y: u16::MAX });
    // Clamped to the last cell of the last row; the cursor sits right after "row12" so that cell is blank.
    assert!(spans(&mut e).iter().all(|s| s.y == 4 && s.x1 == 19));

    for mode in [SelectMode::Word, SelectMode::Line] {
        op(&mut e, SelectOp::Begin { x: u16::MAX, y: u16::MAX, mode });
    }
    op(&mut e, SelectOp::Begin { x: 0, y: 0, mode: SelectMode::Cell });
    op(&mut e, SelectOp::Extend { x: u16::MAX, y: u16::MAX });
    let update = e.update().expect("update");
    assert!(update.selection.iter().all(|s| s.y < update.rows && s.x1 < update.cols));
}

/// The spans the engine reports from its per-row cache must equal what a full pass over every row reports, after
/// any mix of output, scrolling, selecting and resizing: a stale cache would show a highlight where there is none.
#[test]
fn the_cached_spans_equal_a_full_pass_after_random_operations() {
    use common::Xorshift64;

    for seed in 1..=10u64 {
        let mut rng = Xorshift64::new(seed);
        let mut e = engine(20, 6);
        let (mut cols, mut rows) = (20u16, 6u16);
        for step in 0..150 {
            match rng.below(9) {
                0 | 1 => {
                    let word = ["alpha beta", "x", "wrapping line of text that is longer than a row", "\r\n", "gamma"]
                        [rng.below(5)];
                    e.feed(word.as_bytes());
                    e.feed(b"\r\n");
                }
                2 => e.scroll(rng.below(9) as isize - 4),
                3 => e.scroll_to_bottom(),
                4 => {
                    let mode = [SelectMode::Cell, SelectMode::Word, SelectMode::Line][rng.below(3)];
                    let (x, y) = (rng.below(usize::from(cols) + 2) as u16, rng.below(usize::from(rows) + 2) as u16);
                    op(&mut e, SelectOp::Begin { x, y, mode });
                }
                5 | 6 => {
                    let (x, y) = (rng.below(usize::from(cols) + 2) as u16, rng.below(usize::from(rows) + 2) as u16);
                    op(&mut e, SelectOp::Extend { x, y });
                }
                7 => op(&mut e, if rng.chance(50) { SelectOp::All } else { SelectOp::Clear }),
                _ => {
                    cols = 8 + rng.below(20) as u16;
                    rows = 3 + rng.below(6) as u16;
                    e.resize(cols, rows, (0, 0)).expect("resize");
                }
            }
            let cached = spans(&mut e);
            e.request_full();
            let full = spans(&mut e);
            assert_eq!(cached, full, "seed {seed} step {step}: the cache is stale");
        }
    }
}
