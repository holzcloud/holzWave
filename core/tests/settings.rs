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
    assert!(!s.smooth_scroll, "DESK-28: the effects are opt-in");
    assert!(!s.cursor_trail, "DESK-28: the effects are opt-in");
    assert!(s.keybindings.is_empty());
    assert_eq!(s.shell, "", "DESK-30: empty means the platform default");
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
        "\"smooth_scroll\"",
        "\"cursor_trail\"",
        "\"shell\"",
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
fn unknown_keys_are_ignored_and_reported() {
    // Not silently (WR-05): the next save leaves them out, so the load says so and the file is kept as `.bad`.
    let (s, status) = Settings::from_json(r#"{"font_size": 15, "future_thing": {"a": [1, 2]}, "another": true}"#);
    assert_eq!(s.font_size, 15);
    let LoadStatus::Repaired(notes) = status else { panic!("expected Repaired, got {status:?}") };
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("future_thing")), "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("another")), "{notes:?}");
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

// ---- validation and clamping (task 3) -----------------------------------------------------------------

mod validation {
    use super::*;
    use serde_json::json;

    #[test]
    fn sanitise_clamps_every_numeric_setting_and_notes_each_change() {
        let mut s = Settings::default();
        s.font_size = 3;
        s.ui_scale = 500;
        s.scrollback_lines = 5;
        let notes = s.sanitise();
        assert_eq!((s.font_size, s.ui_scale, s.scrollback_lines), (8, 200, 100));
        assert_eq!(notes.len(), 3, "one note per change: {notes:?}");
        assert!(notes[0].contains("font_size"), "{notes:?}");
        assert!(notes[1].contains("ui_scale"), "{notes:?}");
        assert!(notes[2].contains("scrollback_lines"), "{notes:?}");

        s.font_size = 99;
        s.ui_scale = 10;
        s.scrollback_lines = 5_000_000;
        assert_eq!(s.sanitise().len(), 3);
        assert_eq!((s.font_size, s.ui_scale, s.scrollback_lines), (40, 75, 100_000));
    }

    #[test]
    fn sanitise_keeps_values_on_the_bounds_and_says_nothing() {
        let mut s = Settings::default();
        for (font, scale, lines) in [(8, 75, 100), (40, 200, 100_000), (14, 100, 10_000)] {
            s.font_size = font;
            s.ui_scale = scale;
            s.scrollback_lines = lines;
            assert!(s.sanitise().is_empty());
            assert_eq!((s.font_size, s.ui_scale, s.scrollback_lines), (font, scale, lines));
        }
    }

    #[test]
    fn sanitise_replaces_a_theme_name_that_cannot_be_a_theme_name() {
        for bad in ["", "../../etc/passwd", "a/b", "<script>", "x\u{0}y", &"n".repeat(65), " padded"] {
            let mut s = Settings::default();
            s.theme = bad.into();
            let notes = s.sanitise();
            assert_eq!(s.theme, "aubergine", "{bad:?}");
            assert_eq!(notes.len(), 1, "{bad:?}: {notes:?}");
        }
        let mut s = Settings::default();
        s.theme = "My Theme_2-b".into();
        assert!(s.sanitise().is_empty());
        assert_eq!(s.theme, "My Theme_2-b", "a well formed name is kept even when no theme has it");
    }

    #[test]
    fn sanitise_theme_resets_a_name_the_registry_does_not_know_and_keeps_known_ones() {
        let registry = ThemeRegistry::builtin();
        let mut s = Settings::default();
        s.theme = "Schiefer".into();
        assert_eq!(s.sanitise_theme(&registry), None);
        assert_eq!(s.theme, "Schiefer");
        s.theme = "Gone".into();
        let note = s.sanitise_theme(&registry).expect("a note");
        assert!(note.contains("Gone"), "{note}");
        assert_eq!(s.theme, "aubergine");
    }

    #[test]
    fn sanitise_bounds_the_key_binding_overrides() {
        let mut s = Settings::default();
        s.keybindings.insert("pane.close".into(), "Cmd+W".into());
        s.keybindings.insert("pane.unbound".into(), String::new());
        s.keybindings.insert("x".repeat(129), "Cmd+X".into());
        s.keybindings.insert("pane.long".into(), "y".repeat(129));
        s.keybindings.insert(String::new(), "Cmd+Z".into());
        let notes = s.sanitise();
        assert_eq!(notes.len(), 1, "one summary note: {notes:?}");
        let kept: Vec<&str> = s.keybindings.keys().map(String::as_str).collect();
        assert_eq!(kept, ["pane.close", "pane.unbound"], "an empty chord means unbound and is kept");

        let mut many = Settings::default();
        for i in 0..600 {
            many.keybindings.insert(format!("action.{i:04}"), "Cmd+K".into());
        }
        assert_eq!(many.sanitise().len(), 1);
        assert_eq!(many.keybindings.len(), 512);
        assert!(many.keybindings.contains_key("action.0000"), "the first in key order are kept");
    }

    // ---- the `shell` setting (DESK-30) ----

    #[test]
    fn shell_is_empty_by_default_and_written_as_an_empty_string() {
        assert_eq!(Settings::default().shell, "");
        assert!(Settings::default().to_json().contains("\"shell\": \"\""));
    }

    #[test]
    fn a_document_without_shell_loads_clean_and_equals_the_default() {
        let (s, status) = Settings::from_json(r#"{"font_size": 14}"#);
        assert_eq!(status, LoadStatus::Clean);
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn a_shell_program_loads_clean() {
        for program in ["pwsh.exe", r"C:\Program Files\PowerShell\7\pwsh.exe", "/usr/bin/fish", "/opt/my shell/zsh"] {
            let text = serde_json::json!({ "shell": program }).to_string();
            let (s, status) = Settings::from_json(&text);
            assert_eq!(status, LoadStatus::Clean, "{program}");
            assert_eq!(s.shell, program);
        }
    }

    #[test]
    fn a_shell_over_4096_bytes_is_reset_with_one_note_and_costs_no_other_key() {
        let long = "p".repeat(4097);
        let text = serde_json::json!({ "shell": long, "font_size": 17, "theme": "Schiefer" }).to_string();
        let (s, status) = Settings::from_json(&text);
        assert_eq!(s.shell, "");
        assert_eq!((s.font_size, s.theme.as_str()), (17, "Schiefer"), "the other keys keep their values");
        let LoadStatus::Repaired(notes) = status else { panic!("expected Repaired, got {status:?}") };
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("shell"), "{notes:?}");
        assert!(notes[0].len() < 300, "the note does not echo the value: {} bytes", notes[0].len());

        // The bound itself is allowed.
        let (s, status) = Settings::from_json(&serde_json::json!({ "shell": "p".repeat(4096) }).to_string());
        assert_eq!((s.shell.len(), status), (4096, LoadStatus::Clean));
    }

    #[test]
    fn a_shell_with_a_control_character_is_reset_with_one_note_and_costs_no_other_key() {
        for bad in ["pw\u{0}sh", "pwsh\n", "a\u{1b}[31mb", "x\ty", "\u{7f}"] {
            let text = serde_json::json!({ "shell": bad, "scrollback_lines": 500 }).to_string();
            let (s, status) = Settings::from_json(&text);
            assert_eq!(s.shell, "", "{bad:?}");
            assert_eq!(s.scrollback_lines, 500, "{bad:?}");
            let LoadStatus::Repaired(notes) = status else { panic!("{bad:?}: expected Repaired, got {status:?}") };
            assert_eq!(notes.len(), 1, "{bad:?}: {notes:?}");
        }
    }

    #[test]
    fn a_shell_of_only_blanks_counts_as_empty() {
        let (s, status) = Settings::from_json(r#"{"shell": "   \t "}"#);
        assert_eq!(s.shell, "");
        assert_eq!(status, LoadStatus::Clean, "blanks are the default, not a damage");
    }

    #[test]
    fn a_shell_that_is_not_a_string_is_a_wrong_typed_key_that_costs_only_itself() {
        let (s, status) = Settings::from_json(r#"{"shell": 5, "font_size": 16}"#);
        assert_eq!(s.shell, "");
        assert_eq!(s.font_size, 16);
        let LoadStatus::Repaired(notes) = status else { panic!("expected Repaired, got {status:?}") };
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("shell"), "{notes:?}");
    }

    #[test]
    fn set_value_bounds_the_shell_like_a_load_does() {
        let mut s = Settings::default();
        s.font_size = 17;
        s.set_value("shell", json!("p".repeat(5000))).unwrap();
        assert_eq!(s.shell, "", "too long is reset, not refused");
        assert_eq!(s.font_size, 17);
        s.set_value("shell", json!("a\u{0}b")).unwrap();
        assert_eq!(s.shell, "");
        s.set_value("shell", json!("/bin/sh")).unwrap();
        s.set_value("shell", json!("  ")).unwrap();
        assert_eq!(s.shell, "", "blanks put the default back");
    }

    #[test]
    fn from_json_clamps_and_reports_repaired() {
        let (s, status) = Settings::from_json(r#"{"font_size": 99, "ui_scale": 10, "scrollback_lines": 1}"#);
        assert_eq!((s.font_size, s.ui_scale, s.scrollback_lines), (40, 75, 100));
        let LoadStatus::Repaired(notes) = status else { panic!("expected Repaired, got {status:?}") };
        assert_eq!(notes.len(), 3, "{notes:?}");
    }

    #[test]
    fn numbers_beyond_the_field_type_are_clamped_not_rejected() {
        let (s, status) = Settings::from_json(r#"{"font_size": 100000, "ui_scale": -5, "scrollback_lines": 99999999999}"#);
        assert_eq!((s.font_size, s.ui_scale, s.scrollback_lines), (40, 75, 100_000));
        assert!(matches!(status, LoadStatus::Repaired(ref n) if n.len() == 3), "{status:?}");
    }

    #[test]
    fn one_wrong_typed_value_costs_only_that_value() {
        let (s, status) = Settings::from_json(
            r#"{"font_size": "big", "theme": "Schiefer", "option_as_alt": true, "cursor_blink": "sometimes"}"#,
        );
        assert_eq!(s.font_size, 14, "the default for the broken key");
        assert_eq!(s.theme, "Schiefer", "the other keys are kept");
        assert!(s.option_as_alt);
        assert_eq!(s.cursor_blink, Blink::System);
        let LoadStatus::Repaired(notes) = status else { panic!("expected Repaired, got {status:?}") };
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("font_size")));
        assert!(notes.iter().any(|n| n.contains("cursor_blink")));
    }

    #[test]
    fn a_wrong_typed_effect_costs_only_that_key_and_an_old_file_loads_unchanged() {
        // DESK-28: a file from before the two keys existed has neither and is clean; a wrong type is one repair.
        let (old, status) = Settings::from_json(r#"{"font_size": 15, "confirm_multiline_paste": false}"#);
        assert_eq!(status, LoadStatus::Clean);
        assert!(!old.smooth_scroll && !old.cursor_trail);
        assert_eq!(old.font_size, 15);

        let (s, status) = Settings::from_json(r#"{"smooth_scroll": "yes", "cursor_trail": true, "font_size": 16}"#);
        assert!(!s.smooth_scroll, "the default for the broken key");
        assert!(s.cursor_trail, "the other keys are kept");
        assert_eq!(s.font_size, 16);
        let LoadStatus::Repaired(notes) = status else { panic!("expected Repaired, got {status:?}") };
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("smooth_scroll"), "{notes:?}");
    }

    #[test]
    fn a_json_document_that_is_not_an_object_is_reset() {
        for text in ["[]", "null", "42", "\"theme\"", "true"] {
            let (s, status) = Settings::from_json(text);
            assert_eq!(s, Settings::default(), "{text}");
            assert!(matches!(status, LoadStatus::Reset(_)), "{text}: {status:?}");
        }
    }

    #[test]
    fn a_file_with_out_of_range_values_is_repaired_kept_and_left_alone() {
        let dir = scratch_dir("settings-range");
        let text = r#"{"font_size": 99}"#;
        std::fs::write(dir.join(SETTINGS_FILE), text).unwrap();
        let (s, status) = Settings::load(&dir);
        assert_eq!(s.font_size, 40);
        assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");
        assert_eq!(std::fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap(), text, "a load never writes the file");
        // The next save writes 40 over it: the copy is what keeps the 99 (WR-05).
        assert_eq!(std::fs::read_to_string(dir.join("settings.json.bad")).unwrap(), text);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_from_a_newer_build_survives_in_the_copy_when_the_next_save_drops_its_keys() {
        let dir = scratch_dir("settings-newer");
        let text = r#"{"font_size": 15, "future_thing": {"a": [1, 2]}}"#;
        std::fs::write(dir.join(SETTINGS_FILE), text).unwrap();
        let (s, status) = Settings::load(&dir);
        assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");

        s.save(&dir).unwrap();
        let saved = std::fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap();
        assert!(!saved.contains("future_thing"), "this build writes only what it knows");
        assert_eq!(std::fs::read_to_string(dir.join("settings.json.bad")).unwrap(), text);

        // What this build wrote is clean: loading it again leaves the copy of the original alone.
        let (_, again) = Settings::load(&dir);
        assert_eq!(again, LoadStatus::Clean);
        assert_eq!(std::fs::read_to_string(dir.join("settings.json.bad")).unwrap(), text);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_that_is_not_utf8_is_reset_and_kept() {
        let dir = scratch_dir("settings-utf8");
        let bytes = [0xFF, 0xFE, b'{', b'}'];
        std::fs::write(dir.join(SETTINGS_FILE), bytes).unwrap();
        let (s, status) = Settings::load(&dir);
        assert_eq!(s, Settings::default());
        assert!(matches!(status, LoadStatus::Reset(_)), "{status:?}");
        assert_eq!(std::fs::read(dir.join("settings.json.bad")).unwrap(), bytes);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_over_the_limit_is_reset_and_moved_aside_without_being_read() {
        let dir = scratch_dir("settings-huge");
        let big = vec![b' '; 1024 * 1024 + 1];
        std::fs::write(dir.join(SETTINGS_FILE), &big).unwrap();
        let (s, status) = Settings::load(&dir);
        assert_eq!(s, Settings::default());
        assert!(matches!(status, LoadStatus::Reset(_)), "{status:?}");
        assert_eq!(std::fs::metadata(dir.join("settings.json.bad")).unwrap().len(), big.len() as u64);
        assert!(!dir.join(SETTINGS_FILE).exists(), "moved, so the next save does not destroy it");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn set_value_clamps_and_succeeds() {
        let mut s = Settings::default();
        s.set_value("font_size", json!(99)).unwrap();
        assert_eq!(s.font_size, 40);
        s.set_value("ui_scale", json!(1)).unwrap();
        assert_eq!(s.ui_scale, 75);
        s.set_value("scrollback_lines", json!(12345)).unwrap();
        assert_eq!(s.scrollback_lines, 12_345);
        s.set_value("font_size", json!(100_000)).unwrap();
        assert_eq!(s.font_size, 40, "beyond the field type is clamped too");
    }

    #[test]
    fn set_value_accepts_every_key_with_the_right_type() {
        let mut s = Settings::default();
        s.set_value("theme", json!("Papier")).unwrap();
        s.set_value("option_as_alt", json!(true)).unwrap();
        s.set_value("cursor_blink", json!("off")).unwrap();
        s.set_value("motion", json!("reduce")).unwrap();
        s.set_value("contrast", json!("high")).unwrap();
        s.set_value("confirm_multiline_paste", json!(false)).unwrap();
        s.set_value("smooth_scroll", json!(true)).unwrap();
        s.set_value("cursor_trail", json!(true)).unwrap();
        s.set_value("shell", json!(r"C:\Program Files\PowerShell\7\pwsh.exe")).unwrap();
        s.set_value("keybindings", json!({"pane.split_right": "Cmd+D", "pane.close": ""})).unwrap();
        assert_eq!(s.theme, "Papier");
        assert!(s.option_as_alt);
        assert_eq!((s.cursor_blink, s.motion, s.contrast), (Blink::Off, Motion::Reduce, Contrast::High));
        assert!(!s.confirm_multiline_paste);
        assert!(s.smooth_scroll && s.cursor_trail);
        assert_eq!(s.shell, r"C:\Program Files\PowerShell\7\pwsh.exe");
        assert_eq!(s.keybindings.len(), 2);
    }

    #[test]
    fn set_value_errors_leave_the_settings_unchanged() {
        let mut s = Settings::default();
        s.font_size = 17;
        let before = s.clone();
        let wrong = [
            ("font_size", json!("x")),
            ("font_size", json!(14.5)),
            ("font_size", json!(null)),
            ("option_as_alt", json!("yes")),
            ("smooth_scroll", json!(1)),
            ("cursor_trail", json!("on")),
            ("shell", json!(5)),
            ("shell", json!(["pwsh"])),
            ("shell", json!(null)),
            ("cursor_blink", json!("sometimes")),
            ("keybindings", json!({"a": 5})),
            ("keybindings", json!(["a"])),
            ("theme", json!(7)),
            ("theme", json!("../../etc/passwd")),
            ("nope", json!(1)),
            ("version", json!(2)),
            ("", json!(1)),
        ];
        for (key, value) in wrong {
            let result = s.set_value(key, value.clone());
            assert!(result.is_err(), "{key} = {value} must be refused");
            assert_eq!(s, before, "{key} = {value} must not change anything");
        }
    }

    #[test]
    fn set_value_errors_name_the_key_without_echoing_a_huge_one() {
        let mut s = Settings::default();
        let err = s.set_value("nope", json!(1)).unwrap_err();
        assert!(err.contains("nope"), "{err}");
        let huge = "k".repeat(10_000);
        let err = s.set_value(&huge, json!(1)).unwrap_err();
        assert!(err.len() < 300, "the error stays short: {} bytes", err.len());
    }

    #[test]
    fn set_value_bounds_the_keybinding_table() {
        let mut s = Settings::default();
        let big: serde_json::Map<String, serde_json::Value> =
            (0..600).map(|i| (format!("action.{i:04}"), json!("Cmd+K"))).collect();
        s.set_value("keybindings", serde_json::Value::Object(big)).unwrap();
        assert_eq!(s.keybindings.len(), 512);
        assert!(s.to_json().len() < 1024 * 1024, "a saved document always stays loadable");
    }
}
