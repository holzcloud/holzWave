//! Tests for key bindings: the canonical chord form, the default action table, and how overrides from settings.json
//! resolve into the table the matcher and the keybinding editor read.

use std::collections::{BTreeMap, HashSet};

use holzwave_core::keybindings::{Chord, Keybindings, actions};

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

/// The action table, after checking that it is the full set: a loop over an empty table would pass vacuously.
fn all_actions() -> &'static [holzwave_core::keybindings::ActionInfo] {
    assert_eq!(actions().len(), 15 + 9 + 9, "the table holds 33 actions");
    actions()
}

/// The default chord of `action`, from the table.
fn default_of(action: &str) -> Option<&'static str> {
    actions().iter().find(|a| a.id == action).and_then(|a| a.default)
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
        assert_eq!(default_of(action), Some(chord.as_str()), "default of {action}");
    }
}

#[test]
fn closing_a_pane_is_cmd_w_not_cmd_shift_w() {
    // D-14: Cmd+W closes the pane and "Close Window" is gone from the menu.
    assert_eq!(default_of("pane.close"), Some("Cmd+W"));
}

#[test]
fn every_action_has_a_title() {
    for a in all_actions() {
        assert!(!a.title.trim().is_empty(), "{} has no title", a.id);
    }
}

#[test]
fn default_chords_are_written_in_canonical_form() {
    for a in all_actions() {
        if let Some(d) = a.default {
            assert_eq!(canon(d), Ok(d.to_string()), "default of {} is not canonical", a.id);
        }
    }
}

#[test]
fn defaults_resolve_without_warnings_and_match_the_table() {
    let (kb, warnings) = Keybindings::resolve(&BTreeMap::new());
    assert!(warnings.is_empty(), "{warnings:?}");
    for a in all_actions() {
        let want = a.default.map(chord);
        assert_eq!(kb.chord_of(a.id), want.as_ref(), "chord of {}", a.id);
    }
}

// ---- one override travels from the settings map to the resolved table ---------------------------------

#[test]
fn an_override_changes_exactly_one_action_and_is_reported_as_not_default() {
    let (kb, warnings) = Keybindings::resolve(&overrides(&[("pane.split_right", "Cmd+Shift+Enter")]));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(kb.chord_of("pane.split_right").map(ToString::to_string), Some("Cmd+Shift+Enter".to_string()));

    let table = kb.table();
    assert_eq!(table.len(), actions().len(), "one row per action, in the table order");
    for (row, info) in table.iter().zip(actions()) {
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
    let (kb, _) = Keybindings::resolve(&overrides(&[("pane.split_right", "Cmd+Shift+Enter")]));
    assert_eq!(kb.action_of(&chord("Cmd+Shift+Enter")), Some("pane.split_right"));
    assert_eq!(kb.action_of(&chord("Cmd+D")), None, "the default chord is free after the override");
}

#[test]
fn action_of_finds_every_default_chord_and_the_ids_are_unique() {
    let (kb, _) = Keybindings::resolve(&BTreeMap::new());
    let mut ids = HashSet::new();
    for a in all_actions() {
        assert!(ids.insert(a.id), "duplicate action id {}", a.id);
        if let Some(d) = a.default {
            assert_eq!(kb.action_of(&chord(d)), Some(a.id), "chord {d}");
        }
    }
}

#[test]
fn an_override_spelled_loosely_is_stored_in_canonical_form() {
    let (kb, warnings) = Keybindings::resolve(&overrides(&[("tab.new", "command+alt+t")]));
    assert!(warnings.is_empty(), "{warnings:?}");
    let row = kb.table().into_iter().find(|r| r.action == "tab.new").expect("tab.new row");
    assert_eq!(row.chord.as_deref(), Some("Cmd+Alt+T"));
}

#[test]
fn an_override_equal_to_the_default_counts_as_default() {
    let (kb, _) = Keybindings::resolve(&overrides(&[("pane.split_right", "cmd+d")]));
    let row = kb.table().into_iter().find(|r| r.action == "pane.split_right").expect("row");
    assert_eq!(row.chord.as_deref(), Some("Cmd+D"));
    assert!(row.is_default);
}
