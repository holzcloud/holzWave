#![cfg(feature = "ghostty")]

use holzwave_core::{GhosttyEngine, TerminalEngine};

fn engine(cols: u16, rows: u16) -> GhosttyEngine {
    GhosttyEngine::new(cols, rows, 1000).expect("engine")
}

#[test]
fn plain_text_lands_on_the_grid() {
    let mut e = engine(20, 4);
    e.feed(b"hello\r\nworld");
    let text = e.screen().unwrap().to_text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "hello");
    assert_eq!(lines[1], "world");
}

#[test]
fn style_and_colour_are_resolved() {
    let mut e = engine(20, 2);
    e.feed(b"\x1b[1;38;2;255;128;0mX\x1b[0mY");
    let s = e.screen().unwrap();
    let (x, y) = (&s.cells[0], &s.cells[1]);
    assert_eq!(x.text, "X");
    assert!(x.bold);
    assert_eq!((x.fg.r, x.fg.g, x.fg.b), (255, 128, 0));
    assert!(!y.bold);
}

#[test]
fn unicode_and_wide_characters() {
    let mut e = engine(20, 2);
    e.feed("äöü 日本 🙂".as_bytes());
    let text = e.screen().unwrap().to_text();
    assert!(text.starts_with("äöü 日本 🙂"), "got {text:?}");
}

#[test]
fn cursor_follows_output() {
    let mut e = engine(20, 4);
    e.feed(b"abc\r\nde");
    let c = e.screen().unwrap().cursor.expect("cursor");
    assert_eq!((c.x, c.y), (2, 1));
}

#[test]
fn resize_changes_the_grid() {
    let mut e = engine(20, 4);
    e.resize(40, 10, (0, 0)).unwrap();
    assert_eq!(e.size().unwrap(), (40, 10));
    let s = e.screen().unwrap();
    assert_eq!((s.cols, s.rows), (40, 10));
    assert_eq!(s.cells.len(), 400);
}
