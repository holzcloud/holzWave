//! The golden of the consent card: the page keeps its own words for the redaction rules, the refusal reasons of a context
//! call and the four places a request can go, and this test writes the real names to `app/tests/fixtures/ai_consent.json`
//! so that a renamed rule, reason or place fails here and in the page's unit tests in the same commit.
//!
//! The document is built from the real tables only. No network, no engine. `UPDATE_FIXTURES=1` rewrites the file; without it
//! the file must equal what is built now.

use std::path::PathBuf;

use holzwave_core::ai::redact::RULES;
use holzwave_core::ai::service::ContextRefusal;
use holzwave_core::ai::types::Locality;
use serde_json::{Value, json};

/// A key that never belongs in the file. It appears in this test only: a hit anywhere else is a leak.
const CANARY: &str = "zq-golden-ctx-4d2b17";

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../app/tests/fixtures/ai_consent.json")
}

fn update_fixtures() -> bool {
    std::env::var("UPDATE_FIXTURES").is_ok_and(|v| v == "1")
}

/// Every refusal reason, in declaration order. The match has no wildcard arm, so a reason the core adds does not compile
/// here until the golden (and with it the page's sentence for it) knows about it.
fn reasons() -> Vec<&'static str> {
    let all = ContextRefusal::ALL;
    for refusal in all {
        match refusal {
            ContextRefusal::NotShared
            | ContextRefusal::StaleOffer
            | ContextRefusal::BadPick
            | ContextRefusal::BadKinds
            | ContextRefusal::UnsupportedKind
            | ContextRefusal::NothingToShare
            | ContextRefusal::PaneGone
            | ContextRefusal::Denied
            | ContextRefusal::NotText
            | ContextRefusal::Unreadable
            | ContextRefusal::Busy
            | ContextRefusal::Destination
            | ContextRefusal::LocalOnly => {}
        }
    }
    all.iter().map(|refusal| refusal.reason()).collect()
}

/// The serde names of the four places, in declaration order, with the same exhaustive match.
fn localities() -> Vec<String> {
    let all = [Locality::OnDevice, Locality::Loopback, Locality::Lan, Locality::Cloud];
    for place in all {
        match place {
            Locality::OnDevice | Locality::Loopback | Locality::Lan | Locality::Cloud => {}
        }
    }
    all.iter()
        .map(|place| match serde_json::to_value(place).expect("a place is JSON") {
            Value::String(name) => name,
            other => panic!("a place serialises as a string, not {other}"),
        })
        .collect()
}

/// `value` with the members of every object in sorted order.
fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().map(|(name, item)| (name, sorted(item))).collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(list) => Value::Array(list.into_iter().map(sorted).collect()),
        other => other,
    }
}

fn document() -> Value {
    sorted(json!({
        "localities": localities(),
        "reasons": reasons(),
        "rules": RULES,
    }))
}

fn text_of(document: &Value) -> String {
    let mut text = serde_json::to_string_pretty(document).expect("the document is JSON");
    text.push('\n');
    text
}

/// A name is lower-case words joined by underscores.
fn is_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') && !name.starts_with('_') && !name.ends_with('_')
}

#[test]
fn the_consent_golden_matches_the_rule_table_the_refusal_reasons_and_the_localities() {
    let first = document();
    let second = document();
    assert_eq!(first, second, "the document is built the same way twice");

    let text = text_of(&first);
    assert!(!text.contains(CANARY), "the canary is in the golden");

    let object = first.as_object().expect("the document is an object");
    let members: Vec<&str> = object.keys().map(String::as_str).collect();
    assert_eq!(members, ["localities", "reasons", "rules"]);
    let list = |name: &str| -> Vec<String> {
        object[name]
            .as_array()
            .expect("a list")
            .iter()
            .map(|item| item.as_str().expect("a name").to_owned())
            .collect()
    };
    for (name, count) in [("rules", 22), ("reasons", 13), ("localities", 4)] {
        let names = list(name);
        assert_eq!(names.len(), count, "the number of {name}");
        let mut distinct = names.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), names.len(), "the {name} are distinct");
        assert!(names.iter().all(|item| is_name(item)), "the {name} are lower-case names");
    }
    // The first and the last of each list, in the order the core reports them.
    assert_eq!(list("rules").first().map(String::as_str), Some("hidden_text"));
    assert_eq!(list("rules").last().map(String::as_str), Some("truncated"));
    assert_eq!(list("reasons").first().map(String::as_str), Some("not_shared"));
    assert_eq!(list("reasons").last().map(String::as_str), Some("local_only"));
    assert_eq!(list("localities"), ["on_device", "loopback", "lan", "cloud"]);

    let path = golden_path();
    if update_fixtures() {
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("the fixtures directory");
        std::fs::write(&path, text).expect("write ai_consent.json");
        return;
    }
    let on_disk = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is missing ({e}); run with UPDATE_FIXTURES=1", path.display()));
    assert!(
        on_disk == text,
        "ai_consent.json differs from the tables of the core; review the change and run with UPDATE_FIXTURES=1"
    );
}
