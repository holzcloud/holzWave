//! Scrollback search (DESK-21 / F6): one needle is searched in the whole scrollback and both screens, the total and the
//! current match are an engine capability, and every match inside the viewport comes back as spans in the update.
//!
//! The index of a match is its place in reading order: 0 is the oldest match, `total - 1` the newest. A search
//! starts on the newest match (the one nearest the prompt), `Prev` goes up and `Next` goes down, both wrap.
#![cfg(feature = "ghostty")]

mod common;

use common::engine;
use holzwave_core::TerminalEngine;
use holzwave_core::engine::{MatchSpan, SearchOp, SearchState};

/// Run a search operation and assert that the engine accepted it.
fn run(e: &mut impl TerminalEngine, op: SearchOp) -> SearchState {
    match e.search(op.clone()) {
        Ok(state) => state,
        Err(err) => panic!("search({op:?}) must be accepted, got {err:?}"),
    }
}

fn set(e: &mut impl TerminalEngine, needle: &str) -> SearchState {
    run(e, SearchOp::Set(needle.to_owned()))
}

fn matches(e: &mut impl TerminalEngine) -> Vec<MatchSpan> {
    e.update().expect("update").matches
}

fn span(y: u16, x0: u16, x1: u16, current: bool) -> MatchSpan {
    MatchSpan { y, x0, x1, current }
}

/// `line001` to `line100`, one per line, the last without a line break. Every tenth line (`line010`, `line020`, ...)
/// ends in ` hello`. In a 20x5 engine the viewport shows `line096` to `line100`.
fn hundred_lines(e: &mut impl TerminalEngine) {
    for n in 1..=100 {
        let mut line = format!("line{n:03}");
        if n % 10 == 0 {
            line.push_str(" hello");
        }
        if n < 100 {
            line.push_str("\r\n");
        }
        e.feed(line.as_bytes());
    }
}

/// The text of the cells a span covers, read from the engine's screen.
fn text_of(e: &mut impl TerminalEngine, m: &MatchSpan) -> String {
    let screen = e.screen().expect("screen");
    let row = usize::from(m.y) * usize::from(screen.cols);
    (m.x0..=m.x1).map(|x| screen.cells[row + usize::from(x)].text.clone()).collect()
}

#[test]
fn one_needle_is_found_across_the_scrollback_with_a_total_and_a_current_index() {
    let mut e = engine(20, 5);
    hundred_lines(&mut e);

    let state = set(&mut e, "hello");
    assert_eq!(state.total, 10, "every tenth of 100 lines contains the needle");
    assert_eq!(state.current, Some(9), "a search starts on the newest match, which is the last in reading order");

    let update = e.update().expect("update");
    assert_eq!(update.search, Some(state), "the update reports the same totals the call returned");
}

#[test]
fn the_spans_cover_exactly_the_matches_inside_the_viewport() {
    let mut e = engine(20, 5);
    hundred_lines(&mut e);
    let state = set(&mut e, "hello");

    // `line100 hello` is the newest match and sits on the last row; it is the current one.
    assert_eq!(matches(&mut e), vec![span(4, 8, 12, true)]);

    // Five rows up the viewport shows line091 to line095: no match there, but the search is unchanged.
    e.scroll(-5);
    let update = e.update().expect("update");
    assert!(update.matches.is_empty(), "no match in the viewport, got {:?}", update.matches);
    assert_eq!(update.search, Some(state));

    // Five more rows up the viewport shows line086 to line090: `line090 hello` on the last row, not the current one.
    e.scroll(-5);
    assert_eq!(matches(&mut e), vec![span(4, 8, 12, false)]);

    // The list is complete in every update, not only on the update that changed the viewport.
    assert_eq!(matches(&mut e), vec![span(4, 8, 12, false)]);
}

#[test]
fn several_matches_on_one_screen_come_in_reading_order_and_only_the_current_one_is_marked() {
    let mut e = engine(30, 5);
    e.feed(b"hello one\r\nnothing\r\nhello two hello three\r\nx\r\nlast");

    let state = set(&mut e, "hello");
    assert_eq!((state.total, state.current), (3, Some(2)));
    assert_eq!(
        matches(&mut e),
        vec![span(0, 0, 4, false), span(2, 0, 4, false), span(2, 10, 14, true)],
        "reading order, the newest one is current"
    );
}

#[test]
fn ascii_letters_match_in_either_case_and_everything_else_byte_exact() {
    let mut e = engine(40, 3);
    e.feed(b"Hello HELLO hello hElLo");

    let state = set(&mut e, "hello");
    assert_eq!(state.total, 4, "ASCII letters ignore case");
    let found = matches(&mut e);
    let texts: Vec<String> = found.iter().map(|m| text_of(&mut e, m)).collect();
    assert_eq!(texts, ["Hello", "HELLO", "hello", "hElLo"], "the spans cover the cells of each spelling");

    // The accented letter is not ASCII: `\u{e9}` matches only itself, while the ASCII letters around it still fold.
    let mut e = engine(40, 3);
    e.feed("caf\u{e9} CAF\u{e9} CAF\u{c9}".as_bytes());
    let state = set(&mut e, "caf\u{e9}");
    assert_eq!(state.total, 2, "CAF\u{c9} differs in a byte that is not an ASCII letter");
}

#[test]
fn an_empty_needle_behaves_like_clear() {
    let mut e = engine(20, 5);
    hundred_lines(&mut e);
    assert_eq!(set(&mut e, "hello").total, 10);

    let state = set(&mut e, "");
    assert_eq!(state, SearchState::default(), "total 0 and no current match");
    let update = e.update().expect("update");
    assert_eq!(update.search, None, "an empty needle is no search at all");
    assert!(update.matches.is_empty());
}

#[test]
fn a_needle_longer_than_one_kibibyte_is_refused_and_leaves_the_earlier_search_alone() {
    let mut e = engine(20, 5);
    hundred_lines(&mut e);
    let before = set(&mut e, "hello");

    let result = e.search(SearchOp::Set("a".repeat(1025)));
    assert!(result.is_err(), "1025 bytes are over the limit, got {result:?}");
    let update = e.update().expect("update");
    assert_eq!(update.search, Some(before), "the earlier search is untouched");
    assert_eq!(update.matches, vec![span(4, 8, 12, true)]);

    // Exactly 1024 bytes are still accepted; nothing matches, which is a search with a total of zero.
    let state = set(&mut e, &"a".repeat(1024));
    assert_eq!(state.total, 0);
    assert_eq!(e.update().expect("update").search, Some(state));
}

#[test]
fn a_needle_with_no_match_is_a_search_with_a_total_of_zero() {
    let mut e = engine(20, 5);
    hundred_lines(&mut e);

    let state = set(&mut e, "zzz");
    assert_eq!(state, SearchState { total: 0, current: None });
    let update = e.update().expect("update");
    assert_eq!(update.search, Some(state));
    assert!(update.matches.is_empty());
}

#[test]
fn a_match_that_wraps_across_two_rows_is_two_spans() {
    let mut e = engine(20, 5);
    // Seventeen cells, then `hello`: `hel` ends the first row, `lo` starts the second.
    e.feed(format!("{}hello\r\nend", "x".repeat(17)).as_bytes());

    let state = set(&mut e, "hello");
    assert_eq!(state.total, 1, "a soft-wrapped line is one line of text");
    assert_eq!(matches(&mut e), vec![span(0, 17, 19, true), span(1, 0, 1, true)]);
}

#[test]
fn setting_a_needle_brings_the_newest_match_into_view() {
    let mut e = engine(20, 5);
    hundred_lines(&mut e);

    // The viewport shows line036 to line040 when the needle is set.
    e.scroll(-60);
    let state = set(&mut e, "hello");
    assert_eq!(state.current, Some(9));

    let update = e.update().expect("update");
    let current: Vec<&MatchSpan> = update.matches.iter().filter(|m| m.current).collect();
    assert_eq!(current.len(), 1, "the current match is on the screen, got {:?}", update.matches);
    assert!(current[0].y < 5);
}
