//! The canary-key leak suite: where a key and the text of a pane may and may not appear.
//!
//! The API key may exist in the vault and in the one header of its wire, nowhere else. The terminal canaries and the
//! file value may exist nowhere that is shown, sent, stored or printed; only their typed placeholders may. Text the
//! user typed may be in events, snapshots and stored chats, because it is the user's own. The suite checks the `Debug`
//! text of every type that can hold such a value, the serialised views, the errors of five wires that echo the key, every
//! file a full session leaves, the output streams of a run under the trace variable, and the sources: no logger, no print,
//! and a table of the places that read key text. Test code only; a failing assertion names the type, the sink or the
//! canary, never the bytes.

mod common;
mod security;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use common::fake_http::{FakeHttp, Reply, Seen};
use holzwave_core::ai::agent::{AgentEvent, TurnEnd, TurnOutcome, TurnRequest};
use holzwave_core::ai::gate::{
    ApprovedContext, ContextOffer, Destination, Gate, OfferId, OfferItem, OfferRequest, RequestItem,
};
use holzwave_core::ai::http::{Call, Framing, Header, Http, HttpConfig, NetworkPolicy, Origin, SecretHeader, Target};
use holzwave_core::ai::profiles::{AiConfig, KeySlot, KeyVault, Preset, ProfileInput, ResolveError};
use holzwave_core::ai::provider::{AiError, ChatRequest, Detail};
use holzwave_core::ai::redact::{Cleaned, Keep, SourceText, clean_text};
use holzwave_core::ai::service::context::ContextSource;
use holzwave_core::ai::service::events::{
    AiEvent, AiEventKind, ChatMessage, ChatState, EndView, ErrorView, Route, Trimmed,
};
use holzwave_core::ai::service::{
    AiService, AiSink, AskEvent, AskInput, AskView, Asks, ContextState, Fetched, FindingView, OfferView, SendRequest,
    ServiceError,
};
use holzwave_core::ai::store::{
    Conversation, ConversationId, JsonDirStore, NOT_STORED, StoreError,
};
use holzwave_core::ai::tools::{
    Base, Change, Decision, DenyPolicy, DiffKind, Inspected, Pending, PayloadField, Resolved, Scope, Tool, ToolError,
    ToolName, ToolOutput,
};
use holzwave_core::ai::types::{
    ApiKey, Consent, ContextKind, ContextNote, Locality, Message, MessageStatus, NoteItem, Opaque, Part, Role,
    ToolCall, ToolResult, Usage,
};
use holzwave_core::ai::wire::openai_chat::OpenAiChatAdapter;
use holzwave_core::ai::wire::{Endpoint, Wire};
use holzwave_core::exec::LocalExec;
use holzwave_core::secrets::{MemoryStore, SecretStore};
use holzwave_core::settings::Settings;
use holzwave_core::ssh::{ExecEnd, ExecResult};
use serde_json::{Value, json};

use security::canaries::{
    Canary, api_key, canaries, env_value, file_value, leaks, marker, output_value, terminal_text, wire_key,
};
use security::rig::{
    CollectSink, Entry, Notes, PANE, PROFILE, Rig, RigOptions, ScriptedSource, chat_reply, tool_call_reply,
};
use security::world::{GateHost, PANE_A, Scratch, Tree};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The variable that names the folder of the child run of the trace test.
const CHILD_DIR: &str = "HW_GATE_CHILD_DIR";

/// A needle that stands for something that must not appear: a key or a value.
fn needle(name: &'static str, text: &str) -> Canary {
    Canary { name, rule: "", line: String::new(), needle: text.to_owned(), halves: Vec::new() }
}

/// Everything that must appear nowhere but its own place: the terminal canaries, the keys and the three values.
fn forbidden_values(extra_keys: &[String]) -> Vec<Canary> {
    let mut set = canaries();
    set.push(needle("api_key", &api_key()));
    set.push(needle("rotated_key", &rotated_key()));
    for key in extra_keys {
        set.push(needle("wire_key", key));
    }
    set.push(needle("env_value", &env_value()));
    set.push(needle("output_value", &output_value()));
    set.push(needle("file_value", &file_value()));
    set
}

fn rotated_key() -> String {
    format!("{}-rotated", api_key())
}

/// The five wires, with the tag that names the profile and the base path of the Custom profile.
const WIRES: [(&str, Wire, &str); 5] = [
    ("openai_chat", Wire::OpenAiChat, "/v1"),
    ("openai_responses", Wire::OpenAiResponses, "/v1"),
    ("anthropic", Wire::Anthropic, ""),
    ("gemini", Wire::Gemini, "/v1beta"),
    ("ollama", Wire::Ollama, ""),
];

// ---- the sink that keeps events as JSON and as Debug text ---------------------------------------------------------------

/// Keeps every event twice: as JSON and as the text of its `Debug`.
#[derive(Default)]
struct DualSink {
    json: Mutex<Vec<Value>>,
    debug: Mutex<Vec<String>>,
}

impl AiSink for DualSink {
    fn emit(&self, event: &AiEvent) {
        lock(&self.json).push(serde_json::to_value(event).expect("an event serialises"));
        lock(&self.debug).push(format!("{event:?}"));
    }
}

impl DualSink {
    fn events(&self, conversation: &str) -> Vec<Value> {
        lock(&self.json).iter().filter(|event| event["conversation"] == conversation).cloned().collect()
    }

    fn ended(&self, conversation: &str) -> usize {
        self.events(conversation).iter().filter(|event| event["kind"] == "turn_ended").count()
    }

    fn wait_ended(&self, conversation: &str, count: usize) -> Value {
        assert!(common::wait_until(Duration::from_secs(10), || self.ended(conversation) >= count), "a turn did not end");
        self.events(conversation).into_iter().filter(|event| event["kind"] == "turn_ended").nth(count - 1).expect("an end")
    }

    fn everything(&self) -> Vec<u8> {
        let mut out = serde_json::to_vec(&*lock(&self.json)).unwrap_or_default();
        out.extend(lock(&self.debug).join("\n").into_bytes());
        out
    }
}

// ---- the session --------------------------------------------------------------------------------------------------------

/// A full session in one folder: a profile and its key, a shared pane with every canary, three turns and a tool turn,
/// the switch that keeps context, a second chat, and a rotated key.
struct Session {
    _tree_scratch: Scratch,
    fake: FakeHttp,
    sink: Arc<DualSink>,
    secrets: Arc<MemoryStore>,
    service: Arc<AiService>,
    chat_private: ConversationId,
    chat_kept: ConversationId,
    views: Vec<Value>,
    card: Value,
}

/// Builds the session in `root` (the folders `config` and `chats` are made inside it).
fn session(root: &Path) -> Session {
    let set = canaries();
    let tree_scratch = Scratch::new("canary-tree");
    let tree = Tree::new(tree_scratch.path());
    let host = Arc::new(GateHost::new(&tree, &[PANE_A]));
    let leaky = tree.path("proj/leaky.txt");
    let fake = FakeHttp::start(vec![
        chat_reply("one"),
        chat_reply("two"),
        tool_call_reply("call_1", "read_file", &json!({ "path": leaky })),
        chat_reply("three"),
        chat_reply("four"),
    ]);
    let (config, chats) = (root.join("config"), root.join("chats"));
    fs::create_dir_all(&config).expect("a configuration folder");
    let sink = Arc::new(DualSink::default());
    let secrets = Arc::new(MemoryStore::default());
    let source = Arc::new(ScriptedSource::new(vec![Entry::new(
        PANE,
        ContextKind::Screen,
        "Screen of pane 7",
        terminal_text(&set),
    )]));
    let service = AiService::builder(Arc::new(JsonDirStore::new(chats)), sink.clone() as Arc<dyn AiSink>, config)
        .vault(KeyVault::new(secrets.clone()))
        .context_source(source as Arc<dyn ContextSource>)
        .tool_host(host)
        .deny_policy(DenyPolicy::new(tree.protected()))
        .build();
    let service = Arc::new(service);
    let mut input = ProfileInput::new("Local", Preset::Custom);
    input.wire = Some(Wire::OpenAiChat);
    input.base_url = format!("{}/v1", fake.base());
    input.model = "test-model".to_owned();
    input.acknowledge_gateway = true;
    service.save_profile(input).expect("a profile");
    service.set_key(PROFILE, KeySlot::Provider, ApiKey::new(api_key())).expect("a key");

    let mut views = Vec::new();
    let share_and_send = |chat: &ConversationId, text: &str, views: &mut Vec<Value>| {
        let state = service.context_share(chat, PANE, Some(PROFILE), None, true).expect("a share");
        views.push(serde_json::to_value(&state).expect("a state"));
        let view = service.context_preview(chat, PANE, Some(PROFILE), None, &[ContextKind::Screen]).expect("a preview");
        views.push(serde_json::to_value(&view).expect("a view"));
        let request = SendRequest::new(chat.clone(), text.to_owned(), PANE)
            .profile(PROFILE)
            .offer(OfferId::parse(&view.offer).expect("an offer id"), vec![view.items[0].id]);
        service.send(request).expect("the send is accepted");
    };

    // Turn 1 of the private chat: the screen is picked. Turn 2: plain text.
    let chat_private = service.new_conversation().expect("a chat id");
    share_and_send(&chat_private, "explain this screen", &mut views);
    sink.wait_ended(chat_private.as_str(), 1);
    service.send(SendRequest::new(chat_private.clone(), "and a second question".to_owned(), PANE).profile(PROFILE)).expect("send");
    sink.wait_ended(chat_private.as_str(), 2);
    // Turn 3: a tool turn that reads the file with the canaries; the test plays the page.
    service
        .send(SendRequest::new(chat_private.clone(), "read the leaky file".to_owned(), PANE_A).profile(PROFILE))
        .expect("send");
    let proposed = || {
        sink.events(chat_private.as_str()).into_iter().find(|event| event["kind"] == "tool_proposed")
    };
    assert!(common::wait_until(Duration::from_secs(10), || proposed().is_some()), "no card was proposed");
    let card = proposed().expect("a card")["pending"].clone();
    let (call_id, hash) = (card["call_id"].as_str().unwrap_or(""), card["hash"].as_str().unwrap_or(""));
    service.approvals().decide(chat_private.as_str(), call_id, Decision::Allow, hash).expect("the decision is taken");
    assert_eq!(sink.wait_ended(chat_private.as_str(), 3)["end"], "done");

    // The switch that keeps terminal context in the chat file, a second chat, and a rotated key.
    service.apply_settings(&Settings { ai_persist_context: true, ..Settings::default() });
    let chat_kept = service.new_conversation().expect("a chat id");
    share_and_send(&chat_kept, "explain it again", &mut views);
    assert_eq!(sink.wait_ended(chat_kept.as_str(), 1)["end"], "done");
    service.set_key(PROFILE, KeySlot::Provider, ApiKey::new(rotated_key())).expect("a rotated key");
    assert!(!service.profiles().is_empty());
    Session {
        _tree_scratch: tree_scratch,
        fake,
        sink,
        secrets,
        service,
        chat_private,
        chat_kept,
        views,
        card,
    }
}

/// Every file below `root`, never following a link: the path and the bytes.
fn walk(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(root) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if meta.is_dir() {
            out.extend(walk(&path));
        } else if meta.is_file() {
            out.push((path.clone(), fs::read(&path).unwrap_or_default()));
        }
    }
    out
}

/// The names of the files below `root` that hold a key or a canary.
fn files_with_leaks(root: &Path, set: &[Canary]) -> Vec<String> {
    walk(root)
        .into_iter()
        .filter(|(_, bytes)| !leaks(bytes, set).is_empty())
        .map(|(path, _)| path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default())
        .collect()
}

fn file_of(root: &Path, relative: &str) -> Vec<u8> {
    fs::read(root.join(relative)).unwrap_or_else(|_| panic!("the session left no {relative}"))
}

// ---- the tests ----------------------------------------------------------------------------------------------------------

#[test]
fn serialised_views_never_hold_a_key_or_context_text() {
    let root = common::scratch_dir("canary-views");
    let set = forbidden_values(&[]);
    let session = session(&root);
    let service = &session.service;
    let mut views: Vec<(&str, Vec<u8>)> = vec![
        ("profiles", serde_json::to_vec(&service.profiles()).expect("profiles")),
        ("chat list", serde_json::to_vec(&service.list_conversations().expect("a listing").chats).expect("chats")),
        ("snapshot of the private chat", serde_json::to_vec(&service.snapshot(&session.chat_private).expect("a snapshot")).expect("json")),
        ("snapshot of the kept chat", serde_json::to_vec(&service.snapshot(&session.chat_kept).expect("a snapshot")).expect("json")),
        ("events and their debug text", session.sink.everything()),
        ("the card of the tool call", serde_json::to_vec(&session.card).expect("a card")),
        ("ai.json", file_of(&root, "config/ai.json")),
    ];
    for (index, view) in session.views.iter().enumerate() {
        views.push(if index % 2 == 0 { ("a context state", view.to_string().into_bytes()) } else { ("an offer view", view.to_string().into_bytes()) });
    }
    for (name, bytes) in &views {
        assert!(leaks(bytes, &set).is_empty(), "{name}: {:?}", leaks(bytes, &set));
    }
    // Positive control: the views are not empty.
    assert!(views.iter().all(|(_, bytes)| bytes.len() > 20));
    drop(session);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_provider_that_echoes_the_key_leaks_nothing_on_any_wire() {
    #[derive(Clone, Copy)]
    struct Case {
        wire: usize,
        status: u16,
        object: bool,
    }
    let case = Arc::new(Mutex::new(Case { wire: 0, status: 400, object: true }));
    let seen_case = case.clone();
    let fake = FakeHttp::start_with(move |seen: &Seen, _| {
        let now = *lock(&seen_case);
        let key = wire_key(WIRES[now.wire].0);
        let mut echo: Vec<String> = seen.headers.iter().map(|(name, value)| format!("{name}: {value}")).collect();
        echo.push(format!("authorization: Bearer {key}"));
        echo.push(format!("https://example.net/v1?key={key}"));
        let echo = echo.join(" | ");
        let body = if !now.object {
            echo
        } else {
            match WIRES[now.wire].1 {
                Wire::Anthropic => json!({ "type": "error", "error": { "type": "invalid_request_error", "message": echo } }),
                Wire::Gemini => json!({ "error": { "code": now.status, "message": echo, "status": "INVALID_ARGUMENT" } }),
                Wire::Ollama => json!({ "error": echo }),
                _ => json!({ "error": { "message": echo, "type": "invalid_request_error", "code": null } }),
            }
            .to_string()
        };
        Reply::status(now.status).body(body.into_bytes())
    });
    let root = common::scratch_dir("canary-echo");
    let (config, chats) = (root.join("config"), root.join("chats"));
    fs::create_dir_all(&config).expect("a configuration folder");
    let sink = Arc::new(DualSink::default());
    let service = AiService::builder(Arc::new(JsonDirStore::new(chats.clone())), sink.clone() as Arc<dyn AiSink>, config)
        .vault(KeyVault::new(Arc::new(MemoryStore::default())))
        .build();
    let mut keys = Vec::new();
    let mut ids = Vec::new();
    for (tag, wire, path) in WIRES {
        let mut input = ProfileInput::new(tag, Preset::Custom);
        input.wire = Some(wire);
        input.base_url = format!("{}{path}", fake.base());
        input.model = "test-model".to_owned();
        input.acknowledge_gateway = true;
        let id = service.save_profile(input).expect("a profile");
        service.set_key(&id, KeySlot::Provider, ApiKey::new(wire_key(tag))).expect("a key");
        keys.push(wire_key(tag));
        ids.push(id);
    }
    let set = forbidden_values(&keys);
    let mut chat_ids = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        for status in [400u16, 401, 403, 404] {
            for object in [true, false] {
                *lock(&case) = Case { wire: index, status, object };
                let chat = service.new_conversation().expect("a chat id");
                service
                    .send(SendRequest::new(chat.clone(), format!("question {}", marker("typed")), PANE).profile(id.as_str()))
                    .expect("the send is accepted");
                let ended = sink.wait_ended(chat.as_str(), 1);
                assert_eq!(ended["end"], "failed", "{}: status {status} did not fail the turn", WIRES[index].0);
                texts.push(ended["error"]["text"].as_str().unwrap_or("").to_owned());
                chat_ids.push(chat);
            }
        }
    }
    assert_eq!(texts.len(), 40);
    assert_eq!(fake.seen().len(), 40, "one request for each turn: a status that is retried would double it");
    // The positive control: every request carried its key in the header of its wire.
    for (index, request) in fake.seen().iter().enumerate() {
        let wire = index / 8;
        assert!(
            request.headers.iter().any(|(_, value)| value.contains(&keys[wire])),
            "{}: the request carried no key",
            WIRES[wire].0
        );
    }
    let mut all = texts.join("\n").into_bytes();
    all.extend(sink.everything());
    for chat in &chat_ids {
        all.extend(serde_json::to_vec(&service.snapshot(chat).expect("a snapshot")).expect("json"));
    }
    all.extend(walk(&root).into_iter().flat_map(|(_, bytes)| bytes));
    let found = leaks(&all, &set);
    assert!(found.is_empty(), "an echoed key or a canary reached an error text, an event, a snapshot or a file: {found:?}");
    drop(service);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_key_is_kept_in_the_vault_and_no_file_holds_it_after_a_session() {
    let root = common::scratch_dir("canary-files");
    let set = forbidden_values(&[]);

    // The walk itself is proved: a canary planted in a nested file is found.
    let nested = root.join("planted").join("deeper");
    fs::create_dir_all(&nested).expect("a nested folder");
    fs::write(nested.join("note.txt"), canaries()[1].line.as_bytes()).expect("a planted file");
    assert_eq!(files_with_leaks(&root, &set), ["note.txt"], "the walk did not find the planted canary");
    fs::remove_dir_all(root.join("planted")).expect("the plant goes");

    let session = session(&root);
    assert_eq!(files_with_leaks(&root, &set), Vec::<String>::new(), "a file holds a key or a canary");

    // The chat written while the setting was off holds the marker and no screen text.
    let private = String::from_utf8_lossy(&file_of(&root, &format!("chats/{}.json", session.chat_private.as_str()))).into_owned();
    assert!(private.contains("Screen of pane 7"), "the label of the note is not stored");
    assert!(!private.contains(&marker("screen")), "the screen text is in the chat written with the setting off");
    assert!(private.contains(NOT_STORED), "the stored tool result is not the fixed text");
    assert!(private.contains("explain this screen") && private.contains("read the leaky file"), "the typed text is not stored");
    // The chat written after the switch holds the redacted screen text.
    let kept = String::from_utf8_lossy(&file_of(&root, &format!("chats/{}.json", session.chat_kept.as_str()))).into_owned();
    assert!(kept.contains("[REDACTED:anthropic_key]"), "the redacted screen text is not in the kept chat");
    assert!(kept.contains(&marker("screen")), "the screen text is not in the kept chat");

    // The configuration holds the marker and not the key; the vault holds the key and nothing for a gateway.
    let config = String::from_utf8_lossy(&file_of(&root, "config/ai.json")).into_owned();
    assert!(config.contains("\"key_set\": true") || config.contains("\"key_set\":true"), "ai.json does not say a key was saved");
    assert!(!config.contains(&api_key()) && !config.contains(&rotated_key()));
    let record = session.secrets.get("ai:local").expect("a read").expect("the key is in the vault");
    assert!(record.contains(&rotated_key()), "the vault does not hold the rotated key");
    assert!(session.secrets.get("ai:local:gateway").expect("a read").is_none(), "the vault holds a gateway entry");

    // The server saw the original key in a header and the rotated one is not sent yet.
    assert!(session.fake.seen().iter().all(|request| request.header("authorization") == Some(format!("Bearer {}", api_key()).as_str())));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for (path, _) in walk(&root.join("chats")) {
            let mode = fs::metadata(&path).expect("metadata").permissions().mode();
            assert_eq!(mode & 0o077, 0, "a chat file is readable by group or others");
        }
    }
    drop(session);
    let _ = fs::remove_dir_all(&root);
}

// ---- a command that prints a canary --------------------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn command_output_and_environment_canaries_reach_neither_the_server_nor_a_file() {
    let set = forbidden_values(&[]);
    let scratch = Scratch::new("canary-cmd");
    let tree = Tree::new(scratch.path());
    // The runner is built over an explicit list of pairs: a variable with a secret word in its name, the three variables
    // of a holzWave pane, one variable that is allowed through, and the path.
    let base: Vec<(OsString, OsString)> = vec![
        ("PATH".into(), std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into())),
        ("SERVICE_API_TOKEN".into(), env_value().into()),
        ("HOLZWAVE_IPC".into(), "/nowhere/ipc".into()),
        ("HOLZWAVE_TOKEN".into(), "pane-token".into()),
        ("HOLZWAVE_PANE".into(), "7".into()),
        ("LC_GATE".into(), "zq-allowed-1".into()),
    ];
    let host = Arc::new(GateHost::new(&tree, &[PANE_A]).with_exec(LocalExec::new(base)));
    // The secret is printed from pieces, so the command text of the model does not hold it whole.
    let value = output_value();
    let (head, tail) = value.split_at(5);
    let command = format!(
        "cat leaky.txt; printf 'env=%s\\n' \"$SERVICE_API_TOKEN\"; printf '%s%s\\n' '{head}' '{tail}'; env"
    );
    let rig = Rig::new(RigOptions {
        replies: vec![tool_call_reply("call_1", "run_command", &json!({ "command": command })), chat_reply("done")],
        tool_host: Some(host.clone()),
        deny: Some(DenyPolicy::new(tree.protected())),
        ..RigOptions::default()
    });
    let (service, fake) = (rig.service(), rig.fake());
    let chat = rig.chat();
    service.send(SendRequest::new(chat.clone(), "run the command".to_owned(), PANE_A).profile(PROFILE)).expect("send");
    let proposed = || rig.sink().events(chat.as_str()).into_iter().find(|event| event["kind"] == "tool_proposed");
    assert!(common::wait_until(Duration::from_secs(10), || proposed().is_some()), "no card was proposed");
    let card = proposed().expect("a card");
    let (call_id, hash) = (card["pending"]["call_id"].as_str().unwrap_or(""), card["pending"]["hash"].as_str().unwrap_or(""));
    service.approvals().decide(chat.as_str(), call_id, Decision::Allow, hash).expect("the decision is taken");
    assert_eq!(rig.sink().wait_ended(chat.as_str(), Duration::from_secs(20)).expect("an end")["end"], "done");
    assert_eq!(host.run_count(), 1, "the command did not run");

    let requests = security::rig::captures(fake);
    assert_eq!(requests.len(), 2);
    let second = String::from_utf8_lossy(&requests[1].body).into_owned();
    assert!(second.contains("zq-allowed-1"), "the output of the command is not in the second request (the control)");
    let found = leaks(&requests[1].body, &set);
    assert!(found.is_empty(), "a canary is in the second request: {found:?}");
    for name in ["HOLZWAVE_IPC", "HOLZWAVE_TOKEN", "HOLZWAVE_PANE", "pane-token"] {
        assert!(!second.contains(name), "{name} is in the second request");
    }
    assert!(second.contains("[REDACTED:"), "the terminal canaries were not replaced by typed placeholders");
    let (_, chats, _) = rig.dirs();
    let stored: Vec<u8> = walk(&chats).into_iter().flat_map(|(_, bytes)| bytes).collect();
    let stored_text = String::from_utf8_lossy(&stored).into_owned();
    assert!(stored_text.contains(NOT_STORED), "the stored result is not the fixed text");
    assert!(leaks(&stored, &set).is_empty() && !stored_text.contains("zq-allowed-1"), "the output of the command is in the stored chat");
}

// ---- a full session under the trace variable ---------------------------------------------------------------------------------

#[test]
fn a_full_session_under_rust_log_trace_writes_no_canary_to_stderr_stdout_or_disk() {
    let root = common::scratch_dir("canary-trace");
    let set = forbidden_values(&[]);
    let output = Command::new(std::env::current_exe().expect("the path of this test binary"))
        .args(["child_session_for_the_trace_run", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .env("RUST_LOG", "trace")
        .env("RUST_BACKTRACE", "full")
        .env(CHILD_DIR, &root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("the child run starts");
    assert!(output.status.success(), "the child run failed");
    assert!(String::from_utf8_lossy(&output.stdout).contains("child-ok"), "the child run did not finish its session");
    assert!(leaks(&output.stdout, &set).is_empty(), "a key or a canary is on the standard output of the child");
    assert!(leaks(&output.stderr, &set).is_empty(), "a key or a canary is on the standard error of the child");
    assert_eq!(files_with_leaks(&root, &set), Vec::<String>::new(), "a file of the child holds a key or a canary");
    assert!(!walk(&root).is_empty(), "the child left no file (the control)");
    let _ = fs::remove_dir_all(&root);
}

/// The session of the trace run: it does nothing unless the folder is named, so a normal run and `--ignored` alone are
/// harmless. It prints one line when it is done.
#[test]
#[ignore = "run by the trace test in a child process"]
fn child_session_for_the_trace_run() {
    let Some(root) = std::env::var_os(CHILD_DIR) else { return };
    let session = session(Path::new(&root));
    assert!(!session.service.profiles().is_empty());
    println!("child-ok");
}

// ---- the sources ---------------------------------------------------------------------------------------------------------------

/// The lines of the lock file that name a package.
fn locked_packages() -> BTreeSet<String> {
    let lock = fs::read_to_string(repo().join("Cargo.lock")).expect("the lock file");
    lock.lines()
        .filter_map(|line| line.strip_prefix("name = \"").and_then(|rest| rest.strip_suffix('"')))
        .map(str::to_owned)
        .collect()
}

/// The root of the repository.
fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

#[test]
fn no_logger_can_be_switched_on_by_the_environment() {
    let packages = locked_packages();
    assert!(packages.contains("log") && packages.contains("tracing"), "the lock file lists no logging facade: the parse is wrong");
    for logger in [
        "env_logger",
        "pretty_env_logger",
        "tracing-subscriber",
        "tracing-appender",
        "flexi_logger",
        "fern",
        "simple_logger",
        "stderrlog",
        "log4rs",
        "femme",
        "tauri-plugin-log",
    ] {
        assert!(
            !packages.contains(logger),
            "the lock file holds the logger {logger}: add a capture test that runs a full session with the canary key \
             under RUST_LOG=trace (it needs a dev-dependency on the logging facade), then name the crate in this test"
        );
    }
}

/// The text a scan looks at: the part before the first line that is `#[cfg(test)]`, without the comment lines.
fn production_text(source: &str) -> String {
    let mut out = String::new();
    for line in source.lines() {
        if line.trim() == "#[cfg(test)]" {
            break;
        }
        if line.trim_start().starts_with("//") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Whether `needle` occurs in `line` as a word of its own: the character before it, if any, is not part of a name, so
/// `catalog::` does not hold `log::` and `sized_debug!(` does not hold `debug!(`.
fn occurs_as_word(line: &str, needle: &str) -> bool {
    line.match_indices(needle).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The needles of `needles` that occur in `text` as words.
fn hits<'a>(text: &str, needles: &[&'a str]) -> Vec<&'a str> {
    needles.iter().copied().filter(|needle| text.lines().any(|line| occurs_as_word(line, needle))).collect()
}

/// The trimmed lines of `text` that hold one of `needles` as a word.
fn hit_lines(text: &str, needles: &[&str]) -> Vec<String> {
    text.lines()
        .filter(|line| needles.iter().any(|needle| occurs_as_word(line, needle)))
        .map(|line| line.trim().to_owned())
        .collect()
}

/// Every `.rs` file below `dir` (not following links), or the file itself.
fn source_files(paths: &[&str]) -> Vec<PathBuf> {
    fn below(path: &Path, out: &mut Vec<PathBuf>) {
        let Ok(meta) = fs::symlink_metadata(path) else { return };
        if meta.is_dir() {
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    below(&entry.path(), out);
                }
            }
        } else if meta.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path.to_path_buf());
        }
    }
    let mut out = Vec::new();
    for path in paths {
        below(&repo().join(path), &mut out);
    }
    out.sort();
    out
}

const PRINTS: [&str; 12] = [
    "println!", "eprintln!", "print!(", "eprint!(", "dbg!(", "log::", "tracing::", "trace!(", "debug!(", "info!(", "warn!(",
    "error!(",
];

/// The print sites that were read and may stay: the file, the exact line and why. A line that changes, and a print anywhere
/// else, fails the guard; a row without a hit fails it too.
const PRINT_SITES: [(&str, &str, &str); 1] = [(
    "app/src-tauri/src/ai_host.rs",
    "eprintln!(\"holzwave: {left} assistant turn(s) did not end within the shutdown bound\");",
    "a fixed sentence and a count of turns at shutdown: it names no key, no text of a chat and no path",
)];

#[test]
fn first_party_ai_code_prints_and_logs_nothing() {
    let mut files = source_files(&["core/src/ai", "core/src/exec.rs", "core/src/textread.rs", "ipc/src/stream.rs", "wsh/src/ai.rs", "wsh/src/clean.rs"]);
    files.extend(
        fs::read_dir(repo().join("app/src-tauri/src"))
            .expect("the shell sources")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.file_name().is_some_and(|name| name.to_string_lossy().starts_with("ai_"))),
    );
    assert!(files.len() >= 20, "the scan finds {} files", files.len());
    let mut offenders = Vec::new();
    let mut reviewed_found: BTreeSet<&str> = BTreeSet::new();
    for file in &files {
        let relative = file.strip_prefix(repo()).unwrap_or(file).to_string_lossy().replace("/./", "/").replace("..", "");
        let relative = relative.trim_start_matches('/').to_owned();
        let source = fs::read_to_string(file).expect("a source file");
        for line in hit_lines(&production_text(&source), &PRINTS) {
            match PRINT_SITES.iter().find(|(path, text, _)| *path == relative && *text == line) {
                Some((path, _, _)) => {
                    reviewed_found.insert(path);
                }
                None => offenders.push(relative.clone()),
            }
        }
    }
    assert!(offenders.is_empty(), "first-party AI code prints or logs: {offenders:?}");
    for (path, _, reason) in PRINT_SITES {
        assert!(reviewed_found.contains(path), "a row of the table of print sites has no hit any more: {path}");
        assert!(reason.len() > 20, "a row has no reason");
    }
}

#[test]
fn the_source_scanners_flag_what_they_should_and_skip_comments_and_tests() {
    // A print in code is flagged; one in a comment, in a test module after the marker, or a word that only looks like a
    // macro is not.
    assert_eq!(hits(&production_text("fn a() { println!(\"x\"); }\n"), &PRINTS), ["println!"]);
    assert!(hits(&production_text("// println!(\"x\")\n    /// eprintln!(\"y\")\nfn a() {}\n"), &PRINTS).is_empty());
    assert!(hits(&production_text("fn a() {}\n#[cfg(test)]\nmod t { fn b() { println!(); dbg!(1); } }\n"), &PRINTS).is_empty());
    assert!(hits(&production_text("fn a() { x.unwrap_or_default(); let debug = 1; }\n"), &PRINTS).is_empty());
    assert_eq!(hits(&production_text("fn a() { log::info!(\"x\"); }\n"), &PRINTS), ["log::", "info!("]);
    assert_eq!(hits(&production_text("use super::catalog::X; sized_debug!(A: b); compile_error!(\"x\");\n"), &PRINTS), Vec::<&str>::new());
    assert_eq!(hit_lines("fn a() {\n    eprintln!(\"x\");\n}\n", &PRINTS), ["eprintln!(\"x\");"]);
    assert_eq!(expose_sites("fn a() { key.expose(); }\n"), 1);
    assert_eq!(expose_sites("// key.expose()\n/// key.expose()\nfn a() { let exposed = 1; }\n#[cfg(test)]\nmod t { fn b() { k.expose(); } }\n"), 0);
    assert_eq!(expose_sites("pub fn expose(&self) -> &str { &self.0 }\n"), 0, "a definition is not a call");
}

/// How many times the non-test text of `source` calls `.expose()`.
fn expose_sites(source: &str) -> usize {
    production_text(source).matches(".expose()").count()
}

/// The files that call `.expose()` and why each may. The table was made from a first run: each hit was read in the code and
/// its reason written from it.
const KEY_SITES: [(&str, &str); 2] = [
    ("core/src/ai/profiles.rs", "the key is trimmed and checked before it goes into the record the vault stores"),
    ("core/src/ai/http.rs", "the header value is built from the key, and the key is compared with the request address"),
];

#[test]
fn key_text_is_exposed_in_the_reviewed_places_only() {
    let mut roots = vec!["core/src", "app/src-tauri/src", "ipc/src", "wsh/src"];
    let plugins = fs::read_dir(repo().join("plugins")).expect("the plugins folder");
    let plugin_sources: Vec<String> = plugins.flatten().map(|entry| format!("plugins/{}/src", entry.file_name().to_string_lossy())).collect();
    roots.extend(plugin_sources.iter().map(String::as_str));
    let files = source_files(&roots);
    assert!(files.len() > 50, "the scan finds {} files", files.len());
    let mut found: BTreeSet<String> = BTreeSet::new();
    for file in &files {
        let source = fs::read_to_string(file).expect("a source file");
        if expose_sites(&source) > 0 {
            let relative = file.strip_prefix(repo()).unwrap_or(file).to_string_lossy().replace("/./", "/").replace("..", "");
            let relative = relative.trim_start_matches('/').to_owned();
            found.insert(relative);
        }
    }
    let reviewed: BTreeSet<String> = KEY_SITES.iter().map(|(file, _)| (*file).to_owned()).collect();
    let new: Vec<&String> = found.difference(&reviewed).collect();
    let stale: Vec<&String> = reviewed.difference(&found).collect();
    assert!(new.is_empty(), "a file reads key text and is not in the table: {new:?}");
    assert!(stale.is_empty(), "a row of the table has no hit any more: {stale:?}");
    assert!(KEY_SITES.iter().all(|(_, reason)| reason.len() > 20), "a row has no reason");
}

// ---- Debug ---------------------------------------------------------------------------------------------------------------------

/// The `Debug` text of a value, plain and pretty, so that a failure names the type.
fn row<T: std::fmt::Debug + ?Sized>(name: &str, value: &T) -> (String, String) {
    (name.to_owned(), format!("{value:?}\n{value:#?}"))
}

/// The `Display` text of a value.
fn row_display<T: std::fmt::Display + ?Sized>(name: &str, value: &T) -> (String, String) {
    (name.to_owned(), value.to_string())
}

#[test]
fn no_type_prints_a_canary_in_debug() {
    let set = canaries();
    let screen = terminal_text(&set);
    let one_line = set[1].needle.clone();
    let (key, typed) = (api_key(), marker("typed"));
    let (env, output, file) = (env_value(), output_value(), file_value());
    let forbidden = forbidden_values(&[]);
    let mut rows: Vec<(String, String)> = Vec::new();

    // The chokepoint, the profiles and the vault.
    let origin = Origin::from_scope("http://127.0.0.1:8080").expect("an origin");
    let secret = SecretHeader::new("authorization", "Bearer ", ApiKey::new(key.clone()), origin.clone()).expect("a header");
    let target = Target::parse("http://127.0.0.1:8080/v1", false).expect("a target");
    let policy = NetworkPolicy::new(false, false);
    let header = Header::new("anthropic-version", "2023-06-01").expect("a header");
    let body = format!("{typed} {screen} {key}");
    rows.push(row("ApiKey", &ApiKey::new(key.clone())));
    rows.push(row("SecretHeader", &secret));
    rows.push(row("Origin", &origin));
    rows.push(row("Target", &target));
    rows.push(row("Header", &header));
    rows.push(row("Call", &Call::new(&target, policy, "/chat/completions", body.as_bytes(), Framing::Sse).header(&header).secret(&secret)));
    let endpoint = Endpoint::new(Wire::OpenAiChat, target.clone(), policy)
        .with_header(header.clone())
        .with_secret(SecretHeader::new("authorization", "Bearer ", ApiKey::new(key.clone()), origin.clone()).expect("a header"));
    rows.push(row("Endpoint", &endpoint));
    let scrubbed: Detail = endpoint.scrub(&format!("the server said: {key} twice {key}"));
    assert!(!scrubbed.as_str().contains(&key), "Endpoint::scrub left the key in the text");
    rows.push(row("Detail from Endpoint::scrub", &scrubbed));
    rows.push(row_display("Detail from Endpoint::scrub (as text)", &scrubbed.as_str()));
    rows.push(row("Http", &Http::new(HttpConfig::standard())));

    let store = Arc::new(MemoryStore::default());
    let vault = KeyVault::new(store.clone());
    let mut config = AiConfig::default();
    let mut input = ProfileInput::new("Local", Preset::Custom);
    input.wire = Some(Wire::OpenAiChat);
    input.base_url = "http://127.0.0.1:8080/v1".to_owned();
    input.model = "test-model".to_owned();
    let id = config.upsert(input).expect("a profile");
    config.set_key(&vault, &id, KeySlot::Provider, ApiKey::new(key.clone())).expect("a key");
    assert!(store.get("ai:local").expect("a read").is_some_and(|record| record.contains(&key)), "the vault holds the key");
    rows.push(row("KeyVault", &vault));
    rows.push(row("AiConfig", &config));
    rows.push(row("Profile", config.get(&id).expect("the profile")));
    rows.push(row("ProfileView", &config.views(&vault)));
    let resolved = config.resolve(&id, &vault, false).expect("the profile resolves");
    rows.push(row("profiles::Resolved", &resolved));
    let provider = resolved.into_provider(Box::new(OpenAiChatAdapter::new()), Arc::new(Http::new(HttpConfig::standard())));
    rows.push(row("HttpProvider", &provider));
    let errors = [
        ResolveError::UnknownProfile,
        ResolveError::NeedsAddress,
        ResolveError::DisclosureNeeded,
        ResolveError::KeyMissing(KeySlot::Provider),
        ResolveError::KeyScope(KeySlot::Gateway),
        ResolveError::KeyDamaged(KeySlot::Provider),
        ResolveError::NoSecureStore,
        ResolveError::Keychain,
    ];
    rows.push(row("ResolveError", &errors));
    rows.push(row_display("ResolveError (Display)", &errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("|")));

    // A base address that carries the key in its query is refused, and nothing of the key is printed or written.
    let rig = Rig::new(RigOptions::default());
    let mut leaky = ProfileInput::new("Leaky", Preset::Custom);
    leaky.wire = Some(Wire::OpenAiChat);
    leaky.base_url = format!("http://127.0.0.1:8080/v1?key={key}");
    leaky.model = "m".to_owned();
    rows.push(row("ProfileInput", &leaky));
    let refused = rig.service().save_profile(leaky).expect_err("an address with the key in its query is refused");
    assert!(matches!(refused, ServiceError::Profile(_)), "the refusal is not a profile refusal");
    rows.push(row("ServiceError from a refused address", &refused));
    rows.push(row_display("ServiceError from a refused address (Display)", &refused));
    let (_, _, config_dir) = rig.dirs();
    let written = fs::read(config_dir.join("ai.json")).expect("ai.json");
    assert!(!String::from_utf8_lossy(&written).contains(&key), "ai.json holds the key typed in an address");
    rows.push(row("AiService", rig.service().as_ref()));

    // Messages and requests.
    let note = ContextNote {
        hash: "ab".repeat(32),
        items: vec![NoteItem { kind: ContextKind::Screen, label: "Screen of pane 7".to_owned(), bytes: 4096 }],
        stored: Some(screen.clone()),
        consented_to: Consent { provider: "local".to_owned(), locality: Locality::Loopback },
    };
    let call = ToolCall { id: "call_1".to_owned(), name: "read_file".to_owned(), args: json!({ "path": one_line, "note": screen }) };
    let result = ToolResult { call_id: "call_1".to_owned(), content: format!("{output} {screen}"), is_error: false };
    let opaque = Opaque { wire: "openai_chat".to_owned(), json: json!({ "reasoning": screen, "key": key }) };
    let user = Message::new("m1", Role::User, vec![Part::Context(note.clone()), Part::Text { text: format!("{typed} {screen}") }], 1);
    let mut assistant = Message::new("m2", Role::Assistant, vec![Part::ToolCall(call.clone())], 2);
    assistant.native = Some(opaque.clone());
    assistant.usage = Some(Usage { input_tokens: 3, output_tokens: 4 });
    assistant.status = MessageStatus::Complete;
    let tool = Message::new("m3", Role::Tool, vec![Part::ToolResult(result.clone())], 3);
    rows.push(row("ContextNote", &note));
    rows.push(row("ToolCall", &call));
    rows.push(row("ToolResult", &result));
    rows.push(row("Opaque", &opaque));
    rows.push(row("Message (user)", &user));
    rows.push(row("Message (assistant)", &assistant));
    rows.push(row("Message (tool)", &tool));
    rows.push(row("Part", &user.parts));
    let gate = Gate::new();
    let destination = Destination { host: "127.0.0.1".to_owned(), locality: Locality::Loopback };
    let item = RequestItem::new(ContextKind::Screen, "Screen of pane 7", SourceText::plain(screen.clone()));
    rows.push(row("RequestItem", &item));
    rows.push(row("SourceText", &SourceText::plain(screen.clone())));
    let request = OfferRequest::new("local", destination, vec![item]);
    rows.push(row("OfferRequest", &request));
    let offer: ContextOffer = gate.offer(request).expect("an offer");
    rows.push(row("Gate with a pending offer", &gate));
    rows.push(row("ContextOffer", &offer));
    let offered: &OfferItem = &offer.items()[0];
    rows.push(row("OfferItem", offered));
    let approved: ApprovedContext = gate.approve(offer.id(), &[1], "local").expect("an approval");
    rows.push(row("ApprovedContext", &approved));
    let cleaned: Cleaned = clean_text(&screen, Keep::Head, 4096);
    rows.push(row("Cleaned", &cleaned));
    rows.push(row("Fetched", &Fetched::new("Screen of pane 7", SourceText::plain(screen.clone()), false)));
    let mut chat_request = ChatRequest::new("test-model");
    chat_request.system = screen.clone();
    chat_request.messages = vec![user.clone(), assistant.clone(), tool.clone()];
    chat_request.context = vec![approved.clone()];
    rows.push(row("ChatRequest", &chat_request));
    let send = SendRequest::new(rig.chat(), format!("{typed} {screen}"), PANE).profile("local").context(vec![approved.clone()]);
    rows.push(row("SendRequest", &send));

    // Stored chats and events.
    let chat_id = ConversationId::generate().expect("an id");
    let mut conversation = Conversation::new(chat_id.clone(), 1);
    conversation.title = typed.clone();
    conversation.messages = vec![user.clone(), assistant.clone(), tool.clone()];
    rows.push(row("Conversation", &conversation));
    rows.push(row("ConversationSummary", &conversation.summary()));
    let scope = Scope { conversation: chat_id.as_str().to_owned(), turn: 1, pane: 1 };
    let turn = TurnRequest {
        scope: scope.clone(),
        model: "test-model".to_owned(),
        system: screen.clone(),
        messages: vec![user.clone()],
        context: vec![approved.clone()],
        id_prefix: "t1".to_owned(),
    };
    rows.push(row("TurnRequest", &turn));
    let pending = Pending {
        conversation: chat_id.as_str().to_owned(),
        call_id: "call_1".to_owned(),
        tool: ToolName::RunCommand,
        effect: ToolName::RunCommand.effect(),
        pane: 1,
        target: "This computer".to_owned(),
        remote: false,
        payload: vec![PayloadField { name: "command", value: format!("echo {env} {output}"), bytes: 12 }],
        hash: "cd".repeat(32),
        tainted: true,
        taint: vec!["output of read_file".to_owned()],
        diff: None,
    };
    rows.push(row("Pending", &pending));
    let events = [
        AgentEvent::RoundStarted { round: 1 },
        AgentEvent::TextDelta(format!("{typed} {screen}")),
        AgentEvent::ReasoningDelta(screen.clone()),
        AgentEvent::Trimmed { results: 1, exchanges: 2 },
        AgentEvent::Usage(Usage { input_tokens: 1, output_tokens: 2 }),
        AgentEvent::Message(assistant.clone()),
        AgentEvent::ToolProposed(pending.clone()),
        AgentEvent::ToolResult {
            call_id: "call_1".to_owned(),
            tool: "read_file".to_owned(),
            content: format!("{output} {screen}"),
            is_error: false,
            redactions: 3,
        },
    ];
    for (index, event) in events.iter().enumerate() {
        rows.push(row(&format!("AgentEvent variant {index}"), event));
    }
    rows.push(row(
        "TurnOutcome",
        &TurnOutcome {
            end: TurnEnd::Failed(AiError::Auth(Detail::new("the server refused the key"))),
            rounds: 1,
            usage: Usage::default(),
            messages: vec![assistant.clone()],
        },
    ));
    let view_of = |message: &Message| ChatMessage::from_message(message);
    let end = EndView {
        end: "failed".to_owned(),
        error: Some(ErrorView { class: "auth".to_owned(), text: "the server answered with status 401".to_owned() }),
        rounds: 1,
        usage: Usage::default(),
        saved: true,
    };
    let route = Route { profile: "local".to_owned(), model: "test-model".to_owned(), host: Some("127.0.0.1:8080".to_owned()), locality: Locality::Loopback };
    let kinds = vec![
        AiEventKind::TurnStarted { message: view_of(&user), route },
        AiEventKind::Delta { text: format!("{typed} {screen}") },
        AiEventKind::Reasoning { text: screen.clone() },
        AiEventKind::Usage(Usage { input_tokens: 1, output_tokens: 1 }),
        AiEventKind::Trimmed(Trimmed { results: 1, exchanges: 1 }),
        AiEventKind::Message { message: view_of(&assistant) },
        AiEventKind::ToolProposed { pending: pending.clone() },
        AiEventKind::ToolResult {
            call_id: "call_1".to_owned(),
            tool: "read_file".to_owned(),
            content: format!("{output} {screen}"),
            is_error: false,
            redactions: 3,
        },
        AiEventKind::TurnEnded(end.clone()),
    ];
    for kind in kinds {
        let name = kind.name();
        rows.push(row(&format!("AiEvent {name}"), &AiEvent { conversation: chat_id.as_str().to_owned(), turn: 1, seq: 1, kind }));
    }
    rows.push(row("EndView", &end));
    let mut state = ChatState::new(chat_id.as_str());
    state.messages = vec![view_of(&user), view_of(&assistant), view_of(&tool)];
    state.partial.text = screen.clone();
    state.pending.push(pending.clone());
    rows.push(row("ChatState", &state));
    rows.push(row("ChatPart", &view_of(&user).parts));

    // The tools. A tool call refuses control characters, so the text it carries is the screen without them.
    let plain_screen: String = screen.chars().filter(|c| !c.is_control() || *c == '\n').collect();
    let tools = [
        ("read_file", json!({ "path": one_line })),
        ("list_dir", json!({ "path": one_line })),
        ("grep", json!({ "pattern": one_line, "path": one_line })),
        ("read_pane", json!({ "pane": 2 })),
        ("write_file", json!({ "path": one_line, "content": plain_screen })),
        ("edit_file", json!({ "path": one_line, "old_text": plain_screen, "new_text": output })),
        ("run_command", json!({ "command": format!("echo {env} {output}"), "cwd": one_line })),
    ];
    for (name, args) in tools {
        let parsed = Tool::parse(name, &args).unwrap_or_else(|_| panic!("{name}: the call does not parse"));
        rows.push(row(&format!("Tool {name}"), &parsed));
    }
    rows.push(row("Scope", &scope));
    let resolved_tool = Resolved::local(1, "this computer").with_path(one_line.clone()).with_cwd(one_line.clone()).with_stamp(5).with_base(Base::Text(screen.clone()));
    rows.push(row("tools::Resolved", &resolved_tool));
    rows.push(row("ToolOutput", &ToolOutput::new(format!("{output} {file} {screen}"))));
    rows.push(row("Base", &Base::Text(screen.clone())));
    rows.push(row("Change", &Change { before: Some(screen.clone()), after: output.clone(), kind: DiffKind::Edit }));
    rows.push(row("Inspected", &Inspected::new(Base::Text(screen.clone()), Some(1))));
    let tool_errors = vec![
        ToolError::UnknownTool(Detail::new("delete_file")),
        ToolError::InvalidArguments(Detail::new("`path` is required")),
        ToolError::Unavailable,
        ToolError::Denied("secret_location"),
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
        ToolError::Host(Detail::new("the file cannot be read")),
        ToolError::NotShared,
    ];
    rows.push(row("ToolError", &tool_errors));
    rows.push(row_display("ToolError (Display)", &tool_errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("|")));
    let service_errors = vec![
        ServiceError::EmptyText,
        ServiceError::TextTooLong,
        ServiceError::Busy,
        ServiceError::TooManyTurns,
        ServiceError::ShuttingDown,
        ServiceError::NoProfile,
        ServiceError::ContextTooMany,
        ServiceError::ContextDuplicate,
        ServiceError::Context(holzwave_core::ai::service::ContextRefusal::NotShared),
        ServiceError::Spawn,
        ServiceError::Store(StoreError::Invalid("a fixed reason")),
        ServiceError::Profile(holzwave_core::ai::profiles::ProfileError::Name),
        ServiceError::Key(holzwave_core::ai::profiles::KeyError::Store),
        ServiceError::ConfigWrite,
        ServiceError::IdTaken,
    ];
    rows.push(row("ServiceError", &service_errors));
    rows.push(row_display("ServiceError (Display)", &service_errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("|")));

    // The ask desk, the runner and the views of the page.
    let find = FindingView { rule: "anthropic_key".to_owned(), count: 1 };
    let ask_view = AskView {
        ask: "a".repeat(32),
        seal: "b".repeat(64),
        pane: 3,
        prompt: typed.clone(),
        prompt_findings: vec![find.clone()],
        input: None,
        profile: "local".to_owned(),
        model: "test-model".to_owned(),
        host: "127.0.0.1:8080".to_owned(),
        locality: Locality::Loopback,
    };
    rows.push(row("AskInput", &AskInput { pane: 3, prompt: typed.clone(), input: screen.clone() }));
    rows.push(row("AskView", &ask_view));
    rows.push(row("AskEvent waiting", &AskEvent::Waiting { view: ask_view.clone() }));
    rows.push(row("Asks", Asks::new(Arc::new(CollectSink::default()), Arc::new(Notes::default())).as_ref()));
    rows.push(row(
        "ExecResult",
        &ExecResult {
            stdout: format!("{output} {screen}").into_bytes(),
            stderr: env.clone().into_bytes(),
            status: Some(0),
            end: ExecEnd::Finished,
            truncated: false,
            dropped: 0,
        },
    ));
    rows.push(row("LocalExec", &LocalExec::new([(OsString::from("SERVICE_API_TOKEN"), OsString::from(env.clone())), (OsString::from("HOME"), OsString::from(env.clone()))])));
    rows.push(row(
        "ContextState",
        &ContextState {
            profile: "local".to_owned(),
            model: "test-model".to_owned(),
            host: "127.0.0.1:8080".to_owned(),
            locality: Locality::Loopback,
            required: true,
            shared: false,
            kinds: vec![ContextKind::Screen],
            persist: false,
        },
    ));
    rows.push(row(
        "OfferView",
        &OfferView {
            offer: "c".repeat(32),
            profile: "local".to_owned(),
            model: "test-model".to_owned(),
            host: "127.0.0.1:8080".to_owned(),
            locality: Locality::Loopback,
            ttl_seconds: 300,
            items: Vec::new(),
            empty: vec![ContextKind::Selection],
            bytes: 0,
            persist: false,
        },
    ));

    assert!(rows.len() >= 40, "only {} values were formatted", rows.len());
    for (name, text) in &rows {
        let found = leaks(text.as_bytes(), &forbidden);
        assert!(found.is_empty(), "{name}: the debug text holds {found:?}");
        assert!(!text.contains(&typed), "{name}: the debug text holds typed text");
        assert!(!text.is_empty(), "{name}: nothing was formatted");
    }
}
