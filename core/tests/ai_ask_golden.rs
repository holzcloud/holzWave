//! The golden of the `wsh ai` card: the page keeps its own readers and words for a waiting request, the six reasons a card
//! closes, the two verdicts and the three refusals of an answer, and this test writes the real shapes to
//! `app/tests/fixtures/ai_ask.json` so that a renamed or added member, reason or refusal fails here and in the page's unit
//! tests in the same commit.
//!
//! The document is built from the real types only. No network, no engine. `UPDATE_FIXTURES=1` rewrites the file; without
//! it the file must equal what is built now.

use std::path::PathBuf;

use holzwave_core::ai::service::ask::InputView;
use holzwave_core::ai::service::{AskEvent, AskFault, AskVerdict, AskView, ClosedWhy, FindingView};
use holzwave_core::ai::types::Locality;
use serde_json::{Map, Value, json};

/// Piped text that never belongs in the file. It appears in this test only: a hit anywhere else is a leak.
const CANARY: &str = "zq-golden-ask-3c7e51";

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../app/tests/fixtures/ai_ask.json")
}

fn update_fixtures() -> bool {
    std::env::var("UPDATE_FIXTURES").is_ok_and(|v| v == "1")
}

/// The six reasons a card closes, in declaration order. The match has no wildcard arm, so a reason the core adds does not
/// compile here until the golden (and with it the page's sentence for it) knows about it.
fn closed_reasons() -> Vec<ClosedWhy> {
    let all = [
        ClosedWhy::Sent,
        ClosedWhy::Refused,
        ClosedWhy::Gone,
        ClosedWhy::TimedOut,
        ClosedWhy::PaneClosed,
        ClosedWhy::ShuttingDown,
    ];
    for (at, why) in all.iter().enumerate() {
        let position = match why {
            ClosedWhy::Sent => 0,
            ClosedWhy::Refused => 1,
            ClosedWhy::Gone => 2,
            ClosedWhy::TimedOut => 3,
            ClosedWhy::PaneClosed => 4,
            ClosedWhy::ShuttingDown => 5,
        };
        assert_eq!(position, at, "the list holds every reason, in declaration order");
    }
    all.to_vec()
}

/// The two verdicts, with an exhaustive match.
fn verdicts() -> Vec<AskVerdict> {
    let all = [AskVerdict::Send, AskVerdict::Refuse];
    for verdict in all {
        match verdict {
            AskVerdict::Send | AskVerdict::Refuse => {}
        }
    }
    all.to_vec()
}

/// The refusals of an answer the page can be shown, in alphabetical order of their reasons.
fn refusals() -> Vec<AskFault> {
    vec![AskFault::BadVerdict, AskFault::SealMismatch, AskFault::UnknownAsk]
}

/// A card for pane 3: a cloud destination, the piped text counted with one finding, and a prompt that lost two hidden
/// characters in cleaning.
fn view() -> AskView {
    AskView {
        ask: "0123456789abcdef0123456789abcdef".to_owned(),
        seal: "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210".to_owned(),
        pane: 3,
        prompt: "why does this fail".to_owned(),
        prompt_findings: vec![FindingView { rule: "tag_chars".to_owned(), count: 2 }],
        input: Some(InputView {
            lines: 142,
            bytes: 6254,
            sent_bytes: 5939,
            findings: vec![FindingView { rule: "anthropic_key".to_owned(), count: 1 }],
        }),
        profile: "anthropic".to_owned(),
        model: "example-model".to_owned(),
        host: "api.example.org".to_owned(),
        locality: Locality::Cloud,
    }
}

/// A card for a prompt with nothing piped in, for a destination on this computer.
fn view_prompt_only() -> AskView {
    AskView {
        ask: "00112233445566778899aabbccddeeff".to_owned(),
        seal: "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff".to_owned(),
        pane: 1,
        prompt: "explain the last error".to_owned(),
        prompt_findings: Vec::new(),
        input: None,
        profile: "ollama".to_owned(),
        model: "llama3.2".to_owned(),
        host: "localhost".to_owned(),
        locality: Locality::Loopback,
    }
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

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("the value is JSON")
}

fn document() -> Value {
    let faults: Map<String, Value> =
        refusals().iter().map(|fault| (fault.reason().to_owned(), Value::String(fault.to_string()))).collect();
    sorted(json!({
        "closed": closed_reasons().iter().map(|why| why.as_str()).collect::<Vec<_>>(),
        "events": {
            "closed": to_value(&AskEvent::Closed { ask: view().ask, why: ClosedWhy::TimedOut }),
            "waiting": to_value(&AskEvent::Waiting { view: view() }),
        },
        "faults": faults,
        "verdicts": verdicts().iter().map(|verdict| verdict.as_str()).collect::<Vec<_>>(),
        "view": to_value(&view()),
        "view_prompt_only": to_value(&view_prompt_only()),
    }))
}

fn text_of(document: &Value) -> String {
    let mut text = serde_json::to_string_pretty(document).expect("the document is JSON");
    text.push('\n');
    text
}

/// What the document lacks: every closed reason, every verdict and every refusal the page may show, by name.
fn missing(document: &Value) -> Vec<String> {
    let mut lacking = Vec::new();
    let listed = |name: &str| -> Vec<String> {
        document[name]
            .as_array()
            .map(|list| list.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect())
            .unwrap_or_default()
    };
    let closed = listed("closed");
    for why in closed_reasons() {
        if !closed.iter().any(|name| name == why.as_str()) {
            lacking.push(format!("closed:{}", why.as_str()));
        }
    }
    let said = listed("verdicts");
    for verdict in verdicts() {
        if !said.iter().any(|name| name == verdict.as_str()) {
            lacking.push(format!("verdict:{}", verdict.as_str()));
        }
    }
    for fault in refusals() {
        let sentence = document["faults"][fault.reason()].as_str();
        if sentence != Some(fault.to_string().as_str()) {
            lacking.push(format!("fault:{}", fault.reason()));
        }
    }
    lacking
}

#[test]
fn the_golden_equals_what_the_types_produce() {
    let first = document();
    assert_eq!(first, document(), "the document is built the same way twice");

    let text = text_of(&first);
    assert!(!text.contains(CANARY), "the canary is in the golden");

    let object = first.as_object().expect("the document is an object");
    let members: Vec<&str> = object.keys().map(String::as_str).collect();
    assert_eq!(members, ["closed", "events", "faults", "verdicts", "view", "view_prompt_only"]);
    assert_eq!(first["view"]["input"]["lines"], 142);
    assert!(first["view_prompt_only"]["input"].is_null(), "a card without piped text has a null input");
    assert_eq!(first["events"]["waiting"]["kind"], "waiting");
    assert_eq!(first["events"]["closed"]["kind"], "closed");
    assert_eq!(first["events"]["closed"]["why"], "timed_out");
    let view_members: Vec<&str> = first["view"].as_object().expect("a view").keys().map(String::as_str).collect();
    assert_eq!(
        view_members,
        ["ask", "host", "input", "locality", "model", "pane", "profile", "prompt", "prompt_findings", "seal"]
    );

    let path = golden_path();
    if update_fixtures() {
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("the fixtures directory");
        std::fs::write(&path, text).expect("write ai_ask.json");
        return;
    }
    let on_disk = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is missing ({e}); run with UPDATE_FIXTURES=1", path.display()));
    assert!(
        on_disk == text,
        "ai_ask.json differs from the types of the core; review the change and run with UPDATE_FIXTURES=1"
    );
}

#[test]
fn the_golden_names_every_closed_reason_and_every_refusal_the_page_may_show() {
    let real = document();
    assert_eq!(missing(&real), Vec::<String>::new(), "the document names everything");
    assert_eq!(real["closed"], json!(["sent", "refused", "gone", "timed_out", "pane_closed", "shutting_down"]));
    assert_eq!(real["verdicts"], json!(["send", "refuse"]));
    let faults = real["faults"].as_object().expect("faults");
    assert_eq!(faults.keys().map(String::as_str).collect::<Vec<_>>(), ["bad_verdict", "seal_mismatch", "unknown_ask"]);
    for (reason, sentence) in faults {
        let text = sentence.as_str().expect("a sentence");
        assert!(!text.is_empty() && !text.ends_with('.') && text.is_ascii(), "{reason}: {text}");
    }

    // A reason that is left out, or a sentence that is changed, is detected.
    let mut short = real.clone();
    short["closed"] = json!(["sent", "refused", "gone", "timed_out", "pane_closed"]);
    assert_eq!(missing(&short), ["closed:shutting_down"]);
    let mut gap = real.clone();
    gap["faults"].as_object_mut().expect("faults").remove("seal_mismatch");
    gap["verdicts"] = json!(["send"]);
    assert_eq!(missing(&gap), ["verdict:refuse", "fault:seal_mismatch"]);
    let mut reworded = real;
    reworded["faults"]["unknown_ask"] = json!("some other sentence");
    assert_eq!(missing(&reworded), ["fault:unknown_ask"]);
}
