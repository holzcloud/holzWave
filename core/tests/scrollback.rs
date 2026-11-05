//! How much history the engine keeps: the number of lines that the `scrollback` argument promises (the setting
//! `scrollback_lines`, 10 000 by default), not what libghostty-vt happens to keep for its other limit.
//!
//! libghostty-vt has two scrollback limits that work together, a number of lines and a number of bytes, and the first
//! one reached wins. A fresh terminal starts with a byte limit of only 10 000 bytes, so setting the line limit alone
//! left about 1 000 rows at 80 columns whatever the setting said (found by the web transport test of the page, same
//! on the revisions before and after the Zig 0.17 bump). The engine now sets the line limit and removes the byte limit.
//!
//! libghostty-vt prunes whole pages: while the rows above the active screen exceed the limit it drops the oldest page.
//! The kept total is therefore never more than the limit plus the screen rows and, after the first prune, more than that
//! minus one page. A page is not the same size everywhere: its byte size is rounded up to the memory page of the
//! target, so at 80 columns it holds 591 rows on macOS arm64 and 578 rows on Linux arm64 (measured: with 40 000 lines
//! of input, 24 rows and a limit of 1 000 the Mac keeps 995 rows and the Linux container 697). The test that asks for a
//! small limit measures the page of the engine under test and checks the kept total against that page, so the bound
//! is tight on every platform and a limit that is a quarter or double of what was asked still fails. The tests with
//! the limit of 10 000 lines guard the factor of ten that the byte limit used to cost, not a page.
#![cfg(feature = "ghostty")]

use holzwave_core::{GhosttyEngine, TerminalEngine};

/// The rows of the screen and of the scrollback together.
fn total_rows(engine: &mut GhosttyEngine) -> u64 {
    engine.screen().expect("screen").scrollbar.total
}

/// `n` numbered short lines, each ended by CR LF.
fn numbered_lines(n: usize) -> Vec<u8> {
    let mut out = String::with_capacity(n * 12);
    for i in 0..n {
        out.push_str(&format!("line {i}\r\n"));
    }
    out.into_bytes()
}

#[test]
fn the_default_ten_thousand_lines_are_kept_and_not_about_a_thousand() {
    let mut engine = GhosttyEngine::new(80, 24, 10_000).expect("engine");
    engine.feed(&numbered_lines(40_000));
    let total = total_rows(&mut engine);
    assert!(total >= 9_000, "only {total} rows kept for a limit of 10 000 lines");
    assert!(total <= 10_000 + 24 + 500, "{total} rows kept: the limit no longer prunes");
}

/// The rows that one history page of this engine holds, measured: feed lines one at a time until the library drops its
/// oldest page, which shows as the total falling by the page's rows less the one row that the line added.
fn rows_per_page(cols: u16, rows: u16, limit: usize) -> u64 {
    let mut engine = GhosttyEngine::new(cols, rows, limit).expect("engine");
    let mut before = 0;
    // The first drop comes once the history passes the limit: after about `limit + page` lines.
    for i in 0..limit + 5_000 {
        engine.feed(format!("line {i}\r\n").as_bytes());
        let total = total_rows(&mut engine);
        if total < before {
            return before - total + 1;
        }
        before = total;
    }
    panic!("no page was dropped in {} lines for a limit of {limit}", limit + 5_000);
}

#[test]
fn a_smaller_limit_still_prunes() {
    let mut engine = GhosttyEngine::new(80, 24, 1_000).expect("engine");
    engine.feed(&numbered_lines(40_000));
    let total = total_rows(&mut engine);
    let page = rows_per_page(80, 24, 1_000);
    assert!(page >= 100, "a page of {page} rows: the measurement is not of a history page");
    // The oldest page is dropped as long as more than the limit lies above the screen, so what is kept is within one
    // page of the limit plus the screen.
    assert!(
        total + page > 1_000 + 24,
        "only {total} rows kept for a limit of 1 000 lines (a page holds {page} rows)"
    );
    assert!(total <= 1_000 + 24 + 500, "{total} rows kept for a limit of 1 000 lines");
}

#[test]
fn the_limit_does_not_depend_on_the_width() {
    let mut engine = GhosttyEngine::new(200, 50, 10_000).expect("engine");
    engine.feed(&numbered_lines(40_000));
    let total = total_rows(&mut engine);
    assert!(total >= 9_000, "only {total} rows kept at 200 columns");
    assert!(total <= 10_000 + 50 + 500, "{total} rows kept at 200 columns");
}

#[test]
fn a_very_long_wrapped_line_is_kept_whole_when_it_fits_the_limit() {
    // 200 000 characters at 80 columns are 2 500 rows, well inside 10 000 lines.
    let mut engine = GhosttyEngine::new(80, 24, 10_000).expect("engine");
    let long: Vec<u8> = (0..200_000usize).map(|i| b'a' + (i % 26) as u8).collect();
    engine.feed(&long);
    engine.feed(b"\r\nMARK\r\n");
    let total = total_rows(&mut engine);
    // The line, the marker line and the row of the cursor.
    assert!(total >= 2_502, "only {total} rows kept of a 2 500 row line");
}
