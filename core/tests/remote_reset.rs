//! What the engine of a remote pane is fed before a snapshot, found by trying: an engine that showed anything must show exactly
//! what a new engine shows when it is fed the same snapshot. The sequence in `RESET_BEFORE_SNAPSHOT` is the smallest that does
//! this; this file shows what each smaller one leaves behind.
#![cfg(feature = "ghostty")]
#![cfg(unix)]

use holzwave_core::GhosttyEngine;
use holzwave_core::engine::TerminalEngine;
use holzwave_core::remote::link::RESET_BEFORE_SNAPSHOT;
use holzwave_core::textread::TextRegion;

const BOUND: usize = 4 * 1024 * 1024;

/// A terminal with a history, styled text, a cursor that is not at home and modes that differ from the defaults: what a real
/// session has, and what the snapshot of it carries.
fn source() -> Vec<u8> {
    let mut engine = GhosttyEngine::new(80, 24, 10_000).expect("engine");
    for n in 0..200 {
        engine.feed(format!("line {n}\r\n").as_bytes());
    }
    engine.feed(b"\x1b[31;1mred bold\x1b[0m plain \x1b[4munderlined\x1b[0m\r\n\x1b[?2004h\x1b[5;10H\x1b[?25l");
    engine.snapshot_vt(500, BOUND).expect("snapshot")
}

/// Everything a pane that has shown something else could still be showing: history, the alternate screen, a scrolling region,
/// tab stops, modes, styles, a title and a directory, a cursor shape.
fn garbage(engine: &mut GhosttyEngine) {
    for n in 0..300 {
        engine.feed(format!("garbage {n}\r\n").as_bytes());
    }
    engine.feed(b"\x1b[33;7mold style\x1b[0m\x1b[3g\x1bH\x1b[5;10r\x1b[?1h\x1b[?2004l\x1b[?7l\x1b]0;a title\x07\x1b]7;file:///elsewhere\x07\x1b[5 q");
    engine.feed(b"\x1b[?1049hon the alternate screen\x1b[10;10Hmore");
}

fn state(engine: &mut GhosttyEngine) -> (Vec<u8>, String) {
    let snapshot = engine.snapshot_vt(1000, BOUND).expect("snapshot");
    let text = engine.read_text(TextRegion::LastRows(1000), 1 << 20).expect("read").map(|r| r.text).unwrap_or_default();
    (snapshot, text)
}

fn fed(prefix: &[u8], snapshot: &[u8]) -> (Vec<u8>, String) {
    let mut engine = GhosttyEngine::new(80, 24, 10_000).expect("engine");
    garbage(&mut engine);
    engine.feed(prefix);
    engine.feed(snapshot);
    state(&mut engine)
}

fn fresh(snapshot: &[u8]) -> (Vec<u8>, String) {
    let mut engine = GhosttyEngine::new(80, 24, 10_000).expect("engine");
    engine.feed(snapshot);
    state(&mut engine)
}

#[test]
fn a_full_reset_before_the_snapshot_is_the_smallest_prefix_that_makes_an_engine_new() {
    let snapshot = source();
    let new = fresh(&snapshot);
    assert!(new.1.contains("line 199"), "the source has its history");

    // The prefix the link feeds: the engine shows exactly what a new one shows, to the byte of its own snapshot.
    let got = fed(RESET_BEFORE_SNAPSHOT, &snapshot);
    assert_eq!(got.1, new.1, "the text");
    assert!(got.0 == new.0, "the snapshot of the engine that was reset differs from the one of a new engine");

    // Nothing at all leaves the old content in place.
    let none = fed(b"", &snapshot);
    assert_ne!(none.1, new.1, "without a reset the history of the old content is still there");

    // Each smaller thing that one might try leaves something behind.
    let smaller: [(&str, &[u8]); 6] = [
        ("clear the history only", b"\x1b[3J"),
        ("clear the screen", b"\x1b[H\x1b[2J"),
        ("clear the screen and the history", b"\x1b[H\x1b[2J\x1b[3J"),
        ("leave the alternate screen, then clear", b"\x1b[?1049l\x1b[H\x1b[2J\x1b[3J"),
        ("a soft reset (DECSTR) and clear", b"\x1b[!p\x1b[?1049l\x1b[H\x1b[2J\x1b[3J"),
        ("reset the modes by hand", b"\x1b[?1049l\x1b[0m\x1b[r\x1b[3g\x1b[?1l\x1b[?7h\x1b[?2004l\x1b[?25h\x1b[0 q\x1b[H\x1b[2J\x1b[3J"),
    ];
    for (name, prefix) in smaller {
        let got = fed(prefix, &snapshot);
        let same = got.1 == new.1 && got.0 == new.0;
        println!("{name}: {}", if same { "equal to a new engine" } else { "leaves something behind" });
        assert!(!same, "{name} must not be enough: only the full reset is");
    }
}
