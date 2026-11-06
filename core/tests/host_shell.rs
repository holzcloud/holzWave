//! Tests for `host_shell.json`: the choice of the person, hostile files, the reader of the Flatpak permission, the rule that
//! joins them, the example of `docs/config.md`, and that the file never syncs.

mod common;

use std::fs;
use std::sync::Arc;

use common::scratch_dir;
use holzwave_core::host_shell::{
    FILE, HostShell, MAX_BYTES, MAX_INFO_BYTES, State, can_set, permission_in, state_of,
};
use holzwave_core::layout::LoadStatus;
use holzwave_core::secrets::MemoryStore;
use holzwave_core::settings::Settings;
use holzwave_core::sync::engine::{ConfigSource, Source};

/// A Flatpak info text built from pieces, in the shape of a real `/.flatpak-info`.
fn info(policy: &str) -> String {
    let application = "[Application]\nname=ch.holzcloud.holzwave\nruntime=runtime/org.freedesktop.Platform/x86_64/24.08\n";
    let instance = "\n[Instance]\ninstance-id=1234567890\nbranch=stable\nflatpak-version=1.16.1\nsession-bus-proxy=true\n";
    let context = "\n[Context]\nshared=network;ipc;\nsockets=wayland;fallback-x11;\ndevices=dri;\n";
    format!("{application}{instance}{context}\n[Session Bus Policy]\n{policy}\n")
}

#[test]
fn the_default_is_off_and_a_missing_file_is_clean() {
    assert!(!HostShell::default().enabled);
    assert_eq!(HostShell::default().to_json(), "{\n  \"version\": 1,\n  \"host_shell\": false\n}\n");
    let dir = scratch_dir("hs-missing");
    let (value, status) = HostShell::load(&dir);
    assert_eq!(value, HostShell::default());
    assert_eq!(status, LoadStatus::Clean);
    assert!(!dir.join(FILE).exists(), "loading writes nothing");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_choice_round_trips_through_the_file() {
    let dir = scratch_dir("hs-round");
    for enabled in [false, true, false] {
        let value = HostShell { enabled, ..HostShell::default() };
        value.save(&dir).expect("saves");
        let (back, status) = HostShell::load(&dir);
        assert_eq!(back, value);
        assert_eq!(status, LoadStatus::Clean);
    }
    let text = fs::read_to_string(dir.join(FILE)).expect("reads");
    assert!(text.ends_with('\n') && text.contains("\"version\": 1"), "pretty printed with a version: {text:?}");
    assert!(!dir.join("host_shell.json.bad").exists(), "a valid file never leaves a .bad copy");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn only_the_exact_boolean_true_turns_it_on() {
    let (value, status) = HostShell::from_json("{\"version\": 1, \"host_shell\": true}");
    assert!(value.enabled);
    assert_eq!(status, LoadStatus::Clean);
    // Unknown keys are ignored; a missing version is the current one.
    let (value, status) = HostShell::from_json("{\"host_shell\": true, \"other\": [1, 2]}");
    assert!(value.enabled);
    assert_eq!(status, LoadStatus::Clean);
    for (label, text) in [
        ("a string", "{\"version\": 1, \"host_shell\": \"true\"}"),
        ("the number one", "{\"version\": 1, \"host_shell\": 1}"),
        ("null", "{\"version\": 1, \"host_shell\": null}"),
        ("an array", "{\"version\": 1, \"host_shell\": [true]}"),
        ("an object", "{\"version\": 1, \"host_shell\": {\"enabled\": true}}"),
        ("a missing key", "{\"version\": 1}"),
    ] {
        let (value, status) = HostShell::from_json(text);
        assert!(!value.enabled, "{label} must not turn it on");
        assert!(matches!(status, LoadStatus::Repaired(_)), "{label}: {status:?}");
    }
}

#[test]
fn a_hostile_file_reads_off_and_is_kept_as_bad() {
    let big = format!("{{\"host_shell\": true, \"pad\": \"{}\"}}", "x".repeat(5 * 1024));
    assert!(big.len() as u64 > MAX_BYTES);
    let cases: [(&str, String); 12] = [
        ("not json", "{".to_owned()),
        ("a number", "42".to_owned()),
        ("true alone", "true".to_owned()),
        ("an array", "[true]".to_owned()),
        ("a string as value", "{\"version\": 1, \"host_shell\": \"true\"}".to_owned()),
        ("a number as value", "{\"version\": 1, \"host_shell\": 1}".to_owned()),
        ("null as value", "{\"version\": 1, \"host_shell\": null}".to_owned()),
        ("no value", "{\"version\": 1}".to_owned()),
        ("an unknown version", "{\"version\": 2, \"host_shell\": true}".to_owned()),
        ("a version that is text", "{\"version\": \"1\", \"host_shell\": true}".to_owned()),
        ("nul bytes", "\u{0}\u{1}\u{2}".to_owned()),
        ("oversized", big),
    ];
    for (label, text) in cases {
        let dir = scratch_dir("hs-hostile");
        fs::write(dir.join(FILE), &text).expect("writes");

        let (value, status) = HostShell::load(&dir);

        assert!(!value.enabled, "{label}: a damaged file must never turn it on");
        assert!(matches!(status, LoadStatus::Repaired(_) | LoadStatus::Reset(_)), "{label}: {status:?}");
        let kept = fs::read(dir.join("host_shell.json.bad")).unwrap_or_else(|e| panic!("{label}: no .bad copy: {e}"));
        assert_eq!(kept, text.as_bytes(), "{label}: the damaged bytes are kept");
        fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn a_file_that_is_not_utf8_reads_off_and_is_kept() {
    let dir = scratch_dir("hs-utf8");
    fs::write(dir.join(FILE), [0xff, 0xfe, 0x00, 0x7b]).expect("writes");
    let (value, status) = HostShell::load(&dir);
    assert!(!value.enabled);
    assert!(matches!(status, LoadStatus::Reset(_)), "{status:?}");
    assert!(dir.join("host_shell.json.bad").is_file());
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_error_never_names_the_content_of_the_file() {
    let dir = scratch_dir("hs-secret");
    fs::write(dir.join(FILE), "{\"host_shell\": \"hunter2-secret\"").expect("writes");
    let (_, status) = HostShell::load(&dir);
    assert!(!format!("{status:?}").contains("hunter2"), "{status:?}");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_permission_is_read_from_the_session_bus_policy() {
    for value in ["talk", "own"] {
        assert!(permission_in(&info(&format!("org.freedesktop.Flatpak={value}"))), "{value}");
    }
    // Blanks around the key and the value do not matter; other names in the section do not hide it.
    assert!(permission_in(&info("org.freedesktop.portal.Desktop=talk\n org.freedesktop.Flatpak = talk ")));
    // CRLF line ends.
    assert!(permission_in(&info("org.freedesktop.Flatpak=talk").replace('\n', "\r\n")));
    // The shortest text that says it.
    assert!(permission_in("[Session Bus Policy]\norg.freedesktop.Flatpak=talk\n"));
}

#[test]
fn the_permission_is_refused_in_every_other_case() {
    let sections = "[Application]\nname=ch.holzcloud.holzwave\n";
    let cases: Vec<(&str, String)> = vec![
        ("none", info("org.freedesktop.Flatpak=none")),
        ("an unknown value", info("org.freedesktop.Flatpak=see")),
        ("another case of the value", info("org.freedesktop.Flatpak=Talk")),
        ("an empty value", info("org.freedesktop.Flatpak=")),
        ("no such key", info("org.freedesktop.portal.Desktop=talk")),
        ("a commented line", info("# org.freedesktop.Flatpak=talk")),
        ("a semicolon comment", info("; org.freedesktop.Flatpak=talk")),
        ("a longer name", info("org.freedesktop.Flatpak.Extra=talk")),
        ("a name it is the suffix of", info("com.example.org.freedesktop.Flatpak=talk")),
        ("a name it is the prefix of", info("org.freedesktop.FlatpakX=talk")),
        ("another case of the name", info("org.freedesktop.flatpak=talk")),
        ("the first line decides: none before talk", info("org.freedesktop.Flatpak=none\norg.freedesktop.Flatpak=talk")),
        ("the key in the system bus section", format!("{sections}[System Bus Policy]\norg.freedesktop.Flatpak=talk\n")),
        ("the key in the application section", format!("{sections}org.freedesktop.Flatpak=talk\n")),
        ("the key before any section", "org.freedesktop.Flatpak=talk\n".to_owned()),
        ("another case of the section", "[session bus policy]\norg.freedesktop.Flatpak=talk\n".to_owned()),
        ("a section name with a blank", "[ Session Bus Policy ]\norg.freedesktop.Flatpak=talk\n".to_owned()),
        ("the section closed again", "[Session Bus Policy]\n[Context]\norg.freedesktop.Flatpak=talk\n".to_owned()),
        ("a NUL byte", info("org.freedesktop.Flatpak=talk\n\0")),
        ("a line that is no key", info("org.freedesktop.Flatpak=talk\nthis is not a key value line")),
        ("an unclosed section", format!("{}\n[Broken\n", info("org.freedesktop.Flatpak=talk"))),
        ("an empty text", String::new()),
        ("only blanks", "  \n\n".to_owned()),
    ];
    for (label, text) in cases {
        assert!(!permission_in(&text), "{label}");
    }
}

#[test]
fn an_oversized_info_is_refused_and_one_at_the_limit_is_read() {
    let line = "org.freedesktop.Flatpak=talk\n";
    let head = "[Session Bus Policy]\n";
    let pad = "#".repeat(usize::try_from(MAX_INFO_BYTES).unwrap() - head.len() - line.len() - 1) + "\n";
    let at_limit = format!("{head}{pad}{line}");
    assert_eq!(at_limit.len() as u64, MAX_INFO_BYTES);
    assert!(permission_in(&at_limit), "the limit itself is read");
    let over = format!("#\n{at_limit}");
    assert!(over.len() as u64 > MAX_INFO_BYTES);
    assert!(!permission_in(&over), "one byte more is refused");
    assert!(!permission_in(&"x".repeat(1024 * 1024)));
}

#[test]
fn the_state_follows_the_rule() {
    // Not in a Flatpak: the group does not apply, whatever else is true.
    for permission in [false, true] {
        for enabled in [false, true] {
            assert_eq!(state_of(false, permission, enabled), State::NotApplicable, "{permission} {enabled}");
        }
    }
    // In a Flatpak without the permission: unavailable, whatever the choice.
    assert_eq!(state_of(true, false, false), State::Unavailable);
    assert_eq!(state_of(true, false, true), State::Unavailable, "the choice cannot grant the permission");
    // With the permission: the choice decides.
    assert_eq!(state_of(true, true, false), State::Off);
    assert_eq!(state_of(true, true, true), State::On);
    // Only On starts a pane on the host.
    assert!(State::On.spawns_on_host());
    for state in [State::NotApplicable, State::Off, State::Unavailable] {
        assert!(!state.spawns_on_host(), "{state:?}");
    }
}

#[test]
fn the_words_are_the_wire_words_and_round_trip() {
    assert_eq!(State::Off.as_str(), Some("off"));
    assert_eq!(State::On.as_str(), Some("on"));
    assert_eq!(State::Unavailable.as_str(), Some("unavailable"));
    assert_eq!(State::NotApplicable.as_str(), None, "the group does not exist, so there is no word");
    for state in [State::Off, State::On, State::Unavailable] {
        assert_eq!(State::from_word(state.as_str().expect("a word")), Some(state));
    }
    for word in ["", "On", "OFF", " off", "off ", "not_applicable", "not available", "true", "enabled"] {
        assert_eq!(State::from_word(word), None, "{word:?}");
    }
}

#[test]
fn the_choice_can_be_set_only_where_it_means_something() {
    assert!(can_set(State::Off).is_ok());
    assert!(can_set(State::On).is_ok());
    let outside = can_set(State::NotApplicable).expect_err("outside a Flatpak");
    let without = can_set(State::Unavailable).expect_err("without the permission");
    assert!(outside.contains("Flatpak") && without.contains("permission"), "{outside:?} {without:?}");
    assert_ne!(outside, without);
}

#[test]
fn the_file_never_syncs() {
    assert!(!holzwave_core::sync::engine::allowed("host_shell.json"));
    assert!(!holzwave_core::sync::engine::allowed("host_shell.json.bad"));
    assert!(holzwave_core::sync::engine::allowed("settings.json"), "the check can say yes");

    // The choice is not a setting: the settings document has no such key, so it cannot travel inside settings.json.
    let document = Settings::default().to_json();
    let value: serde_json::Value = serde_json::from_str(&document).expect("the settings are JSON");
    let object = value.as_object().expect("an object");
    assert!(!object.is_empty());
    assert!(object.keys().all(|key| !key.starts_with("host_shell")), "{:?}", object.keys().collect::<Vec<_>>());
    let (_, status) = Settings::from_json("{\"host_shell\": true}");
    assert!(matches!(status, LoadStatus::Repaired(_)), "a stray key is reported, not adopted: {status:?}");
    assert!(!Settings::from_json("{\"host_shell\": true}").0.to_json().contains("host_shell"));

    // A real data directory with the file in it: the sync enumerates every name it would carry, and this one is not there.
    let dir = scratch_dir("hs-sync");
    HostShell { enabled: true, ..HostShell::default() }.save(&dir).expect("saves");
    fs::write(dir.join("settings.json"), Settings::default().to_json()).expect("writes");
    let source = ConfigSource::new(&dir, Arc::new(MemoryStore::default()));
    let names = source.names().expect("names");
    assert!(names.contains(&"settings.json".to_owned()), "the enumeration sees the files it carries: {names:?}");
    assert!(!names.iter().any(|name| name.contains("host_shell")), "{names:?}");
    assert_eq!(source.read("host_shell.json").ok().flatten(), None, "a read by name finds nothing either");
    fs::remove_dir_all(&dir).ok();
}

/// The fenced example that follows `<!-- example: host_shell.json -->` in `docs/config.md`.
fn documented_example() -> String {
    let doc = include_str!("../../docs/config.md");
    let marker = "<!-- example: host_shell.json -->";
    let after = doc.split_once(marker).expect("docs/config.md has the example").1;
    let fence = after.trim_start().strip_prefix("```json\n").expect("a json fence follows the marker");
    fence.split_once("\n```").expect("the fence is closed").0.to_owned()
}

#[test]
fn the_documented_example_loads_cleanly() {
    let (value, status) = HostShell::from_json(&documented_example());
    assert_eq!(status, LoadStatus::Clean, "the example of docs/config.md is not clean");
    assert!(value.enabled, "the example shows the switched on file");
    assert_eq!(value.version, 1);
    // The documented file, as a file, through the loader.
    let dir = scratch_dir("hs-doc");
    fs::write(dir.join(FILE), documented_example()).expect("writes");
    assert_eq!(HostShell::load(&dir), (value, LoadStatus::Clean));
    assert!(!dir.join("host_shell.json.bad").exists());
    fs::remove_dir_all(&dir).ok();
}
