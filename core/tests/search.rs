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

// ---- Task 2: moving between matches, clearing, and surviving what the terminal does ------------------------------

/// The one current span of the next update, with the text of its row.
fn current_row(e: &mut impl TerminalEngine) -> (MatchSpan, String) {
    let found = matches(e);
    let current: Vec<MatchSpan> = found.iter().copied().filter(|m| m.current).collect();
    assert_eq!(current.len(), 1, "exactly one current match on the screen, got {found:?}");
    let screen = e.screen().expect("screen");
    let m = current[0];
    let row = usize::from(m.y) * usize::from(screen.cols);
    let text: String = screen.cells[row..row + usize::from(screen.cols)].iter().map(|c| c.text.clone()).collect();
    (m, text.trim_end().to_owned())
}

#[test]
fn next_and_prev_move_one_match_at_a_time_and_wrap_around() {
    let mut e = engine(20, 5);
    hundred_lines(&mut e);
    assert_eq!(set(&mut e, "hello").current, Some(9));

    // Prev goes up (older), Next goes down (newer).
    let state = run(&mut e, SearchOp::Prev);
    assert_eq!(state, SearchState { total: 10, current: Some(8) });
    let state = run(&mut e, SearchOp::Prev);
    assert_eq!(state.current, Some(7));
    let state = run(&mut e, SearchOp::Next);
    assert_eq!(state.current, Some(8));
    assert_eq!(run(&mut e, SearchOp::Next).current, Some(9));

    // Last to first, and first to last.
    assert_eq!(run(&mut e, SearchOp::Next).current, Some(0), "Next after the newest match wraps to the oldest");
    assert_eq!(run(&mut e, SearchOp::Prev).current, Some(9), "Prev before the oldest match wraps to the newest");
}

#[test]
fn the_current_match_is_on_the_screen_centred_after_a_move_and_reported_as_current() {
    let mut e = engine(20, 5);
    hundred_lines(&mut e);
    set(&mut e, "hello");

    // line090 is off screen (the viewport shows 096 to 100): the engine scrolls and centres it.
    assert_eq!(run(&mut e, SearchOp::Prev).current, Some(8));
    let (m, row) = current_row(&mut e);
    assert_eq!(row, "line090 hello");
    assert_eq!((m.y, m.x0, m.x1), (2, 8, 12), "centred: two rows above and two below");
    assert!(row.starts_with("line090"));

    // Wrapping from the oldest to the newest lands on the last page: the viewport cannot go past the end.
    for _ in 0..8 {
        run(&mut e, SearchOp::Prev);
    }
    assert_eq!(run(&mut e, SearchOp::Prev).current, Some(9));
    let (m, row) = current_row(&mut e);
    assert_eq!(row, "line100 hello");
    assert_eq!(m.y, 4, "the newest match is on the last row; the viewport stops at the end");
}

#[test]
fn a_match_that_is_already_visible_does_not_move_the_viewport() {
    let mut e = engine(30, 5);
    e.feed(b"hello one\r\nnothing\r\nhello two\r\nx\r\nlast");
    set(&mut e, "hello");
    let before = e.update().expect("update").scrollbar;

    assert_eq!(run(&mut e, SearchOp::Prev).current, Some(0));
    let update = e.update().expect("update");
    assert_eq!(update.scrollbar, before, "both matches were on screen, nothing scrolls");
    assert_eq!(update.matches, vec![span(0, 0, 4, true), span(2, 0, 4, false)]);
}

#[test]
fn clear_removes_the_search_and_leaves_the_viewport_where_it_was() {
    let mut e = engine(20, 5);
    hundred_lines(&mut e);
    set(&mut e, "hello");
    run(&mut e, SearchOp::Prev);
    let before = e.update().expect("update").scrollbar;
    assert!(before.offset < 95, "the move scrolled up");

    let state = run(&mut e, SearchOp::Clear);
    assert_eq!(state, SearchState::default());
    let update = e.update().expect("update");
    assert_eq!(update.search, None);
    assert!(update.matches.is_empty());
    assert_eq!(update.scrollbar, before, "Clear does not scroll");
}

#[test]
fn moving_and_clearing_without_a_search_is_not_an_error() {
    let mut e = engine(20, 5);
    for op in [SearchOp::Next, SearchOp::Prev, SearchOp::Clear] {
        assert_eq!(run(&mut e, op), SearchState::default());
    }
    assert_eq!(e.update().expect("update").search, None);
}

#[test]
fn more_output_with_the_needle_raises_the_total_without_a_call() {
    let mut e = engine(20, 5);
    hundred_lines(&mut e);
    assert_eq!(set(&mut e, "hello").total, 10);

    // Two lines with the needle, long after the search was set; nobody tells the search.
    e.feed(b"\r\nmore hello\r\nand hello");
    let search = e.update().expect("update").search.expect("the search goes on");
    assert_eq!(search.total, 12);
    assert_eq!(search.current, Some(9), "the current match is the same text, its place in reading order is unchanged");

    // New output scrolls lines into the scrollback; matches that moved there are still counted.
    for n in 0..30 {
        e.feed(format!("\r\nfiller {n}").as_bytes());
    }
    assert_eq!(e.update().expect("update").search.expect("search").total, 12);
}

#[test]
fn leaving_and_entering_the_alternate_screen_does_not_lose_the_search() {
    let mut e = engine(20, 5);
    hundred_lines(&mut e);
    let before = set(&mut e, "hello");

    // A full-screen program: the alternate screen is searched on its own and the search stays active.
    e.feed(b"\x1b[?1049h\x1b[Hhello hello");
    let alt = e.update().expect("update");
    let on_alt = alt.search.expect("the search is still active on the alternate screen");
    assert_eq!(on_alt.total, 2, "the alternate screen has its own two matches");
    assert_eq!(alt.matches.len(), 2);

    // Moving there works and cannot panic.
    run(&mut e, SearchOp::Next);
    run(&mut e, SearchOp::Prev);

    e.feed(b"\x1b[?1049l");
    let back = e.update().expect("update");
    assert_eq!(back.search, Some(before), "the primary screen's total and current match are back");
    assert_eq!(back.matches, vec![span(4, 8, 12, true)]);
}

#[test]
fn a_search_on_an_empty_terminal_and_on_the_alternate_screen_never_fails() {
    // No output, no scrollback.
    let mut e = engine(20, 5);
    assert_eq!(set(&mut e, "x"), SearchState { total: 0, current: None });
    for op in [SearchOp::Next, SearchOp::Prev, SearchOp::Clear] {
        run(&mut e, op);
    }

    // The alternate screen has no scrollback at all.
    e.feed(b"\x1b[?1049h");
    for needle in ["zz", "", "a"] {
        set(&mut e, needle);
        run(&mut e, SearchOp::Next);
        run(&mut e, SearchOp::Prev);
    }
    run(&mut e, SearchOp::Clear);
    e.update().expect("update");
}

#[test]
fn resizing_keeps_the_search_working_and_the_spans_inside_the_grid() {
    let mut e = engine(20, 5);
    hundred_lines(&mut e);
    let before = set(&mut e, "hello");

    // Ten columns wrap `line100 hello` after `he`: the soft-wrapped line is still one match, now over two rows.
    e.resize(10, 5, (0, 0)).expect("resize");
    let update = e.update().expect("update");
    assert_eq!(update.search, Some(before));
    assert_eq!(update.matches, vec![span(3, 8, 9, true), span(4, 0, 2, true)]);

    // Growing back, with more rows than before.
    e.resize(30, 8, (0, 0)).expect("resize");
    let update = e.update().expect("update");
    assert_eq!(update.search, Some(before));
    assert!(update.matches.iter().all(|m| m.y < 8 && m.x0 <= m.x1 && m.x1 < 30), "{:?}", update.matches);
    assert!(update.matches.iter().any(|m| m.current));
    assert_eq!(run(&mut e, SearchOp::Prev).current, Some(8));
}

#[test]
fn a_match_cut_by_the_viewport_edge_is_clipped_to_the_rows_on_screen() {
    // `a`, `b`, a `hello` that wraps (`hel` and `lo`), `c` and `d`: six rows in a three-row viewport.
    let mut e = engine(20, 3);
    e.feed(format!("a\r\nb\r\n{}hello\r\nc\r\nd", "x".repeat(17)).as_bytes());

    // The match is above the viewport (`lo`, `c`, `d`) when the needle is set: it is scrolled in and centred.
    assert_eq!(set(&mut e, "hello").total, 1);
    assert_eq!(matches(&mut e), vec![span(1, 17, 19, true), span(2, 0, 1, true)]);

    // Two rows down the first row of the match is above the viewport: only `lo` is left, on the first row.
    e.scroll(2);
    assert_eq!(matches(&mut e), vec![span(0, 0, 1, true)]);

    // At the very top the last row of the match is below the viewport: only `hel` is left, on the last row.
    e.scroll(-3);
    assert_eq!(matches(&mut e), vec![span(2, 17, 19, true)]);
}
