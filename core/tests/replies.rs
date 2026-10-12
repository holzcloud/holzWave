//! What the engine writes back to the program: query replies and paste (DESK-10, DESK-13).
//!
//! Replies come out of `take_replies` and the shell writes them to the PTY. Program-controlled replies are bounded
//! (a program must not grow memory by flooding queries); user-controlled bytes (paste) are never dropped. Every
//! expectation was measured on the pinned libghostty revision.
#![cfg(feature = "ghostty")]
// These tests start `/bin/sh`, `/bin/bash` or `/bin/cat`, which a Windows host does not have. Plan 04's
// `core/tests/conpty.rs` is their Windows counterpart.
#![cfg(unix)]

mod common;

use std::io::Read;
use std::sync::mpsc;
use std::time::Duration;

use common::{ascii, engine};
use holzwave_core::engine::PasteOutcome;
use holzwave_core::pty::Pty;
use holzwave_core::TerminalEngine;

/// Feed `input` and return what the engine wants written to the PTY, as readable ASCII.
fn reply_to(e: &mut impl TerminalEngine, input: &[u8]) -> String {
    e.feed(input);
    ascii(&e.take_replies())
}

#[test]
fn device_and_status_queries_are_answered() {
    let rows: &[(&str, &[u8], &str)] = &[
        ("cursor position", b"\x1b[6n", r"\x1b[1;1R"),
        ("device status", b"\x1b[5n", r"\x1b[0n"),
        ("DA1", b"\x1b[c", r"\x1b[?62;22c"),
        ("DA2", b"\x1b[>c", r"\x1b[>1;0;0c"),
        ("DA3", b"\x1b[=c", r"\x1bP!|00000000\x1b\\"),
        ("DECRQM wraparound", b"\x1b[?7$p", r"\x1b[?7;1$y"),
        ("XTVERSION", b"\x1b[>q", r"\x1bP>|libghostty\x1b\\"),
        ("kitty keyboard query", b"\x1b[?u", r"\x1b[?0u"),
        ("DECRQSS SGR", b"\x1bP$qm\x1b\\", r"\x1bP1$r0m\x1b\\"),
    ];
    let mut wrong = Vec::new();
    for (name, query, expected) in rows {
        let mut e = engine(80, 24);
        let got = reply_to(&mut e, query);
        if got != *expected {
            wrong.push(format!("{name}: expected {expected}, got {got}"));
        }
    }
    assert!(wrong.is_empty(), "rows that differ:\n{}", wrong.join("\n"));
}

#[test]
fn the_cursor_position_report_follows_the_cursor() {
    let mut e = engine(80, 24);
    e.feed(b"abc\r\n  xy");
    assert_eq!(reply_to(&mut e, b"\x1b[6n"), r"\x1b[2;5R");
}

#[test]
fn size_reports_carry_the_cell_dimensions() {
    let mut e = engine(80, 24);
    e.resize(80, 24, (8, 17)).unwrap();
    assert_eq!(reply_to(&mut e, b"\x1b[18t"), r"\x1b[8;24;80t");
    assert_eq!(reply_to(&mut e, b"\x1b[14t"), r"\x1b[4;408;640t");
    assert_eq!(reply_to(&mut e, b"\x1b[16t"), r"\x1b[6;17;8t");
}

#[test]
fn size_reports_follow_a_resize() {
    let mut e = engine(80, 24);
    e.resize(100, 30, (9, 20)).unwrap();
    assert_eq!(reply_to(&mut e, b"\x1b[18t"), r"\x1b[8;30;100t");
    assert_eq!(reply_to(&mut e, b"\x1b[14t"), r"\x1b[4;600;900t");
}

#[test]
fn a_flood_of_queries_cannot_grow_the_reply_buffer_past_64_kib() {
    const CAP: usize = 64 * 1024;
    // XTVERSION is a 4 byte query with a 16 byte reply: 20 000 of them ask for 320 000 bytes.
    let query = b"\x1b[>q".repeat(20_000);
    let mut e = engine(80, 24);
    e.feed(&query);
    let replies = e.take_replies();
    assert!(replies.len() <= CAP, "kept {} bytes", replies.len());
    assert!(!replies.is_empty(), "the first replies are kept");
    // Whole replies only: a cut-off sequence would corrupt the next thing the program reads.
    assert_eq!(replies.len() % 16, 0, "a reply was cut in the middle");
    assert!(replies.starts_with(b"\x1bP>|libghostty\x1b\\"));

    // The buffer is drained and the engine keeps working.
    assert_eq!(reply_to(&mut e, b"\x1b[5n"), r"\x1b[0n");
}

#[test]
fn undrained_floods_stay_bounded_across_feeds() {
    let query = b"\x1b[>q".repeat(5_000);
    let mut e = engine(80, 24);
    for _ in 0..20 {
        e.feed(&query);
    }
    assert!(e.take_replies().len() <= 64 * 1024);
}

#[test]
fn the_clipboard_cannot_be_read_by_the_program() {
    let mut e = engine(80, 24);
    // OSC 52 read: ask for the system clipboard.
    assert_eq!(reply_to(&mut e, b"\x1b]52;c;?\x1b\\"), "");
    assert_eq!(reply_to(&mut e, b"\x1b]52;c;?\x07"), "");
    // OSC 52 write is ignored too, and says nothing.
    assert_eq!(reply_to(&mut e, b"\x1b]52;c;aGVsbG8=\x1b\\"), "");
}

#[test]
fn the_window_title_is_never_reported_back_into_the_input() {
    let mut e = engine(80, 24);
    e.feed(b"\x1b]2;rm -rf /\x07");
    assert_eq!(reply_to(&mut e, b"\x1b[21t"), "");
    assert_eq!(reply_to(&mut e, b"\x1b[20t"), "");
}

#[test]
fn kitty_paste_events_are_not_enabled_by_the_engine() {
    // Registering a clipboard read callback would turn mode 5522 into "send an event instead of the text".
    let mut e = engine(80, 24);
    e.feed(b"\x1b[?5522h\x1b[?2004h");
    assert_eq!(e.paste("hello", false).unwrap(), PasteOutcome::Written);
    assert_eq!(ascii(&e.take_replies()), r"\x1b[200~hello\x1b[201~");
}

/// Paste `text` into a fresh engine after feeding `setup`; returns the outcome and the bytes written.
fn paste_into(setup: &[u8], text: &str, allow_unsafe: bool) -> (PasteOutcome, String) {
    let mut e = engine(80, 24);
    e.feed(setup);
    let _ = e.take_replies();
    let outcome = e.paste(text, allow_unsafe).expect("paste");
    (outcome, ascii(&e.take_replies()))
}

#[test]
fn paste_without_bracketed_mode() {
    let (outcome, bytes) = paste_into(b"", "ls -la", false);
    assert_eq!((outcome, bytes.as_str()), (PasteOutcome::Written, "ls -la"));
}

#[test]
fn a_multi_line_paste_without_bracketed_mode_is_rejected_and_writes_nothing() {
    let (outcome, bytes) = paste_into(b"", "a\nb", false);
    assert_eq!((outcome, bytes.as_str()), (PasteOutcome::Rejected, ""));
}

#[test]
fn a_rejected_paste_goes_through_when_the_user_confirms() {
    let (outcome, bytes) = paste_into(b"", "a\nb", true);
    assert_eq!((outcome, bytes.as_str()), (PasteOutcome::Written, r"a\rb"));
}

#[test]
fn bracketed_mode_wraps_the_paste_and_keeps_newlines() {
    let (outcome, bytes) = paste_into(b"\x1b[?2004h", "a\nb", false);
    assert_eq!(outcome, PasteOutcome::Written);
    assert_eq!(bytes, r"\x1b[200~a\nb\x1b[201~");
}

#[test]
fn escape_inside_a_bracketed_paste_becomes_a_space() {
    let (outcome, bytes) = paste_into(b"\x1b[?2004h", "a\x1bb", false);
    assert_eq!(outcome, PasteOutcome::Written);
    assert_eq!(bytes, r"\x1b[200~a b\x1b[201~");
}

#[test]
fn the_end_marker_in_the_text_is_rejected() {
    let (outcome, bytes) = paste_into(b"\x1b[?2004h", "x\x1b[201~rm -rf /", false);
    assert_eq!((outcome, bytes.as_str()), (PasteOutcome::Rejected, ""));
}

#[test]
fn the_end_marker_is_defused_when_the_user_confirms() {
    let (outcome, bytes) = paste_into(b"\x1b[?2004h", "x\x1b[201~y", true);
    assert_eq!(outcome, PasteOutcome::Written);
    // The paste ends exactly once, at the very end: the marker inside the text lost its ESC.
    assert_eq!(bytes, r"\x1b[200~x [201~y\x1b[201~");
    assert_eq!(bytes.matches(r"\x1b[201~").count(), 1);
}

#[test]
fn nul_becomes_a_space_and_tab_is_kept() {
    let (outcome, bytes) = paste_into(b"\x1b[?2004h", "a\0b\tc", false);
    assert_eq!(outcome, PasteOutcome::Written);
    assert_eq!(bytes, r"\x1b[200~a b\tc\x1b[201~");
}

#[test]
fn an_empty_paste_reports_empty() {
    let (outcome, bytes) = paste_into(b"", "", false);
    assert_eq!((outcome, bytes.as_str()), (PasteOutcome::Empty, ""));
    let (outcome, bytes) = paste_into(b"\x1b[?2004h", "", false);
    assert_eq!((outcome, bytes.as_str()), (PasteOutcome::Empty, ""));
}

#[test]
fn paste_bytes_come_out_of_take_replies_like_query_replies() {
    let mut e = engine(80, 24);
    e.feed(b"\x1b[5n");
    assert_eq!(e.paste("hi", false).unwrap(), PasteOutcome::Written);
    // Query reply first, then the paste, in order, in one buffer.
    assert_eq!(ascii(&e.take_replies()), r"\x1b[0nhi");
    assert!(e.take_replies().is_empty());
}

#[test]
fn a_large_paste_is_never_dropped() {
    // The 64 KiB cap is for program-controlled replies; the user's paste is whole.
    let text = "x".repeat(1 << 20);
    let mut e = engine(80, 24);
    assert_eq!(e.paste(&text, false).unwrap(), PasteOutcome::Written);
    assert_eq!(e.take_replies().len(), text.len());
}

#[test]
fn bash_cursor_position_query_round_trip() {
    // A real shell asks where the cursor is, the engine answers, and the answer reaches the shell's stdin.
    // `stty -echo -icanon` makes the reply readable at once and keeps it off the screen: without it the reply could
    // arrive before `read` switches the terminal mode.
    let script = r#"stty -echo -icanon
printf 'abc\r\n  xy'
printf '\033[6n'
IFS=';' read -s -d R -t 5 row col
row=${row#*[}
printf '\r\nROW=%s COL=%s\r\n' "$row" "$col""#;

    let mut pty = Pty::spawn("/bin/bash", &["-c", script], 80, 24).expect("spawn bash");
    let mut reader = pty.take_reader().expect("reader");
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    let mut e = engine(80, 24);
    let mut screen = String::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(chunk) => {
                e.feed(&chunk);
                let reply = e.take_replies();
                if !reply.is_empty() {
                    pty.write(&reply).expect("write the reply back");
                }
                screen = e.screen().unwrap().to_text();
                if screen.contains("COL=") {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = pty.wait();
    assert!(screen.contains("ROW=2 COL=5"), "the shell saw: {screen:?}");
}
