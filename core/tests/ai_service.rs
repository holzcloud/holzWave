//! The AI service, proved headless against an in-process server and the scripted provider, with no outside network.
//!
//! A profile and its key sit in `ai.json` and a test keychain, the service reads them when it is built, and a message is
//! sent through the whole chain: the turn thread, the profile resolution, the Chat Completions adapter, the chokepoint,
//! the fake server, the agent loop, the sequence-numbered events and the store. What is proved is the contract the shell
//! and the page rely on: the exact shape of an event and of a snapshot, the one rule set that folds both, what is sent
//! (exactly one request per message, nothing at construction), what is stored, and what happens when a turn cannot
//! start or a save fails. Every wait is bounded.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use common::fake_http::{FakeHttp, Reply};
use holzwave_core::ai::profiles::{AiConfig, KeySlot, KeyVault, Preset, ProfileInput};
use holzwave_core::ai::service::events::{
    AiEvent, AiEventKind, ApplyResult, ChatMessage, ChatPart, ChatState, EndView, ErrorView, Partial, Route, Trimmed,
    title_from,
};
use holzwave_core::ai::service::{AiService, AiSink, SYSTEM_PROMPT, SendRequest, ServiceError};
use holzwave_core::ai::store::{
    ContextStorage, Conversation, ConversationId, ConversationStore, JsonDirStore, Listing, MemoryStore as ChatMemory,
    Scrub, StoreError,
};
use holzwave_core::ai::tools::ToolName;
use holzwave_core::ai::types::{
    ApiKey, Consent, ContextKind, ContextNote, Locality, Message, MessageStatus, NoteItem, Opaque, Part, Role,
    ToolCall, Usage,
};
use holzwave_core::ai::wire::Wire;
use holzwave_core::secrets::MemoryStore;
use serde_json::{Value, json};

/// The key of every test. It appears in test code only: a hit anywhere else is a leak.
const KEY_CANARY: &str = "zq-svc-canary-4c8e19d27b";
/// The text a user types in the tests.
const TEXT_CANARY: &str = "zq-text-canary-91f3a6d0e2";

// ---- the rig ------------------------------------------------------------------------------------------------------

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Keeps every event in the order it arrived.
#[derive(Default)]
struct CollectSink {
    events: Mutex<Vec<AiEvent>>,
}

impl AiSink for CollectSink {
    fn emit(&self, event: &AiEvent) {
        lock(&self.events).push(event.clone());
    }
}

impl CollectSink {
    fn all(&self) -> Vec<AiEvent> {
        lock(&self.events).clone()
    }

    fn of(&self, chat: &ConversationId) -> Vec<AiEvent> {
        self.all().into_iter().filter(|event| event.conversation == chat.as_str()).collect()
    }

    fn kinds(&self, chat: &ConversationId) -> Vec<&'static str> {
        self.of(chat).iter().map(|event| event.kind.name()).collect()
    }

    /// How many turns of `chat` have ended.
    fn ended(&self, chat: &ConversationId) -> usize {
        self.of(chat).iter().filter(|event| matches!(event.kind, AiEventKind::TurnEnded(_))).count()
    }

    /// Waits until `count` turns of `chat` have ended.
    fn wait_ended(&self, chat: &ConversationId, count: usize, timeout: Duration) -> bool {
        common::wait_until(timeout, || self.ended(chat) >= count)
    }

    /// The end of the last turn of `chat`.
    fn last_end(&self, chat: &ConversationId) -> EndView {
        self.of(chat)
            .into_iter()
            .rev()
            .find_map(|event| match event.kind {
                AiEventKind::TurnEnded(end) => Some(end),
                _ => None,
            })
            .expect("the chat has an ended turn")
    }
}

/// A test rig: a scratch directory, a keychain, a sink and a chat store.
struct Rig {
    dir: PathBuf,
    config_dir: PathBuf,
    secrets: Arc<MemoryStore>,
    sink: Arc<CollectSink>,
    store: Arc<dyn ConversationStore>,
}

impl Rig {
    /// A rig over an in-memory chat store.
    fn new(tag: &str) -> Rig {
        Rig::with_store(tag, Arc::new(ChatMemory::new()))
    }

    /// A rig over a chat store in a directory of the scratch directory.
    fn on_disk(tag: &str) -> Rig {
        let dir = common::scratch_dir(tag);
        let store = Arc::new(JsonDirStore::new(dir.join("chats")));
        Rig::in_dir(dir, store)
    }

    fn with_store(tag: &str, store: Arc<dyn ConversationStore>) -> Rig {
        Rig::in_dir(common::scratch_dir(tag), store)
    }

    fn in_dir(dir: PathBuf, store: Arc<dyn ConversationStore>) -> Rig {
        let config_dir = dir.join("config");
        fs::create_dir(&config_dir).expect("a config directory");
        Rig {
            dir,
            config_dir,
            secrets: Arc::new(MemoryStore::default()),
            sink: Arc::new(CollectSink::default()),
            store,
        }
    }

    fn vault(&self) -> KeyVault {
        KeyVault::new(self.secrets.clone())
    }

    fn chats_dir(&self) -> PathBuf {
        self.dir.join("chats")
    }

    /// Writes `ai.json` with a Custom profile named `Local` (id `local`) that speaks the chat wire at `base`.
    fn profile(&self, base: &str, model: &str, key: Option<&str>) -> String {
        let mut config = AiConfig::default();
        let id = add_profile(&mut config, &self.vault(), "Local", Wire::OpenAiChat, base, model, key);
        config.save(&self.config_dir).expect("ai.json is written");
        id
    }

    /// A service over the rig's keychain.
    fn service(&self) -> AiService {
        AiService::builder(self.store.clone(), self.sink.clone() as Arc<dyn AiSink>, &self.config_dir)
            .vault(self.vault())
            .build()
    }

    /// A service whose keychain is unavailable, as on a system without one.
    fn service_without_keychain(&self) -> AiService {
        AiService::builder(self.store.clone(), self.sink.clone() as Arc<dyn AiSink>, &self.config_dir).build()
    }

    /// Sends `text` to the profile `local` and waits for the end of the turn.
    fn say(&self, service: &AiService, chat: &ConversationId, text: &str) -> usize {
        let before = self.sink.ended(chat);
        service
            .send(SendRequest::new(chat.clone(), text.to_owned(), 1).profile("local"))
            .expect("the send is accepted");
        assert!(self.sink.wait_ended(chat, before + 1, Duration::from_secs(5)), "the turn ends");
        before + 1
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Adds a Custom profile to `config` and, when a key is given, saves it in `vault`.
fn add_profile(
    config: &mut AiConfig,
    vault: &KeyVault,
    name: &str,
    wire: Wire,
    base: &str,
    model: &str,
    key: Option<&str>,
) -> String {
    let mut input = ProfileInput::new(name, Preset::Custom);
    input.wire = Some(wire);
    input.base_url = base.to_owned();
    input.model = model.to_owned();
    let id = config.upsert(input).expect("the profile is accepted");
    if let Some(key) = key {
        config.set_key(vault, &id, KeySlot::Provider, ApiKey::new(key.to_owned())).expect("the key is saved");
    }
    id
}

/// One event of a Chat Completions stream.
fn chunk(delta: &Value, finish: Option<&str>) -> String {
    let body = json!({
        "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "test-model",
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }]
    });
    format!("data: {body}\n\n")
}

/// The closing events of a Chat Completions stream: usage in a chunk without choices, then the marker.
fn closing(input: u32, output: u32) -> Vec<String> {
    let usage = json!({
        "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "test-model", "choices": [],
        "usage": { "prompt_tokens": input, "completion_tokens": output, "total_tokens": input + output }
    });
    vec![format!("data: {usage}\n\n"), "data: [DONE]\n\n".to_owned()]
}

/// A Chat Completions answer that says `parts` one after the other, `gap` apart, and reports 11 and 5 tokens.
fn answer(parts: &[&str], gap: Duration) -> Reply {
    let mut chunks = vec![chunk(&json!({ "role": "assistant", "content": "" }), None)];
    chunks.extend(parts.iter().map(|part| chunk(&json!({ "content": part }), None)));
    chunks.push(chunk(&json!({}), Some("stop")));
    chunks.extend(closing(11, 5));
    Reply::sse_bytes(chunks.into_iter().map(String::into_bytes).collect()).gap(gap)
}

/// The state a consumer holds after it folded every event.
struct Fold {
    state: ChatState,
}

impl Fold {
    fn new(chat: &ConversationId) -> Fold {
        Fold { state: ChatState::new(chat.as_str()) }
    }

    fn feed(&mut self, event: &AiEvent) -> ApplyResult {
        self.state.apply(event)
    }

    fn of(chat: &ConversationId, events: &[AiEvent]) -> Fold {
        let mut fold = Fold::new(chat);
        for event in events {
            assert_eq!(fold.feed(event), ApplyResult::Applied, "event {event:?} applies");
        }
        fold
    }
}

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("the value serialises")
}

fn to_text<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("the value serialises")
}

/// The parsed JSON body of a request the fake server saw.
fn body_of(seen: &common::fake_http::Seen) -> Value {
    serde_json::from_slice(&seen.body).expect("the request body is JSON")
}

// ---- the tracer ---------------------------------------------------------------------------------------------------

#[test]
fn a_message_sent_through_the_service_streams_from_the_fake_server_and_a_snapshot_after_a_dropped_event_equals_the_live_state()
 {
    let fake = FakeHttp::start(vec![
        answer(&["Hel", "lo"], Duration::from_millis(15)),
        answer(&["slow", " answer"], Duration::from_millis(60)),
    ]);
    let rig = Rig::new("svc-tracer");
    rig.profile(&format!("{}/v1", fake.base()), "test-model", Some(KEY_CANARY));
    let service = rig.service();
    let chat = service.new_conversation().expect("a chat id");
    let typed = format!("hello, {TEXT_CANARY}");

    // The send returns at once and names the turn.
    let started = Instant::now();
    let ticket = service.send(SendRequest::new(chat.clone(), typed.clone(), 7).profile("local")).expect("accepted");
    assert!(started.elapsed() < Duration::from_millis(100), "send waited {:?}", started.elapsed());
    assert_eq!(ticket.conversation, chat);
    assert!(rig.sink.wait_ended(&chat, 1, Duration::from_secs(5)), "the turn ends");
    let turn = ticket.turn;

    // The events, in order, with seq 1 to n and the ticket's turn.
    let events = rig.sink.of(&chat);
    let kinds = rig.sink.kinds(&chat);
    assert_eq!(kinds.first(), Some(&"turn_started"), "{kinds:?}");
    assert_eq!(&kinds[kinds.len() - 3..], ["usage", "message", "turn_ended"], "{kinds:?}");
    assert!(kinds[1..kinds.len() - 3].iter().all(|kind| *kind == "delta"), "{kinds:?}");
    assert!(kinds.len() >= 5, "{kinds:?}");
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.seq, index as u64 + 1, "seq has no hole: {kinds:?}");
        assert_eq!(event.turn, turn);
    }
    match &events[0].kind {
        AiEventKind::TurnStarted { message, route } => {
            assert_eq!(message.id, format!("m{turn}-user"));
            assert_eq!(message.role, Role::User);
            assert_eq!(message.text(), typed);
            assert_eq!(route.profile, "local");
            assert_eq!(route.model, "test-model");
            assert!(route.host.as_deref().is_some_and(|host| host.starts_with("127.0.0.1")), "{route:?}");
            assert_eq!(route.locality, Locality::Loopback);
        }
        other => panic!("the first event is turn_started, not {other:?}"),
    }
    let streamed: String = events
        .iter()
        .filter_map(|event| match &event.kind {
            AiEventKind::Delta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(streamed, "Hello");
    let usage = events.iter().find_map(|event| match &event.kind {
        AiEventKind::Usage(usage) => Some(*usage),
        _ => None,
    });
    assert_eq!(usage, Some(Usage { input_tokens: 11, output_tokens: 5 }));
    let message = events.iter().find_map(|event| match &event.kind {
        AiEventKind::Message { message } => Some(message.clone()),
        _ => None,
    });
    let message = message.expect("a message event");
    assert_eq!(
        (message.role, message.status, message.text().as_str()),
        (Role::Assistant, MessageStatus::Complete, "Hello")
    );
    assert_eq!(message.id, format!("m{turn}-1"));
    let end = rig.sink.last_end(&chat);
    assert_eq!((end.end.as_str(), end.rounds, end.saved), ("done", 1, true));
    assert!(end.error.is_none());

    // What reached the server: one connection, one request, the key in the header of its wire and nowhere else.
    assert_eq!(fake.accepted(), 1);
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!((seen[0].method.as_str(), seen[0].target.as_str()), ("POST", "/v1/chat/completions"));
    assert_eq!(seen[0].header("authorization"), Some(format!("Bearer {KEY_CANARY}").as_str()));
    for (name, value) in &seen[0].headers {
        if name != "authorization" {
            assert!(!value.contains(KEY_CANARY), "the key is in header {name}");
        }
    }
    assert!(!seen[0].target.contains(KEY_CANARY));
    assert!(!seen[0].body_text().contains(KEY_CANARY));
    let body = body_of(&seen[0]);
    assert_eq!(body["model"], "test-model");
    assert!(body.get("tools").is_none(), "no tool is offered when the host has none");
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"], SYSTEM_PROMPT);
    assert!(seen[0].body_text().contains(&typed));

    // The system prompt is plain, short and names no tool.
    assert!(SYSTEM_PROMPT.contains("holzwave-context") && SYSTEM_PROMPT.contains("data"));
    assert!(SYSTEM_PROMPT.contains("instructions") && SYSTEM_PROMPT.len() <= 2000);
    assert!(!SYSTEM_PROMPT.contains("://"));
    for tool in ToolName::ALL {
        assert!(!SYSTEM_PROMPT.contains(tool.as_str()), "the prompt names {}", tool.as_str());
    }

    // The store holds the user and the assistant message and the pins.
    let stored = rig.store.load(&chat).expect("a readable store").expect("the chat is stored");
    let ids: Vec<&str> = stored.messages.iter().map(|message| message.id.as_str()).collect();
    assert_eq!(ids, [format!("m{turn}-user"), format!("m{turn}-1")]);
    assert_eq!(stored.title, title_from(&typed));
    assert_eq!((stored.profile.as_deref(), stored.model.as_deref()), (Some("local"), Some("test-model")));
    assert!(
        (1_000_000_000..100_000_000_000).contains(&stored.messages[0].ts),
        "ts is Unix seconds: {}",
        stored.messages[0].ts
    );

    // A consumer that saw every event holds the live state, and every event applied.
    let live = Fold::of(&chat, &events);
    assert!(live.state.partial == Partial::default() && !live.state.running);

    // A consumer that misses the event with seq 3 sees a gap at seq 4, changes nothing, asks for a snapshot and then
    // holds exactly the live state: seq included.
    let mut dropper = Fold::new(&chat);
    let mut gap_seen = false;
    for event in &events {
        if event.seq == 3 {
            continue;
        }
        let before = to_value(&dropper.state);
        let result = dropper.feed(event);
        if event.seq == 4 {
            assert_eq!(result, ApplyResult::Gap);
            assert_eq!(to_value(&dropper.state), before, "a gap changes nothing");
            gap_seen = true;
            dropper.state = service.snapshot(&chat).expect("a snapshot");
        } else if event.seq > 4 {
            assert_eq!(result, ApplyResult::Stale, "events the snapshot already holds are stale");
        }
    }
    assert!(gap_seen);
    assert_eq!(to_value(&dropper.state), to_value(&live.state));
    assert_eq!(dropper.state.seq, events.last().map(|event| event.seq).expect("events"));
    assert_eq!(to_value(&service.snapshot(&chat).expect("a snapshot")), to_value(&live.state));

    // A chat has one turn at a time and four may run; another chat is not blocked by it.
    let other = service.new_conversation().expect("a chat id");
    service.send(SendRequest::new(chat.clone(), "again".to_owned(), 7).profile("local")).expect("a second turn");
    assert!(matches!(
        service.send(SendRequest::new(chat.clone(), "too soon".to_owned(), 7).profile("local")),
        Err(ServiceError::Busy)
    ));
    service.send(SendRequest::new(other.clone(), "elsewhere".to_owned(), 8).profile("local")).expect("another chat");

    // A snapshot in the middle of a running turn equals the fold of the events it counts.
    assert!(common::wait_until(Duration::from_secs(5), || rig.sink.of(&chat).len() > events.len() + 2));
    let middle = service.snapshot(&chat).expect("a snapshot");
    assert!(middle.running, "the turn is still running");
    assert!(middle.seq >= 2);
    assert!(rig.sink.wait_ended(&chat, 2, Duration::from_secs(5)));
    assert!(rig.sink.wait_ended(&other, 1, Duration::from_secs(5)));
    let second: Vec<AiEvent> = rig.sink.of(&chat).into_iter().filter(|event| event.turn != turn).collect();
    let prefix: Vec<AiEvent> = second.iter().take(middle.seq as usize).cloned().collect();
    let mut expected = Fold::of(&chat, &events);
    for event in &prefix {
        assert_eq!(expected.feed(event), ApplyResult::Applied);
    }
    assert_eq!(to_value(&middle), to_value(&expected.state));

    // The slot is free before the end is delivered: a send made on seeing the end is never busy.
    service.send(SendRequest::new(chat.clone(), "and again".to_owned(), 7).profile("local")).expect("accepted at once");
    assert!(rig.sink.wait_ended(&chat, 3, Duration::from_secs(5)));
}

// ---- the page contract --------------------------------------------------------------------------------------------

fn user_message(turn: u64, text: &str) -> ChatMessage {
    ChatMessage::from_message(&Message::user_text(format!("m{turn}-user"), text, 100))
}

fn event(turn: u64, seq: u64, kind: AiEventKind) -> AiEvent {
    AiEvent { conversation: "c1".to_owned(), turn, seq, kind }
}

fn started(turn: u64, seq: u64) -> AiEvent {
    event(
        turn,
        seq,
        AiEventKind::TurnStarted {
            message: user_message(turn, "hi"),
            route: Route {
                profile: "local".to_owned(),
                model: "test-model".to_owned(),
                host: Some("127.0.0.1:8080".to_owned()),
                locality: Locality::Loopback,
            },
        },
    )
}

fn delta(turn: u64, seq: u64, text: &str) -> AiEvent {
    event(turn, seq, AiEventKind::Delta { text: text.to_owned() })
}

#[test]
fn events_and_snapshots_have_a_pinned_json_shape_and_the_reducer_rules_are_exact() {
    // Each kind is one object: conversation, turn, seq, kind, then the fields of the kind.
    let assistant = {
        let mut message = Message::new("m5-1", Role::Assistant, vec![Part::Text { text: "Hello".to_owned() }], 101);
        message.model = Some("test-model".to_owned());
        message.usage = Some(Usage { input_tokens: 11, output_tokens: 5 });
        ChatMessage::from_message(&message)
    };
    let ended = |error: Option<ErrorView>| EndView {
        end: if error.is_some() { "failed" } else { "done" }.to_owned(),
        error,
        rounds: 1,
        usage: Usage { input_tokens: 11, output_tokens: 5 },
        saved: true,
    };
    let pins: Vec<(AiEvent, &str)> = vec![
        (
            started(5, 1),
            r#"{"conversation":"c1","turn":5,"seq":1,"kind":"turn_started","message":{"id":"m5-user","role":"user","ts":100,"model":null,"status":"complete","usage":null,"parts":[{"type":"text","text":"hi"}]},"route":{"profile":"local","model":"test-model","host":"127.0.0.1:8080","locality":"loopback"}}"#,
        ),
        (delta(5, 2, "Hel"), r#"{"conversation":"c1","turn":5,"seq":2,"kind":"delta","text":"Hel"}"#),
        (
            event(5, 3, AiEventKind::Reasoning { text: "thinking".to_owned() }),
            r#"{"conversation":"c1","turn":5,"seq":3,"kind":"reasoning","text":"thinking"}"#,
        ),
        (
            event(5, 4, AiEventKind::Usage(Usage { input_tokens: 11, output_tokens: 5 })),
            r#"{"conversation":"c1","turn":5,"seq":4,"kind":"usage","input_tokens":11,"output_tokens":5}"#,
        ),
        (
            event(5, 5, AiEventKind::Trimmed(Trimmed { results: 1, exchanges: 2 })),
            r#"{"conversation":"c1","turn":5,"seq":5,"kind":"trimmed","results":1,"exchanges":2}"#,
        ),
        (
            event(5, 6, AiEventKind::Message { message: assistant }),
            r#"{"conversation":"c1","turn":5,"seq":6,"kind":"message","message":{"id":"m5-1","role":"assistant","ts":101,"model":"test-model","status":"complete","usage":{"input_tokens":11,"output_tokens":5},"parts":[{"type":"text","text":"Hello"}]}}"#,
        ),
        (
            event(
                5,
                7,
                AiEventKind::ToolResult {
                    call_id: "call_1".to_owned(),
                    tool: "read_file".to_owned(),
                    content: "ok".to_owned(),
                    is_error: false,
                    redactions: 0,
                },
            ),
            r#"{"conversation":"c1","turn":5,"seq":7,"kind":"tool_result","call_id":"call_1","tool":"read_file","content":"ok","is_error":false,"redactions":0}"#,
        ),
        (
            event(5, 8, AiEventKind::TurnEnded(ended(None))),
            r#"{"conversation":"c1","turn":5,"seq":8,"kind":"turn_ended","end":"done","error":null,"rounds":1,"usage":{"input_tokens":11,"output_tokens":5},"saved":true}"#,
        ),
        (
            event(
                5,
                9,
                AiEventKind::TurnEnded(ended(Some(ErrorView {
                    class: "auth".to_owned(),
                    text: "the provider refused the credentials".to_owned(),
                }))),
            ),
            r#"{"conversation":"c1","turn":5,"seq":9,"kind":"turn_ended","end":"failed","error":{"class":"auth","text":"the provider refused the credentials"},"rounds":1,"usage":{"input_tokens":11,"output_tokens":5},"saved":true}"#,
        ),
    ];
    for (event, literal) in &pins {
        assert_eq!(to_text(event), *literal);
    }

    // A state has the key order of the contract.
    assert_eq!(
        to_text(&ChatState::new("c1")),
        r#"{"conversation":"c1","title":"","profile":null,"model":null,"turn":0,"seq":0,"running":false,"messages":[],"partial":{"text":"","reasoning":""},"pending":[],"usage":{"input_tokens":0,"output_tokens":0},"trimmed":{"results":0,"exchanges":0},"ended":null}"#
    );

    // The view of a message drops the native turn and the stored context text, and shows a context as labels.
    let mut stored = Message::new(
        "m6-user",
        Role::User,
        vec![
            Part::Context(ContextNote {
                hash: "ab".repeat(32),
                items: vec![NoteItem { kind: ContextKind::Selection, label: "pane 1".to_owned(), bytes: 31 }],
                stored: Some("STORED-CONTEXT-TEXT".to_owned()),
                consented_to: Consent { provider: "local".to_owned(), locality: Locality::Loopback },
            }),
            Part::Text { text: "question".to_owned() },
        ],
        100,
    );
    stored.native = Some(Opaque { wire: "anthropic".to_owned(), json: json!({ "signature": "NATIVE-BLOB" }) });
    let view = to_text(&ChatMessage::from_message(&stored));
    assert!(!view.contains("STORED-CONTEXT-TEXT") && !view.contains("NATIVE-BLOB"), "{view}");
    assert!(
        view.contains(r#""type":"context""#) && view.contains(r#""label":"pane 1""#) && view.contains(r#""bytes":31"#)
    );

    // The summary of a call is one scrubbed line of at most 240 characters.
    let noisy = format!("line one\nline two \u{202e}reversed {}", "x".repeat(500));
    let mut call = Message::new(
        "m6-1",
        Role::Assistant,
        vec![Part::ToolCall(ToolCall {
            id: "call_1".to_owned(),
            name: "read_file".to_owned(),
            args: json!({ "path": noisy }),
        })],
        101,
    );
    call.status = MessageStatus::Complete;
    let summary = match &ChatMessage::from_message(&call).parts[0] {
        ChatPart::ToolCall { summary, .. } => summary.clone(),
        other => panic!("a tool call part, not {other:?}"),
    };
    assert!(summary.chars().count() <= 240, "{} characters", summary.chars().count());
    assert!(!summary.contains('\n') && !summary.contains('\u{202e}'), "{summary:?}");
    assert!(summary.starts_with("{\"path\":\"line one"), "{summary:?}");

    // The reducer rules, one row each.
    let mut state = ChatState::new("c1");
    assert_eq!(state.apply(&started(5, 1)), ApplyResult::Applied);
    assert_eq!((state.turn, state.seq, state.running), (5, 1, true));
    assert_eq!((state.profile.as_deref(), state.model.as_deref()), (Some("local"), Some("test-model")));
    assert_eq!(state.title, "hi");
    let before = to_value(&state);
    assert_eq!(state.apply(&started(5, 1)), ApplyResult::Stale, "the same seq again");
    assert_eq!(state.apply(&delta(5, 3, "x")), ApplyResult::Gap, "seq 3 after seq 1");
    assert_eq!(to_value(&state), before, "stale and gap change nothing");
    assert_eq!(state.apply(&delta(5, 2, "Hel")), ApplyResult::Applied);
    assert_eq!(state.apply(&delta(5, 3, "lo")), ApplyResult::Applied);
    assert_eq!(state.partial.text, "Hello");
    let mut elsewhere = delta(5, 4, "x");
    elsewhere.conversation = "c2".to_owned();
    assert_eq!(state.apply(&elsewhere), ApplyResult::Stale, "another conversation");
    assert_eq!(state.apply(&event(5, 4, AiEventKind::Reasoning { text: "hm".to_owned() })), ApplyResult::Applied);
    assert_eq!(state.partial.reasoning, "hm");
    // A message replaces the partial text and keeps what waits for a decision; a result removes only its call.
    let waiting = |call: &str| holzwave_core::ai::tools::Pending::clone(&pending_card(call));
    state.pending = vec![waiting("call_1"), waiting("call_2")];
    let mut message = Message::new("m5-1", Role::Assistant, vec![Part::Text { text: "Hello".to_owned() }], 101);
    message.model = Some("test-model".to_owned());
    assert_eq!(
        state.apply(&event(5, 5, AiEventKind::Message { message: ChatMessage::from_message(&message) })),
        ApplyResult::Applied
    );
    assert_eq!((state.partial.text.as_str(), state.partial.reasoning.as_str()), ("", ""));
    assert_eq!(state.pending.len(), 2, "a message keeps the pending cards");
    let result = AiEventKind::ToolResult {
        call_id: "call_1".to_owned(),
        tool: "read_file".to_owned(),
        content: "ok".to_owned(),
        is_error: false,
        redactions: 0,
    };
    assert_eq!(state.apply(&event(5, 6, result)), ApplyResult::Applied);
    assert_eq!(state.pending.iter().map(|card| card.call_id.as_str()).collect::<Vec<_>>(), ["call_2"]);
    assert_eq!(
        state.apply(&event(5, 7, AiEventKind::Usage(Usage { input_tokens: 3, output_tokens: 4 }))),
        ApplyResult::Applied
    );
    assert_eq!(
        state.apply(&event(5, 8, AiEventKind::Trimmed(Trimmed { results: 2, exchanges: 1 }))),
        ApplyResult::Applied
    );
    assert_eq!(state.apply(&delta(5, 9, "tail")), ApplyResult::Applied);
    assert_eq!(state.apply(&event(5, 10, AiEventKind::TurnEnded(ended(None)))), ApplyResult::Applied);
    assert!(!state.running && state.pending.is_empty() && state.partial == Partial::default());
    assert_eq!(state.ended.as_ref().map(|end| end.end.as_str()), Some("done"));
    assert_eq!(state.messages.len(), 2);
    // Another turn applies at seq 1 and resets the fields of the turn; at any other seq it is a gap.
    let before = to_value(&state);
    assert_eq!(state.apply(&delta(6, 4, "x")), ApplyResult::Gap, "another turn at seq 4");
    assert_eq!(to_value(&state), before);
    assert_eq!(state.apply(&started(6, 1)), ApplyResult::Applied);
    assert_eq!((state.turn, state.seq, state.running), (6, 1, true));
    assert!(state.ended.is_none() && state.pending.is_empty());
    assert_eq!((state.usage, state.trimmed), (Usage::default(), Trimmed::default()));
    assert_eq!(state.title, "hi", "a chat keeps its title");
    // A fresh state applies the first event of any turn at seq 1.
    assert_eq!(ChatState::new("c1").apply(&started(9, 1)), ApplyResult::Applied);
    assert_eq!(ChatState::new("c1").apply(&started(9, 2)), ApplyResult::Gap);
}

/// A card that waits for a decision, made through the approval table with a host that offers one file tool.
fn pending_card(call: &str) -> holzwave_core::ai::tools::Pending {
    use holzwave_core::ai::provider::CancelToken;
    use holzwave_core::ai::tools::{ApprovalTable, DenyPolicy, Resolved, Scope, Tool, ToolError, ToolHost, ToolOutput};

    struct OneFile;
    impl ToolHost for OneFile {
        fn available(&self, _tool: ToolName, _pane: u64) -> bool {
            true
        }
        fn resolve(&self, _scope: &Scope, _tool: &Tool) -> Result<Resolved, ToolError> {
            Ok(Resolved::local(1, "this computer").with_path("/tmp/example.txt"))
        }
        fn run(
            &self,
            _ticket: &holzwave_core::ai::tools::Ticket,
            _cancel: &CancelToken,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::new("ok".to_owned()))
        }
    }
    let table = ApprovalTable::new(DenyPolicy::new(Vec::new()));
    let scope = Scope { conversation: "c1".to_owned(), turn: 5, pane: 1 };
    let call =
        ToolCall { id: call.to_owned(), name: "read_file".to_owned(), args: json!({ "path": "/tmp/example.txt" }) };
    table.propose(&OneFile, &scope, &call).expect("a card")
}

// ---- turns that cannot start --------------------------------------------------------------------------------------

#[test]
fn a_turn_that_cannot_start_ends_with_a_fixed_error_and_opens_no_connection() {
    let fake = FakeHttp::start(vec![Reply::Close]);
    let rig = Rig::new("svc-not-started");
    let vault = rig.vault();
    let mut config = AiConfig::default();
    let base = format!("{}/v1", fake.base());
    // A profile whose marker says a key exists, read by a service that has no keychain.
    add_profile(&mut config, &vault, "Keyed", Wire::OpenAiChat, &base, "test-model", Some(KEY_CANARY));
    // The Anthropic preset needs a key and has none.
    let mut claude = ProfileInput::new("Claude", Preset::Anthropic);
    claude.model = "claude-test".to_owned();
    config.upsert(claude).expect("the Anthropic profile");
    // A gateway at a public address that was never acknowledged.
    let mut router = ProfileInput::new("Router", Preset::OpenRouter);
    router.base_url = "https://203.0.113.7/api/v1".to_owned();
    router.model = "vendor/model".to_owned();
    config.upsert(router).expect("the gateway profile");
    // A profile with no model.
    add_profile(&mut config, &vault, "Bare", Wire::OpenAiChat, &base, "", None);
    config.save(&rig.config_dir).expect("ai.json");
    let service = rig.service_without_keychain();

    let cases: [(&str, &str, Option<&str>); 5] = [
        ("nobody", "invalid_request", None),
        ("claude", "auth", None),
        ("keyed", "unavailable", None),
        ("router", "policy", Some("203.0.113.7")),
        ("bare", "invalid_request", None),
    ];
    for (profile, class, forbidden) in cases {
        let chat = service.new_conversation().expect("a chat id");
        service
            .send(SendRequest::new(chat.clone(), format!("question {TEXT_CANARY}"), 1).profile(profile))
            .unwrap_or_else(|error| panic!("{profile}: the send is accepted: {error}"));
        assert!(rig.sink.wait_ended(&chat, 1, Duration::from_secs(5)), "{profile}: the turn ends");
        let events = rig.sink.of(&chat);
        assert_eq!(events.len(), 1, "{profile}: exactly one event: {events:?}");
        let AiEventKind::TurnEnded(end) = &events[0].kind else {
            panic!("{profile}: turn_ended, not {:?}", events[0].kind)
        };
        assert_eq!(end.end, "failed", "{profile}");
        let error = end.error.as_ref().unwrap_or_else(|| panic!("{profile}: an error"));
        assert_eq!(error.class, class, "{profile}: {error:?}");
        assert!(
            !error.text.is_empty() && !error.text.contains(TEXT_CANARY) && !error.text.contains(KEY_CANARY),
            "{profile}"
        );
        if let Some(forbidden) = forbidden {
            assert!(!error.text.contains(forbidden), "{profile}: the text names the address");
        }
        assert_eq!(events[0].seq, 1);
        assert!(rig.store.load(&chat).expect("a readable store").is_none(), "{profile}: nothing is stored");
        assert_eq!(service.active_turns(), 0, "{profile}: the slot is free");
    }
    assert!(rig.store.list().expect("a listing").chats.is_empty());
    assert_eq!(fake.accepted(), 0, "no turn that cannot start opens a connection");

    // The refusals made on the caller's thread leave no trace either.
    let chat = service.new_conversation().expect("a chat id");
    for text in ["", "  \n\t "] {
        let result = service.send(SendRequest::new(chat.clone(), text.to_owned(), 1).profile("keyed"));
        assert!(matches!(result, Err(ServiceError::EmptyText)), "{text:?}");
    }
    let long = "x".repeat(131_073);
    assert!(matches!(
        service.send(SendRequest::new(chat.clone(), long, 1).profile("keyed")),
        Err(ServiceError::TextTooLong)
    ));
    let result = service.send(SendRequest::new(chat.clone(), "hello".to_owned(), 1));
    assert!(matches!(result, Err(ServiceError::NoProfile)), "no profile in the request and none pinned");
    assert_eq!(service.active_turns(), 0);
    assert!(rig.store.load(&chat).expect("a readable store").is_none());
    assert_eq!(rig.sink.of(&chat).len(), 0);
    assert_eq!(ServiceError::EmptyText.code(), "empty_text");
    assert_eq!(fake.accepted(), 0);
}

// ---- errors of the provider ---------------------------------------------------------------------------------------

#[test]
fn a_provider_error_reaches_the_page_as_a_class_and_a_scrubbed_text() {
    let body = format!(r#"{{"error":{{"message":"bad key {KEY_CANARY}","type":"invalid_request_error"}}}}"#);
    let fake = FakeHttp::start(vec![Reply::status(401).body(body.into_bytes())]);
    let rig = Rig::on_disk("svc-provider-error");
    rig.profile(&format!("{}/v1", fake.base()), "test-model", Some(KEY_CANARY));
    let service = rig.service();
    let chat = service.new_conversation().expect("a chat id");

    rig.say(&service, &chat, &format!("hello {TEXT_CANARY}"));

    assert_eq!(rig.sink.kinds(&chat), ["turn_started", "turn_ended"]);
    let end = rig.sink.last_end(&chat);
    assert_eq!(end.end, "failed");
    let error = end.error.expect("an error");
    assert_eq!(error.class, "auth");
    assert!(!error.text.is_empty());
    for event in rig.sink.of(&chat) {
        let shown = format!("{} {event:?}", to_text(&event));
        assert!(!shown.contains(KEY_CANARY), "the key reached an event: {shown}");
    }
    assert_eq!(fake.accepted(), 1);
    let stored = rig.store.load(&chat).expect("a readable store").expect("the chat is stored");
    assert_eq!(stored.messages.len(), 1, "the user message is stored and no assistant text exists");
    assert_eq!(stored.messages[0].role, Role::User);
    let file = fs::read_to_string(rig.chats_dir().join(format!("{chat}.json"))).expect("the chat file");
    assert!(!file.contains(KEY_CANARY));
    assert!(file.contains(TEXT_CANARY), "what the user typed is kept");
}

// ---- nothing by itself --------------------------------------------------------------------------------------------

#[test]
fn building_the_service_and_every_idle_call_open_no_socket_and_each_send_makes_exactly_one_request() {
    let fake = FakeHttp::start(vec![answer(&["Hello"], Duration::ZERO), answer(&["Second"], Duration::ZERO)]);
    let rig = Rig::new("svc-quiet");
    rig.profile(&format!("{}/v1", fake.base()), "test-model", Some(KEY_CANARY));
    let service = rig.service();
    let chat = service.new_conversation().expect("a chat id");
    let listing = service.list_conversations().expect("a listing");
    assert!(listing.chats.is_empty());
    let empty = service.snapshot(&chat).expect("a snapshot of a chat with no file");
    assert_eq!((empty.turn, empty.seq, empty.running, empty.messages.len()), (0, 0, false, 0));
    assert_eq!(to_value(&empty)["conversation"], chat.as_str());
    let _ = (service.gate(), service.approvals(), format!("{service:?}"));
    assert_eq!(service.active_turns(), 0);
    assert!(service.config_notes().is_empty());
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(fake.accepted(), 0, "nothing opens by itself");
    assert!(rig.sink.all().is_empty());

    rig.say(&service, &chat, "first question");
    assert_eq!(fake.accepted(), 1, "one send is one request");
    rig.say(&service, &chat, "second question");
    assert_eq!(fake.accepted(), 2, "a second send is one more request");
    let seen = fake.seen();
    assert!(seen.iter().all(|request| request.target == "/v1/chat/completions"), "no title request, no model list");
    let second = body_of(&seen[1]);
    let roles: Vec<&str> =
        second["messages"].as_array().expect("messages").iter().filter_map(|m| m["role"].as_str()).collect();
    assert_eq!(roles, ["system", "user", "assistant", "user"]);
    assert_eq!(second["messages"][2]["content"], "Hello", "the second request holds the first answer");
}

// ---- failing saves ------------------------------------------------------------------------------------------------

/// A chat store whose `save` fails from the `from`-th call on (counting from 1).
struct FailingSaves {
    inner: ChatMemory,
    from: usize,
    saves: AtomicUsize,
}

impl ConversationStore for FailingSaves {
    fn list(&self) -> Result<Listing, StoreError> {
        self.inner.list()
    }

    fn load(&self, id: &ConversationId) -> Result<Option<Conversation>, StoreError> {
        self.inner.load(id)
    }

    fn save(&self, conversation: &Conversation, storage: ContextStorage) -> Result<(), StoreError> {
        let call = self.saves.fetch_add(1, Ordering::SeqCst) + 1;
        if call >= self.from {
            Err(StoreError::Io("the disk is full".to_owned()))
        } else {
            self.inner.save(conversation, storage)
        }
    }

    fn delete(&self, id: &ConversationId) -> Result<bool, StoreError> {
        self.inner.delete(id)
    }

    fn scrub_private_text(&self) -> Result<Scrub, StoreError> {
        self.inner.scrub_private_text()
    }
}

fn failing_from(from: usize) -> Arc<FailingSaves> {
    Arc::new(FailingSaves { inner: ChatMemory::new(), from, saves: AtomicUsize::new(0) })
}

#[test]
fn a_failed_first_save_stops_the_turn_before_any_request_and_a_later_failure_only_clears_the_saved_flag() {
    let fake = FakeHttp::start(vec![answer(&["Hello"], Duration::ZERO)]);

    // The first save fails: nothing is sent and the turn ends with a fixed error.
    let first = Rig::with_store("svc-save-first", failing_from(1));
    first.profile(&format!("{}/v1", fake.base()), "test-model", None);
    let service = first.service();
    let chat = service.new_conversation().expect("a chat id");
    first.say(&service, &chat, "hello");
    assert_eq!(first.sink.kinds(&chat), ["turn_ended"]);
    let end = first.sink.last_end(&chat);
    let error = end.error.expect("an error");
    assert_eq!((end.end.as_str(), error.class.as_str(), end.saved), ("failed", "unavailable", false));
    assert!(!error.text.is_empty() && !error.text.contains("disk"), "{error:?}");
    assert_eq!(fake.accepted(), 0, "a chat that cannot be written does not talk to a provider");

    // The second save fails: the answer streams, the message is delivered and only the saved flag says it.
    let later = Rig::with_store("svc-save-later", failing_from(2));
    later.profile(&format!("{}/v1", fake.base()), "test-model", None);
    let service = later.service();
    let chat = service.new_conversation().expect("a chat id");
    later.say(&service, &chat, "hello");
    assert_eq!(fake.accepted(), 1);
    let kinds = later.sink.kinds(&chat);
    assert!(kinds.contains(&"message") && kinds.last() == Some(&"turn_ended"), "{kinds:?}");
    let end = later.sink.last_end(&chat);
    assert_eq!((end.end.as_str(), end.saved), ("done", false));
    assert!(end.error.is_none());
    let stored = later.store.load(&chat).expect("a readable store").expect("the first save worked");
    assert_eq!(stored.messages.len(), 1, "only the user message reached the store");
}

/// The scratch directory of a rig, for tests that read files.
#[allow(dead_code)]
fn files_below(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(files_below(&path));
            } else {
                found.push(path);
            }
        }
    }
    found
}
