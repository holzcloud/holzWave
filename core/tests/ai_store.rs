//! The conversation store, with no network.
//!
//! A conversation with a tool call and a provider-native turn is saved through `&dyn ConversationStore`, reloaded equal,
//! listed and deleted. The tests prove the privacy defaults (terminal context and tool output reach the disk only as a
//! marker, files are owner-only) by reading the raw bytes of the file, not by trusting the loaded value.

mod common;

use std::path::Path;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::scratch_dir;
use holzwave_core::ai::provider::MAX_DETAIL;
use holzwave_core::ai::store::{
    ContextStorage, Conversation, ConversationId, ConversationStore, JsonDirStore, MAX_CHAT_BYTES, MAX_CHATS, MAX_ID_LEN,
    MAX_LABEL, MAX_MESSAGES, MemoryStore, NOT_STORED, StoreError,
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

// ---- helpers for planted files ------------------------------------------------------------------------------------

/// The chat directory, created.
fn chats_dir(dir: &Path) -> std::path::PathBuf {
    let chats = dir.join("chats");
    std::fs::create_dir_all(&chats).unwrap();
    chats
}

/// Plants a file in the chat directory under `name`.
fn plant(dir: &Path, name: &str, bytes: &[u8]) {
    std::fs::write(chats_dir(dir).join(name), bytes).unwrap();
}

/// A version 1 chat document with the given id, for planting.
fn document(id: &str) -> Value {
    json!({"version": 1, "id": id, "title": "T", "created": 1, "updated": 2, "messages": []})
}

fn damage_reason(error: StoreError) -> String {
    match error {
        StoreError::Damaged { reason, .. } => reason,
        other => panic!("expected Damaged, got {other:?}"),
    }
}

// ---- ids ----------------------------------------------------------------------------------------------------------

#[test]
fn ids_are_checked_at_parse_deserialize_and_for_device_names() {
    let long = "a".repeat(MAX_ID_LEN);
    for good in ["a", "chat-1", "Chat_One-2", long.as_str(), "com10", "console", "nulx"] {
        assert_eq!(ConversationId::parse(good).unwrap().as_str(), good);
    }
    let too_long = "a".repeat(MAX_ID_LEN + 1);
    for bad in [
        "", too_long.as_str(), "../x", "a/b", "a\\b", "a.b", ".hidden", "a b", "\u{e9}", "a\0b", "NUL", "nul", "Con", "aux",
        "PRN", "com1", "COM9", "Lpt1", "lpt9",
    ] {
        assert_eq!(ConversationId::parse(bad), Err(StoreError::InvalidId), "{bad:?} must be refused");
    }
    assert!(serde_json::from_str::<ConversationId>("\"../x\"").is_err());
    assert!(serde_json::from_str::<ConversationId>("\"NUL\"").is_err());
    assert_eq!(serde_json::from_str::<ConversationId>("\"chat-1\"").unwrap(), id("chat-1"));
    assert_eq!(serde_json::to_string(&id("chat-1")).unwrap(), "\"chat-1\"");
}

#[test]
fn generated_ids_are_random_hex_that_parse_back() {
    let (a, b) = (ConversationId::generate().unwrap(), ConversationId::generate().unwrap());
    assert_ne!(a, b);
    for generated in [&a, &b] {
        let text = generated.as_str();
        assert_eq!(text.len(), 32);
        assert!(text.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)), "{text}");
        assert_eq!(&ConversationId::parse(text).unwrap(), generated);
    }
}

// ---- limits -------------------------------------------------------------------------------------------------------

#[test]
fn an_oversize_chat_is_refused_on_save() {
    let dir = scratch_dir("store-big");
    let store = store(&dir);
    let big = chat("big", &"x".repeat(9 << 20));
    match store.save(&big, ContextStorage::default()) {
        Err(StoreError::TooLarge { bytes, max }) => {
            assert_eq!(max, MAX_CHAT_BYTES);
            assert!(bytes > max);
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }
    assert!(!dir.join("chats").exists(), "a refused save must not touch the disk");

    // A refused save over an existing chat leaves the old one as it was.
    store.save(&chat("big", "small"), ContextStorage::default()).unwrap();
    let before = bytes_of(&dir, "big");
    assert!(matches!(store.save(&big, ContextStorage::default()), Err(StoreError::TooLarge { .. })));
    assert_eq!(bytes_of(&dir, "big"), before);
    assert_eq!(names(&dir.join("chats")), vec!["big.json"]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn too_many_messages_or_a_long_label_is_refused() {
    let dir = scratch_dir("store-limits");
    let store = store(&dir);

    let mut many = chat("many", "x");
    many.messages = (0..=MAX_MESSAGES).map(|n| Message::user_text(format!("m{n}"), "x", 1)).collect();
    assert_eq!(many.messages.len(), MAX_MESSAGES + 1);
    assert!(matches!(store.save(&many, ContextStorage::default()), Err(StoreError::Invalid(_))));
    many.messages.pop();
    store.save(&many, ContextStorage::default()).unwrap();
    assert_eq!(store.load(&id("many")).unwrap().unwrap().messages.len(), MAX_MESSAGES);

    let mut labelled = chat("labelled", "x");
    labelled.profile = Some("p".repeat(MAX_LABEL + 1));
    assert!(matches!(store.save(&labelled, ContextStorage::default()), Err(StoreError::Invalid(_))));
    labelled.profile = None;
    labelled.model = Some("m".repeat(MAX_LABEL + 1));
    assert!(matches!(store.save(&labelled, ContextStorage::default()), Err(StoreError::Invalid(_))));
    labelled.model = Some("bad\nmodel".to_owned());
    assert!(matches!(store.save(&labelled, ContextStorage::default()), Err(StoreError::Invalid(_))));
    labelled.model = Some("m".repeat(MAX_LABEL));
    labelled.profile = Some("p".repeat(MAX_LABEL));
    store.save(&labelled, ContextStorage::default()).unwrap();
    assert_eq!(names(&dir.join("chats")), vec!["labelled.json", "many.json"]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_number_of_chats_is_bounded_but_an_existing_chat_can_still_be_saved() {
    let dir = scratch_dir("store-count");
    let store = JsonDirStore::new(dir.join("chats")).with_max_chats(3);
    for name in ["a", "b", "c"] {
        store.save(&chat(name, name), ContextStorage::default()).unwrap();
    }
    assert_eq!(store.save(&chat("d", "d"), ContextStorage::default()), Err(StoreError::Full { max: 3 }));
    assert!(!dir.join("chats").join("d.json").exists());
    store.save(&chat("b", "changed"), ContextStorage::default()).unwrap();
    assert_eq!(store.load(&id("b")).unwrap().unwrap().title, "changed");
    assert!(store.delete(&id("a")).unwrap());
    store.save(&chat("d", "d"), ContextStorage::default()).unwrap();

    // Zero behaves as one.
    let one = JsonDirStore::new(dir.join("one")).with_max_chats(0);
    one.save(&chat("a", "a"), ContextStorage::default()).unwrap();
    assert_eq!(one.save(&chat("b", "b"), ContextStorage::default()), Err(StoreError::Full { max: 1 }));

    // A value above the cap behaves as the cap: fill a directory to it by name and ask for one more.
    let many = scratch_dir("store-count-cap");
    for n in 0..MAX_CHATS {
        plant(&many, &format!("c{n}.json"), b"");
    }
    let capped = JsonDirStore::new(many.join("chats")).with_max_chats(MAX_CHATS * 10);
    assert_eq!(capped.save(&chat("extra", "x"), ContextStorage::default()), Err(StoreError::Full { max: MAX_CHATS }));
    std::fs::remove_dir_all(many).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

// ---- hostile and damaged files ------------------------------------------------------------------------------------

#[test]
fn an_oversize_file_is_never_read_and_is_moved_aside() {
    let dir = scratch_dir("store-oversize");
    let chats = chats_dir(&dir);
    let big = std::fs::File::create(chats.join("big.json")).unwrap();
    big.set_len(16 << 20).unwrap();
    drop(big);
    let store = store(&dir);

    let reason = damage_reason(store.load(&id("big")).unwrap_err());
    assert!(reason.contains(&MAX_CHAT_BYTES.to_string()), "{reason}");
    assert!(!chats.join("big.json").exists());
    assert_eq!(std::fs::metadata(chats.join("big.json.bad")).unwrap().len(), 16 << 20);
    assert!(store.load(&id("big")).unwrap().is_none());

    store.save(&chat("big", "again"), ContextStorage::default()).unwrap();
    assert_eq!(store.load(&id("big")).unwrap().unwrap().title, "again");
    assert!(chats.join("big.json.bad").exists(), "the kept copy stays");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_damaged_chat_is_moved_aside_and_its_bytes_are_kept() {
    let dir = scratch_dir("store-damaged");
    let chats = chats_dir(&dir);
    plant(&dir, "junk.json", b"{ not json");
    plant(&dir, "junk2.json", b"{ not json");
    let store = store(&dir);

    let reason = damage_reason(store.load(&id("junk")).unwrap_err());
    assert!(reason.contains("junk.json.bad"), "{reason}");
    assert!(!chats.join("junk.json").exists());
    assert_eq!(std::fs::read(chats.join("junk.json.bad")).unwrap(), b"{ not json");
    assert!(store.load(&id("junk")).unwrap().is_none());

    let listing = store.list().unwrap();
    assert!(listing.chats.is_empty());
    assert_eq!(listing.notes.len(), 1, "{:?}", listing.notes);
    assert!(listing.notes[0].starts_with("junk2: "), "{:?}", listing.notes);
    assert!(!chats.join("junk2.json").exists());
    assert_eq!(std::fs::read(chats.join("junk2.json.bad")).unwrap(), b"{ not json");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_reason_for_damage_never_quotes_the_file() {
    let dir = scratch_dir("store-quote");
    let store = store(&dir);

    let mut wrong_type = document("c1");
    wrong_type["created"] = json!(CANARY);
    let mut unknown_field = document("c1");
    unknown_field[CANARY] = json!(1);
    let mut bad_id = document("c1");
    bad_id["id"] = json!(format!("{CANARY}/../x"));
    let mut wrong_id = document("c1");
    wrong_id["id"] = json!(CANARY);
    let mut wrong_title_type = document("c1");
    wrong_title_type["title"] = json!({ CANARY: CANARY });

    for (n, doc) in [wrong_type, unknown_field, bad_id, wrong_id, wrong_title_type].iter().enumerate() {
        plant(&dir, "c1.json", doc.to_string().as_bytes());
        let error = store.load(&id("c1")).unwrap_err();
        let shown = error.to_string();
        let debugged = format!("{error:?}");
        let reason = damage_reason(error);
        assert!(!reason.contains(CANARY) && !shown.contains(CANARY) && !debugged.contains(CANARY), "case {n}: {reason}");
        assert!(reason.contains("line") && reason.contains("column") || reason.contains("id does not match"), "{reason}");
        std::fs::remove_file(dir.join("chats").join("c1.json.bad")).unwrap();
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_file_that_does_not_match_its_name_or_the_format_is_damaged() {
    let dir = scratch_dir("store-format");
    let chats = chats_dir(&dir);
    let store = store(&dir);

    let mut version_zero = document("c1");
    version_zero["version"] = json!(0);
    let mut no_version = document("c1");
    no_version.as_object_mut().unwrap().remove("version");
    let mut extra_field = document("c1");
    extra_field["extra"] = json!(true);
    let other_id = document("someone-else");
    let mut many = document("c1");
    many["messages"] = Value::Array(vec![serde_json::to_value(Message::user_text("m", "x", 1)).unwrap(); MAX_MESSAGES + 1]);

    for (case, doc) in [
        ("version 0", version_zero),
        ("no version", no_version),
        ("an unknown field", extra_field),
        ("a different id", other_id),
        ("too many messages", many),
    ] {
        let bytes = doc.to_string().into_bytes();
        plant(&dir, "c1.json", &bytes);
        assert!(matches!(store.load(&id("c1")), Err(StoreError::Damaged { .. })), "{case}");
        assert!(!chats.join("c1.json").exists(), "{case}: the file is moved");
        assert_eq!(std::fs::read(chats.join("c1.json.bad")).unwrap(), bytes, "{case}: its bytes are kept");
    }

    // A document that is not an object is damage as well.
    plant(&dir, "c1.json", b"[1, \"c1\", \"T\", 1, 2, null, null, []]");
    assert!(matches!(store.load(&id("c1")), Err(StoreError::Damaged { .. })));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_newer_version_is_neither_read_moved_nor_overwritten() {
    let dir = scratch_dir("store-newer");
    let chats = chats_dir(&dir);
    let bytes = br#"{"version":2,"anything":true}"#;
    plant(&dir, "future.json", bytes);
    let store = store(&dir);

    match store.load(&id("future")) {
        Err(StoreError::Newer { found: 2, supported: 1, id: found_id }) => assert_eq!(found_id, id("future")),
        other => panic!("expected Newer, got {other:?}"),
    }
    let listing = store.list().unwrap();
    assert!(listing.chats.is_empty());
    assert_eq!(listing.notes.len(), 1);
    assert!(listing.notes[0].starts_with("future: ") && listing.notes[0].contains("2"), "{:?}", listing.notes);
    assert_eq!(std::fs::read(chats.join("future.json")).unwrap(), bytes);
    assert_eq!(names(&chats), vec!["future.json"], "no .bad file appears");
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn a_symlink_named_like_a_chat_is_not_followed() {
    let dir = scratch_dir("store-symlink");
    let chats = chats_dir(&dir);
    let target = dir.join("secret.txt");
    std::fs::write(&target, b"SECRET-TARGET-TEXT").unwrap();
    std::os::unix::fs::symlink(&target, chats.join("evil.json")).unwrap();
    let store = store(&dir);

    let reason = damage_reason(store.load(&id("evil")).unwrap_err());
    assert_eq!(reason, "not a regular file");
    assert!(std::fs::symlink_metadata(chats.join("evil.json")).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read(&target).unwrap(), b"SECRET-TARGET-TEXT");

    let listing = store.list().unwrap();
    assert!(listing.chats.is_empty());
    assert_eq!(listing.notes, vec!["evil: not a regular file".to_owned()]);

    assert!(store.delete(&id("evil")).unwrap());
    assert!(std::fs::symlink_metadata(chats.join("evil.json")).is_err(), "the link is gone");
    assert_eq!(std::fs::read(&target).unwrap(), b"SECRET-TARGET-TEXT", "the target is untouched");
    std::fs::remove_dir_all(dir).unwrap();
}

// ---- what is written ----------------------------------------------------------------------------------------------

#[test]
fn titles_are_scrubbed_of_control_and_bidi_characters() {
    let dir = scratch_dir("store-title");
    let store = store(&dir);
    let hostile = format!("bad\u{1b}[2J\nline\u{202e}rtl\u{200b}zw\u{e0041}tag {}", "w".repeat(500));
    let mut c = chat("title", "x");
    c.title = hostile;
    store.save(&c, ContextStorage::default()).unwrap();

    let title = store.load(&id("title")).unwrap().unwrap().title;
    assert!(
        !title.chars().any(|ch| ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{200b}'..='\u{200f}' | '\u{e0000}'..='\u{e007f}')),
        "{title:?}"
    );
    assert!(!title.contains("  "), "white space is collapsed");
    assert!(title.chars().count() <= MAX_DETAIL + 1, "{} characters", title.chars().count());
    assert!(title.starts_with("bad [2J line rtl zw tag"), "{title:?}");

    // The scrub is idempotent, so a saved chat saved again changes no byte.
    let first = bytes_of(&dir, "title");
    store.save(&store.load(&id("title")).unwrap().unwrap(), ContextStorage::default()).unwrap();
    assert_eq!(bytes_of(&dir, "title"), first);

    let mut plain = chat("plain", "x");
    plain.title = "Fix the build in the CI pipeline".to_owned();
    store.save(&plain, ContextStorage::default()).unwrap();
    assert_eq!(store.load(&id("plain")).unwrap().unwrap().title, "Fix the build in the CI pipeline");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_chat_file_shape_is_pinned() {
    // A change to these literals is a change of the file format: it needs a new version and a migration in the store
    // module, not an edited test.
    let dir = scratch_dir("store-shape");
    let store = store(&dir);

    let mut c = Conversation::new(id("c1"), 1);
    c.title = "T".to_owned();
    c.updated = 2;
    store.save(&c, ContextStorage::default()).unwrap();
    assert_eq!(
        String::from_utf8(bytes_of(&dir, "c1")).unwrap(),
        "{\"version\":1,\"id\":\"c1\",\"title\":\"T\",\"created\":1,\"updated\":2,\"messages\":[]}\n"
    );

    let message = Message::user_text("m1", "hi", 3);
    c.profile = Some("p".to_owned());
    c.model = Some("m".to_owned());
    c.messages.push(message.clone());
    store.save(&c, ContextStorage::default()).unwrap();
    let expected = format!(
        "{{\"version\":1,\"id\":\"c1\",\"title\":\"T\",\"created\":1,\"updated\":2,\"profile\":\"p\",\"model\":\"m\",\"messages\":[{}]}}\n",
        serde_json::to_string(&message).unwrap()
    );
    assert_eq!(String::from_utf8(bytes_of(&dir, "c1")).unwrap(), expected);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn list_reports_what_it_skipped() {
    let dir = scratch_dir("store-skipped");
    let chats = chats_dir(&dir);
    let store = store(&dir);
    store.save(&chat("fine", "fine"), ContextStorage::default()).unwrap();
    plant(&dir, "junk.json", b"{ not json");
    plant(&dir, "future.json", br#"{"version":9}"#);
    let mut quoted = document("quoted");
    quoted["created"] = json!(CANARY);
    plant(&dir, "quoted.json", quoted.to_string().as_bytes());
    std::fs::create_dir(chats.join("folder.json")).unwrap();

    let listing = store.list().unwrap();
    assert_eq!(listing.chats.len(), 1);
    assert_eq!(listing.chats[0].id, id("fine"));
    let mut notes = listing.notes.clone();
    notes.sort();
    assert_eq!(notes.len(), 4, "{notes:?}");
    for (note, prefix) in notes.iter().zip(["folder: ", "future: ", "junk: ", "quoted: "]) {
        assert!(note.starts_with(prefix), "{note}");
        assert!(!note.contains(CANARY));
    }
    assert!(notes[0].ends_with("not a regular file"));
    assert!(chats.join("folder.json").is_dir(), "a directory is reported, not removed");
    std::fs::remove_dir_all(dir).unwrap();

    // A directory with more chat files than the limit is not read beyond it.
    let many = scratch_dir("store-skipped-many");
    for n in 0..=MAX_CHATS {
        plant(&many, &format!("c{n}.json"), b"");
    }
    let listing = JsonDirStore::new(many.join("chats")).list().unwrap();
    assert_eq!(listing.notes.len(), MAX_CHATS + 1);
    assert!(listing.notes.iter().any(|n| n.starts_with("more than ")), "{:?}", listing.notes.last());
    std::fs::remove_dir_all(many).unwrap();
}

// ---- listing and scrubbing ----------------------------------------------------------------------------------------

/// Name, bytes and modification time of every regular file in `dir`.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>, std::time::SystemTime)> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let e = e.unwrap();
            let meta = e.metadata().unwrap();
            meta.is_file().then(|| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap(), meta.modified().unwrap()))
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

#[test]
fn list_orders_newest_first_and_ignores_files_that_are_not_chats() {
    let dir = scratch_dir("store-order");
    let chats = chats_dir(&dir);
    let store = store(&dir);
    for (name, updated) in [("b", 5), ("c", 9), ("a", 9)] {
        let mut c = chat(name, name);
        c.updated = updated;
        store.save(&c, ContextStorage::default()).unwrap();
    }
    for ignored in ["notes.txt", "x.json.tmp", "y.json.bad", "bad id.json", "..json"] {
        std::fs::write(chats.join(ignored), b"not a chat").unwrap();
    }
    std::fs::create_dir(chats.join("dir.json")).unwrap();
    let before = snapshot(&chats);

    let listing = store.list().unwrap();
    let order: Vec<&str> = listing.chats.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(order, ["a", "c", "b"]);
    assert_eq!(listing.notes.len(), 1, "{:?}", listing.notes);
    assert!(listing.notes[0].starts_with("dir: "), "{:?}", listing.notes);
    assert_eq!(snapshot(&chats), before, "a listing modifies no file");
    assert!(chats.join("dir.json").is_dir());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn scrubbing_rewrites_only_chats_that_hold_private_text() {
    let dir = scratch_dir("store-scrub");
    let chats = chats_dir(&dir);
    let store = store(&dir);

    let with_note = private_chat("with-note");
    let mut with_output = chat("with-output", "run it");
    with_output.messages.push(Message::new("m2", Role::Tool, vec![tool_result("call_1", "CANARY-7f3a output", false)], 12));
    let clean = chat("clean", "nothing private");
    for c in [&with_note, &with_output, &clean] {
        store.save(c, ContextStorage::Text).unwrap();
    }
    plant(&dir, "damaged.json", b"{ not json");
    plant(&dir, "future.json", br#"{"version":3}"#);
    std::fs::write(chats.join("old.json.bad"), b"kept aside").unwrap();
    assert!(contains(&bytes_of(&dir, "with-note"), CANARY) && contains(&bytes_of(&dir, "with-output"), CANARY));

    let clean_before = (bytes_of(&dir, "clean"), std::fs::metadata(chats.join("clean.json")).unwrap().modified().unwrap());
    std::thread::sleep(Duration::from_millis(30));

    let scrub = store.scrub_private_text().unwrap();
    assert_eq!(scrub.rewritten, 2);
    assert_eq!(scrub.notes.len(), 2, "{:?}", scrub.notes);
    for name in ["with-note", "with-output", "clean"] {
        assert!(!contains(&bytes_of(&dir, name), CANARY), "{name}");
    }
    let clean_after = (bytes_of(&dir, "clean"), std::fs::metadata(chats.join("clean.json")).unwrap().modified().unwrap());
    assert_eq!(clean_after, clean_before, "a chat without private text keeps its bytes and its time");

    let loaded = store.load(&id("with-note")).unwrap().unwrap();
    assert_eq!(loaded.messages[0].text(), "Explain this");
    assert_eq!(loaded.messages[3].text(), "It is a prompt.");
    let Part::Context(kept) = &loaded.messages[1].parts[0] else { panic!("the context part is kept") };
    assert_eq!(kept.stored, None);
    let Part::ToolResult(output) = &store.load(&id("with-output")).unwrap().unwrap().messages[1].parts[0] else {
        panic!("the tool result is kept")
    };
    assert_eq!(output.content, NOT_STORED);

    assert_eq!(std::fs::read(chats.join("future.json")).unwrap(), br#"{"version":3}"#, "a newer file is left alone");
    assert_eq!(std::fs::read(chats.join("damaged.json.bad")).unwrap(), b"{ not json");
    assert_eq!(std::fs::read(chats.join("old.json.bad")).unwrap(), b"kept aside", "a kept-aside copy is not touched");

    assert_eq!(store.scrub_private_text().unwrap().rewritten, 0, "the second call has nothing to do");
    std::fs::remove_dir_all(dir).unwrap();
}

// ---- concurrency --------------------------------------------------------------------------------------------------

#[test]
fn concurrent_saves_of_one_chat_leave_one_whole_file() {
    let dir = scratch_dir("store-concurrent");
    let store = Arc::new(JsonDirStore::new(dir.join("chats")));
    let mut handles = Vec::new();
    for thread in 0..8usize {
        let store = Arc::clone(&store);
        handles.push(std::thread::spawn(move || {
            let mut counts = Vec::new();
            for round in 0..20usize {
                let count = 1 + (thread * 20 + round) % 160;
                let mut c = chat("shared", "shared");
                c.messages = (0..count).map(|n| Message::user_text(format!("m{n}"), "x", 1)).collect();
                store.save(&c, ContextStorage::default()).expect("no save may fail");
                counts.push(count);
            }
            counts
        }));
    }
    let saved: BTreeSet<usize> = handles.into_iter().flat_map(|h| h.join().expect("no thread may panic")).collect();
    assert_eq!(names(&dir.join("chats")), vec!["shared.json"], "no temporary file is left");
    let loaded = store.load(&id("shared")).unwrap().expect("the chat exists");
    assert!(saved.contains(&loaded.messages.len()), "{} is not a saved count", loaded.messages.len());
    std::fs::remove_dir_all(dir).unwrap();
}

// ---- the contract both stores meet --------------------------------------------------------------------------------

/// What every store must do, run through the trait object. `max_chats` is the limit the store was built with.
fn contract(store: &dyn ConversationStore, max_chats: usize) {
    assert_eq!(max_chats, 3);
    let none = store.list().unwrap();
    assert!(none.chats.is_empty() && none.notes.is_empty());
    assert!(store.load(&id("unknown")).unwrap().is_none());

    // Save, load equal, list.
    let mut a = chat("a", "first");
    a.profile = Some("local".to_owned());
    a.model = Some("m1".to_owned());
    a.updated = 20;
    store.save(&a, ContextStorage::default()).unwrap();
    assert_eq!(value(&store.load(&id("a")).unwrap().unwrap()), value(&a));
    let listing = store.list().unwrap();
    assert_eq!(listing.chats.len(), 1);
    assert_eq!((listing.chats[0].title.as_str(), listing.chats[0].updated), ("first", 20));

    // A second save of the same id changes the title and the messages.
    a.title = "renamed".to_owned();
    a.updated = 30;
    a.messages.push(Message::user_text("m2", "more", 31));
    store.save(&a, ContextStorage::default()).unwrap();
    let listing = store.list().unwrap();
    assert_eq!(listing.chats.len(), 1);
    assert_eq!((listing.chats[0].title.as_str(), listing.chats[0].updated), ("renamed", 30));
    assert_eq!(store.load(&id("a")).unwrap().unwrap().messages.len(), 2);

    // The storage policy.
    let private = private_chat("p");
    store.save(&private, ContextStorage::default()).unwrap();
    let marked = store.load(&id("p")).unwrap().unwrap();
    let Part::Context(note_back) = &marked.messages[1].parts[0] else { panic!("context part") };
    assert_eq!(note_back.stored, None);
    let Part::ToolResult(result_back) = &marked.messages[2].parts[0] else { panic!("tool result") };
    assert_eq!(result_back.content, NOT_STORED);
    store.save(&private, ContextStorage::Text).unwrap();
    assert_eq!(value(&store.load(&id("p")).unwrap().unwrap()), value(&private));

    // Limits.
    assert!(matches!(store.save(&chat("big", &"x".repeat(9 << 20)), ContextStorage::default()), Err(StoreError::TooLarge { .. })));
    let mut many = chat("many", "x");
    many.messages = (0..=MAX_MESSAGES).map(|n| Message::user_text(format!("m{n}"), "x", 1)).collect();
    assert!(matches!(store.save(&many, ContextStorage::default()), Err(StoreError::Invalid(_))));

    // The number of chats.
    store.save(&chat("q", "q"), ContextStorage::default()).unwrap();
    assert_eq!(store.save(&chat("r", "r"), ContextStorage::default()), Err(StoreError::Full { max: 3 }));
    store.save(&a, ContextStorage::default()).unwrap();

    // Scrubbing brings the stored text of "p" (saved with Text above) into the marker form.
    let scrub = store.scrub_private_text().unwrap();
    assert_eq!(scrub.rewritten, 1);
    assert!(scrub.notes.is_empty());
    let Part::Context(note_back) = &store.load(&id("p")).unwrap().unwrap().messages[1].parts[0] else { panic!("context part") };
    assert_eq!(note_back.stored, None);

    // Delete.
    assert!(store.delete(&id("q")).unwrap());
    assert!(!store.delete(&id("q")).unwrap());
    assert!(store.load(&id("q")).unwrap().is_none());
    store.save(&chat("r", "r"), ContextStorage::default()).unwrap();
}

#[test]
fn the_json_dir_store_meets_the_contract() {
    let dir = scratch_dir("store-contract-dir");
    contract(&JsonDirStore::new(dir.join("chats")).with_max_chats(3), 3);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_memory_store_meets_the_contract() {
    contract(&MemoryStore::new().with_max_chats(3), 3);
}

#[test]
fn the_memory_store_scrubs_and_marks_exactly_like_the_file_store() {
    let dir = scratch_dir("store-same");
    let file_store = JsonDirStore::new(dir.join("chats"));
    let memory = MemoryStore::new();

    let mut c = private_chat("same");
    c.title = "line one\nline \u{1b}[31mtwo\u{202e}".to_owned();
    for store in [&file_store as &dyn ConversationStore, &memory] {
        store.save(&c, ContextStorage::default()).unwrap();
    }
    let from_file = file_store.load(&id("same")).unwrap().unwrap();
    let from_memory = memory.load(&id("same")).unwrap().unwrap();
    assert_eq!(value(&from_memory), value(&from_file));
    assert_eq!(from_memory.title, "line one line [31mtwo");
    let encoded = serde_json::to_string(&from_memory.messages).unwrap();
    assert!(!encoded.contains(CANARY), "the double drops stored context too");
    assert_eq!(
        serde_json::to_value(&memory.list().unwrap().chats).unwrap(),
        serde_json::to_value(&file_store.list().unwrap().chats).unwrap()
    );
    std::fs::remove_dir_all(dir).unwrap();
}

// ---- Debug and sync -----------------------------------------------------------------------------------------------

#[test]
fn debug_output_never_shows_chat_text() {
    let dir = scratch_dir("store-debug");
    let mut c = private_chat("dbg");
    c.title = format!("{CANARY} title");
    c.messages[0] = Message::user_text("m1", format!("{CANARY} user text"), 11);
    let summary = c.summary();
    let listing = holzwave_core::ai::store::Listing { chats: vec![summary.clone()], notes: vec!["x: not valid JSON".to_owned()] };
    let file_store = JsonDirStore::new(dir.join("chats"));
    let memory = MemoryStore::new();
    file_store.save(&c, ContextStorage::Text).unwrap();
    memory.save(&c, ContextStorage::Text).unwrap();

    let errors = [
        StoreError::Damaged { id: id("dbg"), reason: "not valid JSON at line 1 column 3".to_owned() },
        StoreError::Newer { id: id("dbg"), found: 2, supported: 1 },
        StoreError::Invalid("too many messages"),
    ];
    let mut shown = vec![
        format!("{c:?}"), format!("{c:#?}"), format!("{summary:?}"), format!("{summary:#?}"), format!("{listing:?}"),
        format!("{listing:#?}"), format!("{file_store:?}"), format!("{file_store:#?}"), format!("{memory:?}"),
        format!("{memory:#?}"),
    ];
    for error in &errors {
        shown.push(format!("{error:?}"));
        shown.push(format!("{error}"));
    }
    for text in &shown {
        assert!(!text.contains(CANARY), "{text}");
    }
    assert!(shown[0].contains("messages: 4"), "{}", shown[0]);
    assert!(shown[0].contains("dbg"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn chats_are_not_carried_by_the_sync() {
    use holzwave_core::sync::engine::allowed;
    assert!(!allowed("chats/chat-one.json"));
    assert!(!allowed("chats.chat-one.json"));
    assert!(!allowed("chat-one.json"));
}
