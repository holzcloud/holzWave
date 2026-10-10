//! Keys through the engine's own encoder (DESK-09).
//!
//! Every expectation here was measured on the pinned libghostty revision. The webview never builds escape sequences:
//! it sends facts and the engine encodes them against the terminal's live modes.
#![cfg(feature = "ghostty")]

mod common;

use common::{ascii, engine};
use holzwave_core::TerminalEngine;
use holzwave_core::engine::{KeyAction, KeyEvent, Mods};

/// A press of the key with this W3C `code`, no text, no modifiers.
fn press(code: &str) -> KeyEvent {
    KeyEvent {
        code: code.to_string(),
        action: KeyAction::Press,
        ..KeyEvent::default()
    }
}

fn encode(e: &mut impl TerminalEngine, event: &KeyEvent) -> String {
    ascii(&e.encode_key(event).expect("encode"))
}

#[test]
fn arrow_up_is_csi_a_on_a_fresh_engine() {
    let mut e = engine(20, 4);
    assert_eq!(encode(&mut e, &press("ArrowUp")), r"\x1b[A");
}

#[test]
fn arrow_up_follows_application_cursor_keys() {
    let mut e = engine(20, 4);
    e.feed(b"\x1b[?1h");
    assert_eq!(encode(&mut e, &press("ArrowUp")), r"\x1bOA");
    e.feed(b"\x1b[?1l");
    assert_eq!(encode(&mut e, &press("ArrowUp")), r"\x1b[A");
}

#[test]
fn all_four_arrows_follow_the_mode() {
    let mut e = engine(20, 4);
    for (code, normal, application) in [
        ("ArrowUp", r"\x1b[A", r"\x1bOA"),
        ("ArrowDown", r"\x1b[B", r"\x1bOB"),
        ("ArrowRight", r"\x1b[C", r"\x1bOC"),
        ("ArrowLeft", r"\x1b[D", r"\x1bOD"),
    ] {
        e.feed(b"\x1b[?1l");
        assert_eq!(encode(&mut e, &press(code)), normal, "{code}");
        e.feed(b"\x1b[?1h");
        assert_eq!(encode(&mut e, &press(code)), application, "{code} in application mode");
    }
}

#[test]
fn modifiers_are_applied_to_the_arrow() {
    let mut e = engine(20, 4);
    let shifted = KeyEvent { mods: Mods::SHIFT, ..press("ArrowUp") };
    assert_eq!(encode(&mut e, &shifted), r"\x1b[1;2A");
}
