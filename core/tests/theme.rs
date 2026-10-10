//! Tests for the themes: the three built-ins and their numeric contrast, the colour helpers, and the file format.

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
