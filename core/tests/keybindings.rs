//! Tests for key bindings: the canonical chord form, the default action table, and how overrides from settings.json
//! resolve into the table the matcher and the keybinding editor read.

use std::collections::{BTreeMap, HashSet};

use holzwave_core::keybindings::{ActionInfo, Chord, Keybindings, actions, actions_for};

/// The three default tables, by the OS name `std::env::consts::OS` and the app's `app_info` use.
const OSES: [&str; 3] = ["macos", "windows", "linux"];
/// The table the macOS literals of the tests below are written against; the override policy is the same on all three.
const MAC: &str = "macos";

/// `text` parsed and printed back in canonical form.
fn canon(text: &str) -> Result<String, String> {
    Chord::parse(text).map(|c| c.to_string())
}

fn overrides(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(a, c)| ((*a).to_string(), (*c).to_string())).collect()
}

fn chord(text: &str) -> Chord {
    Chord::parse(text).unwrap_or_else(|e| panic!("{text:?} should parse: {e}"))
}

// ---- the canonical form ------------------------------------------------------------------------------

#[test]
fn spellings_of_one_chord_print_the_same_canonical_form() {
    assert_eq!(canon("cmd+shift+d"), Ok("Cmd+Shift+D".to_string()));
    assert_eq!(canon("Command+Shift+D"), Ok("Cmd+Shift+D".to_string()));
    assert_eq!(canon("CMD+SHIFT+d"), Ok("Cmd+Shift+D".to_string()));
}

#[test]
fn modifiers_print_in_the_fixed_order_whatever_order_they_were_written_in() {
    assert_eq!(canon("Shift+Alt+Ctrl+Cmd+Up"), Ok("Cmd+Ctrl+Alt+Shift+Up".to_string()));
    assert_eq!(canon("alt+cmd+1"), Ok("Cmd+Alt+1".to_string()));
}

#[test]
fn option_meta_and_super_are_accepted_aliases() {
    assert_eq!(canon("Option+Shift+X"), Ok("Alt+Shift+X".to_string()));
    assert_eq!(canon("Meta+K"), Ok("Cmd+K".to_string()));
    assert_eq!(canon("Super+K"), Ok("Cmd+K".to_string()));
    assert_eq!(canon("Control+Alt+Left"), Ok("Ctrl+Alt+Left".to_string()));
}

#[test]
fn letters_print_upper_case_and_punctuation_prints_as_itself() {
    assert_eq!(canon("cmd+x"), Ok("Cmd+X".to_string()));
    assert_eq!(canon("Cmd+]"), Ok("Cmd+]".to_string()));
    assert_eq!(canon("Cmd+,"), Ok("Cmd+,".to_string()));
    assert_eq!(canon("Cmd+="), Ok("Cmd+=".to_string()));
    assert_eq!(canon("Cmd+-"), Ok("Cmd+-".to_string()));
}

#[test]
fn named_keys_are_case_insensitive_and_print_in_their_canonical_spelling() {
    assert_eq!(canon("cmd+space"), Ok("Cmd+Space".to_string()));
    assert_eq!(canon("cmd+RETURN"), Ok("Cmd+Enter".to_string()));
    assert_eq!(canon("cmd+esc"), Ok("Cmd+Escape".to_string()));
    assert_eq!(canon("cmd+pagedown"), Ok("Cmd+PageDown".to_string()));
    assert_eq!(canon("cmd+f12"), Ok("Cmd+F12".to_string()));
}

#[test]
fn parse_exposes_modifiers_and_key_as_fields() {
    let c = chord("Cmd+Alt+1");
    assert!(c.cmd && c.alt && !c.ctrl && !c.shift);
    assert_eq!(c.key, "1");
}

// ---- the default table ----------------------------------------------------------------------------------

/// The action table of `os`, after checking that it is the full set: a loop over an empty table would pass vacuously.
fn all_actions(os: &str) -> &'static [ActionInfo] {
    assert_eq!(actions_for(os).len(), 15 + 9 + 9 + 3 + 6 + 3 + 2 + 2, "the {os} table holds 49 actions");
    actions_for(os)
}

/// The default chord of `action` in the table of `os`.
fn default_of(os: &str, action: &str) -> Option<&'static str> {
    actions_for(os).iter().find(|a| a.id == action).and_then(|a| a.default)
}

#[test]
fn the_default_table_holds_every_action_of_the_decisions() {
    let expected: Vec<(String, String)> = [
        ("pane.split_right", "Cmd+D"),
        ("pane.split_down", "Cmd+Shift+D"),
        ("pane.close", "Cmd+W"),
        ("pane.focus_next", "Cmd+]"),
        ("pane.focus_prev", "Cmd+["),
        ("tab.new", "Cmd+T"),
        ("workspace.new", "Cmd+Shift+N"),
        ("palette.open", "Cmd+K"),
        ("search.open", "Cmd+F"),
        ("copy_mode.enter", "Cmd+Shift+X"),
        ("hints.open", "Cmd+Shift+Space"),
        ("font.increase", "Cmd+="),
        ("font.decrease", "Cmd+-"),
        ("font.reset", "Cmd+0"),
        ("settings.open", "Cmd+,"),
    ]
    .iter()
    .map(|(a, c)| ((*a).to_string(), (*c).to_string()))
    .chain((1..=9).map(|n| (format!("tab.select_{n}"), format!("Cmd+{n}"))))
    .chain((1..=9).map(|n| (format!("workspace.select_{n}"), format!("Cmd+Alt+{n}"))))
    .collect();
    for (action, chord) in &expected {
        assert_eq!(default_of(MAC, action), Some(chord.as_str()), "default of {action}");
    }
}

#[test]
fn closing_a_pane_is_cmd_w_not_cmd_shift_w() {
    // D-14: Cmd+W closes the pane and "Close Window" is gone from the menu.
    assert_eq!(default_of(MAC, "pane.close"), Some("Cmd+W"));
}

#[test]
fn every_action_has_a_title() {
    for os in OSES {
        for a in all_actions(os) {
            assert!(!a.title.trim().is_empty(), "{} has no title on {os}", a.id);
        }
    }
}

#[test]
fn default_chords_are_written_in_canonical_form() {
    for os in OSES {
        for a in all_actions(os) {
            if let Some(d) = a.default {
                assert_eq!(canon(d), Ok(d.to_string()), "default of {} on {os} is not canonical", a.id);
            }
        }
    }
}

#[test]
fn defaults_resolve_without_warnings_and_match_the_table() {
    for os in OSES {
        let (kb, warnings) = Keybindings::resolve_for(os, &BTreeMap::new());
        assert!(warnings.is_empty(), "{os}: {warnings:?}");
        for a in all_actions(os) {
            let want = a.default.map(chord);
            assert_eq!(kb.chord_of(a.id), want.as_ref(), "chord of {} on {os}", a.id);
        }
        assert_eq!(Keybindings::default_for(os).table(), kb.table(), "{os}");
    }
}

#[test]
fn the_three_tables_hold_the_same_actions_with_the_same_titles_in_the_same_order() {
    let mac = all_actions("macos");
    for os in ["windows", "linux"] {
        let other = all_actions(os);
        for (m, o) in mac.iter().zip(other) {
            assert_eq!((m.id, m.title), (o.id, o.title), "{os}");
        }
    }
}

#[test]
fn a_name_that_is_neither_macos_nor_windows_is_the_linux_table() {
    for name in ["linux", "freebsd", "", "android", "MacOS"] {
        assert_eq!(actions_for(name), all_actions("linux"), "{name:?}");
    }
}

#[test]
fn the_host_functions_use_the_table_of_the_os_the_build_runs_on() {
    let host = std::env::consts::OS;
    assert_eq!(actions(), all_actions(host));
    assert_eq!(Keybindings::default().table(), Keybindings::default_for(host).table());
    let o = overrides(&[("tab.new", "Ctrl+Alt+Shift+Y"), ("nonsense", "Ctrl+Alt+Q")]);
    let (host_table, host_warnings) = Keybindings::resolve(&o);
    let (named_table, named_warnings) = Keybindings::resolve_for(host, &o);
    assert_eq!(host_table.table(), named_table.table());
    assert_eq!(host_warnings, named_warnings);
}

#[test]
fn the_macos_table_still_has_its_ten_unbound_actions_on_every_platform() {
    for os in OSES {
        for id in [
            "tab.rename",
            "pane.rename",
            "workspace.rename",
            "pane.swap_next",
            "pane.move_next_tab",
            "pane.move_prev_tab",
            "pane.move_to_new_window",
            "tab.move_to_new_window",
            "tab.close",
            "workspace.close",
        ] {
            assert_eq!(default_of(os, id), None, "{id} on {os}");
        }
    }
}

#[test]
fn windows_and_linux_use_ctrl_shift_and_the_decided_chords() {
    let mut expected: Vec<(String, String)> = [
        ("pane.split_right", "Ctrl+Shift+D"),
        ("pane.split_down", "Ctrl+Shift+O"),
        ("pane.close", "Ctrl+Shift+W"),
        ("pane.focus_next", "Ctrl+Shift+]"),
        ("pane.focus_prev", "Ctrl+Shift+["),
        ("tab.new", "Ctrl+Shift+T"),
        ("workspace.new", "Ctrl+Shift+N"),
        ("palette.open", "Ctrl+Shift+P"),
        ("search.open", "Ctrl+Shift+F"),
        ("copy_mode.enter", "Ctrl+Shift+X"),
        ("hints.open", "Ctrl+Shift+Space"),
        ("font.increase", "Ctrl+Shift+="),
        ("font.decrease", "Ctrl+Shift+-"),
        ("font.reset", "Ctrl+Shift+0"),
        ("settings.open", "Ctrl+Shift+,"),
    ]
    .iter()
    .map(|(a, c)| ((*a).to_string(), (*c).to_string()))
    .collect();
    expected.extend((1..=9).map(|n| (format!("tab.select_{n}"), format!("Alt+Shift+{n}"))));
    expected.extend((1..=9).map(|n| (format!("workspace.select_{n}"), format!("Ctrl+Shift+{n}"))));
    for os in ["windows", "linux"] {
        for (action, chord) in &expected {
            assert_eq!(default_of(os, action), Some(chord.as_str()), "default of {action} on {os}");
        }
    }
}

#[test]
fn no_windows_or_linux_default_takes_the_super_key_or_plain_typing() {
    for os in ["windows", "linux"] {
        for a in all_actions(os) {
            let Some(d) = a.default else { continue };
            let c = chord(d);
            assert!(!c.cmd, "{d} of {} on {os} uses the Super key", a.id);
            // A single Ctrl or Alt on a printable key belongs to the shell (Ctrl+C is SIGINT).
            if c.modifier_count() == 1 {
                assert!(NAVIGATION.contains(&c.key.as_str()), "{d} of {} on {os} is a plain Ctrl or Alt key", a.id);
            }
            // Ctrl+Alt is AltGr on Windows: a printable key with both would take typing away.
            if os == "windows" && c.ctrl && c.alt {
                assert!(!is_printable(&c.key), "{d} of {} would take AltGr typing on Windows", a.id);
            }
        }
    }
}

/// The keys on which a single Ctrl or Alt is allowed, as in the bindability policy.
const NAVIGATION: [&str; 8] = ["Left", "Right", "Up", "Down", "Home", "End", "Backspace", "Delete"];

fn is_printable(key: &str) -> bool {
    key.chars().count() == 1 && key != "Space"
}

#[test]
fn the_override_policy_is_the_same_on_every_table() {
    // The displaced default is that platform's own chord; the policy itself does not change.
    for (os, default_chord) in [("macos", "Cmd+K"), ("windows", "Ctrl+Shift+P"), ("linux", "Ctrl+Shift+P")] {
        let (kb, warnings) = Keybindings::resolve_for(os, &overrides(&[("tab.new", default_chord)]));
        assert_eq!(kb.action_of(&chord(default_chord)), Some("tab.new"), "{os}");
        assert_eq!(kb.chord_of("palette.open"), None, "{os}: the displaced default is unbound");
        assert_eq!(warnings.len(), 1, "{os}: {warnings:?}");
    }
    // A macOS default is just a free chord on Windows.
    let (kb, warnings) = Keybindings::resolve_for("windows", &overrides(&[("tab.new", "Cmd+K")]));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(kb.chord_of("palette.open"), Some(&chord("Ctrl+Shift+P")));
}

// ---- one override travels from the settings map to the resolved table ---------------------------------

#[test]
fn an_override_changes_exactly_one_action_and_is_reported_as_not_default() {
    let (kb, warnings) = Keybindings::resolve_for(MAC, &overrides(&[("pane.split_right", "Cmd+Shift+Enter")]));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(kb.chord_of("pane.split_right").map(ToString::to_string), Some("Cmd+Shift+Enter".to_string()));

    let table = kb.table();
    assert_eq!(table.len(), actions_for(MAC).len(), "one row per action, in the table order");
    for (row, info) in table.iter().zip(actions_for(MAC)) {
        assert_eq!(row.action, info.id);
        assert_eq!(row.title, info.title);
        if row.action == "pane.split_right" {
            assert_eq!(row.chord.as_deref(), Some("Cmd+Shift+Enter"));
            assert!(!row.is_default);
        } else {
            assert_eq!(row.chord.as_deref(), info.default, "{} keeps its default", row.action);
            assert!(row.is_default, "{} is still default", row.action);
        }
    }
}

#[test]
fn an_override_is_matched_by_chord_and_the_old_default_no_longer_is() {
    let (kb, _) = Keybindings::resolve_for(MAC, &overrides(&[("pane.split_right", "Cmd+Shift+Enter")]));
    assert_eq!(kb.action_of(&chord("Cmd+Shift+Enter")), Some("pane.split_right"));
    assert_eq!(kb.action_of(&chord("Cmd+D")), None, "the default chord is free after the override");
}

#[test]
fn action_of_finds_every_default_chord_and_the_ids_are_unique() {
    for os in OSES {
        let (kb, _) = Keybindings::resolve_for(os, &BTreeMap::new());
        let mut ids = HashSet::new();
        for a in all_actions(os) {
            assert!(ids.insert(a.id), "duplicate action id {} on {os}", a.id);
            if let Some(d) = a.default {
                assert_eq!(kb.action_of(&chord(d)), Some(a.id), "chord {d} on {os}");
            }
        }
    }
}

#[test]
fn an_override_spelled_loosely_is_stored_in_canonical_form() {
    let (kb, warnings) = Keybindings::resolve_for(MAC, &overrides(&[("tab.new", "command+alt+t")]));
    assert!(warnings.is_empty(), "{warnings:?}");
    let row = kb.table().into_iter().find(|r| r.action == "tab.new").expect("tab.new row");
    assert_eq!(row.chord.as_deref(), Some("Cmd+Alt+T"));
}

#[test]
fn an_override_equal_to_the_default_counts_as_default() {
    let (kb, _) = Keybindings::resolve_for(MAC, &overrides(&[("pane.split_right", "cmd+d")]));
    let row = kb.table().into_iter().find(|r| r.action == "pane.split_right").expect("row");
    assert_eq!(row.chord.as_deref(), Some("Cmd+D"));
    assert!(row.is_default);
}

// ---- the bindability policy ---------------------------------------------------------------------------

#[test]
fn a_chord_is_bindable_with_cmd_or_with_two_modifiers_including_ctrl_or_alt() {
    for text in [
        "Cmd+D",
        "Cmd+Shift+D",
        "Cmd+Alt+1",
        "Cmd+Ctrl+Space",
        "Cmd+Tab",
        "Cmd+F12",
        "Ctrl+Alt+D",
        "Ctrl+Shift+PageDown",
        "Alt+Shift+X",
        "Ctrl+Alt+Shift+Up",
    ] {
        assert!(chord(text).is_bindable(), "{text} should be bindable");
    }
}

#[test]
fn a_single_alt_or_ctrl_on_a_navigation_key_is_bindable_but_not_on_a_letter() {
    for text in ["Alt+Left", "Alt+Right", "Ctrl+Left", "Ctrl+Right", "Alt+Backspace", "Ctrl+Backspace", "Alt+Home", "Ctrl+End", "Alt+Delete"] {
        assert!(chord(text).is_bindable(), "{text} should be bindable");
    }
    // Option+7 types a bracket on a Swiss layout: a letter, digit or symbol with one Alt or Ctrl stays typing.
    for text in ["Alt+D", "Alt+7", "Ctrl+D", "Alt+[", "Alt+Space", "Alt+Enter", "Alt+Tab", "Shift+Left", "Alt+F5"] {
        assert!(!chord(text).is_bindable(), "{text} must stay with the terminal");
    }
}

#[test]
fn the_edit_actions_are_in_the_table_with_the_platform_standard_as_default() {
    let ids = [
        "edit.word_back",
        "edit.word_forward",
        "edit.line_start",
        "edit.line_end",
        "edit.delete_word_back",
        "edit.delete_to_line_start",
    ];
    for os in OSES {
        for id in ids {
            assert!(actions_for(os).iter().any(|a| a.id == id), "{id} is missing from the {os} table");
        }
    }
    assert_eq!(default_of("macos", "edit.word_back"), Some("Alt+Left"));
    assert_eq!(default_of("macos", "edit.word_forward"), Some("Alt+Right"));
    assert_eq!(default_of("macos", "edit.line_start"), Some("Cmd+Left"));
    assert_eq!(default_of("macos", "edit.line_end"), Some("Cmd+Right"));
    assert_eq!(default_of("macos", "edit.delete_word_back"), Some("Alt+Backspace"));
    assert_eq!(default_of("macos", "edit.delete_to_line_start"), Some("Cmd+Backspace"));
    // Linux: Ctrl moves and deletes by word; Home and End already work through the encoder.
    assert_eq!(default_of("linux", "edit.word_back"), Some("Ctrl+Left"));
    assert_eq!(default_of("linux", "edit.word_forward"), Some("Ctrl+Right"));
    assert_eq!(default_of("linux", "edit.delete_word_back"), Some("Ctrl+Backspace"));
    assert_eq!(default_of("linux", "edit.line_start"), None);
    assert_eq!(default_of("linux", "edit.line_end"), None);
    assert_eq!(default_of("linux", "edit.delete_to_line_start"), None);
    // Windows: PSReadLine and cmd bind Ctrl+Arrow, Home and End themselves, so none of the six has a default.
    for id in ids {
        assert_eq!(default_of("windows", id), None, "{id} on Windows");
    }
}

#[test]
fn an_edit_action_can_be_moved_and_unbound_like_any_other() {
    let (table, warnings) =
        Keybindings::resolve(&overrides(&[("edit.word_back", "Ctrl+Alt+B"), ("edit.word_forward", "")]));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(table.chord_of("edit.word_back").map(ToString::to_string), Some("Ctrl+Alt+B".to_string()));
    assert_eq!(table.chord_of("edit.word_forward"), None);
}

#[test]
fn plain_keys_and_single_ctrl_or_alt_chords_stay_with_the_terminal() {
    for text in ["D", "Shift+D", "Ctrl+D", "Alt+D", "Escape", "Enter", "Shift+Tab", "Ctrl+C", "F5", "Up", "Shift+Up"] {
        assert!(!chord(text).is_bindable(), "{text} must stay with the terminal");
    }
}

#[test]
fn ctrl_or_alt_with_one_more_modifier_is_bindable_even_on_a_letter() {
    // Ctrl+Shift+C is the usual terminal copy chord on other platforms; the policy leaves it to the user.
    assert!(chord("Ctrl+Shift+C").is_bindable());
    assert!(chord("Alt+Shift+C").is_bindable());
}

#[test]
fn every_default_chord_is_unique_and_bindable_under_the_policy() {
    for os in OSES {
        let mut seen = HashSet::new();
        for a in all_actions(os) {
            let Some(d) = a.default else { continue };
            let c = chord(d);
            assert!(c.is_bindable(), "default {d} of {} on {os} is refused by the policy", a.id);
            assert!(seen.insert(c), "default chord {d} is used twice on {os}");
        }
    }
}

// ---- the validation policy of resolve -----------------------------------------------------------------

#[test]
fn an_unknown_action_id_is_reported_and_ignored() {
    let (kb, warnings) = Keybindings::resolve_for(MAC, &overrides(&[("pane.explode", "Cmd+Alt+E")]));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("pane.explode"), "{warnings:?}");
    assert_eq!(kb.table(), Keybindings::default_for(MAC).table(), "the table is the default table");
    assert_eq!(kb.action_of(&chord("Cmd+Alt+E")), None);
}

#[test]
fn an_unparsable_chord_is_reported_and_the_default_stays() {
    for bad in ["Cmd++", "Hyper+X", "Cmd+Shift", "Cmd+Cmd+D", "Cmd+F25", "Cmd+F01", "Cmd+ab", "+"] {
        let (kb, warnings) = Keybindings::resolve_for(MAC, &overrides(&[("pane.close", bad)]));
        assert_eq!(warnings.len(), 1, "{bad:?}: {warnings:?}");
        assert!(warnings[0].contains("pane.close"), "{bad:?}: {warnings:?}");
        assert_eq!(kb.chord_of("pane.close"), Some(&chord("Cmd+W")), "{bad:?}");
        assert!(kb.table().iter().all(|r| r.is_default), "{bad:?} changed something");
    }
}

#[test]
fn the_empty_string_unbinds_a_known_action_without_a_warning() {
    let (kb, warnings) = Keybindings::resolve_for(MAC, &overrides(&[("pane.close", "")]));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(kb.chord_of("pane.close"), None);
    assert_eq!(kb.action_of(&chord("Cmd+W")), None);
    let row = kb.table().into_iter().find(|r| r.action == "pane.close").expect("row");
    assert_eq!(row.chord, None);
    assert!(!row.is_default);
}

#[test]
fn a_chord_that_would_steal_terminal_input_is_refused_with_a_warning() {
    for refused in ["D", "Shift+D", "Ctrl+D", "Alt+D", "Escape", "shift+enter", "F5"] {
        let (kb, warnings) = Keybindings::resolve_for(MAC, &overrides(&[("tab.new", refused)]));
        assert_eq!(warnings.len(), 1, "{refused:?}: {warnings:?}");
        assert!(warnings[0].contains("tab.new"), "{refused:?}: {warnings:?}");
        assert_eq!(kb.chord_of("tab.new"), Some(&chord("Cmd+T")), "{refused:?} keeps the default");
        assert!(kb.table().iter().all(|r| r.is_default), "{refused:?} changed something");
    }
}

#[test]
fn an_override_beats_a_default_and_the_displaced_action_is_unbound_with_a_warning() {
    // Cmd+K is the default of palette.open.
    let (kb, warnings) = Keybindings::resolve_for(MAC, &overrides(&[("tab.new", "Cmd+K")]));
    assert_eq!(kb.chord_of("tab.new"), Some(&chord("Cmd+K")));
    assert_eq!(kb.action_of(&chord("Cmd+K")), Some("tab.new"));
    assert_eq!(kb.chord_of("palette.open"), None, "the displaced default is unbound");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("palette.open") && warnings[0].contains("tab.new"), "{warnings:?}");
    let row = kb.table().into_iter().find(|r| r.action == "palette.open").expect("row");
    assert!(!row.is_default && row.chord.is_none());
}

#[test]
fn two_overrides_on_one_chord_drop_the_alphabetically_later_action() {
    // The same chord in two spellings, so the conflict is found on the canonical form.
    let (kb, warnings) =
        Keybindings::resolve_for(MAC, &overrides(&[("tab.new", "cmd+alt+y"), ("pane.split_down", "Command+Alt+Y")]));
    assert_eq!(kb.action_of(&chord("Cmd+Alt+Y")), Some("pane.split_down"), "pane.* sorts before tab.*");
    assert_eq!(kb.chord_of("tab.new"), Some(&chord("Cmd+T")), "the dropped override leaves the default");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("tab.new") && warnings[0].contains("pane.split_down"), "{warnings:?}");
}

#[test]
fn a_dropped_override_still_loses_its_default_to_another_override() {
    // tab.new is dropped by the conflict and falls back to Cmd+T, which workspace.new then claims.
    let (kb, warnings) = Keybindings::resolve_for(MAC, &overrides(&[
        ("pane.split_down", "Cmd+Alt+Y"),
        ("tab.new", "Cmd+Alt+Y"),
        ("workspace.new", "Cmd+T"),
    ]));
    assert_eq!(kb.action_of(&chord("Cmd+T")), Some("workspace.new"));
    assert_eq!(kb.chord_of("tab.new"), None);
    assert_eq!(warnings.len(), 2, "{warnings:?}");
}

#[test]
fn swapping_two_defaults_is_not_a_conflict() {
    let (kb, warnings) =
        Keybindings::resolve_for(MAC, &overrides(&[("pane.split_right", "Cmd+Shift+D"), ("pane.split_down", "Cmd+D")]));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(kb.action_of(&chord("Cmd+D")), Some("pane.split_down"));
    assert_eq!(kb.action_of(&chord("Cmd+Shift+D")), Some("pane.split_right"));
}

#[test]
fn every_chord_in_the_resolved_table_belongs_to_at_most_one_action() {
    let (kb, _) = Keybindings::resolve_for(MAC, &overrides(&[
        ("tab.new", "Cmd+K"),
        ("search.open", "Cmd+K"),
        ("font.reset", "Cmd+W"),
        ("palette.open", "Cmd+Alt+P"),
    ]));
    let mut seen = HashSet::new();
    for row in kb.table() {
        if let Some(c) = row.chord {
            assert!(seen.insert(c.clone()), "{c} is bound twice");
            assert_eq!(kb.action_of(&chord(&c)), Some(row.action), "reverse lookup of {c}");
        }
    }
}

#[test]
fn warnings_never_carry_control_characters_from_the_file() {
    let (_, warnings) = Keybindings::resolve_for(MAC, &overrides(&[
        ("evil\u{1b}[2J", "Cmd+X"),
        ("tab.new", "Cmd+\u{7}\u{1b}]0;x"),
        ("pane.close", "Hyper+\n+X"),
    ]));
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    for w in &warnings {
        assert!(!w.chars().any(char::is_control), "control character in {w:?}");
    }
}

// ---- the grammar shared with the frontend --------------------------------------------------------------

/// The 14 chord strings the webview must produce from a `KeyboardEvent`. The frontend test copies this literal;
/// chord generation there has to yield exactly these canonical strings.
const SHARED_SAMPLES: [&str; 14] = [
    "Cmd+D",
    "Cmd+Shift+D",
    "Cmd+]",
    "Cmd+Alt+1",
    "Cmd+Ctrl+Space",
    "Ctrl+Alt+Shift+Up",
    "Cmd+,",
    "Cmd+=",
    "Cmd+-",
    "Cmd+Shift+Space",
    "Ctrl+Shift+PageDown",
    "Cmd+F12",
    "Cmd+Alt+Enter",
    "Cmd+Tab",
];

#[test]
fn the_shared_chord_samples_parse_to_themselves() {
    assert_eq!(SHARED_SAMPLES.len(), 14);
    for sample in SHARED_SAMPLES {
        assert_eq!(canon(sample), Ok(sample.to_string()), "{sample}");
        assert!(chord(sample).is_bindable(), "{sample} must be bindable");
    }
}

#[test]
fn the_shared_samples_survive_a_round_trip_through_the_settings_map() {
    // Each sample bound to font.reset comes back as the same string. A sample that is the default chord of another
    // action takes it away from that action, which is one warning; any other sample is silent.
    for sample in SHARED_SAMPLES {
        let (kb, warnings) = Keybindings::resolve_for(MAC, &overrides(&[("font.reset", sample)]));
        assert_eq!(kb.chord_of("font.reset").map(ToString::to_string).as_deref(), Some(sample));
        let taken = actions_for(MAC).iter().filter(|a| a.id != "font.reset" && a.default == Some(sample)).count();
        assert_eq!(warnings.len(), taken, "{sample}: {warnings:?}");
    }
}

#[test]
fn format_and_combining_characters_are_not_keys() {
    // Right-to-left override, zero width space, combining acute accent: the editor could not show them.
    for key in ["\u{202e}", "\u{200b}", "\u{301}", "\u{ad}"] {
        let text = format!("Cmd+{key}");
        assert!(Chord::parse(&text).is_err(), "{text:?} must not parse");
    }
}

#[test]
fn every_printable_ascii_key_parses_and_prints_back_idempotently() {
    for c in ('!'..='~').filter(|c| *c != '+') {
        let first = Chord::parse(&format!("Cmd+{c}")).unwrap_or_else(|e| panic!("{c:?} should be a key: {e}"));
        let again = Chord::parse(&first.to_string()).expect("the canonical form parses");
        assert_eq!(first, again, "{c:?}");
        assert_eq!(first.key, c.to_ascii_uppercase().to_string());
    }
}

#[test]
fn spaces_around_the_plus_signs_are_tolerated() {
    assert_eq!(canon(" cmd + shift + d "), Ok("Cmd+Shift+D".to_string()));
}

// ---- the link to settings.json --------------------------------------------------------------------------

#[test]
fn overrides_in_settings_json_flow_into_the_resolved_table() {
    use holzwave_core::settings::Settings;

    let text = r#"{"version": 1, "keybindings": {"pane.split_right": "cmd+shift+enter", "palette.open": ""}}"#;
    let (settings, _) = Settings::from_json(text);
    let (kb, warnings) = Keybindings::resolve(&settings.keybindings);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(kb.action_of(&chord("Cmd+Shift+Enter")), Some("pane.split_right"));
    assert_eq!(kb.chord_of("palette.open"), None, "the empty string unbinds");
}
