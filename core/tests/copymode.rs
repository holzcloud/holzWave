//! Copy mode (DESK-21 / F6): a keyboard cursor over screen and scrollback, character, line and word motions, a
//! selection anchor and a yank, all owned by the engine and driven by `CopyOp`.
//!
//! Positions in these tests are viewport cells `(x, y)` as `Update.copy_cursor` reports them.
#![cfg(feature = "ghostty")]

mod common;

use common::engine;
use holzwave_core::TerminalEngine;
use holzwave_core::engine::{CopyMotion, CopyOp, CopyState, Span};

fn op(e: &mut impl TerminalEngine, op: CopyOp) -> CopyState {
    match e.copy_mode(op) {
        Ok(state) => state,
        Err(err) => panic!("copy_mode({op:?}) must be accepted, got {err:?}"),
    }
}

fn mv(e: &mut impl TerminalEngine, motion: CopyMotion) -> CopyState {
    op(e, CopyOp::Move(motion))
}

fn moves(e: &mut impl TerminalEngine, motion: CopyMotion, times: usize) {
    for _ in 0..times {
        mv(e, motion);
    }
}

fn cursor(e: &mut impl TerminalEngine) -> Option<(u16, u16)> {
    e.update().expect("update").copy_cursor
}

/// The text of every viewport row, trailing blanks removed.
fn rows_text(e: &mut impl TerminalEngine) -> Vec<String> {
    let screen = e.screen().expect("screen");
    let cols = usize::from(screen.cols);
    screen
        .cells
        .chunks(cols)
        .map(|row| row.iter().map(|c| c.text.as_str()).collect::<String>().trim_end().to_owned())
        .collect()
}

/// A 20x5 engine with four rows of text and the terminal cursor behind `zeta`:
///
/// ```text
/// y 0  alpha beta
/// y 1    gamma delta       (two leading blanks)
/// y 2                      (empty)
/// y 3  zeta                (the terminal cursor is at x 4)
/// y 4                      (empty)
/// ```
fn four_rows() -> impl TerminalEngine {
    let mut e = engine(20, 5);
    e.feed(b"alpha beta\r\n  gamma delta\r\n\r\nzeta");
    e
}

/// `row01` to `row30`, one per line, the last without a line break: the viewport of a 20x5 engine shows `row26` to
/// `row30`, the terminal cursor is at x 5 on the last row.
fn thirty_rows() -> impl TerminalEngine {
    let mut e = engine(20, 5);
    for n in 1..=30 {
        let line = if n < 30 { format!("row{n:02}\r\n") } else { format!("row{n:02}") };
        e.feed(line.as_bytes());
    }
    e
}

fn span(y: u16, x0: u16, x1: u16) -> Span {
    Span { y, x0, x1 }
}

#[test]
fn enter_puts_the_cursor_on_the_terminal_cursor_and_is_not_selecting() {
    let mut e = four_rows();
    assert_eq!(cursor(&mut e), None, "no copy cursor outside copy mode");

    let state = op(&mut e, CopyOp::Enter);
    assert_eq!(state, CopyState { active: true, selecting: false, yanked: None });
    assert_eq!(cursor(&mut e), Some((4, 3)));

    // Entering again changes nothing.
    assert_eq!(op(&mut e, CopyOp::Enter), CopyState { active: true, selecting: false, yanked: None });
    assert_eq!(cursor(&mut e), Some((4, 3)));
}

#[test]
fn a_hidden_terminal_cursor_starts_the_copy_cursor_on_the_last_row_in_column_zero() {
    let mut e = four_rows();
    e.feed(b"\x1b[?25l");
    op(&mut e, CopyOp::Enter);
    assert_eq!(cursor(&mut e), Some((0, 4)));
}

#[test]
fn enter_brings_the_cursor_into_view_when_the_viewport_was_scrolled_back() {
    let mut e = thirty_rows();
    e.scroll(-10);
    op(&mut e, CopyOp::Enter);
    assert_eq!(cursor(&mut e), Some((5, 4)));
    assert_eq!(rows_text(&mut e)[4], "row30");
}

#[test]
fn the_cell_motions_move_one_step_and_stop_at_the_edges() {
    let mut e = four_rows();
    op(&mut e, CopyOp::Enter);

    mv(&mut e, CopyMotion::Left);
    assert_eq!(cursor(&mut e), Some((3, 3)));
    mv(&mut e, CopyMotion::Right);
    assert_eq!(cursor(&mut e), Some((4, 3)));
    mv(&mut e, CopyMotion::Up);
    assert_eq!(cursor(&mut e), Some((4, 2)), "one row, onto the empty row as well");
    mv(&mut e, CopyMotion::Down);
    assert_eq!(cursor(&mut e), Some((4, 3)));

    moves(&mut e, CopyMotion::Left, 30);
    assert_eq!(cursor(&mut e), Some((0, 3)), "the left edge stops, the cursor does not wrap");
    moves(&mut e, CopyMotion::Right, 40);
    assert_eq!(cursor(&mut e), Some((19, 3)), "the right edge stops");
    moves(&mut e, CopyMotion::Up, 10);
    assert_eq!(cursor(&mut e), Some((19, 0)), "the first row stops");
    moves(&mut e, CopyMotion::Down, 10);
    assert_eq!(cursor(&mut e), Some((19, 4)), "the last row stops");
}

#[test]
fn line_start_and_line_end_go_to_the_first_and_last_non_blank_cell() {
    let mut e = four_rows();
    op(&mut e, CopyOp::Enter);
    moves(&mut e, CopyMotion::Up, 2);
    assert_eq!(cursor(&mut e), Some((4, 1)));

    mv(&mut e, CopyMotion::LineStart);
    assert_eq!(cursor(&mut e), Some((2, 1)), "two blanks of indentation are skipped");
    mv(&mut e, CopyMotion::LineEnd);
    assert_eq!(cursor(&mut e), Some((12, 1)), "`  gamma delta` ends at x 12, not at the right edge");

    // An empty row has nothing to go to: the start of the row.
    mv(&mut e, CopyMotion::Down);
    mv(&mut e, CopyMotion::LineEnd);
    assert_eq!(cursor(&mut e), Some((0, 2)));
    mv(&mut e, CopyMotion::LineStart);
    assert_eq!(cursor(&mut e), Some((0, 2)));
}

#[test]
fn top_and_bottom_go_to_the_ends_of_the_scrollback_and_the_viewport_follows() {
    let mut e = thirty_rows();
    op(&mut e, CopyOp::Enter);

    mv(&mut e, CopyMotion::Top);
    assert_eq!(cursor(&mut e), Some((0, 0)));
    assert_eq!(rows_text(&mut e)[0], "row01", "the viewport follows to the first row of the scrollback");
    assert_eq!(e.update().expect("update").scrollbar.offset, 0);

    mv(&mut e, CopyMotion::Bottom);
    assert_eq!(cursor(&mut e), Some((0, 4)));
    assert_eq!(rows_text(&mut e)[4], "row30");
}

#[test]
fn page_up_and_page_down_move_one_screen_height_and_the_cursor_stays_visible() {
    let mut e = thirty_rows();
    op(&mut e, CopyOp::Enter);
    assert_eq!(cursor(&mut e), Some((5, 4)));

    // Five rows up: `row25`, which was just above the viewport, so the viewport moved by one row.
    mv(&mut e, CopyMotion::PageUp);
    let (x, y) = cursor(&mut e).expect("the cursor is on screen");
    assert_eq!(rows_text(&mut e)[usize::from(y)], "row25");
    assert_eq!(x, 5, "the column is kept");

    moves(&mut e, CopyMotion::PageUp, 10);
    let (_, y) = cursor(&mut e).expect("the cursor is on screen");
    assert_eq!(rows_text(&mut e)[usize::from(y)], "row01", "the first row stops the page motion");

    moves(&mut e, CopyMotion::PageDown, 10);
    let (_, y) = cursor(&mut e).expect("the cursor is on screen");
    assert_eq!(rows_text(&mut e)[usize::from(y)], "row30", "the last row stops it");
}

#[test]
fn toggle_selection_extends_from_the_anchor_and_yank_returns_the_text_and_leaves_the_mode() {
    let mut e = four_rows();
    op(&mut e, CopyOp::Enter);
    mv(&mut e, CopyMotion::Top);

    let state = op(&mut e, CopyOp::ToggleSelection);
    assert_eq!(state, CopyState { active: true, selecting: true, yanked: None });
    moves(&mut e, CopyMotion::Right, 4);
    assert_eq!(e.update().expect("update").selection, vec![span(0, 0, 4)], "`alpha` is selected");

    let state = op(&mut e, CopyOp::Yank);
    assert_eq!(state, CopyState { active: false, selecting: false, yanked: Some("alpha".into()) });

    let update = e.update().expect("update");
    assert_eq!(update.copy_cursor, None, "yank leaves copy mode");
    assert!(update.selection.is_empty(), "and clears the selection");
}

#[test]
fn a_selection_over_two_rows_is_unwrapped_text_in_either_direction() {
    let mut e = four_rows();
    op(&mut e, CopyOp::Enter);
    mv(&mut e, CopyMotion::Top);
    moves(&mut e, CopyMotion::Right, 6); // x 6 on `beta`
    op(&mut e, CopyOp::ToggleSelection);
    mv(&mut e, CopyMotion::Down); // x 6 on `gamma`
    assert_eq!(cursor(&mut e), Some((6, 1)));
    assert_eq!(e.update().expect("update").selection, vec![span(0, 6, 19), span(1, 0, 6)]);
    let forward = op(&mut e, CopyOp::Yank).yanked;
    assert_eq!(forward.as_deref(), Some("beta\n  gamma"));

    // The same cells selected upwards: the anchor is the lower one and the cursor is the end that moves.
    op(&mut e, CopyOp::Enter);
    mv(&mut e, CopyMotion::Top);
    moves(&mut e, CopyMotion::Right, 6);
    mv(&mut e, CopyMotion::Down);
    op(&mut e, CopyOp::ToggleSelection);
    mv(&mut e, CopyMotion::Up);
    assert_eq!(cursor(&mut e), Some((6, 0)));
    assert_eq!(e.update().expect("update").selection, vec![span(0, 6, 19), span(1, 0, 6)]);
    assert_eq!(op(&mut e, CopyOp::Yank).yanked, forward);
}

#[test]
fn the_cursor_is_the_end_that_moves_when_the_selection_shrinks_and_grows_again() {
    let mut e = four_rows();
    op(&mut e, CopyOp::Enter);
    mv(&mut e, CopyMotion::Top);
    op(&mut e, CopyOp::ToggleSelection);

    moves(&mut e, CopyMotion::Right, 3);
    moves(&mut e, CopyMotion::Left, 2);
    assert_eq!(cursor(&mut e), Some((1, 0)));
    assert_eq!(e.update().expect("update").selection, vec![span(0, 0, 1)]);
    moves(&mut e, CopyMotion::Down, 1);
    assert_eq!(cursor(&mut e), Some((1, 1)));
    assert_eq!(e.update().expect("update").selection, vec![span(0, 0, 19), span(1, 0, 1)]);
}

#[test]
fn toggling_again_collapses_the_selection_to_the_cursor_cell() {
    let mut e = four_rows();
    op(&mut e, CopyOp::Enter);
    mv(&mut e, CopyMotion::Top);
    op(&mut e, CopyOp::ToggleSelection);
    moves(&mut e, CopyMotion::Right, 3);

    let state = op(&mut e, CopyOp::ToggleSelection);
    assert_eq!(state, CopyState { active: true, selecting: false, yanked: None });
    assert_eq!(cursor(&mut e), Some((3, 0)));
    assert_eq!(e.update().expect("update").selection, vec![span(0, 3, 3)], "only the cursor cell");

    // A motion without a selection moves the one-cell selection, it does not extend it.
    mv(&mut e, CopyMotion::Right);
    assert_eq!(e.update().expect("update").selection, vec![span(0, 4, 4)]);

    assert_eq!(op(&mut e, CopyOp::Yank).yanked.as_deref(), Some("a"), "the cell under the cursor, `alpha`'s last `a`");
}

#[test]
fn word_forward_goes_to_the_first_character_of_the_next_word_and_over_line_ends() {
    let mut e = four_rows();
    op(&mut e, CopyOp::Enter);
    mv(&mut e, CopyMotion::Top);

    mv(&mut e, CopyMotion::WordForward);
    assert_eq!(cursor(&mut e), Some((6, 0)), "`beta`");
    mv(&mut e, CopyMotion::WordForward);
    assert_eq!(cursor(&mut e), Some((2, 1)), "`gamma` on the next row, past the end of this line");
    mv(&mut e, CopyMotion::WordForward);
    assert_eq!(cursor(&mut e), Some((8, 1)), "`delta`");
    mv(&mut e, CopyMotion::WordForward);
    assert_eq!(cursor(&mut e), Some((0, 3)), "`zeta`, over the empty row");

    // Nothing is left: the cursor stays on the last word.
    mv(&mut e, CopyMotion::WordForward);
    assert_eq!(cursor(&mut e), Some((0, 3)));
}

#[test]
fn word_back_goes_to_the_start_of_this_word_or_else_of_the_previous_one() {
    let mut e = four_rows();
    op(&mut e, CopyOp::Enter);
    moves(&mut e, CopyMotion::Left, 2); // x 2 on `zeta`
    assert_eq!(cursor(&mut e), Some((2, 3)));

    mv(&mut e, CopyMotion::WordBack);
    assert_eq!(cursor(&mut e), Some((0, 3)), "inside a word: its first character");
    mv(&mut e, CopyMotion::WordBack);
    assert_eq!(cursor(&mut e), Some((8, 1)), "already at a word start: the previous word, over the empty row");
    mv(&mut e, CopyMotion::WordBack);
    assert_eq!(cursor(&mut e), Some((2, 1)));
    mv(&mut e, CopyMotion::WordBack);
    assert_eq!(cursor(&mut e), Some((6, 0)), "over the line end upwards");
    mv(&mut e, CopyMotion::WordBack);
    assert_eq!(cursor(&mut e), Some((0, 0)));
    mv(&mut e, CopyMotion::WordBack);
    assert_eq!(cursor(&mut e), Some((0, 0)), "the very first cell stops");
}

#[test]
fn exit_leaves_copy_mode_without_yanking_and_clears_the_selection() {
    let mut e = four_rows();
    op(&mut e, CopyOp::Enter);
    mv(&mut e, CopyMotion::Top);
    op(&mut e, CopyOp::ToggleSelection);
    moves(&mut e, CopyMotion::Right, 4);

    let state = op(&mut e, CopyOp::Exit);
    assert_eq!(state, CopyState { active: false, selecting: false, yanked: None });
    let update = e.update().expect("update");
    assert_eq!(update.copy_cursor, None);
    assert!(update.selection.is_empty());
    assert_eq!(e.selection_text().expect("selection_text"), None);
}

#[test]
fn enter_replaces_a_selection_made_with_the_pointer() {
    let mut e = four_rows();
    e.select(holzwave_core::engine::SelectOp::All).expect("select all");
    assert!(!e.update().expect("update").selection.is_empty());

    op(&mut e, CopyOp::Enter);
    assert_eq!(e.update().expect("update").selection, vec![span(3, 4, 4)]);
}

#[test]
fn yank_of_a_soft_wrapped_line_has_no_line_break() {
    let mut e = engine(20, 5);
    e.feed("x".repeat(25).as_bytes()); // 20 on the first row, 5 wrapped onto the second
    op(&mut e, CopyOp::Enter);
    mv(&mut e, CopyMotion::Top);
    op(&mut e, CopyOp::ToggleSelection);
    mv(&mut e, CopyMotion::Down);
    mv(&mut e, CopyMotion::LineEnd);
    assert_eq!(cursor(&mut e), Some((4, 1)));
    assert_eq!(op(&mut e, CopyOp::Yank).yanked, Some("x".repeat(25)));
}

#[test]
fn new_output_keeps_the_cursor_on_the_same_text_and_the_spans_follow() {
    let mut e = engine(20, 5);
    for n in 1..=12 {
        let line = if n < 12 { format!("row{n:02}\r\n") } else { format!("row{n:02}") };
        e.feed(line.as_bytes());
    }
    op(&mut e, CopyOp::Enter);
    moves(&mut e, CopyMotion::Up, 2);
    assert_eq!(cursor(&mut e), Some((5, 2)));
    assert_eq!(rows_text(&mut e)[2], "row10");

    // Two more lines scroll the screen by two rows; the cursor stays on `row10`, which moved up.
    e.feed(b"\r\nrow13\r\nrow14");
    let update = e.update().expect("update");
    assert_eq!(update.copy_cursor, Some((5, 0)));
    assert_eq!(update.selection, vec![span(0, 5, 5)], "the reported selection is the cursor cell");
    assert_eq!(rows_text(&mut e)[0], "row10");

    // Five more push it out of the viewport; nothing is lost, the cursor is simply not on screen.
    for n in 15..=19 {
        e.feed(format!("\r\nrow{n}").as_bytes());
    }
    let update = e.update().expect("update");
    assert_eq!(update.copy_cursor, None);
    assert!(update.selection.is_empty());

    // The next motion brings it back from where it was: `row10` is nine rows above the bottom, one up is `row09`.
    mv(&mut e, CopyMotion::Up);
    let (_, y) = cursor(&mut e).expect("a motion scrolls the cursor into view");
    assert_eq!(rows_text(&mut e)[usize::from(y)], "row09");
}

#[test]
fn nothing_panics_without_enter_on_the_alternate_screen_in_a_tiny_grid_or_with_endless_motions() {
    // Without Enter every operation is accepted and does nothing.
    let mut e = four_rows();
    for operation in [
        CopyOp::Move(CopyMotion::Left),
        CopyOp::ToggleSelection,
        CopyOp::Yank,
        CopyOp::Exit,
        CopyOp::Move(CopyMotion::WordBack),
    ] {
        assert_eq!(op(&mut e, operation), CopyState::default(), "{operation:?}");
    }
    assert_eq!(cursor(&mut e), None);

    // The alternate screen has no scrollback and its own selection.
    e.feed(b"\x1b[?1049h\x1b[Halt screen");
    op(&mut e, CopyOp::Enter);
    for motion in ALL_MOTIONS {
        mv(&mut e, motion);
    }
    op(&mut e, CopyOp::ToggleSelection);
    for motion in ALL_MOTIONS {
        mv(&mut e, motion);
    }
    e.feed(b"\x1b[?1049l");
    // Back on the primary screen the selection of the alternate one is gone; a motion starts again at the cursor.
    for motion in ALL_MOTIONS {
        mv(&mut e, motion);
    }
    op(&mut e, CopyOp::Exit);

    // A one-cell terminal.
    let mut tiny = engine(1, 1);
    tiny.feed(b"x");
    op(&mut tiny, CopyOp::Enter);
    for motion in ALL_MOTIONS {
        mv(&mut tiny, motion);
    }
    op(&mut tiny, CopyOp::ToggleSelection);
    for motion in ALL_MOTIONS {
        mv(&mut tiny, motion);
    }
    assert_eq!(cursor(&mut tiny), Some((0, 0)));
    assert_eq!(op(&mut tiny, CopyOp::Yank).yanked.as_deref(), Some("x"));

    // A resize in copy mode keeps things working.
    let mut e = thirty_rows();
    op(&mut e, CopyOp::Enter);
    e.resize(8, 3, (0, 0)).expect("resize");
    for motion in ALL_MOTIONS {
        mv(&mut e, motion);
    }
    let (x, y) = cursor(&mut e).expect("cursor on screen");
    assert!(x < 8 && y < 3);
}

const ALL_MOTIONS: [CopyMotion; 12] = [
    CopyMotion::Left,
    CopyMotion::Right,
    CopyMotion::Up,
    CopyMotion::Down,
    CopyMotion::LineStart,
    CopyMotion::LineEnd,
    CopyMotion::Top,
    CopyMotion::Bottom,
    CopyMotion::PageUp,
    CopyMotion::PageDown,
    CopyMotion::WordForward,
    CopyMotion::WordBack,
];
