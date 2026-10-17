//! The golden of the on-device row: the page keeps its own words for every state the shell can send and for every advice
//! key, and this test writes the real keys to `app/tests/fixtures/ai_device.json` so that a renamed or added state fails
//! here and in the page's unit tests in the same commit.
//!
//! The document is built from the real `Availability` and `Advice` enums. No Swift tool, no engine, no network: it runs on
//! any host. `UPDATE_FIXTURES=1` rewrites the file; without it the file must equal what is built now.

use std::collections::BTreeSet;
use std::path::PathBuf;

use holzwave_apple_ai::{Advice, Availability, MODEL_ID, PROVIDER_ID};

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../app/tests/fixtures/ai_device.json")
}

fn update_fixtures() -> bool {
    std::env::var("UPDATE_FIXTURES").is_ok_and(|v| v == "1")
}

/// Whether the shell can send the state. The match has no wildcard arm, so a variant that is added does not compile here
/// until it is classified, and with it the golden (and the page's tables) knows about it.
fn sent(state: Availability) -> bool {
    match state {
        Availability::Available
        | Availability::AppleIntelligenceNotEnabled
        | Availability::DeviceNotEligible
        | Availability::ModelNotReady
        | Availability::Other
        | Availability::OsTooOld
        | Availability::SdkLacksFramework => true,
        // The shell compiles the bridge on macOS only, where nothing is asked on another platform.
        Availability::UnsupportedPlatform => false,
    }
}

/// Every state, in declaration order.
const ALL: [Availability; 8] = [
    Availability::Available,
    Availability::AppleIntelligenceNotEnabled,
    Availability::DeviceNotEligible,
    Availability::ModelNotReady,
    Availability::Other,
    Availability::OsTooOld,
    Availability::SdkLacksFramework,
    Availability::UnsupportedPlatform,
];

/// Every advice key, in declaration order. The match has no wildcard arm for the same reason as above.
fn advice_keys() -> [&'static str; 7] {
    let all = [
        Advice::None,
        Advice::EnableAppleIntelligence,
        Advice::WaitForModel,
        Advice::NotSupported,
        Advice::UpdateMacos,
        Advice::NotInThisBuild,
        Advice::Unknown,
    ];
    for advice in all {
        match advice {
            Advice::None
            | Advice::EnableAppleIntelligence
            | Advice::WaitForModel
            | Advice::NotSupported
            | Advice::UpdateMacos
            | Advice::NotInThisBuild
            | Advice::Unknown => {}
        }
    }
    all.map(Advice::as_str)
}

/// The text of the golden: sorted keys, two-space indent, one trailing newline. Built as plain text because the crate has no
/// JSON dependency, and the keys it writes are ASCII words.
fn golden_text() -> String {
    let mut text = String::from("{\n");
    text.push_str(&format!("  \"id\": \"{PROVIDER_ID}\",\n"));
    text.push_str(&format!("  \"model\": \"{MODEL_ID}\",\n"));
    text.push_str("  \"states\": [\n");
    let sent_states: Vec<Availability> = ALL.into_iter().filter(|state| sent(*state)).collect();
    for (at, state) in sent_states.iter().enumerate() {
        text.push_str("    {\n");
        text.push_str(&format!("      \"advice\": \"{}\",\n", state.advice().as_str()));
        text.push_str(&format!("      \"state\": \"{}\"\n", state.as_str()));
        text.push_str(if at + 1 == sent_states.len() { "    }\n" } else { "    },\n" });
    }
    text.push_str("  ],\n");
    text.push_str("  \"unsent\": [\n");
    let unsent: Vec<Availability> = ALL.into_iter().filter(|state| !sent(*state)).collect();
    for (at, state) in unsent.iter().enumerate() {
        text.push_str(&format!("    \"{}\"{}\n", state.as_str(), if at + 1 == unsent.len() { "" } else { "," }));
    }
    text.push_str("  ]\n}\n");
    text
}

fn is_word(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 48
        && key.starts_with(|c: char| c.is_ascii_lowercase())
        && key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

#[test]
fn the_page_golden_matches_the_availability_table() {
    // The words the page is held to are snake_case, distinct, short, and `available` is the only state with no advice.
    let states: Vec<&str> = ALL.iter().map(|state| state.as_str()).collect();
    let distinct: BTreeSet<&str> = states.iter().copied().collect();
    assert_eq!(distinct.len(), states.len(), "the state keys are distinct");
    for key in &states {
        assert!(is_word(key), "{key} is a snake_case word of at most 48 characters");
    }
    let none: Vec<&str> =
        ALL.iter().filter(|state| state.advice().as_str() == "none").map(|state| state.as_str()).collect();
    assert_eq!(none, ["available"], "available is the only state whose advice is none");
    let used: BTreeSet<&str> = ALL.iter().map(|state| state.advice().as_str()).collect();
    let keys: BTreeSet<&str> = advice_keys().into_iter().collect();
    assert_eq!(keys.len(), 7, "the seven advice keys are distinct");
    assert_eq!(used, keys, "every advice key of the enum belongs to a state and the other way round");
    for key in &keys {
        assert!(is_word(key), "{key} is a snake_case word of at most 48 characters");
    }

    let text = golden_text();
    let path = golden_path();
    if update_fixtures() {
        std::fs::write(&path, &text).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
        return;
    }
    let on_disk = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is missing ({e}); run with UPDATE_FIXTURES=1", path.display()));
    assert!(
        on_disk == text,
        "ai_device.json differs from the availability table of the bridge; review the change and run with UPDATE_FIXTURES=1"
    );
}
