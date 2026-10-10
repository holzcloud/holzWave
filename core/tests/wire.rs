//! The frame protocol: engine updates become sequenced JSON frames with only the rows that changed (DESK-11, D-09,
//! D-11).
#![cfg(feature = "ghostty")]

mod common;

use common::engine;
use holzwave_core::TerminalEngine;
use holzwave_core::wire::{FrameEncoder, FrameMsg};

/// Run the engine's update through the encoder.
fn next_frame(e: &mut impl TerminalEngine, enc: &mut FrameEncoder) -> Option<FrameMsg> {
    enc.encode(e.update().expect("update"))
}

#[test]
fn typing_one_character_is_one_changed_row_with_a_sequence_number() {
    let mut e = engine(20, 5);
    let mut enc = FrameEncoder::new(7);

    let first = next_frame(&mut e, &mut enc).expect("the first frame is always emitted");
    assert!(first.full);
    assert_eq!(first.seq, 1);
    assert_eq!(first.pane, 7);
    assert_eq!((first.cols, first.rows), (20, 5));

    e.feed(b"hi");
    let typed = next_frame(&mut e, &mut enc).expect("typing changes a row");
    assert!(!typed.full);
    assert_eq!(typed.seq, 2);
    assert_eq!(typed.changed.len(), 1);
    let row = &typed.changed[0];
    assert_eq!(row.y, 0);
    assert_eq!(row.runs.len(), 1, "runs: {:?}", row.runs);
    assert!(row.runs[0].0.starts_with("hi"), "run text {:?}", row.runs[0].0);
    assert!(row.extras.is_empty());
}

#[test]
fn an_unchanged_update_encodes_to_nothing() {
    let mut e = engine(20, 5);
    let mut enc = FrameEncoder::new(1);
    next_frame(&mut e, &mut enc).expect("first");
    e.feed(b"hi");
    next_frame(&mut e, &mut enc).expect("typed");
    assert!(next_frame(&mut e, &mut enc).is_none(), "nothing changed, nothing is sent");
}

#[test]
fn the_json_frame_has_exactly_the_documented_keys() {
    let mut e = engine(20, 5);
    let mut enc = FrameEncoder::new(3);
    next_frame(&mut e, &mut enc).expect("first");
    e.feed(b"hi");
    let typed = next_frame(&mut e, &mut enc).expect("typed");

    let json = serde_json::to_value(&typed).expect("json");
    let mut keys: Vec<&str> = json.as_object().expect("object").keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut expected = vec![
        "pane", "seq", "full", "cols", "rows", "fg", "bg", "cursor", "scrollbar", "selection", "matches", "search",
        "copy_cursor", "modes", "changed",
    ];
    expected.sort_unstable();
    assert_eq!(keys, expected);

    assert!(json["search"].is_null());
    assert!(json["copy_cursor"].is_null());
    let cursor = &json["cursor"];
    assert_eq!(cursor["x"], 2);
    assert_eq!(cursor["y"], 0);
    assert_eq!(cursor["style"], "block");
    assert!(cursor["blink"].is_boolean());
    assert_eq!(json["scrollbar"].as_array().expect("scrollbar").len(), 3);
    let modes = json["modes"].as_object().expect("modes");
    let mut mode_keys: Vec<&str> = modes.keys().map(String::as_str).collect();
    mode_keys.sort_unstable();
    assert_eq!(mode_keys, vec!["alt", "bracketed_paste", "mouse"]);

    let row = &json["changed"][0];
    assert_eq!(row["y"], 0);
    assert!(row["runs"].is_array());
    assert!(row.get("extras").is_none(), "extras is omitted when empty");
    // A run is `[text, cols, fg, bg, flags]`.
    let run = row["runs"][0].as_array().expect("run");
    assert_eq!(run.len(), 5);
    assert!(run[0].as_str().expect("text").starts_with("hi"));
}
