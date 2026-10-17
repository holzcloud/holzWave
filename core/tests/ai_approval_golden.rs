//! The golden of the approval card: the page keeps its own words for the refusals of a decision, the tools it can show, the
//! effects it knows and the two verdicts, and this test writes the real names to `app/tests/fixtures/ai_approval.json` so
//! that a renamed or added code, tool, effect or verdict fails here and in the page's unit tests in the same commit.
//!
//! The document is built from the real tables only. No network, no engine. `UPDATE_FIXTURES=1` rewrites the file; without it
//! the file must equal what is built now.

use std::path::PathBuf;

use holzwave_core::ai::provider::Detail;
use holzwave_core::ai::tools::{Decision, Effect, ToolError, ToolName};
use serde_json::{Map, Value, json};

/// A key that never belongs in the file. It appears in this test only: a hit anywhere else is a leak.
const CANARY: &str = "zq-golden-approval-8e41b6";

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../app/tests/fixtures/ai_approval.json")
}

fn update_fixtures() -> bool {
    std::env::var("UPDATE_FIXTURES").is_ok_and(|v| v == "1")
}

/// The place of a variant in the declaration of `ToolError`. The match has no wildcard arm, so a variant the core adds does
/// not compile here until the golden (and with it the page's sentence for it) knows about it.
fn position(error: &ToolError) -> usize {
    match error {
        ToolError::UnknownTool(_) => 0,
        ToolError::InvalidArguments(_) => 1,
        ToolError::Unavailable => 2,
        ToolError::Denied(_) => 3,
        ToolError::TooMany => 4,
        ToolError::DuplicateCall => 5,
        ToolError::UnknownCall => 6,
        ToolError::NotPending => 7,
        ToolError::HashMismatch => 8,
        ToolError::NotApproved => 9,
        ToolError::AlreadyUsed => 10,
        ToolError::PayloadChanged => 11,
        ToolError::ScopeMismatch => 12,
        ToolError::TaintChanged => 13,
        ToolError::Cancelled => 14,
        ToolError::Host(_) => 15,
        ToolError::NotShared => 16,
    }
}

/// One value of every variant, in declaration order. The variants that hold a text are made through `Detail::new`.
fn errors() -> Vec<ToolError> {
    let list = vec![
        ToolError::UnknownTool(Detail::new("tool")),
        ToolError::InvalidArguments(Detail::new("arguments")),
        ToolError::Unavailable,
        ToolError::Denied("rule"),
        ToolError::TooMany,
        ToolError::DuplicateCall,
        ToolError::UnknownCall,
        ToolError::NotPending,
        ToolError::HashMismatch,
        ToolError::NotApproved,
        ToolError::AlreadyUsed,
        ToolError::PayloadChanged,
        ToolError::ScopeMismatch,
        ToolError::TaintChanged,
        ToolError::Cancelled,
        ToolError::Host(Detail::new("host")),
        ToolError::NotShared,
    ];
    for (at, error) in list.iter().enumerate() {
        assert_eq!(position(error), at, "the list of errors is in declaration order and holds every variant");
    }
    list
}

/// The serde name of a value that serialises as a string.
fn name_of<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value).expect("a name is JSON") {
        Value::String(name) => name,
        other => panic!("a name serialises as a string, not {other}"),
    }
}

/// The serde names of the three effects, in declaration order, with an exhaustive match.
fn effects() -> Vec<Effect> {
    let all = [Effect::Read, Effect::Write, Effect::Exec];
    for effect in all {
        match effect {
            Effect::Read | Effect::Write | Effect::Exec => {}
        }
    }
    all.to_vec()
}

/// The two verdicts, with an exhaustive match.
fn decisions() -> Vec<Decision> {
    let all = [Decision::Allow, Decision::Deny];
    for decision in all {
        match decision {
            Decision::Allow | Decision::Deny => {}
        }
    }
    all.to_vec()
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
    let tools: Map<String, Value> =
        ToolName::ALL.iter().map(|tool| (tool.as_str().to_owned(), Value::String(name_of(&tool.effect())))).collect();
    sorted(json!({
        "codes": errors().iter().map(|error| error.code()).collect::<Vec<_>>(),
        "decisions": decisions().iter().map(name_of).collect::<Vec<_>>(),
        "effects": effects().iter().map(name_of).collect::<Vec<_>>(),
        "tools": tools,
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
fn the_approval_golden_matches_the_error_codes_the_tools_the_effects_and_the_decisions() {
    let first = document();
    let second = document();
    assert_eq!(first, second, "the document is built the same way twice");

    let text = text_of(&first);
    assert!(!text.contains(CANARY), "the canary is in the golden");

    let object = first.as_object().expect("the document is an object");
    let members: Vec<&str> = object.keys().map(String::as_str).collect();
    assert_eq!(members, ["codes", "decisions", "effects", "tools"]);
    let list = |name: &str| -> Vec<String> {
        object[name]
            .as_array()
            .expect("a list")
            .iter()
            .map(|item| item.as_str().expect("a name").to_owned())
            .collect()
    };
    for (name, count) in [("codes", 17), ("decisions", 2), ("effects", 3)] {
        let names = list(name);
        assert_eq!(names.len(), count, "the number of {name}");
        let mut distinct = names.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), names.len(), "the {name} are distinct");
        assert!(names.iter().all(|item| is_name(item)), "the {name} are lower-case names");
    }
    let tools = object["tools"].as_object().expect("an object of tools");
    assert_eq!(tools.len(), 7, "the number of tools");
    assert!(tools.keys().all(|name| is_name(name)), "the tools are lower-case names");
    let effect_names = list("effects");
    assert!(tools.values().all(|effect| effect_names.iter().any(|name| Some(name.as_str()) == effect.as_str())), "every tool has a known effect");
    // The first and the last of each list, in the order the core declares them.
    assert_eq!(list("codes").first().map(String::as_str), Some("unknown_tool"));
    assert_eq!(list("codes").last().map(String::as_str), Some("not_shared"));
    assert_eq!(list("decisions"), ["allow", "deny"]);
    assert_eq!(list("effects"), ["read", "write", "exec"]);
    assert_eq!(tools["read_file"], "read");
    assert_eq!(tools["write_file"], "write");
    assert_eq!(tools["run_command"], "exec");

    let path = golden_path();
    if update_fixtures() {
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("the fixtures directory");
        std::fs::write(&path, text).expect("write ai_approval.json");
        return;
    }
    let on_disk = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is missing ({e}); run with UPDATE_FIXTURES=1", path.display()));
    assert!(
        on_disk == text,
        "ai_approval.json differs from the tables of the core; review the change and run with UPDATE_FIXTURES=1"
    );
}
