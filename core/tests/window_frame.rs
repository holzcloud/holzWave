//! Tests for `window_frame.json`: the two choices of the window frame, the words of their tables, hostile files, the example
//! of `docs/config.md`, and that the file never syncs.

mod common;

use std::fs;
use std::sync::Arc;

use common::scratch_dir;
use holzwave_core::layout::LoadStatus;
use holzwave_core::secrets::MemoryStore;
use holzwave_core::settings::Settings;
use holzwave_core::sync::engine::{ConfigSource, Source};
use holzwave_core::window_frame::{ButtonChoice, FILE, FrameChoice, MAX_BYTES, WindowFrame};

const FRAMES: [FrameChoice; 3] = [FrameChoice::Auto, FrameChoice::Own, FrameChoice::System];
const BUTTONS: [ButtonChoice; 3] = [ButtonChoice::Auto, ButtonChoice::Flat, ButtonChoice::Round];

#[test]
fn the_default_is_auto_and_a_missing_file_is_clean() {
    let default = WindowFrame::default();
    assert_eq!((default.frame, default.buttons), (FrameChoice::Auto, ButtonChoice::Auto));
    assert_eq!(
        default.to_json(),
        "{\n  \"version\": 1,\n  \"window_frame\": \"auto\",\n  \"window_buttons\": \"auto\"\n}\n"
    );
    let dir = scratch_dir("wf-missing");
    let (value, status) = WindowFrame::load(&dir);
    assert_eq!(value, default);
    assert_eq!(status, LoadStatus::Clean);
    assert!(!dir.join(FILE).exists(), "loading writes nothing");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn all_nine_pairs_round_trip_through_the_file() {
    let dir = scratch_dir("wf-round");
    for frame in FRAMES {
        for buttons in BUTTONS {
            let value = WindowFrame { frame, buttons, ..WindowFrame::default() };
            value.save(&dir).expect("saves");
            let (back, status) = WindowFrame::load(&dir);
            assert_eq!(back, value);
            assert_eq!(status, LoadStatus::Clean);
            // The text alone says the same.
            assert_eq!(WindowFrame::from_json(&value.to_json()), (value, LoadStatus::Clean));
        }
    }
    let text = fs::read_to_string(dir.join(FILE)).expect("reads");
    assert!(text.ends_with('\n') && text.contains("\"version\": 1"), "pretty printed with a version: {text:?}");
    assert!(!dir.join("window_frame.json.bad").exists(), "a valid file never leaves a .bad copy");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn each_member_reads_only_an_exact_word_of_its_table() {
    let (value, status) = WindowFrame::from_json("{\"version\": 1, \"window_frame\": \"own\", \"window_buttons\": \"round\"}");
    assert_eq!((value.frame, value.buttons), (FrameChoice::Own, ButtonChoice::Round));
    assert_eq!(status, LoadStatus::Clean);
    // Unknown keys are ignored; a missing version is the current one.
    let (value, status) = WindowFrame::from_json("{\"window_frame\": \"system\", \"window_buttons\": \"flat\", \"other\": [1]}");
    assert_eq!((value.frame, value.buttons), (FrameChoice::System, ButtonChoice::Flat));
    assert_eq!(status, LoadStatus::Clean);

    let bad_values = [
        "\"Own\"",
        "\"OWN\"",
        "\" own\"",
        "\"own \"",
        "\"\"",
        "\"flat\"",
        "\"round\"",
        "1",
        "true",
        "null",
        "[\"own\"]",
        "{\"own\": true}",
    ];
    for bad in bad_values {
        // A bad frame costs the frame alone: the buttons keep their good word.
        let text = format!("{{\"version\": 1, \"window_frame\": {bad}, \"window_buttons\": \"round\"}}");
        let (value, status) = WindowFrame::from_json(&text);
        assert_eq!(value.frame, FrameChoice::Auto, "{bad}: a bad frame reads auto");
        assert_eq!(value.buttons, ButtonChoice::Round, "{bad}: the good buttons survive");
        assert!(matches!(status, LoadStatus::Repaired(_)), "{bad}: {status:?}");
    }
    for bad in ["\"Round\"", "\"own\"", "\"system\"", "\"\"", "\"round \"", "0", "false", "null", "[]", "{}"] {
        let text = format!("{{\"version\": 1, \"window_frame\": \"system\", \"window_buttons\": {bad}}}");
        let (value, status) = WindowFrame::from_json(&text);
        assert_eq!(value.buttons, ButtonChoice::Auto, "{bad}: a bad button word reads auto");
        assert_eq!(value.frame, FrameChoice::System, "{bad}: the good frame survives");
        assert!(matches!(status, LoadStatus::Repaired(_)), "{bad}: {status:?}");
    }
    // A missing key is a doubt for that member alone.
    let (value, status) = WindowFrame::from_json("{\"version\": 1, \"window_buttons\": \"flat\"}");
    assert_eq!((value.frame, value.buttons), (FrameChoice::Auto, ButtonChoice::Flat));
    assert!(matches!(status, LoadStatus::Repaired(_)));
    let (value, status) = WindowFrame::from_json("{\"version\": 1, \"window_frame\": \"own\"}");
    assert_eq!((value.frame, value.buttons), (FrameChoice::Own, ButtonChoice::Auto));
    assert!(matches!(status, LoadStatus::Repaired(_)));
}

#[test]
fn a_hostile_file_reads_auto_and_is_kept_as_bad() {
    let big = format!("{{\"window_frame\": \"system\", \"pad\": \"{}\"}}", "x".repeat(5 * 1024));
    assert!(big.len() as u64 > MAX_BYTES);
    let cases: [(&str, String); 12] = [
        ("not json", "{".to_owned()),
        ("a number", "42".to_owned()),
        ("a word alone", "\"system\"".to_owned()),
        ("an array", "[\"system\"]".to_owned()),
        ("empty", String::new()),
        ("a bad frame", "{\"version\": 1, \"window_frame\": \"Sys\", \"window_buttons\": \"auto\"}".to_owned()),
        ("a bad button word", "{\"version\": 1, \"window_frame\": \"auto\", \"window_buttons\": 7}".to_owned()),
        ("no members", "{\"version\": 1}".to_owned()),
        ("an unknown version", "{\"version\": 2, \"window_frame\": \"system\", \"window_buttons\": \"round\"}".to_owned()),
        ("a version that is text", "{\"version\": \"1\", \"window_frame\": \"system\"}".to_owned()),
        ("nul bytes", "\u{0}\u{1}\u{2}".to_owned()),
        ("oversized", big),
    ];
    for (label, text) in cases {
        let dir = scratch_dir("wf-hostile");
        fs::write(dir.join(FILE), &text).expect("writes");

        let (value, status) = WindowFrame::load(&dir);

        assert!(matches!(status, LoadStatus::Repaired(_) | LoadStatus::Reset(_)), "{label}: {status:?}");
        if label != "a bad frame" && label != "a bad button word" && label != "no members" {
            assert_eq!(value, WindowFrame::default(), "{label}: an unusable file gives the default");
        }
        let kept = fs::read(dir.join("window_frame.json.bad")).unwrap_or_else(|e| panic!("{label}: no .bad copy: {e}"));
        assert_eq!(kept, text.as_bytes(), "{label}: the damaged bytes are kept");
        fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn a_file_that_is_not_utf8_reads_auto_and_is_kept() {
    let dir = scratch_dir("wf-utf8");
    fs::write(dir.join(FILE), [0xff, 0xfe, 0x00, 0x7b]).expect("writes");
    let (value, status) = WindowFrame::load(&dir);
    assert_eq!(value, WindowFrame::default());
    assert!(matches!(status, LoadStatus::Reset(_)), "{status:?}");
    assert!(dir.join("window_frame.json.bad").is_file());
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_status_never_names_the_content_of_the_file() {
    for text in [
        "{\"window_frame\": \"hunter2-secret\", \"window_buttons\": \"auto\"}",
        "{\"window_frame\": \"auto\", \"window_buttons\": \"hunter2-secret\"}",
        "{\"window_frame\": \"hunter2-secret\"",
    ] {
        let dir = scratch_dir("wf-secret");
        fs::write(dir.join(FILE), text).expect("writes");
        let (_, status) = WindowFrame::load(&dir);
        assert!(!matches!(status, LoadStatus::Clean), "{text}");
        assert!(!format!("{status:?}").contains("hunter2"), "{status:?}");
        fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn the_words_have_a_method_each_way() {
    let frame_words = ["auto", "own", "system"];
    for (choice, word) in FRAMES.into_iter().zip(frame_words) {
        assert_eq!(choice.word(), word);
        assert_eq!(FrameChoice::from_word(word), Some(choice));
    }
    let button_words = ["auto", "flat", "round"];
    for (choice, word) in BUTTONS.into_iter().zip(button_words) {
        assert_eq!(choice.word(), word);
        assert_eq!(ButtonChoice::from_word(word), Some(choice));
    }
    assert_eq!(FrameChoice::default(), FrameChoice::Auto);
    assert_eq!(ButtonChoice::default(), ButtonChoice::Auto);
    for word in ["", "Auto", "AUTO", " auto", "auto ", "own ", "Own", "true", "0", "system\n", "sys", "flat", "round"] {
        assert_eq!(FrameChoice::from_word(word), None, "{word:?} is no frame word");
    }
    for word in ["", "Auto", "AUTO", " auto", "auto ", "flat ", "Round", "true", "0", "round\n", "own", "system"] {
        assert_eq!(ButtonChoice::from_word(word), None, "{word:?} is no button word");
    }
    assert_eq!(FrameChoice::from_word("flat"), None);
    assert_eq!(FrameChoice::from_word("round"), None);
    assert_eq!(FrameChoice::from_word(""), None);
    assert_eq!(ButtonChoice::from_word("own"), None);
    assert_eq!(ButtonChoice::from_word("system"), None);
    assert_eq!(ButtonChoice::from_word(""), None);
}

#[test]
fn the_file_never_syncs() {
    assert!(!holzwave_core::sync::engine::allowed("window_frame.json"));
    assert!(!holzwave_core::sync::engine::allowed("window_frame.json.bad"));
    assert!(holzwave_core::sync::engine::allowed("settings.json"), "the check can say yes");

    // The choices are not settings: the settings document has no such key, so they cannot travel inside settings.json.
    let document = Settings::default().to_json();
    let value: serde_json::Value = serde_json::from_str(&document).expect("the settings are JSON");
    let object = value.as_object().expect("an object");
    assert!(!object.is_empty());
    assert!(object.keys().all(|key| !key.starts_with("window_")), "{:?}", object.keys().collect::<Vec<_>>());
    for stray in ["{\"window_frame\": \"system\"}", "{\"window_buttons\": \"round\"}"] {
        let (settings, status) = Settings::from_json(stray);
        assert!(matches!(status, LoadStatus::Repaired(_)), "a stray key is reported, not adopted: {status:?}");
        assert!(!settings.to_json().contains("window_"), "{stray}");
    }

    // A real data directory with the file in it: the sync enumerates every name it would carry, and this one is not there.
    let dir = scratch_dir("wf-sync");
    WindowFrame { frame: FrameChoice::System, buttons: ButtonChoice::Round, ..WindowFrame::default() }
        .save(&dir)
        .expect("saves");
    fs::write(dir.join("settings.json"), Settings::default().to_json()).expect("writes");
    let source = ConfigSource::new(&dir, Arc::new(MemoryStore::default()));
    let names = source.names().expect("names");
    assert!(names.contains(&"settings.json".to_owned()), "the enumeration sees the files it carries: {names:?}");
    assert!(!names.iter().any(|name| name.contains("window_frame")), "{names:?}");
    assert_eq!(source.read("window_frame.json").ok().flatten(), None, "a read by name finds nothing either");
    fs::remove_dir_all(&dir).ok();
}

/// The fenced example that follows `<!-- example: window_frame.json -->` in `docs/config.md`.
fn documented_example() -> String {
    let doc = include_str!("../../docs/config.md");
    let marker = "<!-- example: window_frame.json -->";
    let after = doc.split_once(marker).expect("docs/config.md has the example").1;
    let fence = after.trim_start().strip_prefix("```json\n").expect("a json fence follows the marker");
    fence.split_once("\n```").expect("the fence is closed").0.to_owned()
}

#[test]
fn the_documented_example_loads_cleanly() {
    let (value, status) = WindowFrame::from_json(&documented_example());
    assert_eq!(status, LoadStatus::Clean, "the example of docs/config.md is not clean");
    assert_eq!((value.frame, value.buttons), (FrameChoice::System, ButtonChoice::Round));
    assert_eq!(value.version, 1);
    // The documented file, as a file, through the loader.
    let dir = scratch_dir("wf-doc");
    fs::write(dir.join(FILE), documented_example()).expect("writes");
    assert_eq!(WindowFrame::load(&dir), (value, LoadStatus::Clean));
    assert!(!dir.join("window_frame.json.bad").exists());
    fs::remove_dir_all(&dir).ok();
}
