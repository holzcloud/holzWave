//! What the engine reports as changed between two calls of `update` (DESK-11).
//!
//! libghostty marks rows dirty; `update` turns that into rows plus a header (cursor, scrollbar, colours, modes) that
//! is correct even when no row changed. Every expectation was measured on the pinned libghostty revision (RESEARCH
//! Pitfall 7).
#![cfg(feature = "ghostty")]

mod common;

use common::engine;
use holzwave_core::TerminalEngine;
use holzwave_core::engine::Update;

fn update(e: &mut impl TerminalEngine) -> Update {
    e.update().expect("update")
}

/// The text of one row of an update, cells joined, blanks kept.
fn row_text(u: &Update, y: u16) -> String {
    let row = u.changed.iter().find(|r| r.y == y).unwrap_or_else(|| panic!("row {y} is not in the update"));
    row.cells.iter().map(|c| if c.text.is_empty() { " " } else { c.text.as_str() }).collect()
}

#[test]
fn the_first_update_is_full_and_reset_and_has_every_row() {
    let mut e = engine(20, 5);
    let u = update(&mut e);
    assert!(u.full, "first update must be full");
    assert!(u.reset, "first update must reset the consumer");
    assert_eq!((u.cols, u.rows), (20, 5));
    let ys: Vec<u16> = u.changed.iter().map(|r| r.y).collect();
    assert_eq!(ys, vec![0, 1, 2, 3, 4]);
    assert!(u.changed.iter().all(|r| r.cells.len() == 20));
}

#[test]
fn typing_changes_exactly_the_cursor_row() {
    let mut e = engine(20, 5);
    update(&mut e);
    e.feed(b"hi");
    let u = update(&mut e);
    assert!(!u.full, "typing is a partial update");
    assert!(!u.reset);
    assert_eq!(u.changed.len(), 1, "changed rows: {:?}", u.changed.iter().map(|r| r.y).collect::<Vec<_>>());
    assert_eq!(u.changed[0].y, 0);
    assert!(row_text(&u, 0).starts_with("hi"), "row 0 is {:?}", row_text(&u, 0));
    let cursor = u.cursor.expect("cursor");
    assert_eq!((cursor.x, cursor.y), (2, 0));
}

#[test]
fn an_update_with_nothing_fed_has_no_rows_and_the_same_header() {
    let mut e = engine(20, 5);
    update(&mut e);
    e.feed(b"hi");
    let before = update(&mut e);
    let after = update(&mut e);
    assert!(after.changed.is_empty(), "rows: {:?}", after.changed.iter().map(|r| r.y).collect::<Vec<_>>());
    assert!(!after.full);
    assert!(!after.reset);
    assert_eq!(after.cursor, before.cursor);
    assert_eq!(after.scrollbar, before.scrollbar);
    assert_eq!((after.cols, after.rows), (before.cols, before.rows));
    assert_eq!((after.default_fg, after.default_bg), (before.default_fg, before.default_bg));
    assert_eq!(after.modes, before.modes);
}
