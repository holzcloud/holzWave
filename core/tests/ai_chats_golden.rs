//! The golden of the chat list: the page reads the summary of a stored chat member by member, and this test writes the
//! real serialised `ConversationSummary` to `app/tests/fixtures/ai_chats.json` so that a renamed, added or removed member
//! fails here and in the page's unit and browser tests in the same commit.
//!
//! `created` and `updated` are Unix seconds: the service stamps them with its clock, whose default is `unix_seconds`, the
//! unit of `Message::ts`. The page tells the age of a row from them in that unit.
//!
//! The document is built from two conversations made in this test through the public API of the store. No network, no
//! engine. `UPDATE_FIXTURES=1` rewrites the file; without it the file must equal what is built now.

use std::path::PathBuf;

use holzwave_core::ai::store::{Conversation, ConversationId};
use serde_json::{Value, json};

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../app/tests/fixtures/ai_chats.json")
}

fn update_fixtures() -> bool {
    std::env::var("UPDATE_FIXTURES").is_ok_and(|v| v == "1")
}

/// A chat pinned to a profile and a model, changed an hour after it was made.
fn pinned() -> Conversation {
    let id = ConversationId::parse("chat-golden").expect("a valid id");
    let mut chat = Conversation::new(id, 1_700_000_000);
    chat.title = "Why is talos-3 NotReady?".to_owned();
    chat.updated = 1_700_003_600;
    chat.profile = Some("local-1".to_owned());
    chat.model = Some("llama3.2".to_owned());
    chat
}

/// A chat that was never pinned: its profile and model are absent.
fn unpinned() -> Conversation {
    let id = ConversationId::parse("chat-bare").expect("a valid id");
    let mut chat = Conversation::new(id, 1_700_000_000);
    chat.title = "Plan the backup job".to_owned();
    chat.updated = 1_700_000_100;
    chat
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
    let summary = serde_json::to_value(pinned().summary()).expect("a summary is JSON");
    let bare = serde_json::to_value(unpinned().summary()).expect("a summary is JSON");
    sorted(json!({ "summary": summary, "bare": bare }))
}

fn text_of(document: &Value) -> String {
    let mut text = serde_json::to_string_pretty(document).expect("the document is JSON");
    text.push('\n');
    text
}

#[test]
fn the_chat_summary_golden_matches_the_real_summary() {
    let first = document();
    let second = document();
    assert_eq!(first, second, "the document is built the same way twice");

    let object = first.as_object().expect("the document is an object");
    let members: Vec<&str> = object.keys().map(String::as_str).collect();
    assert_eq!(members, ["bare", "summary"]);
    for name in ["bare", "summary"] {
        let keys: Vec<&str> = object[name].as_object().expect("a summary is an object").keys().map(String::as_str).collect();
        assert_eq!(keys, ["created", "id", "model", "profile", "title", "updated"], "the members of {name}");
    }
    assert!(object["bare"]["profile"].is_null() && object["bare"]["model"].is_null(), "an absent member is null");
    assert_eq!(object["summary"]["profile"], "local-1");
    assert_eq!(object["summary"]["model"], "llama3.2");
    assert_eq!(object["summary"]["updated"], 1_700_003_600_i64);

    // A list entry carries nothing of the messages.
    let text = text_of(&first);
    for member in ["\"messages\"", "\"parts\"", "\"text\"", "\"context\""] {
        assert!(!text.contains(member), "{member} is in the golden");
    }

    let path = golden_path();
    if update_fixtures() {
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("the fixtures directory");
        std::fs::write(&path, text).expect("write ai_chats.json");
        return;
    }
    let on_disk = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is missing ({e}); run with UPDATE_FIXTURES=1", path.display()));
    assert!(
        on_disk == text,
        "ai_chats.json differs from the summary of the core; review the change and run with UPDATE_FIXTURES=1"
    );
}
