//! What a program tells the terminal and the shell shows: the window title (OSC 0 and 2), the working directory
//! (OSC 7) and the bell (BEL). Both are program-controlled text, so they arrive length-capped and free of control characters (V5,
//! T-02-35), and OSC 7 is only ever turned into an absolute path without `..`.
#![cfg(feature = "ghostty")]

mod common;

use std::path::PathBuf;

use common::{ascii, engine};
use holzwave_core::TerminalEngine;
use holzwave_core::engine::{EngineEvent, MAX_EVENT_BYTES, parse_osc7, sanitize_program_text};

fn title(s: &str) -> EngineEvent {
    EngineEvent::Title(s.to_owned())
}

#[test]
fn osc_2_is_a_title_event_and_events_are_taken_once() {
    let mut e = engine(20, 3);
    e.feed(b"\x1b]2;hello\x07");
    assert_eq!(e.take_events(), vec![title("hello")]);
    assert!(e.take_events().is_empty(), "an event is delivered once");
}

#[test]
fn osc_0_and_st_terminated_titles_are_events_too() {
    let mut e = engine(20, 3);
    e.feed(b"\x1b]0;first\x1b\\\x1b]2;second\x07");
    assert_eq!(e.take_events(), vec![title("first"), title("second")]);
}

#[test]
fn an_oversized_title_is_cut_to_4096_bytes_on_a_character_boundary() {
    let mut e = engine(20, 3);
    e.feed(format!("\x1b]2;{}\x07", "a".repeat(10_000)).as_bytes());
    // Whatever libghostty itself keeps of a 10 000 character title, no more than the cap gets through.
    for ev in e.take_events() {
        let EngineEvent::Title(t) = ev else { panic!("unexpected event {ev:?}") };
        assert!(t.len() <= MAX_EVENT_BYTES, "{} bytes", t.len());
    }

    // Three-byte characters: 4096 is not a multiple of three, so the cut must step back to a boundary.
    let cut = sanitize_program_text(&"\u{65E5}".repeat(4000));
    assert_eq!(cut, "\u{65E5}".repeat(1365));
    assert_eq!(cut.len(), 4095);
    let ascii_cut = sanitize_program_text(&"a".repeat(10_000));
    assert_eq!(ascii_cut.len(), MAX_EVENT_BYTES);
}

#[test]
fn control_characters_are_removed_from_text_a_program_controls() {
    assert_eq!(sanitize_program_text("a\u{1}b\u{1b}c\u{7f}d\u{85}e\u{9b}f\u{9f}g\tz\nq"), "abcdefgzq");
    assert_eq!(sanitize_program_text("plain \u{65E5}\u{1F600} text"), "plain \u{65E5}\u{1F600} text");
    assert_eq!(sanitize_program_text(""), "");

    // The same through the engine: whatever the terminal lets through carries no control character.
    let mut e = engine(20, 3);
    e.feed("\x1b]2;a\u{85}b\u{7f}c\u{9b}d\x07".as_bytes());
    for ev in e.take_events() {
        let EngineEvent::Title(t) = ev else { panic!("unexpected event {ev:?}") };
        assert!(!t.chars().any(char::is_control), "title {t:?}");
    }
}

#[test]
fn osc_7_is_a_pwd_event_with_the_raw_string() {
    let mut e = engine(20, 3);
    e.feed(b"\x1b]7;file://localhost/Users/me/src%20dir\x07");
    assert_eq!(e.take_events(), vec![EngineEvent::Pwd("file://localhost/Users/me/src%20dir".to_owned())]);
}

#[test]
fn parse_osc7_returns_the_host_and_the_decoded_absolute_path() {
    assert_eq!(
        parse_osc7("file://localhost/Users/me/src%20dir"),
        Some(("localhost".to_owned(), PathBuf::from("/Users/me/src dir")))
    );
    assert_eq!(parse_osc7("file:///tmp"), Some((String::new(), PathBuf::from("/tmp"))));
    assert_eq!(parse_osc7("file://box/"), Some(("box".to_owned(), PathBuf::from("/"))));
    // UTF-8 and upper-case hex digits.
    assert_eq!(
        parse_osc7("file://box/home/j%C3%BCrgen/%7Ex"),
        Some(("box".to_owned(), PathBuf::from("/home/j\u{fc}rgen/~x")))
    );
    // A dot inside a name is a name.
    assert_eq!(parse_osc7("file://box/a..b/.hidden"), Some(("box".to_owned(), PathBuf::from("/a..b/.hidden"))));
}

#[test]
fn parse_osc7_refuses_what_could_point_anywhere() {
    for bad in [
        "",
        "relative/path",
        "file:relative",
        "file://host",
        "file://host/a/../b",
        "file://host/..",
        "file://host/%2e%2e/etc",
        "file://host/a%2F..%2Fb",
        "http://host/a",
        "ftp://host/a",
        "FILE//host/a",
        "file://host/a%00b",
        "file://host/a%1bb",
        "file://host/a%zzb",
        "file://host/a%",
        "file://host/a%c3",
        "file://ho\x07st/a",
        "file://host/a\x01b",
    ] {
        assert_eq!(parse_osc7(bad), None, "{bad:?}");
    }
    let long = format!("file://host/{}", "a".repeat(MAX_EVENT_BYTES));
    assert_eq!(parse_osc7(&long), None, "longer than the cap");
    assert_eq!(parse_osc7(&"%".repeat(1 << 20)), None);
}

#[test]
fn a_lone_bell_is_one_bell_event() {
    let mut e = engine(20, 3);
    e.feed(b"\x07");
    assert_eq!(e.take_events(), vec![EngineEvent::Bell]);
    assert!(e.take_events().is_empty(), "an event is delivered once");
}

#[test]
fn a_run_of_bells_in_one_feed_is_folded_into_one_event() {
    let mut e = engine(20, 3);
    e.feed(b"\x07\x07\x07");
    assert_eq!(e.take_events(), vec![EngineEvent::Bell]);
}

#[test]
fn the_bell_that_ends_a_title_is_a_terminator_and_makes_only_the_title_event() {
    let mut e = engine(20, 3);
    e.feed(b"\x1b]2;done\x07");
    assert_eq!(e.take_events(), vec![title("done")]);
    // ST-terminated, then a real bell after it.
    e.feed(b"\x1b]2;again\x1b\\\x07");
    assert_eq!(e.take_events(), vec![title("again"), EngineEvent::Bell]);
}

#[test]
fn clipboard_escapes_make_no_event_and_no_reply() {
    let mut e = engine(20, 3);
    e.take_replies();
    // OSC 52 write and read, ended by BEL: no clipboard callbacks (T-02-36), and a terminator is no bell.
    e.feed(b"\x1b]52;c;aGVsbG8=\x07\x1b]52;c;?\x07");
    assert!(e.take_events().is_empty());
    assert_eq!(ascii(&e.take_replies()), "");
}

#[test]
fn a_d_mark_fed_to_the_engine_is_a_command_end_event_with_its_status() {
    let mut e = engine(20, 3);
    e.feed(b"\x1b]133;D;3\x1b\\");
    assert_eq!(e.take_events(), vec![EngineEvent::CommandEnd { exit: Some(3) }]);
    assert!(e.take_events().is_empty(), "an event is delivered once");

    e.feed(b"\x1b]133;D\x07");
    assert_eq!(e.take_events(), vec![EngineEvent::CommandEnd { exit: None }]);

    // The other marks of the protocol make no event; a bell after the mark is a bell.
    e.feed(b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07");
    assert!(e.take_events().is_empty());
}

#[test]
fn two_identical_marks_in_a_row_fold_into_one_event() {
    let mut e = engine(20, 3);
    e.feed(b"\x1b]133;D;1\x07\x1b]133;D;1\x1b\\");
    assert_eq!(e.take_events(), vec![EngineEvent::CommandEnd { exit: Some(1) }]);
    e.feed(b"\x1b]133;D;1\x07\x1b]133;D;0\x07");
    assert_eq!(
        e.take_events(),
        vec![EngineEvent::CommandEnd { exit: Some(1) }, EngineEvent::CommandEnd { exit: Some(0) }]
    );
}

#[test]
fn the_terminator_of_a_d_mark_is_no_bell() {
    let mut e = engine(20, 3);
    e.feed(b"\x1b]133;D;0\x07");
    assert_eq!(e.take_events(), vec![EngineEvent::CommandEnd { exit: Some(0) }]);
}
