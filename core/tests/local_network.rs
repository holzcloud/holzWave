//! Tests for `local_network.json`: the choice of the person, hostile files, the rule that nothing is probed unless the
//! person pressed `Allow`, and that the file never syncs.

mod common;

use std::cell::Cell;
use std::fs;

use common::scratch_dir;
use holzwave_core::layout::LoadStatus;
use holzwave_core::local_network::{
    Choice, FILE, LocalNetwork, MAX_BYTES, Reading, State, state_after_request, state_of,
};

#[test]
fn the_default_is_unasked_and_a_missing_file_is_clean() {
    assert_eq!(LocalNetwork::default().choice, Choice::Unasked);
    assert_eq!(Choice::default(), Choice::Unasked);
    let dir = scratch_dir("ln-missing");
    let (value, status) = LocalNetwork::load(&dir);
    assert_eq!(value, LocalNetwork::default());
    assert_eq!(status, LoadStatus::Clean);
    assert!(!dir.join(FILE).exists(), "loading writes nothing");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_choice_round_trips_through_the_file() {
    let dir = scratch_dir("ln-round");
    for choice in [Choice::Unasked, Choice::Declined, Choice::Requested] {
        let value = LocalNetwork { choice, ..LocalNetwork::default() };
        value.save(&dir).expect("saves");
        let (back, status) = LocalNetwork::load(&dir);
        assert_eq!(back, value);
        assert_eq!(status, LoadStatus::Clean);
    }
    let text = fs::read_to_string(dir.join(FILE)).expect("reads");
    assert!(text.ends_with('\n') && text.contains("\"version\": 1"), "pretty printed with a version: {text:?}");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn each_word_of_the_file_is_read() {
    for (word, choice) in [("unasked", Choice::Unasked), ("declined", Choice::Declined), ("requested", Choice::Requested)] {
        let (value, status) = LocalNetwork::from_json(&format!("{{\"version\": 1, \"choice\": \"{word}\"}}"));
        assert_eq!(value.choice, choice, "{word}");
        assert_eq!(status, LoadStatus::Clean, "{word}");
        assert_eq!(choice.as_str(), word);
    }
    // Unknown keys are ignored.
    let (value, status) = LocalNetwork::from_json("{\"choice\": \"declined\", \"other\": [1, 2]}");
    assert_eq!(value.choice, Choice::Declined);
    assert_eq!(status, LoadStatus::Clean);
}

#[test]
fn a_hostile_file_costs_the_choice_and_is_kept_as_bad() {
    let big = format!("{{\"choice\": \"requested\", \"pad\": \"{}\"}}", "x".repeat(5 * 1024));
    assert!(big.len() as u64 > MAX_BYTES);
    let cases: [(&str, String); 9] = [
        ("not json", "{".to_owned()),
        ("a number", "42".to_owned()),
        ("an array", "[\"requested\"]".to_owned()),
        ("an unknown word", "{\"choice\": \"Requested\"}".to_owned()),
        ("a blank after the word", "{\"choice\": \"requested \"}".to_owned()),
        ("a number as choice", "{\"choice\": 2}".to_owned()),
        ("no choice", "{\"version\": 1}".to_owned()),
        ("nul bytes", "\u{0}\u{1}\u{2}".to_owned()),
        ("oversized", big),
    ];
    for (label, text) in cases {
        let dir = scratch_dir("ln-hostile");
        fs::write(dir.join(FILE), &text).expect("writes");

        let (value, status) = LocalNetwork::load(&dir);

        assert_eq!(value.choice, Choice::Unasked, "{label}");
        assert!(matches!(status, LoadStatus::Repaired(_) | LoadStatus::Reset(_)), "{label}: {status:?}");
        let kept = fs::read(dir.join("local_network.json.bad")).unwrap_or_else(|e| panic!("{label}: no .bad copy: {e}"));
        assert_eq!(kept, text.as_bytes(), "{label}: the damaged bytes are kept");
        fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn a_file_that_is_not_utf8_is_reset_and_kept() {
    let dir = scratch_dir("ln-utf8");
    fs::write(dir.join(FILE), [0xff, 0xfe, 0x00, 0x7b]).expect("writes");
    let (value, status) = LocalNetwork::load(&dir);
    assert_eq!(value.choice, Choice::Unasked);
    assert!(matches!(status, LoadStatus::Reset(_)), "{status:?}");
    assert!(dir.join("local_network.json.bad").is_file());
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_answer_rule_has_six_cases() {
    // Allow always requests.
    assert_eq!(Choice::Unasked.answered(true), Choice::Requested);
    assert_eq!(Choice::Declined.answered(true), Choice::Requested);
    assert_eq!(Choice::Requested.answered(true), Choice::Requested);
    // Not now declines only from unasked, and never replaces a request.
    assert_eq!(Choice::Unasked.answered(false), Choice::Declined);
    assert_eq!(Choice::Declined.answered(false), Choice::Declined);
    assert_eq!(Choice::Requested.answered(false), Choice::Requested);
}

#[test]
fn nothing_is_probed_unless_the_choice_is_requested() {
    for choice in [Choice::Unasked, Choice::Declined] {
        let calls = Cell::new(0);
        let state = state_of(choice, || {
            calls.set(calls.get() + 1);
            Reading::Allowed
        });
        assert_eq!(state, State::NotAsked, "{choice:?}");
        assert_eq!(calls.get(), 0, "{choice:?} must not touch the network");
    }
    let calls = Cell::new(0);
    let state = state_of(Choice::Requested, || {
        calls.set(calls.get() + 1);
        Reading::Denied
    });
    assert_eq!(state, State::Denied);
    assert_eq!(calls.get(), 1, "a request probes exactly once");
}

#[test]
fn a_denial_right_after_the_request_reads_unknown() {
    assert_eq!(state_after_request(Reading::Allowed), State::Allowed);
    assert_eq!(state_after_request(Reading::Denied), State::Unknown);
    assert_eq!(state_after_request(Reading::Unknown), State::Unknown);
}

#[test]
fn the_state_words_are_the_wire_words() {
    assert_eq!(State::NotAsked.as_str(), "not_asked");
    assert_eq!(State::Allowed.as_str(), "allowed");
    assert_eq!(State::Denied.as_str(), "denied");
    assert_eq!(State::Unknown.as_str(), "unknown");
}

#[test]
fn the_file_never_syncs() {
    assert!(!holzwave_core::sync::engine::allowed("local_network.json"));
    assert!(holzwave_core::sync::engine::allowed("settings.json"), "the check can say yes");
}
