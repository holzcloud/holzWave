//! Keys through the engine's own encoder (DESK-09).
//!
//! Every expectation here was measured on the pinned libghostty revision. The webview never builds escape sequences:
//! it sends facts and the engine encodes them against the terminal's live modes.
#![cfg(feature = "ghostty")]

mod common;

use common::{ascii, engine};
use holzwave_core::TerminalEngine;
use holzwave_core::engine::{DomKey, KeyAction, KeyEvent, Mods};
use holzwave_core::platform::Os;

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

/// The facts of a key press as the webview sends them.
fn dom(code: &str, key: &str, mods: Mods) -> DomKey {
    DomKey { code: code.into(), key: key.into(), mods, ..DomKey::default() }
}

/// The whole path of a key: DOM facts, conversion, the engine's encoder. Returns the bytes as readable ASCII.
fn encode_dom(e: &mut impl TerminalEngine, d: &DomKey, option_as_alt: bool) -> String {
    e.set_option_as_alt(option_as_alt);
    encode(e, &KeyEvent::from_dom(d, option_as_alt))
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

#[test]
fn legacy_key_table() {
    let none = Mods::NONE;
    let rows: &[(&str, &str, &str, Mods, &str)] = &[
        ("ArrowUp", "ArrowUp", "ArrowUp", none, r"\x1b[A"),
        ("Shift+Up", "ArrowUp", "ArrowUp", Mods::SHIFT, r"\x1b[1;2A"),
        ("Ctrl+Right", "ArrowRight", "ArrowRight", Mods::CTRL, r"\x1b[1;5C"),
        ("Alt+Left", "ArrowLeft", "ArrowLeft", Mods::ALT, r"\x1b[1;3D"),
        ("a", "KeyA", "a", none, "a"),
        ("Shift+a", "KeyA", "A", Mods::SHIFT, "A"),
        ("Ctrl+C", "KeyC", "c", Mods::CTRL, r"\x03"),
        ("Ctrl+Space", "Space", " ", Mods::CTRL, r"\x00"),
        ("Ctrl+Backslash", "Backslash", "\\", Mods::CTRL, r"\x1c"),
        ("Ctrl+Shift+C", "KeyC", "C", Mods::CTRL | Mods::SHIFT, r"\x1b[99;6u"),
        ("Enter", "Enter", "Enter", none, r"\r"),
        ("Backspace", "Backspace", "Backspace", none, r"\x7f"),
        ("Shift+Tab", "Tab", "Tab", Mods::SHIFT, r"\x1b[Z"),
        ("Tab", "Tab", "Tab", none, r"\t"),
        ("Escape", "Escape", "Escape", none, r"\x1b"),
        ("F1", "F1", "F1", none, r"\x1bOP"),
        ("F5", "F5", "F5", none, r"\x1b[15~"),
        ("Home", "Home", "Home", none, r"\x1b[H"),
        ("Delete", "Delete", "Delete", none, r"\x1b[3~"),
        ("PageUp", "PageUp", "PageUp", none, r"\x1b[5~"),
        ("Shift+Enter", "Enter", "Enter", Mods::SHIFT, r"\x1b[27;2;13~"),
        ("Ctrl+Tab", "Tab", "Tab", Mods::CTRL, r"\x1b[27;5;9~"),
    ];
    let mut e = engine(20, 4);
    let mut wrong = Vec::new();
    for (name, code, key, mods, expected) in rows {
        let got = encode_dom(&mut e, &dom(code, key, *mods), false);
        if got != *expected {
            wrong.push(format!("{name}: expected {expected}, got {got}"));
        }
    }
    assert!(wrong.is_empty(), "rows that differ:\n{}", wrong.join("\n"));
}

#[test]
fn an_emoji_on_an_unidentified_key_is_its_utf8() {
    let mut e = engine(20, 4);
    let got = encode_dom(&mut e, &dom("", "\u{1F600}", Mods::NONE), false);
    assert_eq!(got, ascii("\u{1F600}".as_bytes()));
    assert_eq!(got, r"\xf0\x9f\x98\x80");
}

#[test]
fn ctrl_bracket_ctrl_i_and_ctrl_m_are_not_overridden() {
    // D-13: the engine encodes these as CSI u so that Ctrl+I is not Tab and Ctrl+M is not Enter. holzWave does not
    // change that.
    let mut e = engine(20, 4);
    assert_eq!(encode_dom(&mut e, &dom("BracketLeft", "[", Mods::CTRL), false), r"\x1b[91;5u");
    assert_eq!(encode_dom(&mut e, &dom("KeyI", "i", Mods::CTRL), false), r"\x1b[105;5u");
    assert_eq!(encode_dom(&mut e, &dom("KeyM", "m", Mods::CTRL), false), r"\x1b[109;5u");
}

#[test]
fn releases_and_composing_keys_send_nothing_in_legacy_mode() {
    let mut e = engine(20, 4);
    let release = DomKey { release: true, ..dom("KeyA", "a", Mods::NONE) };
    assert_eq!(encode_dom(&mut e, &release, false), "");
    let composing = DomKey { composing: true, ..dom("KeyA", "a", Mods::NONE) };
    assert_eq!(encode_dom(&mut e, &composing, false), "");
    let arrow_release = DomKey { release: true, ..dom("ArrowUp", "ArrowUp", Mods::NONE) };
    assert_eq!(encode_dom(&mut e, &arrow_release, false), "");
    // A repeat is a press.
    let repeat = DomKey { repeat: true, ..dom("ArrowUp", "ArrowUp", Mods::NONE) };
    assert_eq!(encode_dom(&mut e, &repeat, false), r"\x1b[A");
}

#[test]
fn unknown_codes_are_encoded_from_their_text() {
    let mut e = engine(20, 4);
    assert_eq!(encode_dom(&mut e, &dom("SomethingNew", "x", Mods::NONE), false), "x");
    assert_eq!(encode_dom(&mut e, &dom("SomethingNew", "Enter", Mods::NONE), false), "");
}

#[test]
fn committed_text_is_encoded_as_text_and_not_bracketed() {
    let mut e = engine(20, 4);
    e.feed(b"\x1b[?2004h");
    assert_eq!(ascii(&e.encode_text("\u{00E4}\u{1F600}").unwrap()), r"\xc3\xa4\xf0\x9f\x98\x80");
    assert_eq!(ascii(&e.encode_text("").unwrap()), "");
}

#[cfg(target_os = "macos")]
mod option_as_alt {
    use super::*;

    #[test]
    fn off_by_default_option_composes_text_and_sends_just_the_text() {
        // D-12: a Swiss layout types | with Option+7 and ƒ with Option+f.
        let mut e = engine(20, 4);
        assert_eq!(encode_dom(&mut e, &dom("KeyF", "\u{0192}", Mods::ALT), false), ascii("\u{0192}".as_bytes()));
        assert_eq!(encode_dom(&mut e, &dom("Digit7", "|", Mods::ALT), false), "|");
    }

    #[test]
    fn on_option_is_alt_and_sends_an_escape_prefix() {
        let mut e = engine(20, 4);
        assert_eq!(encode_dom(&mut e, &dom("KeyF", "\u{0192}", Mods::ALT), true), r"\x1bf");
        assert_eq!(encode_dom(&mut e, &dom("Digit7", "|", Mods::ALT), true), r"\x1b|");
    }

    #[test]
    fn the_setting_survives_an_options_reset_between_keys() {
        // set_options_from_terminal resets option-as-alt inside the library; every key must put it back.
        let mut e = engine(20, 4);
        e.set_option_as_alt(true);
        for _ in 0..3 {
            let ev = KeyEvent::from_dom(&dom("KeyF", "\u{0192}", Mods::ALT), true);
            assert_eq!(encode(&mut e, &ev), r"\x1bf");
        }
        e.set_option_as_alt(false);
        let ev = KeyEvent::from_dom(&dom("KeyF", "\u{0192}", Mods::ALT), false);
        assert_eq!(encode(&mut e, &ev), ascii("\u{0192}".as_bytes()));
    }
}

/// Phase 2.2: AltGr on Windows arrives as Ctrl+Alt. The page drops both when the browser says AltGraph; the core
/// backs it up with the Windows platform value, so these run the real encoder on any host.
mod windows_altgr {
    use super::*;

    fn encode_on_windows(e: &mut impl TerminalEngine, d: &DomKey) -> String {
        encode(e, &KeyEvent::from_dom_on(d, false, Os::Windows))
    }

    #[test]
    fn the_swiss_altgr_characters_arrive_as_the_character() {
        let both = Mods::CTRL | Mods::ALT;
        let mut e = engine(20, 4);
        // Swiss AltGr+2, +7, +< and the Swiss `{` and `}` keys, German AltGr+E and AltGr+Q: each types a character
        // other than the US one of its physical key. (Swiss AltGr+ü is `[` on BracketLeft, the US character of that
        // key: only the page's AltGraph rule can tell it from Ctrl+Alt+[; hardware row W4 covers it.)
        for (code, key) in [
            ("Digit2", "@"),
            ("Digit7", "|"),
            ("KeyE", "\u{20AC}"),
            ("KeyQ", "@"),
            ("IntlBackslash", "\\"),
            ("Quote", "{"),
            ("Backslash", "}"),
            ("Equal", "~"),
        ] {
            let got = encode_on_windows(&mut e, &dom(code, key, both));
            assert_eq!(got, ascii(key.as_bytes()), "{code} {key}");
        }
    }

    #[test]
    fn a_ctrl_alt_chord_of_a_us_layout_is_encoded_as_before() {
        let mut e = engine(20, 4);
        let d = dom("KeyC", "c", Mods::CTRL | Mods::ALT);
        // The same bytes the other platforms produce: Windows changes nothing for this event.
        let on_linux = encode(&mut e, &KeyEvent::from_dom_on(&d, false, Os::Linux));
        assert_eq!(encode_on_windows(&mut e, &d), on_linux);
        assert_ne!(on_linux, "c");
    }

    #[test]
    fn linux_does_not_drop_anything_for_altgr() {
        // Off Windows the page drops the bits; the core does not guess.
        let mut e = engine(20, 4);
        let d = dom("Digit7", "|", Mods::CTRL | Mods::ALT);
        assert_ne!(encode(&mut e, &KeyEvent::from_dom_on(&d, false, Os::Linux)), "|");
    }
}

mod kitty {
    use super::*;

    #[test]
    fn with_all_flags_press_release_and_super_are_reported() {
        let mut e = engine(20, 4);
        e.feed(b"\x1b[>31u");
        assert_eq!(encode_dom(&mut e, &dom("KeyA", "a", Mods::NONE), false), r"\x1b[97;;97u");
        let release = DomKey { release: true, ..dom("KeyA", "a", Mods::NONE) };
        assert_eq!(encode_dom(&mut e, &release, false), r"\x1b[97;1:3u");
        assert_eq!(encode_dom(&mut e, &dom("KeyC", "c", Mods::SUPER), false), r"\x1b[99;9u");
    }

    #[test]
    fn disambiguate_only_makes_escape_a_csi_u() {
        let mut e = engine(20, 4);
        e.feed(b"\x1b[>1u");
        assert_eq!(encode_dom(&mut e, &dom("Escape", "Escape", Mods::NONE), false), r"\x1b[27u");
    }

    #[test]
    fn popping_the_flags_returns_to_legacy() {
        let mut e = engine(20, 4);
        e.feed(b"\x1b[>1u");
        e.feed(b"\x1b[<u");
        assert_eq!(encode_dom(&mut e, &dom("Escape", "Escape", Mods::NONE), false), r"\x1b");
    }
}
