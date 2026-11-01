//! How much history the engine keeps: the number of lines that the `scrollback` argument promises (the setting
//! `scrollback_lines`, 10 000 by default), not what libghostty-vt happens to keep for its other limit.
//!
//! libghostty-vt has two scrollback limits that work together, a number of lines and a number of bytes, and the first
//! one reached wins. A fresh terminal starts with a byte limit of only 10 000 bytes, so setting the line limit alone
//! left about 1 000 rows at 80 columns whatever the setting said (found by the web transport test of the page, same
//! on the revisions before and after the Zig 0.17 bump). The engine now sets the line limit and removes the byte limit.
//!
//! libghostty-vt prunes whole pages, so the kept history is near the limit and not exactly on it (measured: 9 836 rows
//! for 10 000 lines, 10 013 rows at 200 columns); the bounds here are that slack, not an exact count. The failure that
//! these tests guard is a factor of ten, not a page.
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

#[test]
fn a_smaller_limit_still_prunes() {
    let mut engine = GhosttyEngine::new(80, 24, 1_000).expect("engine");
    engine.feed(&numbered_lines(40_000));
    let total = total_rows(&mut engine);
    assert!(total >= 900, "only {total} rows kept for a limit of 1 000 lines");
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
