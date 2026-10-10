//! Tests for the themes: the three built-ins and their numeric contrast, the colour helpers, and the file format.

mod common;

use holzwave_core::theme::{ChromeTokens, Rgb, TerminalColors, Theme, ThemeKind, ThemeRegistry, blend, contrast};

fn c(text: &str) -> Rgb {
    Rgb::parse(text).unwrap_or_else(|| panic!("{text} is a colour"))
}

fn builtins() -> Vec<Theme> {
    let registry = ThemeRegistry::builtin();
    registry.list().iter().map(|info| registry.get(&info.name).clone()).collect()
}

// ---- the helpers -----------------------------------------------------------------------------------

#[test]
fn contrast_follows_the_wcag_formula() {
    let ink_on_ground = contrast(c("#F6EFE6"), c("#14091A"));
    assert!((ink_on_ground - 16.99).abs() < 0.05, "design.md says 16.99, got {ink_on_ground}");
    assert!((contrast(c("#000000"), c("#FFFFFF")) - 21.0).abs() < 1e-9);
    assert!((contrast(c("#808080"), c("#808080")) - 1.0).abs() < 1e-9);
    assert_eq!(contrast(c("#F6EFE6"), c("#14091A")), contrast(c("#14091A"), c("#F6EFE6")), "symmetric");
    // The cream token itself (design.md quotes 17.49 and 4.55 for "cream", which is a slightly lighter tint than
    // #FFE6F0: the formula gives 16.45 and 4.28 for the token).
    assert!((contrast(c("#14091A"), c("#FFE6F0")) - 16.45).abs() < 0.05);
    assert!((contrast(c("#C23B72"), c("#FFE6F0")) - 4.28).abs() < 0.05);
}

#[test]
fn blend_mixes_per_channel_and_rounds() {
    let top = c("#FF8FB8");
    let bottom = c("#14091A");
    assert_eq!(blend(top, 0.0, bottom), bottom);
    assert_eq!(blend(top, 1.0, bottom), top);
    // 0.35 * 255 + 0.65 * 20 = 102.25, 0.35 * 143 + 0.65 * 9 = 55.9, 0.35 * 184 + 0.65 * 26 = 81.3
    assert_eq!(blend(top, 0.35, bottom), Rgb::new(102, 56, 81));
    assert_eq!(blend(top, -1.0, bottom), bottom, "alpha is clamped");
    assert_eq!(blend(top, 7.0, bottom), top, "alpha is clamped");
    assert_eq!(blend(top, f32::NAN, bottom), bottom, "NaN is treated as transparent");
}

// ---- the set ---------------------------------------------------------------------------------------

#[test]
fn the_registry_lists_exactly_the_three_builtins_aubergine_first() {
    let list = ThemeRegistry::builtin().list();
    let names: Vec<&str> = list.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["aubergine", "Schiefer", "Papier"]);
    let kinds: Vec<ThemeKind> = list.iter().map(|t| t.kind).collect();
    assert_eq!(kinds, [ThemeKind::Dark, ThemeKind::Dark, ThemeKind::Light]);
    assert!(list.iter().all(|t| t.builtin));
}

#[test]
fn every_builtin_resolves_by_its_own_name_and_has_the_current_version() {
    let registry = ThemeRegistry::builtin();
    for info in registry.list() {
        let theme = registry.get(&info.name);
        assert_eq!(theme.name, info.name);
        assert_eq!(theme.kind, info.kind);
        assert_eq!(theme.version, 1);
    }
}

#[test]
fn the_window_ground_is_the_terminal_background_in_every_theme() {
    for t in builtins() {
        assert_eq!(t.chrome.ground, t.terminal.background, "{}", t.name);
    }
}

// ---- contrast of the palettes (design.md section 6 rule 4) ----------------------------------------------

#[test]
fn foreground_over_background_is_at_least_7_to_1() {
    for t in builtins() {
        let ratio = contrast(t.terminal.foreground, t.terminal.background);
        assert!(ratio >= 7.0, "{}: foreground {ratio:.2}", t.name);
    }
}

#[test]
fn ansi_colours_1_to_7_and_9_to_15_are_at_least_4_5_to_1_on_the_background() {
    for t in builtins() {
        for i in (1..=7).chain(9..=15) {
            let ratio = contrast(t.terminal.ansi[i], t.terminal.background);
            assert!(ratio >= 4.5, "{}: ANSI {i} {} is {ratio:.2}", t.name, t.terminal.ansi[i]);
        }
    }
}

#[test]
fn bright_black_and_the_cursor_are_at_least_3_to_1_on_the_background() {
    for t in builtins() {
        let bright_black = contrast(t.terminal.ansi[8], t.terminal.background);
        assert!(bright_black >= 3.0, "{}: ANSI 8 {} is {bright_black:.2}", t.name, t.terminal.ansi[8]);
        let cursor = contrast(t.terminal.cursor, t.terminal.background);
        assert!(cursor >= 3.0, "{}: cursor {cursor:.2}", t.name);
    }
}

#[test]
fn chrome_text_meets_its_ratios_on_the_ground() {
    for t in builtins() {
        let ink = contrast(t.chrome.ink, t.chrome.ground);
        assert!(ink >= 7.0, "{}: ink {ink:.2}", t.name);
        let ink_3 = contrast(t.chrome.ink_3, t.chrome.ground);
        assert!(ink_3 >= 4.5, "{}: ink_3 {ink_3:.2}", t.name);
    }
}

#[test]
fn selected_text_is_at_least_3_to_1_on_the_blended_selection() {
    for t in builtins() {
        let selected = blend(t.terminal.selection, t.terminal.selection_alpha, t.terminal.background);
        let ratio = contrast(t.terminal.foreground, selected);
        assert!(ratio >= 3.0, "{}: foreground on selection {ratio:.2}", t.name);
        for i in (1..=7).chain(9..=15) {
            let ratio = contrast(t.terminal.ansi[i], selected);
            assert!(ratio >= 3.0, "{}: ANSI {i} {} on selection {selected} is {ratio:.2}", t.name, t.terminal.ansi[i]);
        }
    }
}

#[test]
fn secondary_ink_is_the_primary_ink_at_70_percent_over_the_ground() {
    for t in builtins() {
        assert_eq!(t.chrome.ink_2, blend(t.chrome.ink, 0.70, t.chrome.ground), "{}", t.name);
    }
}

#[test]
fn light_themes_are_light_and_dark_themes_are_dark() {
    for t in builtins() {
        let to_white = contrast(t.terminal.background, c("#FFFFFF"));
        match t.kind {
            ThemeKind::Light => assert!(to_white < 1.2, "{}: background is {to_white:.2} from white", t.name),
            ThemeKind::Dark => assert!(to_white > 10.0, "{}: background is {to_white:.2} from white", t.name),
        }
    }
}

// ---- the palettes follow design.md and D-20 ----------------------------------------------------------

#[test]
fn aubergine_has_the_design_tokens() {
    let t = Theme::aubergine();
    assert_eq!(t.terminal.background, c("#14091A"));
    assert_eq!(t.terminal.foreground, c("#F6EFE6"));
    assert_eq!(t.terminal.cursor, c("#FF8FB8"));
    assert_eq!(t.terminal.selection, c("#FF8FB8"));
    assert!((t.terminal.selection_alpha - 0.35).abs() < 1e-6);
    assert_eq!(t.chrome.ground_2, c("#1D0F26"));
    assert_eq!(t.chrome.pane, c("#1B0E20"));
    assert_eq!(t.chrome.edge, c("#3A2144"));
    assert_eq!(t.chrome.ink_3, c("#9C8EA0"));
    assert_eq!(t.chrome.rose, c("#FF8FB8"));
    assert_eq!(t.chrome.rose_deep, c("#C23B72"));
    let status = [t.chrome.ok, t.chrome.warn, t.chrome.danger, t.chrome.info];
    assert_eq!(status, [c("#8CC271"), c("#E8C15C"), c("#EC8272"), c("#8FB8D8")]);
    // The normal palette is the design table; only bright black differs from it.
    let normal = ["#1B0E20", "#EC8272", "#8CC271", "#E8C15C", "#6F9BD0", "#FF8FB8", "#7FD1C7", "#D9C9D6"];
    for (i, want) in normal.iter().enumerate() {
        assert_eq!(t.terminal.ansi[i], c(want), "ANSI {i}");
    }
    let bright = ["#FFA396", "#AEDD93", "#F5D883", "#93B8E6", "#FFB3D1", "#A4E6DD", "#F6EFE6"];
    for (i, want) in bright.iter().enumerate() {
        assert_eq!(t.terminal.ansi[9 + i], c(want), "ANSI {}", 9 + i);
    }
    assert_ne!(t.terminal.ansi[8], c("#4A3652"), "the design value fails 3:1 (1.79:1)");
}

#[test]
fn schiefer_is_a_neutral_dark_grey_with_the_rose_accent() {
    let t = Theme::schiefer();
    assert_eq!(t.name, "Schiefer");
    assert_eq!(t.kind, ThemeKind::Dark);
    let ground = t.chrome.ground;
    // Neutral: the three channels are within a few steps of each other (the planned edge #34363B is 7 apart).
    let spread = |x: Rgb| x.r.max(x.g).max(x.b) - x.r.min(x.g).min(x.b);
    assert!(spread(ground) <= 8, "ground {ground} is not neutral");
    assert!(spread(t.chrome.edge) <= 8, "edge {} is not neutral", t.chrome.edge);
    assert!((0x18..=0x20).contains(&ground.r), "ground {ground} should be near #1B1C1F");
    assert_eq!(t.terminal.cursor, c("#FF8FB8"), "same rose accent");
    assert_eq!(t.chrome.rose, c("#FF8FB8"));
    assert_eq!(t.chrome.rose_deep, c("#C23B72"));
}

#[test]
fn papier_is_light_with_aubergine_ink_and_rose_deep_accent() {
    let t = Theme::papier();
    assert_eq!(t.name, "Papier");
    assert_eq!(t.kind, ThemeKind::Light);
    assert_eq!(t.terminal.foreground, c("#14091A"), "aubergine ink");
    assert_eq!(t.chrome.ink, c("#14091A"));
    assert_eq!(t.terminal.cursor, c("#C23B72"), "rose-deep: rose #FF8FB8 fails on a light ground");
    assert_eq!(t.chrome.rose, c("#C23B72"));
    // The ground is the cream token lightened towards white: red stays at full, green and blue move up.
    let (cream, ground) = (c("#FFE6F0"), t.chrome.ground);
    assert_eq!(ground.r, 0xFF);
    assert!(ground.g > cream.g && ground.b > cream.b, "ground {ground} is lighter than cream");
}

// ---- the file format -----------------------------------------------------------------------------------

#[test]
fn a_theme_survives_a_json_round_trip_unchanged() {
    for t in builtins() {
        let text = serde_json::to_string_pretty(&t).unwrap();
        let back: Theme = serde_json::from_str(&text).unwrap();
        assert_eq!(back, t, "{}", t.name);
    }
}

#[test]
fn a_theme_is_written_with_the_wire_shape_the_webview_reads() {
    let value = serde_json::to_value(Theme::aubergine()).unwrap();
    assert_eq!(value["name"], "aubergine");
    assert_eq!(value["kind"], "dark");
    assert_eq!(value["version"], 1);
    assert_eq!(value["terminal"]["background"], "#14091A");
    assert_eq!(value["terminal"]["ansi"].as_array().map(Vec::len), Some(16));
    assert!(value["terminal"]["selection_alpha"].is_number());
    assert_eq!(value["chrome"]["rose_deep"], "#C23B72");
    for key in ["ground", "ground_2", "pane", "edge", "ink", "ink_2", "ink_3", "rose", "rose_deep", "ok", "warn", "danger", "info"] {
        assert!(value["chrome"][key].is_string(), "chrome.{key}");
    }
}

#[test]
fn engine_theme_carries_the_terminal_colours_unchanged() {
    for t in builtins() {
        let e = t.engine_theme();
        let same = |x: Rgb, y: holzwave_core::Color| (x.r, x.g, x.b) == (y.r, y.g, y.b);
        assert!(same(t.terminal.foreground, e.fg), "{}", t.name);
        assert!(same(t.terminal.background, e.bg), "{}", t.name);
        assert!(same(t.terminal.cursor, e.cursor), "{}", t.name);
        for i in 0..16 {
            assert!(same(t.terminal.ansi[i], e.ansi[i]), "{} ANSI {i}", t.name);
        }
    }
}

#[test]
fn the_data_types_are_nameable_for_the_wire_layer() {
    // The webview shape is built from these types; this keeps them public.
    let _ = (std::mem::size_of::<TerminalColors>(), std::mem::size_of::<ChromeTokens>());
}

// ---- user themes (task 3) -----------------------------------------------------------------------------------

mod user_themes {
    use super::*;
    use std::fs;
    use std::path::Path;

    use super::common::scratch_dir;
    use holzwave_core::theme::valid_theme_name;

    /// A valid theme document called `name`, as a user would write it.
    fn theme_json(name: &str) -> String {
        let mut t = Theme::papier();
        t.name = name.into();
        serde_json::to_string_pretty(&t).unwrap()
    }

    fn put(dir: &Path, file: &str, text: &str) {
        let themes = dir.join("themes");
        fs::create_dir_all(&themes).unwrap();
        fs::write(themes.join(file), text).unwrap();
    }

    fn names(registry: &ThemeRegistry) -> Vec<String> {
        registry.list().into_iter().map(|t| t.name).collect()
    }

    #[test]
    fn theme_names_are_letters_digits_space_dash_underscore_up_to_64() {
        for good in ["a", "Solarized Dark", "dark-2", "my_theme", "Größe", &"n".repeat(64)] {
            assert!(valid_theme_name(good), "{good:?}");
        }
        for bad in ["", " ", " x", "x ", "a/b", "a\\b", "..", "a.b", "<b>", "a\nb", "a\u{0}b", &"n".repeat(65)] {
            assert!(!valid_theme_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn validate_checks_version_name_and_selection_opacity() {
        assert!(Theme::aubergine().validate().is_ok());
        let mut t = Theme::papier();
        t.version = 2;
        assert!(t.validate().is_err(), "unknown version");
        let mut t = Theme::papier();
        t.name = "../x".into();
        assert!(t.validate().is_err(), "bad name");
        for alpha in [0.0, 0.04, 0.91, 1.0, f32::NAN, f32::INFINITY, -0.5] {
            let mut t = Theme::papier();
            t.terminal.selection_alpha = alpha;
            assert!(t.validate().is_err(), "alpha {alpha}");
        }
        for alpha in [0.05, 0.5, 0.9] {
            let mut t = Theme::papier();
            t.terminal.selection_alpha = alpha;
            assert!(t.validate().is_ok(), "alpha {alpha}");
        }
    }

    #[test]
    fn no_themes_directory_is_the_builtins_and_no_warnings() {
        let dir = scratch_dir("themes-none");
        let (registry, warnings) = ThemeRegistry::load(&dir);
        assert_eq!(names(&registry), ["aubergine", "Schiefer", "Papier"]);
        assert!(warnings.is_empty(), "{warnings:?}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_valid_user_theme_is_listed_after_the_builtins_and_resolves() {
        let dir = scratch_dir("themes-valid");
        put(&dir, "solar.json", &theme_json("Solar Flare"));
        let (registry, warnings) = ThemeRegistry::load(&dir);
        assert!(warnings.is_empty(), "{warnings:?}");
        let list = registry.list();
        assert_eq!(list.len(), 4);
        assert_eq!(list[3].name, "Solar Flare");
        assert!(!list[3].builtin);
        assert!(list[..3].iter().all(|t| t.builtin));
        assert_eq!(registry.get("Solar Flare").terminal.background, Theme::papier().terminal.background);
        assert!(registry.contains("Solar Flare"));
        assert!(!registry.contains("Nobody"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_theme_name_comes_from_the_file_content_and_the_file_name_is_irrelevant() {
        let dir = scratch_dir("themes-names");
        put(&dir, "whatever-it-is-called.json", &theme_json("Inside"));
        let (registry, _) = ThemeRegistry::load(&dir);
        assert!(registry.contains("Inside"));
        assert!(!registry.contains("whatever-it-is-called"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn user_themes_are_listed_in_file_name_order() {
        let dir = scratch_dir("themes-order");
        put(&dir, "c.json", &theme_json("Third"));
        put(&dir, "a.json", &theme_json("First"));
        put(&dir, "b.json", &theme_json("Second"));
        let (registry, _) = ThemeRegistry::load(&dir);
        assert_eq!(names(&registry)[3..], ["First", "Second", "Third"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hostile_themes_are_skipped_with_one_warning_each() {
        let dir = scratch_dir("themes-hostile");
        let good = theme_json("Good One");
        let with = |from: &str, to: &str| good.replace(from, to);
        let cases: Vec<(&str, String)> = vec![
            ("bad-colour.json", with("#FFF3F8", "red")),
            ("css-injection.json", with("#FFF3F8", "url(javascript:alert(1))")),
            ("short-colour.json", with("#FFF3F8", "#FFF")),
            ("long-name.json", with("Good One", &"n".repeat(65))),
            ("slash-name.json", with("Good One", "../../etc/passwd")),
            ("html-name.json", with("Good One", "<img src=x>")),
            ("empty-name.json", with("Good One", "")),
            ("alpha-low.json", with("0.25", "0.01")),
            ("alpha-high.json", with("0.25", "0.95")),
            ("version.json", with("\"version\": 1", "\"version\": 7")),
            ("not-json.json", "{ nope".into()),
            ("array.json", "[1, 2, 3]".into()),
            ("empty.json", String::new()),
            ("shadow.json", with("Good One", "aubergine")),
            ("shadow-case.json", with("Good One", "SCHIEFER")),
        ];
        for (file, text) in &cases {
            put(&dir, file, text);
        }
        put(&dir, "zz-good.json", &good);
        let (registry, warnings) = ThemeRegistry::load(&dir);
        assert_eq!(names(&registry), ["aubergine", "Schiefer", "Papier", "Good One"]);
        assert_eq!(warnings.len(), cases.len(), "one warning per skipped file: {warnings:#?}");
        for (file, _) in &cases {
            assert!(warnings.iter().any(|w| w.contains(file)), "a warning names {file}: {warnings:#?}");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_wrong_number_of_ansi_colours_is_skipped() {
        let dir = scratch_dir("themes-ansi");
        let mut value: serde_json::Value = serde_json::from_str(&theme_json("Short Row")).unwrap();
        value["terminal"]["ansi"].as_array_mut().unwrap().pop();
        put(&dir, "short.json", &value.to_string());
        let (registry, warnings) = ThemeRegistry::load(&dir);
        assert_eq!(registry.list().len(), 3);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_version_means_the_current_one_and_unknown_keys_are_ignored() {
        let dir = scratch_dir("themes-lenient");
        let mut value: serde_json::Value = serde_json::from_str(&theme_json("Hand Made")).unwrap();
        value.as_object_mut().unwrap().remove("version");
        value["comment"] = "my own notes".into();
        put(&dir, "hand.json", &value.to_string());
        let (registry, warnings) = ThemeRegistry::load(&dir);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(registry.get("Hand Made").version, 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_second_theme_with_the_same_name_is_skipped_even_in_another_case() {
        let dir = scratch_dir("themes-dup");
        put(&dir, "a.json", &theme_json("Twin"));
        put(&dir, "b.json", &theme_json("twin"));
        let (registry, warnings) = ThemeRegistry::load(&dir);
        assert_eq!(names(&registry)[3..], ["Twin"]);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("b.json"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_over_64_kib_is_skipped_unread() {
        let dir = scratch_dir("themes-big");
        let padding = " ".repeat(64 * 1024);
        put(&dir, "big.json", &format!("{}{padding}", theme_json("Big One")));
        let (registry, warnings) = ThemeRegistry::load(&dir);
        assert_eq!(registry.list().len(), 3);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn at_most_64_files_are_read_and_the_rest_cause_one_warning() {
        let dir = scratch_dir("themes-many");
        for i in 0..70 {
            put(&dir, &format!("t{i:02}.json"), &theme_json(&format!("Theme {i:02}")));
        }
        let (registry, warnings) = ThemeRegistry::load(&dir);
        assert_eq!(registry.list().len(), 3 + 64);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(registry.contains("Theme 00") && registry.contains("Theme 63"));
        assert!(!registry.contains("Theme 64"), "files are taken in name order");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn other_files_are_ignored_without_a_warning() {
        let dir = scratch_dir("themes-other");
        put(&dir, "README.txt", "notes");
        put(&dir, ".DS_Store", "x");
        put(&dir, "theme.json.bak", &theme_json("Backup"));
        let (registry, warnings) = ThemeRegistry::load(&dir);
        assert_eq!(registry.list().len(), 3);
        assert!(warnings.is_empty(), "{warnings:?}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_directory_named_like_a_theme_is_skipped_with_a_warning() {
        let dir = scratch_dir("themes-dir");
        fs::create_dir_all(dir.join("themes").join("folder.json")).unwrap();
        let (registry, warnings) = ThemeRegistry::load(&dir);
        assert_eq!(registry.list().len(), 3);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_is_not_followed() {
        let dir = scratch_dir("themes-link");
        let elsewhere = scratch_dir("themes-link-target");
        fs::write(elsewhere.join("real.json"), theme_json("Linked")).unwrap();
        fs::create_dir_all(dir.join("themes")).unwrap();
        std::os::unix::fs::symlink(elsewhere.join("real.json"), dir.join("themes").join("link.json")).unwrap();
        let (registry, warnings) = ThemeRegistry::load(&dir);
        assert!(!registry.contains("Linked"));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(elsewhere).unwrap();
    }

    #[test]
    fn the_themes_path_being_a_file_is_a_warning_not_a_failure() {
        let dir = scratch_dir("themes-file");
        fs::write(dir.join("themes"), b"not a directory").unwrap();
        let (registry, warnings) = ThemeRegistry::load(&dir);
        assert_eq!(registry.list().len(), 3);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_unknown_name_still_falls_back_to_aubergine_with_user_themes_present() {
        let dir = scratch_dir("themes-fallback");
        put(&dir, "a.json", &theme_json("Mine"));
        let (registry, _) = ThemeRegistry::load(&dir);
        assert_eq!(registry.get("Mine").name, "Mine");
        assert_eq!(registry.get("Nobody").name, "aubergine");
        assert_eq!(registry.get("schiefer").name, "Schiefer", "a hand-typed lower case name still finds it");
        fs::remove_dir_all(dir).unwrap();
    }
}
