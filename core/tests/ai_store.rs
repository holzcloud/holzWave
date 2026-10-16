//! The conversation store, with no network.
//!
//! A conversation with a tool call and a provider-native turn is saved through `&dyn ConversationStore`, reloaded equal,
//! listed and deleted. The tests prove the privacy defaults (terminal context and tool output reach the disk only as a
//! marker, files are owner-only) by reading the raw bytes of the file, not by trusting the loaded value.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::scratch_dir;
use holzwave_core::ai::store::{
    ContextStorage, Conversation, ConversationId, ConversationStore, JsonDirStore, NOT_STORED,
};
use holzwave_core::ai::types::{
    Consent, ContextKind, ContextNote, Locality, Message, MessageStatus, NoteItem, Opaque, Part, Role, ToolCall,
    ToolResult,
};
use serde_json::{Value, json};

/// Text that must never reach a file, a log line or an error under the default storage.
const CANARY: &str = "CANARY-7f3a";

/// The store over `<dir>/chats`, held the way the service holds it.
fn store(dir: &Path) -> Arc<dyn ConversationStore> {
    Arc::new(JsonDirStore::new(dir.join("chats")))
}

fn id(text: &str) -> ConversationId {
    ConversationId::parse(text).unwrap()
}

/// A conversation with one user message.
fn chat(name: &str, text: &str) -> Conversation {
    let mut c = Conversation::new(id(name), 10);
    c.title = text.to_owned();
    c.messages.push(Message::user_text("m1", text, 11));
    c
}

/// A context note whose item labels, sizes and consent are fixed and whose text is `stored`.
fn note(stored: Option<&str>) -> ContextNote {
    ContextNote {
        hash: "h1".to_owned(),
        items: vec![NoteItem { kind: ContextKind::Screen, label: "visible screen".to_owned(), bytes: 11 }],
        stored: stored.map(str::to_owned),
        consented_to: Consent { provider: "p".to_owned(), locality: Locality::Loopback },
    }
}

fn tool_result(call_id: &str, content: &str, is_error: bool) -> Part {
    Part::ToolResult(ToolResult { call_id: call_id.to_owned(), content: content.to_owned(), is_error })
}

/// Sorted file names of a directory.
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> =
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

/// A conversation as a JSON value; `Conversation` is only `Clone`, so tests compare through this.
fn value(c: &Conversation) -> Value {
    json!({
        "id": c.id.as_str(),
        "title": c.title,
        "created": c.created,
        "updated": c.updated,
        "profile": c.profile,
        "model": c.model,
        "messages": serde_json::to_value(&c.messages).unwrap(),
    })
}

/// A conversation holding private text of every kind the marker rule covers.
fn private_chat(name: &str) -> Conversation {
    let mut c = chat(name, "Explain this");
    let mut context = Message::new("m2", Role::User, vec![Part::Context(note(Some("CANARY-7f3a pane text")))], 12);
    context.status = MessageStatus::Complete;
    c.messages.push(context);
    c.messages.push(Message::new(
        "m3",
        Role::Tool,
        vec![
            tool_result("call_1", "CANARY-7f3a file body", false),
            tool_result("call_2", "CANARY-7f3a error text", true),
        ],
        13,
    ));
    c.messages.push(Message::new("m4", Role::Assistant, vec![Part::Text { text: "It is a prompt.".to_owned() }], 14));
    c
}

fn bytes_of(dir: &Path, name: &str) -> Vec<u8> {
    std::fs::read(dir.join("chats").join(format!("{name}.json"))).unwrap()
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle.as_bytes())
}

// ---- the tracer ---------------------------------------------------------------------------------------------------

#[test]
fn a_chat_is_saved_reloaded_and_deleted() {
    let dir = scratch_dir("store-tracer");
    let store = store(&dir);

    let empty = store.list().unwrap();
    assert!(empty.chats.is_empty() && empty.notes.is_empty());
    assert!(!dir.join("chats").exists(), "listing must not create the directory");

    let mut c = Conversation::new(id("chat-one"), 10);
    c.title = "First chat".to_owned();
    c.updated = 20;
    c.profile = Some("local".to_owned());
    c.model = Some("m1".to_owned());
    c.messages.push(Message::user_text("m1", "Hello", 11));
    let mut reply = Message::new(
        "m2",
        Role::Assistant,
        vec![
            Part::Text { text: "Hi".to_owned() },
            Part::ToolCall(ToolCall { id: "call_1".to_owned(), name: "read_file".to_owned(), args: json!({"path": "a"}) }),
        ],
        12,
    );
    reply.native = Some(Opaque {
        wire: "anthropic".to_owned(),
        json: json!({"content": [{"type": "thinking", "thinking": "t", "signature": "s"}]}),
    });
    c.messages.push(reply);

    store.save(&c, ContextStorage::default()).unwrap();
    let file = dir.join("chats").join("chat-one.json");
    assert!(file.is_file());
    assert_eq!(names(&dir.join("chats")), vec!["chat-one.json"], "no temporary file is left");
    println!("tracer file size: {} bytes", std::fs::metadata(&file).unwrap().len());

    let loaded = store.load(&id("chat-one")).unwrap().expect("the chat was saved");
    assert_eq!(value(&loaded), value(&c));

    let listing = store.list().unwrap();
    assert!(listing.notes.is_empty());
    assert_eq!(listing.chats.len(), 1);
    let summary = &listing.chats[0];
    assert_eq!(summary.id, id("chat-one"));
    assert_eq!(summary.title, "First chat");
    assert_eq!(summary.profile.as_deref(), Some("local"));
    assert_eq!(summary.model.as_deref(), Some("m1"));
    assert_eq!((summary.created, summary.updated), (10, 20));

    assert!(store.delete(&id("chat-one")).unwrap());
    assert!(!file.exists());
    assert_eq!(store.load(&id("chat-one")).unwrap().map(|c| value(&c)), None);
    assert!(store.list().unwrap().chats.is_empty());
    assert!(!store.delete(&id("chat-one")).unwrap(), "a second delete finds nothing");
    assert!(store.load(&id("never-saved")).unwrap().is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

// ---- the storage policy -------------------------------------------------------------------------------------------

#[test]
fn terminal_context_and_tool_output_are_stored_as_a_marker_by_default() {
    let dir = scratch_dir("store-marker");
    let store = store(&dir);
    let c = private_chat("marker");
    store.save(&c, ContextStorage::default()).unwrap();

    let raw = bytes_of(&dir, "marker");
    assert!(!contains(&raw, CANARY), "the raw file must hold no private text");

    let loaded = store.load(&id("marker")).unwrap().unwrap();
    let Part::Context(kept) = &loaded.messages[1].parts[0] else { panic!("the context part is kept") };
    assert_eq!(kept.stored, None);
    let original = note(None);
    assert_eq!(kept.hash, original.hash);
    assert_eq!(kept.items, original.items);
    assert_eq!(kept.consented_to, original.consented_to);

    let parts = &loaded.messages[2].parts;
    assert_eq!(parts.len(), 2);
    let results: Vec<&ToolResult> = parts
        .iter()
        .map(|p| match p {
            Part::ToolResult(r) => r,
            other => panic!("a tool result was expected, got {other:?}"),
        })
        .collect();
    assert_eq!((results[0].call_id.as_str(), results[0].content.as_str(), results[0].is_error), ("call_1", NOT_STORED, false));
    assert_eq!((results[1].call_id.as_str(), results[1].content.as_str(), results[1].is_error), ("call_2", NOT_STORED, true));

    assert_eq!(loaded.messages[0].text(), "Explain this");
    assert_eq!(loaded.messages[3].text(), "It is a prompt.");

    // The rule is idempotent: saving what was loaded changes no byte.
    store.save(&loaded, ContextStorage::default()).unwrap();
    assert_eq!(bytes_of(&dir, "marker"), raw);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn context_text_is_kept_only_when_asked() {
    let dir = scratch_dir("store-text");
    let store = store(&dir);
    let c = private_chat("kept");
    store.save(&c, ContextStorage::Text).unwrap();

    assert!(contains(&bytes_of(&dir, "kept"), CANARY));
    let loaded = store.load(&id("kept")).unwrap().unwrap();
    assert_eq!(value(&loaded), value(&c));
    let Part::Context(kept) = &loaded.messages[1].parts[0] else { panic!("the context part is kept") };
    assert_eq!(kept.stored.as_deref(), Some("CANARY-7f3a pane text"));
    std::fs::remove_dir_all(dir).unwrap();
}

// ---- privacy and delete -------------------------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn the_chat_file_and_its_directory_are_private_to_the_owner() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch_dir("store-mode");
    let store = store(&dir);
    let c = chat("private", "hello");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

    store.save(&c, ContextStorage::default()).unwrap();
    let file = dir.join("chats").join("private.json");
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(&dir.join("chats")), 0o700);

    store.save(&c, ContextStorage::default()).unwrap();
    assert_eq!(mode(&file), 0o600, "a second save over the file keeps it private");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn delete_removes_every_copy_of_the_chat() {
    let dir = scratch_dir("store-delete");
    let store = store(&dir);
    store.save(&chat("gone", "a"), ContextStorage::default()).unwrap();
    store.save(&chat("stays", "b"), ContextStorage::default()).unwrap();
    let chats = dir.join("chats");
    std::fs::write(chats.join("gone.json.tmp"), b"half").unwrap();
    std::fs::write(chats.join("gone.json.bad"), b"kept aside").unwrap();
    std::fs::write(chats.join("notes.txt"), b"unrelated").unwrap();

    assert!(store.delete(&id("gone")).unwrap());
    assert_eq!(names(&chats), vec!["notes.txt", "stays.json"]);

    // Only the leftovers exist: delete still reports that it removed something.
    std::fs::write(chats.join("only.json.tmp"), b"half").unwrap();
    std::fs::write(chats.join("only.json.bad"), b"kept aside").unwrap();
    assert!(store.delete(&id("only")).unwrap());
    assert_eq!(names(&chats), vec!["notes.txt", "stays.json"]);
    std::fs::remove_dir_all(dir).unwrap();
}
