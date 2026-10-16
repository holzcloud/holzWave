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
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use common::fake_http::{FakeHttp, Reply};
use holzwave_core::ai::agent::INTERRUPTED_RESULT;
use holzwave_core::ai::gate::{ApprovedContext, Destination, OfferRequest, RequestItem};
use holzwave_core::ai::profiles::{
    AiConfig, KeyError, KeySlot, KeyStatus, KeyVault, Preset, ProfileError, ProfileInput, account,
};
use holzwave_core::ai::provider::{AiError, CancelToken, ChatRequest, Completion, Flow, Provider, StreamEvent};
use holzwave_core::ai::redact::SourceText;
use holzwave_core::ai::scripted::{Script, ScriptedProvider};
use holzwave_core::ai::service::coalesce::{Batch, Coalescer, DeltaKind};
use holzwave_core::ai::service::events::{
    AiEvent, AiEventKind, ApplyResult, ChatMessage, ChatPart, ChatState, EndView, ErrorView, Partial, Route, Trimmed,
    title_from,
};
use holzwave_core::ai::service::{
    AiService, AiSink, MAX_TURN_THREADS, SYSTEM_PROMPT, SendRequest, ServiceError, TurnTicket,
};
use holzwave_core::ai::store::{
    ContextStorage, Conversation, ConversationId, ConversationStore, JsonDirStore, Listing, MemoryStore as ChatMemory,
    NOT_STORED, Scrub, StoreError,
};
use holzwave_core::ai::tools::{Decision, Resolved, Scope, Ticket, Tool, ToolError, ToolHost, ToolName, ToolOutput};
use holzwave_core::ai::types::{
    ApiKey, Capabilities, Consent, ContextKind, ContextNote, Locality, Message, MessageStatus, NoteItem, Opaque, Part,
    Role, StopReason, ToolCall, Usage,
};
use holzwave_core::ai::wire::Wire;
use holzwave_core::layout::PaneId;
use holzwave_core::secrets::{MemoryStore, SecretStore};
use holzwave_core::settings::Settings;
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
    // The shell holds one service and calls it from many threads.
    fn assert_send_sync<T: Send + Sync>(_: &T) {}
    assert_send_sync(&service);
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

// ---- helpers for scripted turns -----------------------------------------------------------------------------------

/// Registers a scripted provider under `id` and keeps a handle to read what it was asked.
fn register(service: &AiService, id: &str, provider: ScriptedProvider) -> Arc<ScriptedProvider> {
    let provider = Arc::new(provider);
    service.register_provider(id, provider.clone() as Arc<dyn Provider>).expect("the provider is registered");
    provider
}

/// Sends `text` to the registered provider `id` and returns the ticket.
fn send_to(service: &AiService, chat: &ConversationId, id: &str, text: &str, pane: PaneId) -> TurnTicket {
    service.send(SendRequest::new(chat.clone(), text.to_owned(), pane).profile(id)).expect("the send is accepted")
}

/// The answer text streamed so far in `chat`, from the delta events.
fn streamed(sink: &CollectSink, chat: &ConversationId) -> String {
    sink.of(chat)
        .iter()
        .filter_map(|event| match &event.kind {
            AiEventKind::Delta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The message events of `chat`.
fn messages(sink: &CollectSink, chat: &ConversationId) -> Vec<ChatMessage> {
    sink.of(chat)
        .into_iter()
        .filter_map(|event| match event.kind {
            AiEventKind::Message { message } => Some(message),
            _ => None,
        })
        .collect()
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).expect("a short time")
}

// ---- the coalescer ------------------------------------------------------------------------------------------------

/// Adjacent pieces of one kind, merged, in order.
fn runs(pieces: &[(DeltaKind, String)]) -> Vec<(DeltaKind, String)> {
    let mut out: Vec<(DeltaKind, String)> = Vec::new();
    for (kind, text) in pieces {
        match out.last_mut() {
            Some((last, joined)) if last == kind => joined.push_str(text),
            _ => out.push((*kind, text.clone())),
        }
    }
    out
}

#[test]
fn the_coalescer_merges_adjacent_deltas_at_about_sixty_a_second_and_keeps_order() {
    let base = Instant::now();
    let at = |ms: u64| base + Duration::from_millis(ms);
    let batch = |kind: DeltaKind, text: &str| Batch { kind, text: text.to_owned() };
    let mut merger = Coalescer::new(Duration::from_millis(16), 8192);

    // The first text goes out at once; the rest within the interval is buffered and due one interval after the last
    // delivery.
    assert_eq!(merger.push(DeltaKind::Text, "a", at(0)), vec![batch(DeltaKind::Text, "a")]);
    assert!(merger.is_empty() && merger.due_at().is_none());
    assert!(merger.push(DeltaKind::Text, "b", at(5)).is_empty());
    assert!(merger.push(DeltaKind::Text, "c", at(10)).is_empty());
    assert_eq!(merger.due_at(), Some(at(16)));
    // Pieces that are delivered together are one batch, and an empty piece changes nothing.
    assert!(merger.push(DeltaKind::Text, "", at(12)).is_empty());
    assert_eq!(merger.push(DeltaKind::Text, "d", at(20)), vec![batch(DeltaKind::Text, "bcd")]);
    // `take` flushes whether it is due or not, and records the time.
    assert!(merger.push(DeltaKind::Text, "e", at(21)).is_empty());
    assert_eq!(merger.take(at(22)), Some(batch(DeltaKind::Text, "e")));
    assert_eq!(merger.take(at(23)), None);
    assert!(merger.push(DeltaKind::Text, "f", at(23)).is_empty(), "the take counted as a delivery");
    // A change of kind hands the buffered text out first and keeps the order.
    assert_eq!(merger.push(DeltaKind::Reasoning, "g", at(24)), vec![batch(DeltaKind::Text, "f")]);
    assert_eq!(merger.due_at(), Some(at(40)));
    assert_eq!(merger.take(at(40)), Some(batch(DeltaKind::Reasoning, "g")));

    // A buffer that reaches the byte bound goes out whole, even inside the interval.
    let mut bounded = Coalescer::new(Duration::from_millis(16), 100);
    assert_eq!(bounded.push(DeltaKind::Text, &"x".repeat(10), at(0)).len(), 1);
    assert!(bounded.push(DeltaKind::Text, &"y".repeat(60), at(1)).is_empty());
    let out = bounded.push(DeltaKind::Text, &"z".repeat(50), at(2));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].text.len(), 110, "the whole buffer goes out");

    // A seeded run: 20000 pieces of 1 to 40 bytes, up to 3 ms apart (about 30 seconds), with the helper flushing at the
    // due time. The kind changes every 2500 pieces, the way a stream goes from reasoning to its answer.
    const SEED: u64 = 0x5eed_c0a1_e5ce;
    let mut rng = common::Xorshift64::new(SEED);
    let mut merger = Coalescer::new(Duration::from_millis(16), 8192);
    let mut input: Vec<(DeltaKind, String)> = Vec::new();
    let mut delivered: Vec<(u64, Batch)> = Vec::new();
    let mut now_us = 0u64;
    for index in 0..20_000usize {
        now_us += rng.below(3001) as u64;
        while let Some(due) = merger.due_at() {
            let due_us = micros(due - base);
            if due_us > now_us {
                break;
            }
            delivered.extend(merger.take(due).map(|batch| (due_us, batch)));
        }
        let kind = if (index / 2500) % 2 == 0 { DeltaKind::Reasoning } else { DeltaKind::Text };
        let length = 1 + rng.below(40);
        let text: String = (0..length).map(|_| char::from(b'a' + rng.below(26) as u8)).collect();
        input.push((kind, text.clone()));
        delivered
            .extend(merger.push(kind, &text, base + Duration::from_micros(now_us)).into_iter().map(|b| (now_us, b)));
    }
    while let Some(due) = merger.due_at() {
        delivered.extend(merger.take(due).map(|batch| (micros(due - base), batch)));
    }
    let out: Vec<(DeltaKind, String)> = delivered.iter().map(|(_, batch)| (batch.kind, batch.text.clone())).collect();
    assert!(runs(&out) == runs(&input), "seed {SEED:#x}: the bytes and the order of the stream changed");
    let longest = delivered.iter().map(|(_, batch)| batch.text.len()).max().unwrap_or(0);
    assert!(longest <= 8192 + 40, "seed {SEED:#x}: a batch of {longest} bytes");
    let mut peak = 0;
    let mut end = 0;
    for start in 0..delivered.len() {
        while end < delivered.len() && delivered[end].0 <= delivered[start].0 + 1_000_000 {
            end += 1;
        }
        peak = peak.max(end - start);
    }
    println!("coalescer: {} pieces became {} batches, at most {peak} in any second", input.len(), delivered.len());
    assert!(peak <= 64, "seed {SEED:#x}: {peak} batches in one second");
    assert!(delivered.len() < 3000, "seed {SEED:#x}: {} batches for 20000 pieces", delivered.len());
}

// ---- the flush helper ---------------------------------------------------------------------------------------------

#[test]
fn a_delta_buffered_before_a_stall_is_flushed_without_waiting_for_the_next_one() {
    let rig = Rig::new("svc-flush");
    let service = rig.service();
    let mut fast = Script::new();
    for _ in 0..400 {
        fast = fast.text("x");
    }
    register(
        &service,
        "scripted",
        ScriptedProvider::new("scripted")
            .with_turn(Script::new().text("a").text("b").stall(Duration::from_millis(600)).text("c"))
            .with_turn(fast),
    );
    let chat = service.new_conversation().expect("a chat id");

    send_to(&service, &chat, "scripted", "stall", 1);
    assert!(common::wait_until(Duration::from_secs(5), || streamed(&rig.sink, &chat).starts_with('a')));
    let stalled_at = Instant::now();
    assert!(
        common::wait_until(Duration::from_millis(300), || streamed(&rig.sink, &chat) == "ab"),
        "the buffered piece waited for the next one: {:?}",
        streamed(&rig.sink, &chat)
    );
    assert!(stalled_at.elapsed() < Duration::from_millis(300));
    assert_eq!(rig.sink.ended(&chat), 0, "the turn is still in its stall");
    assert!(rig.sink.wait_ended(&chat, 1, Duration::from_secs(5)));
    assert_eq!(streamed(&rig.sink, &chat), "abc");
    let all = messages(&rig.sink, &chat);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].text(), "abc");
    Fold::of(&chat, &rig.sink.of(&chat));

    // A fast turn of 400 single characters becomes far fewer events, with the same bytes, and the message comes last.
    send_to(&service, &chat, "scripted", "fast", 1);
    assert!(rig.sink.wait_ended(&chat, 2, Duration::from_secs(5)));
    let second: Vec<AiEvent> = rig.sink.of(&chat).into_iter().skip_while(|event| event.turn == 0).collect();
    let turn = second.last().map(|event| event.turn).expect("events");
    let second: Vec<AiEvent> = second.into_iter().filter(|event| event.turn == turn).collect();
    let texts: Vec<&str> = second
        .iter()
        .filter_map(|event| match &event.kind {
            AiEventKind::Delta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts.concat(), "x".repeat(400));
    assert!(texts.len() < 100, "{} delta events for 400 pieces", texts.len());
    let last_delta = second.iter().rposition(|event| matches!(event.kind, AiEventKind::Delta { .. })).expect("deltas");
    let message = second.iter().position(|event| matches!(event.kind, AiEventKind::Message { .. })).expect("a message");
    assert!(message > last_delta, "the message comes after the last delta");
}

// ---- the five wires -----------------------------------------------------------------------------------------------

#[test]
fn each_wire_is_dispatched_to_its_adapter_with_the_key_in_the_header_of_its_wire() {
    let fake = FakeHttp::start(vec![Reply::status(400)]);
    let rig = Rig::new("svc-wires");
    let vault = rig.vault();
    let mut config = AiConfig::default();
    let base = fake.base();
    let wires = [
        ("Chat", Wire::OpenAiChat, format!("{base}/v1")),
        ("Responses", Wire::OpenAiResponses, format!("{base}/v1")),
        ("Messages", Wire::Anthropic, base.clone()),
        ("Gemini", Wire::Gemini, format!("{base}/v1beta")),
        ("Ollama", Wire::Ollama, base.clone()),
    ];
    for (name, wire, address) in &wires {
        add_profile(&mut config, &vault, name, *wire, address, "test-model", Some(KEY_CANARY));
    }
    config.save(&rig.config_dir).expect("ai.json");
    let service = rig.service();

    for (index, (name, _, _)) in wires.iter().enumerate() {
        let chat = service.new_conversation().expect("a chat id");
        let id = name.to_lowercase();
        let typed = format!("question {index} {TEXT_CANARY}");
        send_to(&service, &chat, &id, &typed, 1);
        assert!(rig.sink.wait_ended(&chat, 1, Duration::from_secs(5)), "{id}: the turn ends");
        assert_eq!(rig.sink.last_end(&chat).end, "failed", "{id}: the server answered 400");
        let seen = fake.seen();
        assert_eq!(seen.len(), index + 1, "{id}: one request");
        let request = &seen[index];
        let text = request.body_text();
        // The Gemini wire names the model in the address, every other wire in the body.
        assert!(text.contains("test-model") || request.target.contains("test-model"), "{id}: the model is asked for");
        assert!(text.contains(&typed), "{id}: the text is in the body");
        assert!(!text.contains(KEY_CANARY) && !request.target.contains(KEY_CANARY), "{id}: the key is in the body");
        let key_headers: Vec<&str> = request
            .headers
            .iter()
            .filter(|(_, value)| value.contains(KEY_CANARY))
            .map(|(name, _)| name.as_str())
            .collect();
        match id.as_str() {
            "chat" => {
                assert_eq!(request.target, "/v1/chat/completions");
                assert_eq!(key_headers, ["authorization"]);
                assert_eq!(request.header("authorization"), Some(format!("Bearer {KEY_CANARY}").as_str()));
            }
            "responses" => {
                assert_eq!(request.target, "/v1/responses");
                assert_eq!(request.header("authorization"), Some(format!("Bearer {KEY_CANARY}").as_str()));
                assert_eq!(key_headers, ["authorization"]);
            }
            "messages" => {
                assert_eq!(request.target, "/v1/messages");
                assert_eq!(request.header("x-api-key"), Some(KEY_CANARY), "no prefix before the key");
                assert_eq!(request.header("anthropic-version"), Some(holzwave_core::ai::wire::anthropic::API_VERSION));
                assert!(request.header("authorization").is_none());
                assert_eq!(key_headers, ["x-api-key"]);
                // A loopback address gets the gateway form: the beta of the vendor's own service is not asked for.
                assert!(request.header("anthropic-beta").is_none(), "{:?}", request.headers);
            }
            "gemini" => {
                assert!(request.target.starts_with("/v1beta/models/"), "{}", request.target);
                assert!(request.target.ends_with(":streamGenerateContent?alt=sse"), "{}", request.target);
                assert_eq!(request.header("x-goog-api-key"), Some(KEY_CANARY));
                assert_eq!(key_headers, ["x-goog-api-key"]);
            }
            _ => {
                assert_eq!(request.target, "/api/chat");
                assert_eq!(request.header("authorization"), Some(format!("Bearer {KEY_CANARY}").as_str()));
                assert_eq!(key_headers, ["authorization"]);
            }
        }
    }
}

// ---- the thread cap -----------------------------------------------------------------------------------------------

#[test]
fn at_most_four_turns_run_at_once() {
    assert_eq!(MAX_TURN_THREADS, 4);
    let rig = Rig::new("svc-cap");
    let service = rig.service();
    let mut provider = ScriptedProvider::new("scripted");
    for _ in 0..MAX_TURN_THREADS {
        provider = provider.with_turn(Script::new().text("x").stall(Duration::from_secs(30)));
    }
    let provider = register(&service, "scripted", provider.with_turn(Script::reply("ok")));
    let chats: Vec<ConversationId> =
        (0..MAX_TURN_THREADS).map(|_| service.new_conversation().expect("a chat id")).collect();
    for (index, chat) in chats.iter().enumerate() {
        send_to(&service, chat, "scripted", "stall", index as PaneId + 1);
    }
    assert!(common::wait_until(Duration::from_secs(5), || provider.calls() == MAX_TURN_THREADS));
    assert_eq!(service.active_turns(), 4);

    let fifth = service.new_conversation().expect("a chat id");
    let refused = service.send(SendRequest::new(fifth.clone(), "one too many".to_owned(), 9).profile("scripted"));
    assert!(matches!(refused, Err(ServiceError::TooManyTurns)), "{refused:?}");
    assert_eq!(ServiceError::TooManyTurns.code(), "too_many_turns");
    assert_eq!(service.active_turns(), 4);
    assert!(rig.sink.of(&fifth).is_empty() && rig.store.load(&fifth).expect("a readable store").is_none());

    // A turn that ends frees its slot.
    assert!(service.cancel(&chats[0]));
    assert!(rig.sink.wait_ended(&chats[0], 1, Duration::from_secs(5)));
    send_to(&service, &fifth, "scripted", "now there is room", 9);
    assert!(rig.sink.wait_ended(&fifth, 1, Duration::from_secs(5)));
    assert_eq!(rig.sink.last_end(&fifth).end, "done");
    assert_eq!(service.shutdown(Duration::from_secs(5)), 0, "shutdown ends the three stalled turns");
    for chat in &chats[1..] {
        assert_eq!(rig.sink.last_end(chat).end, "cancelled");
    }
}

// ---- cancel -------------------------------------------------------------------------------------------------------

#[test]
fn cancel_ends_a_turn_within_a_quarter_second_and_keeps_the_partial_text_as_cancelled() {
    let rig = Rig::new("svc-cancel");
    let service = rig.service();
    register(
        &service,
        "scripted",
        ScriptedProvider::new("scripted")
            .with_turn(Script::new().text("partial").stall(Duration::from_secs(30)))
            .with_turn(Script::new().stall(Duration::from_secs(30)).text("late")),
    );
    let chat = service.new_conversation().expect("a chat id");

    send_to(&service, &chat, "scripted", "go on", 1);
    assert!(common::wait_until(Duration::from_secs(5), || streamed(&rig.sink, &chat) == "partial"));
    let started = Instant::now();
    assert!(service.cancel(&chat));
    assert!(rig.sink.wait_ended(&chat, 1, Duration::from_secs(1)), "the turn did not end within a second");
    println!("cancel: the turn ended {:?} after the call", started.elapsed());
    let end = rig.sink.last_end(&chat);
    assert_eq!(end.end, "cancelled");
    assert!(end.error.is_none());
    let kinds = rig.sink.kinds(&chat);
    assert!(kinds.iter().position(|kind| *kind == "message") < kinds.iter().position(|kind| *kind == "turn_ended"));
    let all = messages(&rig.sink, &chat);
    assert_eq!((all.len(), all[0].status, all[0].text().as_str()), (1, MessageStatus::Cancelled, "partial"));
    let stored = rig.store.load(&chat).expect("a readable store").expect("the chat is stored");
    assert_eq!(stored.messages.len(), 2);
    assert_eq!((stored.messages[1].status, stored.messages[1].text().as_str()), (MessageStatus::Cancelled, "partial"));
    assert!(!service.cancel(&chat), "a chat with no turn has nothing to cancel");

    // A turn that is cancelled before the provider produced anything ends cancelled with no assistant text.
    send_to(&service, &chat, "scripted", "and again", 1);
    assert!(common::wait_until(Duration::from_secs(5), || rig
        .sink
        .kinds(&chat)
        .iter()
        .filter(|k| **k == "turn_started")
        .count()
        == 2));
    assert!(service.cancel(&chat));
    assert!(rig.sink.wait_ended(&chat, 2, Duration::from_secs(1)));
    assert_eq!(rig.sink.last_end(&chat).end, "cancelled");
    assert_eq!(messages(&rig.sink, &chat).len(), 1, "no assistant message for the second turn");
}

// ---- delete -------------------------------------------------------------------------------------------------------

#[test]
fn deleting_a_chat_cancels_its_turn_and_the_turn_cannot_write_it_back() {
    let rig = Rig::on_disk("svc-delete");
    let service = rig.service();
    register(
        &service,
        "scripted",
        ScriptedProvider::new("scripted").with_turn(Script::new().text("partial").stall(Duration::from_secs(30))),
    );
    register(&service, "quick", ScriptedProvider::new("quick").with_turn(Script::reply("done")));
    let chat = service.new_conversation().expect("a chat id");

    send_to(&service, &chat, "scripted", "write me a poem", 1);
    assert!(common::wait_until(Duration::from_secs(5), || streamed(&rig.sink, &chat) == "partial"));
    assert!(rig.chats_dir().join(format!("{chat}.json")).exists(), "the user message was saved before the request");
    assert!(matches!(service.delete_conversation(&chat), Ok(true)));
    assert!(rig.sink.wait_ended(&chat, 1, Duration::from_secs(1)), "the turn ends after the delete");
    assert_eq!(rig.sink.last_end(&chat).end, "cancelled");
    let cancelled = messages(&rig.sink, &chat);
    assert_eq!((cancelled.len(), cancelled[0].status), (1, MessageStatus::Cancelled), "emitted after the delete");

    assert!(matches!(rig.store.load(&chat), Ok(None)));
    for suffix in ["json", "json.tmp", "json.bad"] {
        assert!(!rig.chats_dir().join(format!("{chat}.{suffix}")).exists(), "{chat}.{suffix} exists");
    }
    let leftovers: Vec<PathBuf> = files_below(&rig.chats_dir())
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|name| name.to_string_lossy().starts_with(chat.as_str())))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let state = service.snapshot(&chat).expect("a snapshot");
    assert!(state.messages.is_empty() && !state.running, "the deleted chat is gone: {state:?}");
    let unknown = service.new_conversation().expect("a chat id");
    assert!(matches!(service.delete_conversation(&unknown), Ok(false)));

    // The turn that ended after the delete left nothing behind: a new chat with the same id starts clean.
    let fresh = Conversation::new(chat.clone(), 5);
    rig.store.save(&fresh, ContextStorage::Marker).expect("stored");
    let state = service.snapshot(&chat).expect("a snapshot");
    assert!(state.turn == 0 && state.ended.is_none(), "a deleted turn is not remembered: {state:?}");

    // Deleting a chat whose turn has ended forgets that turn too.
    let finished = service.new_conversation().expect("a chat id");
    send_to(&service, &finished, "quick", "hello", 1);
    assert!(rig.sink.wait_ended(&finished, 1, Duration::from_secs(5)));
    assert!(service.snapshot(&finished).expect("a snapshot").ended.is_some());
    assert!(matches!(service.delete_conversation(&finished), Ok(true)));
    rig.store.save(&Conversation::new(finished.clone(), 5), ContextStorage::Marker).expect("stored");
    let state = service.snapshot(&finished).expect("a snapshot");
    assert!(state.turn == 0 && state.ended.is_none(), "an ended turn is forgotten with its chat: {state:?}");
}

// ---- pane close ---------------------------------------------------------------------------------------------------

/// A host that offers the file reader and answers with a fixed text.
struct ReadHost {
    output: String,
}

impl ReadHost {
    fn new(output: &str) -> ReadHost {
        ReadHost { output: output.to_owned() }
    }
}

impl ToolHost for ReadHost {
    fn available(&self, tool: ToolName, _pane: PaneId) -> bool {
        tool == ToolName::ReadFile
    }

    fn resolve(&self, scope: &Scope, tool: &Tool) -> Result<Resolved, ToolError> {
        let mut resolved = Resolved::remote(scope.pane, "build box");
        if let Some(path) = tool.path() {
            resolved = resolved.with_path(path);
        }
        Ok(resolved)
    }

    fn run(&self, _ticket: &Ticket, _cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new(self.output.clone()))
    }
}

#[test]
fn closing_a_pane_voids_its_pending_approval_and_the_turn_ends_cancelled() {
    let rig = Rig::new("svc-pane");
    let service = AiService::builder(rig.store.clone(), rig.sink.clone() as Arc<dyn AiSink>, &rig.config_dir)
        .tool_host(Arc::new(ReadHost::new("short text")))
        .build();
    register(
        &service,
        "scripted",
        ScriptedProvider::new("scripted").with_turn(
            Script::new()
                .tool_call("call_1", "read_file", json!({ "path": "/srv/notes.txt" }))
                .stop(StopReason::ToolUse),
        ),
    );
    let chat = service.new_conversation().expect("a chat id");

    send_to(&service, &chat, "scripted", "read the notes", 7);
    assert!(common::wait_until(Duration::from_secs(5), || rig.sink.kinds(&chat).contains(&"tool_proposed")));
    let waiting = service.approvals().pending(chat.as_str());
    assert_eq!(waiting.len(), 1);
    let proposed = rig
        .sink
        .of(&chat)
        .into_iter()
        .find(|event| matches!(event.kind, AiEventKind::ToolProposed { .. }))
        .expect("a tool_proposed event");
    assert_eq!(
        to_value(&proposed)["pending"],
        to_value(&waiting[0]),
        "the event carries the card as the table built it"
    );
    let text = to_text(&proposed);
    assert!(
        text.starts_with(r#"{"conversation":""#)
            && text.contains(r#""kind":"tool_proposed","pending":{"conversation":""#),
        "{text}"
    );

    // A pane that does not own the card leaves it alone.
    assert_eq!(service.pane_closed(99), 0);
    assert_eq!(service.approvals().pending(chat.as_str()).len(), 1);
    assert_eq!(rig.sink.ended(&chat), 0);

    // The pane that owns it closes: the card is voided and the turn ends cancelled without any decision.
    assert_eq!(service.pane_closed(7), 1);
    assert!(rig.sink.wait_ended(&chat, 1, Duration::from_secs(1)));
    assert_eq!(rig.sink.last_end(&chat).end, "cancelled");
    assert!(service.approvals().pending(chat.as_str()).is_empty());
    let results =
        rig.sink.of(&chat).into_iter().filter(|event| matches!(event.kind, AiEventKind::ToolResult { .. })).count();
    assert_eq!(results, 1, "the card gets its result so the page can close it");
    let end_state = service.snapshot(&chat).expect("a snapshot");
    assert!(end_state.pending.is_empty() && !end_state.running);
}

// ---- panic --------------------------------------------------------------------------------------------------------

/// A provider that panics with a message that holds the text canary.
struct Exploding;

impl Provider for Exploding {
    fn id(&self) -> &str {
        "exploding"
    }

    fn capabilities(&self, _model: &str) -> Capabilities {
        Capabilities::conservative(Locality::Loopback)
    }

    fn stream(
        &self,
        _req: &ChatRequest,
        _out: &mut dyn FnMut(StreamEvent) -> Flow,
        _cancel: &CancelToken,
    ) -> Result<Completion, AiError> {
        panic!("the provider exploded: {TEXT_CANARY}");
    }
}

#[test]
fn a_panic_in_a_provider_ends_the_turn_and_frees_its_slot() {
    let rig = Rig::new("svc-panic");
    let service = rig.service();
    service.register_provider("exploding", Arc::new(Exploding)).expect("registered");
    let chat = service.new_conversation().expect("a chat id");

    send_to(&service, &chat, "exploding", "boom", 1);
    assert!(rig.sink.wait_ended(&chat, 1, Duration::from_secs(5)));
    let end = rig.sink.last_end(&chat);
    assert_eq!(end.end, "failed");
    assert_eq!(end.error.as_ref().map(|error| error.text.as_str()), Some("the assistant stopped unexpectedly"));
    for event in rig.sink.of(&chat) {
        assert!(!to_text(&event).contains("exploded") && !to_text(&event).contains(TEXT_CANARY));
    }
    assert!(common::wait_until(Duration::from_secs(5), || service.active_turns() == 0));
    // The slot is free: a following send in the same chat is accepted.
    send_to(&service, &chat, "exploding", "boom again", 1);
    assert!(rig.sink.wait_ended(&chat, 2, Duration::from_secs(5)));
    Fold::of(
        &chat,
        &rig.sink.of(&chat).into_iter().filter(|event| event.turn == rig.sink.of(&chat)[0].turn).collect::<Vec<_>>(),
    );
}

// ---- the registry and the output budget ---------------------------------------------------------------------------

#[test]
fn a_registered_provider_is_used_before_a_profile_and_the_output_budget_follows_the_model() {
    let fake = FakeHttp::start(vec![Reply::Close]);
    let rig = Rig::new("svc-registry");
    rig.profile(&format!("{}/v1", fake.base()), "test-model", None);
    let service = rig.service();

    let scripted =
        register(&service, "scripted", ScriptedProvider::new("scripted").with_turn(Script::reply("registered")));
    let taken = service.register_provider("scripted", Arc::new(ScriptedProvider::new("scripted")));
    assert!(matches!(taken, Err(ServiceError::IdTaken)), "registered twice");
    let taken = service.register_provider("local", Arc::new(ScriptedProvider::new("local")));
    assert!(matches!(taken, Err(ServiceError::IdTaken)), "the id of a profile");
    assert_eq!(ServiceError::IdTaken.code(), "id_taken");

    let chat = service.new_conversation().expect("a chat id");
    send_to(&service, &chat, "scripted", "hello", 1);
    assert!(rig.sink.wait_ended(&chat, 1, Duration::from_secs(5)));
    assert_eq!(rig.sink.last_end(&chat).end, "done");
    assert_eq!(scripted.calls(), 1);
    assert_eq!(fake.accepted(), 0, "a registered provider does not reach the network");
    match &rig.sink.of(&chat)[0].kind {
        AiEventKind::TurnStarted { route, .. } => {
            assert_eq!((route.profile.as_str(), route.model.as_str()), ("scripted", "scripted"));
            assert!(route.host.is_none());
            assert_eq!(route.locality, Locality::Loopback);
        }
        other => panic!("turn_started, not {other:?}"),
    }
    let stored = rig.store.load(&chat).expect("a readable store").expect("stored");
    assert_eq!((stored.profile.as_deref(), stored.model.as_deref()), (Some("scripted"), Some("scripted")));

    // The output budget: 16000 for a model that reasons, the loop's 8192 for any other, both clamped by the model.
    let caps = |reasoning: bool, max: u32| Capabilities {
        tools: true,
        reasoning,
        vision: false,
        context_tokens: Some(200_000),
        max_output_tokens: Some(max),
        locality: Locality::Loopback,
    };
    let budgets = [
        ("thinker", caps(true, 64_000), Some(16_000)),
        ("plain", caps(false, 64_000), Some(8192)),
        ("small", caps(true, 4096), Some(4096)),
    ];
    for (id, capabilities, expected) in budgets {
        let provider = register(
            &service,
            id,
            ScriptedProvider::new(id).with_capabilities(capabilities).with_turn(Script::reply("ok")),
        );
        let chat = service.new_conversation().expect("a chat id");
        send_to(&service, &chat, id, "how long may you write", 1);
        assert!(rig.sink.wait_ended(&chat, 1, Duration::from_secs(5)), "{id}");
        assert_eq!(provider.requests()[0].max_output_tokens, expected, "{id}");
    }

    // A service that is shut down ends its turns and refuses new ones.
    assert_eq!(service.shutdown(Duration::from_secs(1)), 0);
    let chat = service.new_conversation().expect("a chat id");
    let refused = service.send(SendRequest::new(chat, "late".to_owned(), 1).profile("scripted"));
    assert!(matches!(refused, Err(ServiceError::ShuttingDown)));
    assert_eq!(ServiceError::ShuttingDown.code(), "shutting_down");
}

// ---- context ------------------------------------------------------------------------------------------------------

/// The text of a context that appears in test code only: a hit anywhere else is a leak.
const CTX_CANARY: &str = "zq-ctx-canary-0b7d52c8a1";

/// A context the user approved for `provider`, made through the service's own gate.
fn approve(service: &AiService, provider: &str, text: &str) -> ApprovedContext {
    let destination = Destination { host: "127.0.0.1".to_owned(), locality: Locality::Loopback };
    let item = RequestItem::new(ContextKind::Selection, "pane 1", SourceText::plain(text));
    let offer = service.gate().offer(OfferRequest::new(provider, destination, vec![item])).expect("an offer");
    service.gate().approve(offer.id(), &[1], provider).expect("an approval")
}

/// The raw text of the file of a chat.
fn chat_file(rig: &Rig, chat: &ConversationId) -> String {
    fs::read_to_string(rig.chats_dir().join(format!("{chat}.json"))).expect("the chat file")
}

fn setting(local_only: bool, persist: bool) -> Settings {
    Settings { ai_local_only: local_only, ai_persist_context: persist, ..Settings::default() }
}

#[test]
fn approved_context_goes_out_once_with_its_turn_and_a_follow_up_does_not_send_it_again() {
    let fake = FakeHttp::start(vec![answer(&["Hello"], Duration::ZERO)]);
    let rig = Rig::on_disk("svc-context");
    rig.profile(&format!("{}/v1", fake.base()), "test-model", None);
    let service = rig.service();
    let chat = service.new_conversation().expect("a chat id");
    let approved = approve(&service, "local", &format!("output {CTX_CANARY}"));
    let hash = approved.hash_hex();
    let typed = format!("what does this mean? {TEXT_CANARY}");

    service
        .send(SendRequest::new(chat.clone(), typed.clone(), 1).profile("local").context(vec![approved.clone()]))
        .expect("accepted");
    assert!(rig.sink.wait_ended(&chat, 1, Duration::from_secs(5)));
    assert_eq!(rig.sink.last_end(&chat).end, "done");

    // The text goes out once, wrapped in the envelope, before the instruction.
    let first = fake.seen()[0].body_text();
    assert_eq!(first.matches(CTX_CANARY).count(), 1, "the context text is in the request exactly once");
    assert!(first.find(CTX_CANARY) < first.find(TEXT_CANARY), "the context comes before the instruction");
    assert!(first.contains("holzwave-context"));
    // The page sees the label and the size, never the text.
    let started = rig.sink.of(&chat).into_iter().next().expect("events");
    let AiEventKind::TurnStarted { message, .. } = &started.kind else { panic!("turn_started first") };
    let ChatPart::Context { hash: shown, items, provider, locality } = &message.parts[0] else {
        panic!("a context part first, not {:?}", message.parts[0])
    };
    assert_eq!((shown.as_str(), provider.as_str(), *locality), (hash.as_str(), "local", Locality::Loopback));
    assert_eq!((items[0].label.as_str(), items[0].bytes), ("pane 1", approved.items()[0].bytes));
    assert!(matches!(message.parts[1], ChatPart::Text { .. }));
    for event in rig.sink.of(&chat) {
        assert!(!to_text(&event).contains(CTX_CANARY) && !format!("{event:?}").contains(CTX_CANARY));
    }
    // The file keeps the note with its hash and the consent, and no text.
    let file = chat_file(&rig, &chat);
    assert!(file.contains(&hash) && file.contains("consented_to"), "{file}");
    assert!(!file.contains(CTX_CANARY));

    // A follow-up does not send the context again; it holds the first answer.
    rig.say(&service, &chat, "and one more thing");
    let second = fake.seen()[1].body_text();
    assert!(!second.contains(CTX_CANARY), "the context was sent again");
    assert!(second.contains("Hello"), "the first answer is part of the history");
    assert_eq!(fake.accepted(), 2);

    // Too many, the same twice, and one approved for another provider.
    let fresh = service.new_conversation().expect("a chat id");
    let five: Vec<ApprovedContext> = (0..5).map(|index| approve(&service, "local", &format!("part {index}"))).collect();
    let result = service.send(SendRequest::new(fresh.clone(), "x".to_owned(), 1).profile("local").context(five));
    assert!(matches!(result, Err(ServiceError::ContextTooMany)), "{result:?}");
    let twice = vec![approved.clone(), approved.clone()];
    let result = service.send(SendRequest::new(fresh.clone(), "x".to_owned(), 1).profile("local").context(twice));
    assert!(matches!(result, Err(ServiceError::ContextDuplicate)), "{result:?}");
    assert!(rig.store.load(&fresh).expect("a readable store").is_none() && rig.sink.of(&fresh).is_empty());
    assert_eq!(service.active_turns(), 0);
    let foreign = approve(&service, "other", &format!("secret {CTX_CANARY}"));
    service
        .send(SendRequest::new(fresh.clone(), "x".to_owned(), 1).profile("local").context(vec![foreign]))
        .expect("accepted; the refusal arrives as the end of the turn");
    assert!(rig.sink.wait_ended(&fresh, 1, Duration::from_secs(5)));
    let end = rig.sink.last_end(&fresh);
    let error = end.error.expect("an error");
    assert_eq!((end.end.as_str(), error.class.as_str()), ("failed", "policy"));
    assert_eq!(error.text, "the shared context was approved for another provider");
    assert_eq!(rig.sink.kinds(&fresh), ["turn_ended"]);
    assert!(rig.store.load(&fresh).expect("a readable store").is_none(), "nothing is stored");
    assert_eq!(fake.accepted(), 2, "no request left for a context of another provider");
}

/// A chat store whose private-text rewrite can fail or wait for the test.
struct ScrubStore {
    inner: JsonDirStore,
    mode: ScrubMode,
    calls: AtomicUsize,
}

enum ScrubMode {
    Pass,
    Fail,
    /// Waits until the flag is set.
    Block(Arc<(Mutex<bool>, Condvar)>),
}

impl ConversationStore for ScrubStore {
    fn list(&self) -> Result<Listing, StoreError> {
        self.inner.list()
    }

    fn load(&self, id: &ConversationId) -> Result<Option<Conversation>, StoreError> {
        self.inner.load(id)
    }

    fn save(&self, conversation: &Conversation, storage: ContextStorage) -> Result<(), StoreError> {
        self.inner.save(conversation, storage)
    }

    fn delete(&self, id: &ConversationId) -> Result<bool, StoreError> {
        self.inner.delete(id)
    }

    fn scrub_private_text(&self) -> Result<Scrub, StoreError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.mode {
            ScrubMode::Pass => self.inner.scrub_private_text(),
            ScrubMode::Fail => Err(StoreError::Io("the disk went away".to_owned())),
            ScrubMode::Block(gate) => {
                let (released, wake) = &**gate;
                let mut open = lock(released);
                while !*open {
                    open = wake.wait(open).unwrap_or_else(PoisonError::into_inner);
                }
                drop(open);
                self.inner.scrub_private_text()
            }
        }
    }
}

fn scrub_rig(tag: &str, mode: ScrubMode) -> (Rig, Arc<ScrubStore>) {
    let dir = common::scratch_dir(tag);
    let store = Arc::new(ScrubStore { inner: JsonDirStore::new(dir.join("chats")), mode, calls: AtomicUsize::new(0) });
    (Rig::in_dir(dir, store.clone()), store)
}

#[test]
fn terminal_context_is_stored_as_a_marker_and_switching_the_persist_setting_off_scrubs_the_files() {
    let fake = FakeHttp::start(vec![answer(&["Hello"], Duration::ZERO)]);
    let (rig, store) = scrub_rig("svc-persist", ScrubMode::Pass);
    rig.profile(&format!("{}/v1", fake.base()), "test-model", None);
    let service = rig.service();
    let chat = service.new_conversation().expect("a chat id");
    let say_with = |name: &str, expected_turns: usize| {
        let approved = approve(&service, "local", &format!("{CTX_CANARY}-{name}"));
        service
            .send(SendRequest::new(chat.clone(), format!("about {name}"), 1).profile("local").context(vec![approved]))
            .expect("accepted");
        assert!(rig.sink.wait_ended(&chat, expected_turns, Duration::from_secs(5)));
        chat_file(&rig, &chat)
    };

    // The default stores a marker.
    let file = say_with("a", 1);
    assert!(!file.contains(&format!("{CTX_CANARY}-a")), "a context is a marker by default");
    // Switching it on stores the text of the next context.
    let effect = service.apply_settings(&setting(false, true));
    assert_eq!((effect.cancelled, effect.scrub_started), (0, false));
    let file = say_with("b", 2);
    assert_eq!(file.matches(&format!("{CTX_CANARY}-b")).count(), 1, "the text is kept while the setting is on");
    assert!(!file.contains(&format!("{CTX_CANARY}-a")));
    // The same value again changes nothing.
    assert_eq!(
        service.apply_settings(&setting(false, true)),
        holzwave_core::ai::service::SettingsEffect { cancelled: 0, scrub_started: false }
    );
    // Switching it off rewrites the files on a thread of its own.
    assert!(service.scrub_report().is_none());
    let effect = service.apply_settings(&setting(false, false));
    assert!(effect.scrub_started);
    assert!(common::wait_until(Duration::from_secs(5), || service.scrub_report().is_some()));
    let report = service.scrub_report().expect("a report");
    assert!(!report.failed && report.rewritten >= 1, "{report:?}");
    assert!(!chat_file(&rig, &chat).contains(&format!("{CTX_CANARY}-b")), "the text is gone from the file");
    // And a later context is a marker again.
    let file = say_with("c", 3);
    assert!(!file.contains(&format!("{CTX_CANARY}-c")));
    assert!(!service.apply_settings(&setting(false, false)).scrub_started, "nothing changed");
    assert_eq!(store.calls.load(Ordering::SeqCst), 1);

    // Tool output follows the same switch as terminal context.
    let tools = Rig::on_disk("svc-persist-tools");
    let service = AiService::builder(tools.store.clone(), tools.sink.clone() as Arc<dyn AiSink>, &tools.config_dir)
        .tool_host(Arc::new(ReadHost::new(&format!("{CTX_CANARY}-tool"))))
        .build();
    let mut provider = ScriptedProvider::new("scripted");
    for _ in 0..2 {
        provider = provider
            .with_turn(
                Script::new()
                    .tool_call("call_1", "read_file", json!({ "path": "/srv/notes.txt" }))
                    .stop(StopReason::ToolUse),
            )
            .with_turn(Script::reply("done"));
    }
    register(&service, "scripted", provider);
    for keep in [false, true] {
        service.apply_settings(&setting(false, keep));
        let chat = service.new_conversation().expect("a chat id");
        send_to(&service, &chat, "scripted", "read the notes", 1);
        assert!(common::wait_until(Duration::from_secs(5), || tools.sink.kinds(&chat).contains(&"tool_proposed")));
        let card = service.approvals().pending(chat.as_str()).remove(0);
        service.approvals().decide(chat.as_str(), &card.call_id, Decision::Allow, &card.hash).expect("decided");
        assert!(tools.sink.wait_ended(&chat, 1, Duration::from_secs(5)));
        assert_eq!(tools.sink.last_end(&chat).end, "done");
        let file = chat_file(&tools, &chat);
        assert_eq!(
            file.contains(&format!("{CTX_CANARY}-tool")),
            keep,
            "tool output is stored only while the setting is on"
        );
        assert_eq!(file.contains(NOT_STORED), !keep);
    }

    // A store that fails: the report says so.
    let (rig, _) = scrub_rig("svc-scrub-fail", ScrubMode::Fail);
    let service = rig.service();
    service.apply_settings(&setting(false, true));
    assert!(service.apply_settings(&setting(false, false)).scrub_started);
    assert!(common::wait_until(Duration::from_secs(5), || service.scrub_report().is_some()));
    assert_eq!(service.scrub_report().map(|report| report.failed), Some(true));

    // A store that waits: a second switch-off does not start a second rewrite.
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (rig, store) = scrub_rig("svc-scrub-wait", ScrubMode::Block(gate.clone()));
    let service = rig.service();
    service.apply_settings(&setting(false, true));
    assert!(service.apply_settings(&setting(false, false)).scrub_started);
    assert!(common::wait_until(Duration::from_secs(5), || store.calls.load(Ordering::SeqCst) == 1));
    service.apply_settings(&setting(false, true));
    assert!(!service.apply_settings(&setting(false, false)).scrub_started, "one rewrite runs at a time");
    assert!(service.scrub_report().is_none(), "the rewrite has not ended");
    *lock(&gate.0) = true;
    gate.1.notify_all();
    assert!(common::wait_until(Duration::from_secs(5), || service.scrub_report().is_some()));
    assert_eq!(store.calls.load(Ordering::SeqCst), 1);
}

// ---- restart ------------------------------------------------------------------------------------------------------

fn fixed_clock() -> i64 {
    1_800_000_000
}

#[test]
fn a_restarted_service_continues_a_stored_chat_repairs_a_dangling_call_and_never_reuses_an_id() {
    let fake = FakeHttp::start(vec![answer(&["Hello"], Duration::ZERO)]);
    let rig = Rig::on_disk("svc-restart");
    rig.profile(&format!("{}/v1", fake.base()), "test-model", None);
    let build = || {
        AiService::builder(rig.store.clone(), rig.sink.clone() as Arc<dyn AiSink>, &rig.config_dir)
            .vault(rig.vault())
            .clock(fixed_clock)
            .build()
    };
    let first = build();
    let chat = first.new_conversation().expect("a chat id");
    let before =
        first.send(SendRequest::new(chat.clone(), "first run".to_owned(), 1).profile("local")).expect("accepted");
    assert!(rig.sink.wait_ended(&chat, 1, Duration::from_secs(5)));
    assert_eq!(first.shutdown(Duration::from_secs(1)), 0);
    drop(first);

    // A second service over the same directory with the same clock draws the same first number; the chat's ids stay unique.
    let second = build();
    let reopened = second.snapshot(&chat).expect("a snapshot");
    assert_eq!(reopened.messages.len(), 2, "both stored messages are there");
    assert!(!reopened.running && reopened.title == "first run");
    let after =
        second.send(SendRequest::new(chat.clone(), "second run".to_owned(), 1)).expect("accepted: the chat is pinned");
    assert_ne!(after.turn, before.turn, "the number of the first run is not used again");
    assert!(rig.sink.wait_ended(&chat, 2, Duration::from_secs(5)));
    let stored = rig.store.load(&chat).expect("a readable store").expect("stored");
    let mut ids: Vec<&str> = stored.messages.iter().map(|message| message.id.as_str()).collect();
    assert_eq!(ids.len(), 4);
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 4, "every message id is unique: {ids:?}");
    assert_eq!(fake.accepted(), 2);

    // A chat that was interrupted holds a call with no result; the snapshot and the next request show the repair.
    let broken = second.new_conversation().expect("a chat id");
    let mut stored = Conversation::new(broken.clone(), 100);
    stored.profile = Some("local".to_owned());
    stored.model = Some("test-model".to_owned());
    stored.title = "interrupted".to_owned();
    stored.messages = vec![
        Message::user_text("m5-user", "read it", 100),
        Message::new(
            "m5-1",
            Role::Assistant,
            vec![Part::ToolCall(ToolCall {
                id: "call_1".to_owned(),
                name: "read_file".to_owned(),
                args: json!({ "path": "/srv/a" }),
            })],
            101,
        ),
    ];
    rig.store.save(&stored, ContextStorage::Marker).expect("a chat is stored");
    let state = second.snapshot(&broken).expect("a snapshot");
    assert_eq!(state.messages[1].status, MessageStatus::Interrupted);
    let repair = state.messages.last().expect("a repair message");
    let ChatPart::ToolResult { content, is_error, .. } = &repair.parts[0] else {
        panic!("a tool result, not {:?}", repair.parts[0])
    };
    assert_eq!((content.as_str(), *is_error), (INTERRUPTED_RESULT, true));
    second.send(SendRequest::new(broken.clone(), "go on".to_owned(), 1)).expect("accepted");
    assert!(rig.sink.wait_ended(&broken, 1, Duration::from_secs(5)));
    assert!(fake.seen()[2].body_text().contains(INTERRUPTED_RESULT), "the next request holds the repaired transcript");
    let repaired = rig.store.load(&broken).expect("a readable store").expect("stored");
    let ids: Vec<&str> = repaired.messages.iter().map(|message| message.id.as_str()).collect();
    assert!(ids.contains(&"m5-1-repair"), "the saved chat holds the repair: {ids:?}");
    assert_eq!(repaired.messages[1].status, MessageStatus::Interrupted);

    // A chat id with no file is an empty state.
    let unknown = second.new_conversation().expect("a chat id");
    let empty = second.snapshot(&unknown).expect("a snapshot");
    assert!(empty.messages.is_empty() && empty.title.is_empty() && empty.turn == 0);

    // The list is newest first.
    for (name, updated) in [("old", 10), ("new", 30), ("mid", 20)] {
        let mut listed = Conversation::new(ConversationId::parse(name).expect("an id"), updated);
        listed.title = name.to_owned();
        rig.store.save(&listed, ContextStorage::Marker).expect("stored");
    }
    let listing = second.list_conversations().expect("a listing");
    assert!(listing.chats.windows(2).all(|pair| pair[0].updated >= pair[1].updated), "newest first");
    let order: Vec<&str> =
        listing.chats.iter().map(|listed| listed.id.as_str()).filter(|id| ["old", "new", "mid"].contains(id)).collect();
    assert_eq!(order, ["new", "mid", "old"]);
}

// ---- titles -------------------------------------------------------------------------------------------------------

#[test]
fn titles_are_the_first_line_of_the_user_text_scrubbed_and_cut() {
    let line = "abcdefghij".repeat(20);
    let long = format!("  \n {line} \n second line");
    assert_eq!(title_from(&long), format!("{}\u{2026}", "abcdefghij".repeat(6)));
    assert_eq!(title_from("short title\nsecond"), "short title");
    assert_eq!(title_from(&"x".repeat(60)), "x".repeat(60), "a line of exactly 60 characters is not cut");
    let dirty = title_from("one\u{202e}two\u{200b}three\u{1b}four");
    assert!(!dirty.chars().any(|c| matches!(c, '\u{202e}' | '\u{200b}' | '\u{1b}')), "{dirty:?}");
    assert!(dirty.starts_with("one") && dirty.ends_with("four"));
    assert_eq!(title_from(" \n\t \n"), "");

    // A chat keeps its first title, and the title of the live state and of the stored chat are the same. No request is
    // made for a title.
    let fake = FakeHttp::start(vec![answer(&["Hello"], Duration::ZERO)]);
    let rig = Rig::new("svc-titles");
    rig.profile(&format!("{}/v1", fake.base()), "test-model", None);
    let service = rig.service();
    let chat = service.new_conversation().expect("a chat id");
    rig.say(&service, &chat, &format!("How do I list files?\nsecond line {TEXT_CANARY}"));
    rig.say(&service, &chat, "something else entirely");
    let stored = rig.store.load(&chat).expect("a readable store").expect("stored");
    assert_eq!(stored.title, "How do I list files?");
    assert_eq!(service.snapshot(&chat).expect("a snapshot").title, stored.title);
    let folded = Fold::of(
        &chat,
        &rig.sink.of(&chat).into_iter().filter(|event| event.turn == rig.sink.of(&chat)[0].turn).collect::<Vec<_>>(),
    );
    assert_eq!(folded.state.title, stored.title);
    assert_eq!(fake.accepted(), 2, "two sends, two requests, none for a title");
}

// ---- revocation and local only ------------------------------------------------------------------------------------

/// A scripted provider that says `partial` and then goes quiet, with these capabilities.
fn quiet_provider(id: &str, locality: Locality) -> ScriptedProvider {
    let capabilities = Capabilities {
        tools: false,
        reasoning: false,
        vision: false,
        context_tokens: Some(32_768),
        max_output_tokens: Some(4096),
        locality,
    };
    ScriptedProvider::new(id)
        .with_capabilities(capabilities)
        .with_turn(Script::new().text("partial").stall(Duration::from_secs(30)))
}

#[test]
fn revoking_a_profile_or_switching_local_only_on_cancels_the_turns_it_forbids() {
    // Removing a profile and clearing its key stop the turns that use it, and only those.
    let fake = FakeHttp::start(vec![Reply::Hang]);
    let rig = Rig::new("svc-revoke");
    let vault = rig.vault();
    let mut config = AiConfig::default();
    let base = format!("{}/v1", fake.base());
    add_profile(&mut config, &vault, "Alpha", Wire::OpenAiChat, &base, "test-model", Some(KEY_CANARY));
    add_profile(&mut config, &vault, "Beta", Wire::OpenAiChat, &base, "test-model", Some(KEY_CANARY));
    config.save(&rig.config_dir).expect("ai.json");
    let service = rig.service();
    let chats: Vec<ConversationId> = (0..3).map(|_| service.new_conversation().expect("a chat id")).collect();
    for (chat, profile) in chats.iter().zip(["alpha", "alpha", "beta"]) {
        send_to(&service, chat, profile, "hang", 1);
    }
    assert!(fake.wait_accepted(3, Duration::from_secs(5)), "all three requests are out");
    assert_eq!(service.active_turns(), 3);

    let removal = service.remove_profile("alpha").expect("removed");
    assert!(removal.removed && removal.key_deleted);
    assert!(
        rig.sink.wait_ended(&chats[0], 1, Duration::from_secs(1))
            && rig.sink.wait_ended(&chats[1], 1, Duration::from_secs(1))
    );
    assert_eq!(rig.sink.last_end(&chats[0]).end, "cancelled");
    assert_eq!(rig.sink.last_end(&chats[1]).end, "cancelled");
    assert_eq!(rig.sink.ended(&chats[2]), 0, "a turn of another profile keeps running");
    assert_eq!(service.active_turns(), 1);
    let names: Vec<String> = service.profiles().into_iter().map(|profile| profile.id).collect();
    assert_eq!(names, ["beta"]);
    assert!(rig.secrets.get(&account("alpha", KeySlot::Provider)).expect("a readable keychain").is_none());

    service.clear_key("beta", KeySlot::Provider).expect("cleared");
    assert!(rig.sink.wait_ended(&chats[2], 1, Duration::from_secs(1)));
    assert_eq!(rig.sink.last_end(&chats[2]).end, "cancelled");
    assert_eq!(service.profiles()[0].key, KeyStatus::NotSet);
    assert!(rig.secrets.get(&account("beta", KeySlot::Provider)).expect("a readable keychain").is_none());

    // Switching "local only" on stops the turns whose destination leaves the device, and refuses new ones.
    let rig = Rig::new("svc-local-only");
    let service = rig.service();
    let cloud =
        register(&service, "cloud", quiet_provider("cloud", Locality::Cloud).with_turn(Script::reply("now allowed")));
    let device = register(&service, "device", quiet_provider("device", Locality::OnDevice));
    let (cloud_chat, device_chat) =
        (service.new_conversation().expect("a chat id"), service.new_conversation().expect("a chat id"));
    send_to(&service, &cloud_chat, "cloud", "hello", 1);
    send_to(&service, &device_chat, "device", "hello", 2);
    assert!(common::wait_until(Duration::from_secs(5), || cloud.calls() == 1 && device.calls() == 1));
    let effect = service.apply_settings(&setting(true, false));
    assert_eq!((effect.cancelled, effect.scrub_started), (1, false));
    assert!(rig.sink.wait_ended(&cloud_chat, 1, Duration::from_secs(1)));
    assert_eq!(rig.sink.last_end(&cloud_chat).end, "cancelled");
    assert_eq!(rig.sink.ended(&device_chat), 0, "an on-device turn keeps running");
    assert_eq!(service.apply_settings(&setting(true, false)).cancelled, 0, "the same setting again stops nothing");

    let refused = service.new_conversation().expect("a chat id");
    send_to(&service, &refused, "cloud", "hello again", 1);
    assert!(rig.sink.wait_ended(&refused, 1, Duration::from_secs(5)));
    let end = rig.sink.last_end(&refused);
    let error = end.error.expect("an error");
    assert_eq!((end.end.as_str(), error.class.as_str(), error.text.as_str()), ("failed", "policy", "local only is on"));
    assert_eq!(cloud.calls(), 1, "the provider was not called");

    service.apply_settings(&setting(false, false));
    let allowed = service.new_conversation().expect("a chat id");
    send_to(&service, &allowed, "cloud", "hello once more", 1);
    assert!(rig.sink.wait_ended(&allowed, 1, Duration::from_secs(5)));
    assert_eq!(rig.sink.last_end(&allowed).end, "done");
    assert_eq!(service.shutdown(Duration::from_secs(5)), 0);
}

// ---- profile calls ------------------------------------------------------------------------------------------------

#[test]
fn profile_calls_save_after_every_change_and_no_call_returns_a_key() {
    let fake = FakeHttp::start(vec![Reply::Close]);
    let rig = Rig::new("svc-profiles");
    let service = rig.service();
    let file = rig.config_dir.join("ai.json");
    let base = format!("{}/v1", fake.base());

    let mut input = ProfileInput::new("Workstation", Preset::Custom);
    input.wire = Some(Wire::OpenAiChat);
    input.base_url = base.clone();
    input.model = "test-model".to_owned();
    let id = service.save_profile(input).expect("saved");
    assert_eq!(id, "workstation");
    let text = fs::read_to_string(&file).expect("ai.json is written");
    let (parsed, status) = AiConfig::from_json(&text);
    assert!(matches!(status, holzwave_core::layout::LoadStatus::Clean));
    assert!(parsed.get("workstation").is_some_and(|profile| !profile.key_set));
    assert!(!text.contains(KEY_CANARY));

    service.set_key("workstation", KeySlot::Provider, ApiKey::new(KEY_CANARY.to_owned())).expect("the key is saved");
    let views = service.profiles();
    assert_eq!(views[0].key, KeyStatus::Present);
    let text = fs::read_to_string(&file).expect("ai.json");
    assert!(
        AiConfig::from_json(&text).0.get("workstation").is_some_and(|profile| profile.key_set),
        "the marker is saved"
    );
    assert!(!text.contains(KEY_CANARY) && !to_text(&views).contains(KEY_CANARY));
    let entry = rig.secrets.get(&account("workstation", KeySlot::Provider)).expect("a keychain").expect("an entry");
    assert!(entry.contains(KEY_CANARY), "the key lives in the keychain");

    service.clear_key("workstation", KeySlot::Provider).expect("cleared");
    assert_eq!(service.profiles()[0].key, KeyStatus::NotSet);
    assert!(rig.secrets.get(&account("workstation", KeySlot::Provider)).expect("a keychain").is_none());
    let removal = service.remove_profile("workstation").expect("removed");
    assert!(removal.removed);
    assert!(AiConfig::from_json(&fs::read_to_string(&file).expect("ai.json")).0.profiles().is_empty());

    // The id of a registered provider is taken, whether it is typed or made from the name.
    register(&service, "scripted", ScriptedProvider::new("scripted"));
    let mut typed = ProfileInput::new("Anything", Preset::Custom);
    typed.id = Some("scripted".to_owned());
    typed.wire = Some(Wire::OpenAiChat);
    typed.base_url = base.clone();
    assert!(matches!(service.save_profile(typed), Err(ServiceError::IdTaken)));
    let mut named = ProfileInput::new("Scripted", Preset::Custom);
    named.wire = Some(Wire::OpenAiChat);
    named.base_url = base;
    assert!(matches!(service.save_profile(named), Err(ServiceError::IdTaken)));
    assert!(service.profiles().is_empty(), "a refused profile is not kept");

    // A refusal shows its code and a fixed phrase, never the input.
    let bad = ProfileInput::new(format!("bad\u{7}name {TEXT_CANARY}"), Preset::Custom);
    let error = service.save_profile(bad).expect_err("a name with a control character");
    assert_eq!(error.code(), "profile");
    assert!(error.to_string().contains(&ProfileError::Name.to_string()) && !error.to_string().contains(TEXT_CANARY));
    assert!(!format!("{error:?}").contains(TEXT_CANARY));
    let error =
        service.set_key("nobody", KeySlot::Provider, ApiKey::new(KEY_CANARY.to_owned())).expect_err("no such profile");
    assert_eq!(error.code(), "key");
    assert!(matches!(error, ServiceError::Key(KeyError::UnknownProfile)));
    assert!(!error.to_string().contains(KEY_CANARY) && !format!("{error:?}").contains(KEY_CANARY));
    assert_eq!(fake.accepted(), 0, "the profile calls open no connection");

    // A damaged file starts the service with no profiles, a note that quotes nothing, and the bytes kept aside.
    let damaged = Rig::new("svc-damaged");
    fs::write(damaged.config_dir.join("ai.json"), format!("{{\"profiles\": [ {TEXT_CANARY}")).expect("a damaged file");
    let service = damaged.service();
    assert!(service.profiles().is_empty());
    let notes = service.config_notes();
    assert!(!notes.is_empty() && notes.iter().all(|note| !note.contains(TEXT_CANARY)), "{notes:?}");
    let kept = fs::read_to_string(damaged.config_dir.join("ai.json.bad")).expect("the bytes are kept");
    assert!(kept.contains(TEXT_CANARY));
}

// ---- Debug and views ----------------------------------------------------------------------------------------------

#[test]
fn no_service_type_prints_text_or_a_key_in_debug_and_no_view_carries_a_native_turn_or_stored_context() {
    let rig = Rig::new("svc-debug");
    let base = "http://127.0.0.1:9/v1";
    rig.profile(base, "test-model", Some(KEY_CANARY));
    let builder =
        AiService::builder(rig.store.clone(), rig.sink.clone() as Arc<dyn AiSink>, &rig.config_dir).vault(rig.vault());
    let shown = format!("{builder:?}{builder:#?}");
    let service = builder.build();
    let chat = service.new_conversation().expect("a chat id");
    let approved = approve(&service, "local", &format!("output {CTX_CANARY}"));
    let request = SendRequest::new(chat.clone(), format!("question {TEXT_CANARY}"), 3)
        .profile("local")
        .context(vec![approved.clone()]);
    let ticket = TurnTicket { conversation: chat.clone(), turn: 5 };
    let errors = [
        ServiceError::EmptyText,
        ServiceError::TextTooLong,
        ServiceError::Busy,
        ServiceError::TooManyTurns,
        ServiceError::ShuttingDown,
        ServiceError::NoProfile,
        ServiceError::ContextTooMany,
        ServiceError::ContextDuplicate,
        ServiceError::Spawn,
        ServiceError::Store(StoreError::Io(format!("{KEY_CANARY} {TEXT_CANARY}"))),
        ServiceError::Profile(ProfileError::Name),
        ServiceError::Key(KeyError::Invalid("the key is empty")),
        ServiceError::ConfigWrite,
        ServiceError::IdTaken,
    ];
    let mut shown = shown;
    shown.push_str(&format!("{service:?}{service:#?}{request:?}{request:#?}{ticket:?}{ticket:#?}"));
    for error in &errors {
        shown.push_str(&format!("{error:?}{error:#?}{error}"));
    }

    // Every kind of event, a state and its parts, with the canaries in every text field.
    let mut message = Message::new(
        "m1-user",
        Role::User,
        vec![
            Part::Context(ContextNote::for_approved(&approved, Some(format!("stored {CTX_CANARY}")))),
            Part::Text { text: format!("question {TEXT_CANARY}") },
            Part::ToolCall(ToolCall {
                id: "c1".to_owned(),
                name: "read_file".to_owned(),
                args: json!({ "path": TEXT_CANARY }),
            }),
        ],
        100,
    );
    message.native = Some(Opaque { wire: "anthropic".to_owned(), json: json!({ "signature": "NATIVE-BLOB" }) });
    let view = ChatMessage::from_message(&message);
    let route = Route {
        profile: "local".to_owned(),
        model: "test-model".to_owned(),
        host: Some("127.0.0.1:9".to_owned()),
        locality: Locality::Loopback,
    };
    let end = EndView {
        end: "failed".to_owned(),
        error: Some(ErrorView { class: "server".to_owned(), text: format!("{TEXT_CANARY} {KEY_CANARY}") }),
        rounds: 1,
        usage: Usage::default(),
        saved: true,
    };
    let kinds = vec![
        AiEventKind::TurnStarted { message: view.clone(), route: route.clone() },
        AiEventKind::Delta { text: TEXT_CANARY.to_owned() },
        AiEventKind::Reasoning { text: TEXT_CANARY.to_owned() },
        AiEventKind::Usage(Usage::default()),
        AiEventKind::Trimmed(Trimmed { results: 1, exchanges: 1 }),
        AiEventKind::Message { message: view.clone() },
        AiEventKind::ToolProposed { pending: pending_card("c1") },
        AiEventKind::ToolResult {
            call_id: "c1".to_owned(),
            tool: "read_file".to_owned(),
            content: TEXT_CANARY.to_owned(),
            is_error: false,
            redactions: 0,
        },
        AiEventKind::TurnEnded(end.clone()),
    ];
    let mut state = ChatState::new("c1");
    for (index, kind) in kinds.into_iter().enumerate() {
        let event = event(1, index as u64 + 1, kind);
        shown.push_str(&format!("{event:?}{event:#?}"));
        state.apply(&event);
    }
    state.partial = Partial { text: TEXT_CANARY.to_owned(), reasoning: TEXT_CANARY.to_owned() };
    shown.push_str(&format!("{state:?}{state:#?}{view:?}{view:#?}{route:?}{end:?}{end:#?}"));
    for part in &view.parts {
        shown.push_str(&format!("{part:?}{part:#?}"));
    }
    for canary in [KEY_CANARY, TEXT_CANARY, CTX_CANARY, "NATIVE-BLOB"] {
        assert!(!shown.contains(canary), "{canary} is printed");
    }

    // A stored assistant message with a native turn reaches the page without it.
    let held = service.new_conversation().expect("a chat id");
    let mut stored = Conversation::new(held.clone(), 100);
    let mut reply = Message::new("m1-1", Role::Assistant, vec![Part::Text { text: "an answer".to_owned() }], 101);
    reply.native = Some(Opaque { wire: "anthropic".to_owned(), json: json!({ "signature": "NATIVE-BLOB" }) });
    stored.messages = vec![Message::user_text("m1-user", "hello", 100), reply];
    rig.store.save(&stored, ContextStorage::Text).expect("stored");
    let snapshot = to_text(&service.snapshot(&held).expect("a snapshot"));
    assert!(snapshot.contains("an answer"));
    for word in ["NATIVE-BLOB", "native", "stored", "signature"] {
        assert!(!snapshot.contains(word), "the snapshot holds {word}: {snapshot}");
    }
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
