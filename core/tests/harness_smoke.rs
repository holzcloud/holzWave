//! Smoke test for the shared helper module: every helper that later plans rely on is exercised once.
#![cfg(feature = "ghostty")]

mod common;

use std::time::Duration;

use common::{Xorshift64, ascii, engine, scratch_dir, wait_until};
use holzwave_core::TerminalEngine;

#[test]
fn engine_helper_feeds_text() {
    let mut e = engine(20, 4);
    e.feed(b"hello");
    let text = e.screen().unwrap().to_text();
    assert_eq!(text.lines().next(), Some("hello"));
}

#[test]
fn ascii_helper_escapes_controls() {
    assert_eq!(ascii(b"\x1b[A"), r"\x1b[A");
    assert_eq!(ascii(b"a\r\n\t"), r"a\r\n\t");
}

#[test]
fn xorshift_is_deterministic_and_covers_the_range() {
    let mut a = Xorshift64::new(42);
    let mut b = Xorshift64::new(42);
    let first: Vec<u64> = (0..10).map(|_| a.next_u64()).collect();
    let second: Vec<u64> = (0..10).map(|_| b.next_u64()).collect();
    assert_eq!(first, second);

    let mut r = Xorshift64::new(7);
    let mut seen = [false; 10];
    for _ in 0..1000 {
        seen[r.below(10)] = true;
    }
    assert!(seen.iter().all(|&s| s), "residues seen: {seen:?}");

    let mut zero = Xorshift64::new(0);
    assert_ne!(zero.next_u64(), 0);
    assert!(!zero.chance(0));
    assert!(zero.chance(100));
}

#[test]
fn scratch_dir_is_unique_and_wait_until_times_out() {
    let a = scratch_dir("smoke");
    let b = scratch_dir("smoke");
    assert_ne!(a, b);
    assert!(a.is_dir());
    assert!(b.is_dir());

    assert!(!wait_until(Duration::from_millis(50), || false));
    assert!(wait_until(Duration::from_millis(50), || true));

    std::fs::remove_dir(&a).unwrap();
    std::fs::remove_dir(&b).unwrap();
}
