//! Tests for `settings.json`: defaults, the on-disk document, damaged files, and the path from a setting on disk to
//! the engine palette.

mod common;

use common::scratch_dir;
use holzwave_core::engine::Color;
use holzwave_core::layout::LoadStatus;
use holzwave_core::settings::{Blink, Contrast, Motion, SETTINGS_FILE, Settings};
use holzwave_core::theme::{Rgb, ThemeKind, ThemeRegistry};

fn rgb(text: &str) -> Color {
    let c = Rgb::parse(text).unwrap_or_else(|| panic!("{text} is a colour"));
    Color { r: c.r, g: c.g, b: c.b }
}

// ---- defaults --------------------------------------------------------------------------------------

#[test]
fn defaults_match_the_decisions() {
    let s = Settings::default();
    assert_eq!(s.version, 1);
    assert_eq!(s.theme, "aubergine", "D-20: aubergine is the default");
    assert_eq!(s.font_size, 14);
    assert_eq!(s.ui_scale, 100);
    assert!(!s.option_as_alt, "D-12: Option is not Alt by default");
    assert!(s.confirm_multiline_paste, "D-15: multi-line paste is confirmed by default");
    assert_eq!(s.scrollback_lines, 10_000);
    assert_eq!(s.cursor_blink, Blink::System);
    assert_eq!(s.motion, Motion::System);
    assert_eq!(s.contrast, Contrast::System);
    assert!(s.keybindings.is_empty());
}

// ---- the document ----------------------------------------------------------------------------------

#[test]
fn json_round_trip_is_lossless_and_clean() {
    let mut s = Settings::default();
    s.theme = "Schiefer".into();
    s.font_size = 17;
    s.option_as_alt = true;
    s.cursor_blink = Blink::Off;
    s.keybindings.insert("pane.split_right".into(), "Cmd+D".into());
    let (back, status) = Settings::from_json(&s.to_json());
    assert_eq!(back, s);
    assert_eq!(status, LoadStatus::Clean);
}

#[test]
fn the_document_is_pretty_printed_with_a_trailing_newline_and_a_stable_key_order() {
    let text = Settings::default().to_json();
    assert!(text.ends_with("}\n"), "trailing newline: {text:?}");
    assert!(text.lines().count() > 5, "one key per line, not a single line: {text:?}");
    let order = [
        "\"version\"",
        "\"theme\"",
        "\"font_size\"",
        "\"ui_scale\"",
        "\"option_as_alt\"",
        "\"cursor_blink\"",
        "\"motion\"",
        "\"contrast\"",
        "\"scrollback_lines\"",
        "\"confirm_multiline_paste\"",
        "\"keybindings\"",
    ];
    let mut at = 0;
    for key in order {
        let found = text[at..].find(key).unwrap_or_else(|| panic!("{key} missing or out of order in {text}"));
        at += found + key.len();
    }
    assert_eq!(text, Settings::default().to_json(), "serialising twice gives the same text");
}

#[test]
fn enums_are_written_in_lower_case() {
    let mut s = Settings::default();
    s.cursor_blink = Blink::On;
    s.motion = Motion::Reduce;
    s.contrast = Contrast::High;
    let text = s.to_json();
    assert!(text.contains("\"cursor_blink\": \"on\""), "{text}");
    assert!(text.contains("\"motion\": \"reduce\""), "{text}");
    assert!(text.contains("\"contrast\": \"high\""), "{text}");
}

#[test]
fn a_partial_file_keeps_what_it_has_and_defaults_the_rest() {
    let (s, status) = Settings::from_json(r#"{"theme": "aubergine", "font_size": 16}"#);
    assert_eq!(s.font_size, 16);
    assert_eq!(s.theme, "aubergine");
    assert_eq!(s.ui_scale, 100);
    assert_eq!(s.scrollback_lines, 10_000);
    assert!(s.confirm_multiline_paste);
    assert_eq!(status, LoadStatus::Clean);
}

#[test]
fn unknown_keys_are_ignored() {
    let (s, status) = Settings::from_json(r#"{"font_size": 15, "future_thing": {"a": [1, 2]}, "another": true}"#);
    assert_eq!(s.font_size, 15);
    assert_eq!(status, LoadStatus::Clean);
}

// ---- the file --------------------------------------------------------------------------------------

#[test]
fn a_directory_without_settings_gives_defaults_and_clean() {
    let dir = scratch_dir("settings-missing");
    let (s, status) = Settings::load(&dir);
    assert_eq!(s, Settings::default());
    assert_eq!(status, LoadStatus::Clean);
    assert!(!dir.join(SETTINGS_FILE).exists(), "load never creates the file");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_file_that_is_not_json_gives_defaults_reset_and_a_bad_copy() {
    let dir = scratch_dir("settings-garbage");
    let path = dir.join(SETTINGS_FILE);
    std::fs::write(&path, b"{ this is not json").unwrap();
    let (s, status) = Settings::load(&dir);
    assert_eq!(s, Settings::default());
    assert!(matches!(&status, LoadStatus::Reset(reason) if !reason.is_empty()), "got {status:?}");
    assert_eq!(std::fs::read(dir.join("settings.json.bad")).unwrap(), b"{ this is not json");
    assert_eq!(std::fs::read(&path).unwrap(), b"{ this is not json", "load itself does not rewrite the file");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_valid_file_is_never_rewritten_by_load() {
    let dir = scratch_dir("settings-valid");
    let path = dir.join(SETTINGS_FILE);
    let text = "{\"theme\":\"aubergine\",   \"font_size\": 16}";
    std::fs::write(&path, text).unwrap();
    let (s, status) = Settings::load(&dir);
    assert_eq!((s.font_size, status), (16, LoadStatus::Clean));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "byte for byte what the user wrote");
    assert!(!dir.join("settings.json.bad").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn save_then_load_round_trips_and_leaves_one_file() {
    let dir = scratch_dir("settings-save");
    let nested = dir.join("config");
    let mut s = Settings::default();
    s.font_size = 19;
    s.theme = "Papier".into();
    s.save(&nested).unwrap();
    let (back, status) = Settings::load(&nested);
    assert_eq!(back, s);
    assert_eq!(status, LoadStatus::Clean);
    let names: Vec<String> =
        std::fs::read_dir(&nested).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, vec!["settings.json"]);
    std::fs::remove_dir_all(dir).unwrap();
}

// ---- a setting on disk reaches the engine palette (tracer) -------------------------------------------

#[test]
fn a_theme_selected_in_settings_json_resolves_to_the_engine_palette() {
    let dir = scratch_dir("settings-tracer");
    std::fs::write(dir.join(SETTINGS_FILE), r#"{"theme": "aubergine", "font_size": 16}"#).unwrap();
    let (settings, status) = Settings::load(&dir);
    assert_eq!(status, LoadStatus::Clean);

    let registry = ThemeRegistry::builtin();
    let engine = registry.get(&settings.theme).engine_theme();
    assert_eq!(engine.bg, rgb("#14091A"));
    assert_eq!(engine.cursor, rgb("#FF8FB8"));
    assert_eq!(engine.fg, rgb("#F6EFE6"));
    assert_eq!(engine.ansi[1], rgb("#EC8272"), "ANSI red");
    assert_eq!(engine.ansi[15], rgb("#F6EFE6"), "ANSI bright white");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_unknown_theme_name_resolves_to_aubergine() {
    let registry = ThemeRegistry::builtin();
    let theme = registry.get("no such theme");
    assert_eq!(theme.name, "aubergine");
    assert_eq!(theme.kind, ThemeKind::Dark);
    assert_eq!(theme.engine_theme().bg, rgb("#14091A"));
}

// ---- colours -----------------------------------------------------------------------------------------

#[test]
fn rgb_parses_either_case_and_prints_upper_case() {
    assert_eq!(Rgb::parse("#14091a"), Some(Rgb::new(0x14, 0x09, 0x1A)));
    assert_eq!(Rgb::parse("#14091A"), Some(Rgb::new(0x14, 0x09, 0x1A)));
    assert_eq!(Rgb::new(0x14, 0x09, 0x1A).to_string(), "#14091A");
    assert_eq!(Rgb::new(0, 0, 0).to_string(), "#000000");
}

#[test]
fn rgb_refuses_everything_that_is_not_exactly_hash_and_six_hex_digits() {
    for bad in [
        "", "#", "#fff", "14091A", "#14091", "#14091AB", "#14091G", "red", "rgb(1,2,3)", " #14091A", "#14091A ",
        "#14091A;", "url(x)", "#１４０９１Ａ",
    ] {
        assert_eq!(Rgb::parse(bad), None, "{bad:?} must be refused");
    }
}
