//! The service rig of the release gate: the service built the way the shell builds it, over the in-process server.
//!
//! The rig makes a scratch directory with a chat folder and a configuration folder, starts the server, builds the
//! service through the builder with a memory vault and a scripted context source, saves one profile and keys it. The
//! capture helpers read what the server received from the outside. Test code only.

use std::fs;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use holzwave_core::ai::profiles::{KeySlot, KeyVault, Preset, ProfileInput};
use holzwave_core::ai::redact::SourceText;
use holzwave_core::ai::service::context::{ContextSource, Fetched, ReadRefusal};
use holzwave_core::ai::service::events::AiEvent;
use holzwave_core::ai::service::{AiService, AiSink};
use holzwave_core::ai::store::{ConversationId, JsonDirStore};
use holzwave_core::ai::types::{ApiKey, ContextKind};
use holzwave_core::ai::wire::Wire;
use holzwave_core::layout::PaneId;
use holzwave_core::secrets::MemoryStore;
use holzwave_core::settings::{AiLocalContext, Settings};
use serde_json::{Value, json};

use crate::common::fake_http::{FakeHttp, Reply, Seen};
use crate::common::{scratch_dir, wait_until};
use crate::security::canaries::api_key;

/// The model of the main profile.
pub const MODEL: &str = "test-model";
/// The id of the main profile.
pub const PROFILE: &str = "local";
/// The pane the scripted source offers.
pub const PANE: PaneId = 7;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---- the sink -----------------------------------------------------------------------------------------------------

/// Keeps every event as JSON, in the order it arrived. Events are read as JSON so that a new member never breaks the
/// gate.
#[derive(Default)]
pub struct CollectSink {
    events: Mutex<Vec<Value>>,
}

impl AiSink for CollectSink {
    fn emit(&self, event: &AiEvent) {
        lock(&self.events).push(serde_json::to_value(event).expect("an event serialises"));
    }
}

impl CollectSink {
    /// Every event of every chat.
    pub fn all(&self) -> Vec<Value> {
        lock(&self.events).clone()
    }

    /// The events of one chat.
    pub fn events(&self, conversation: &str) -> Vec<Value> {
        self.all().into_iter().filter(|event| event["conversation"] == conversation).collect()
    }

    /// The kinds of the events of one chat, in order.
    pub fn kinds(&self, conversation: &str) -> Vec<String> {
        self.events(conversation).iter().map(|event| event["kind"].as_str().unwrap_or("").to_owned()).collect()
    }

    /// The `turn_ended` event of a chat, waiting up to `timeout` for it.
    pub fn wait_ended(&self, conversation: &str, timeout: Duration) -> Option<Value> {
        let find = || self.events(conversation).into_iter().rev().find(|event| event["kind"] == "turn_ended");
        wait_until(timeout, || find().is_some());
        find()
    }

    /// How many turns of a chat have ended.
    pub fn ended(&self, conversation: &str) -> usize {
        self.events(conversation).iter().filter(|event| event["kind"] == "turn_ended").count()
    }
}

// ---- the replies --------------------------------------------------------------------------------------------------

/// One event of a Chat Completions stream.
fn chunk(delta: &Value, finish: Option<&str>) -> String {
    let body = json!({
        "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": MODEL,
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }]
    });
    format!("data: {body}\n\n")
}

/// The closing events of a stream: usage in a chunk without choices, then the marker.
fn closing() -> Vec<String> {
    let usage = json!({
        "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": MODEL, "choices": [],
        "usage": { "prompt_tokens": 11, "completion_tokens": 2, "total_tokens": 13 }
    });
    vec![format!("data: {usage}\n\n"), "data: [DONE]\n\n".to_owned()]
}

/// A Chat Completions answer that says `text`: a role chunk, two content chunks, a finish chunk, usage, the marker.
pub fn chat_reply(text: &str) -> Reply {
    let split = text.char_indices().nth(text.chars().count() / 2).map_or(text.len(), |(at, _)| at);
    let (first, second) = text.split_at(split);
    let mut chunks = vec![
        chunk(&json!({ "role": "assistant", "content": "" }), None),
        chunk(&json!({ "content": first }), None),
        chunk(&json!({ "content": second }), None),
        chunk(&json!({}), Some("stop")),
    ];
    chunks.extend(closing());
    Reply::sse_bytes(chunks.into_iter().map(String::into_bytes).collect())
}

/// An error status with a body.
pub fn error_reply(status: u16, body: &str) -> Reply {
    Reply::status(status).body(body.as_bytes().to_vec())
}

// ---- the context source -------------------------------------------------------------------------------------------

/// One thing a pane gives: the kind, a label, the raw text and the byte ranges the engine drew hidden.
#[derive(Clone)]
pub struct Entry {
    pub pane: PaneId,
    pub kind: ContextKind,
    pub label: String,
    pub text: String,
    pub hidden: Vec<Range<usize>>,
}

impl Entry {
    /// An entry with nothing hidden.
    pub fn new(pane: PaneId, kind: ContextKind, label: &str, text: String) -> Entry {
        Entry { pane, kind, label: label.to_owned(), text, hidden: Vec::new() }
    }
}

/// A context source over a list of entries that counts its reads.
#[derive(Default)]
pub struct ScriptedSource {
    entries: Mutex<Vec<Entry>>,
    reads: AtomicUsize,
}

impl ScriptedSource {
    /// A source that offers these entries.
    pub fn new(entries: Vec<Entry>) -> ScriptedSource {
        ScriptedSource { entries: Mutex::new(entries), reads: AtomicUsize::new(0) }
    }

    /// How often a pane was read.
    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    /// The kinds the entries of `pane` offer, in the order of the entries.
    pub fn kinds_of(&self, pane: PaneId) -> Vec<ContextKind> {
        lock(&self.entries).iter().filter(|entry| entry.pane == pane).map(|entry| entry.kind).collect()
    }
}

impl ContextSource for ScriptedSource {
    fn kinds(&self, pane: PaneId) -> Vec<ContextKind> {
        self.kinds_of(pane)
    }

    fn read(&self, pane: PaneId, kind: ContextKind, _max_bytes: usize) -> Result<Option<Fetched>, ReadRefusal> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let entries = lock(&self.entries);
        let found = entries.iter().find(|entry| entry.pane == pane && entry.kind == kind);
        Ok(found.map(|entry| {
            Fetched::new(entry.label.clone(), SourceText::with_hidden(entry.text.clone(), entry.hidden.clone()), false)
        }))
    }
}

// ---- the rig ------------------------------------------------------------------------------------------------------

/// What a rig is built from. Every field has a sensible default.
pub struct RigOptions {
    /// What the server answers, request by request; the last reply repeats.
    pub replies: Vec<Reply>,
    /// What the panes offer.
    pub source: Vec<Entry>,
    /// Keep the terminal text of a chat in its file.
    pub persist_context: bool,
    /// A destination on this computer is not asked about (the setting `ai_local_context` is `auto`).
    pub local_context_auto: bool,
}

impl Default for RigOptions {
    fn default() -> Self {
        RigOptions { replies: vec![chat_reply("ok.")], source: Vec::new(), persist_context: false, local_context_auto: false }
    }
}

/// The service, the server, the vault and the folders of one test.
pub struct Rig {
    dir: PathBuf,
    fake: FakeHttp,
    sink: Arc<CollectSink>,
    secrets: Arc<MemoryStore>,
    source: Arc<ScriptedSource>,
    service: Arc<AiService>,
}

impl Rig {
    /// Builds the service the way the shell does, saves the profile `local` on the server and keys it.
    pub fn new(options: RigOptions) -> Rig {
        let dir = scratch_dir("gate");
        let (chats, config) = (dir.join("chats"), dir.join("config"));
        fs::create_dir(&config).expect("a configuration folder");
        let fake = FakeHttp::start(options.replies);
        let sink = Arc::new(CollectSink::default());
        let secrets = Arc::new(MemoryStore::default());
        let source = Arc::new(ScriptedSource::new(options.source));
        let service = AiService::builder(Arc::new(JsonDirStore::new(chats)), sink.clone() as Arc<dyn AiSink>, config)
            .vault(KeyVault::new(secrets.clone()))
            .context_source(source.clone() as Arc<dyn ContextSource>)
            .build();
        let settings = Settings {
            ai_persist_context: options.persist_context,
            ai_local_context: if options.local_context_auto { AiLocalContext::Auto } else { AiLocalContext::Ask },
            ..Settings::default()
        };
        service.apply_settings(&settings);
        let rig = Rig { dir, fake, sink, secrets, source, service: Arc::new(service) };
        rig.save_local_profile();
        rig
    }

    /// Saves the profile `local` on the server of the rig and keys it with the key canary.
    fn save_local_profile(&self) {
        let mut input = ProfileInput::new("Local", Preset::Custom);
        input.wire = Some(Wire::OpenAiChat);
        input.base_url = format!("{}/v1", self.fake.base());
        input.model = MODEL.to_owned();
        input.acknowledge_gateway = true;
        let id = self.service.save_profile(input).expect("the profile is saved");
        assert_eq!(id, PROFILE, "the profile id");
        self.service.set_key(PROFILE, KeySlot::Provider, ApiKey::new(api_key())).expect("the key is saved");
    }

    /// The service.
    pub fn service(&self) -> &Arc<AiService> {
        &self.service
    }

    /// The server.
    pub fn fake(&self) -> &FakeHttp {
        &self.fake
    }

    /// The sink that collects the events.
    pub fn sink(&self) -> &Arc<CollectSink> {
        &self.sink
    }

    /// The source the panes are read from.
    pub fn source(&self) -> &Arc<ScriptedSource> {
        &self.source
    }

    /// The memory vault the keys live in.
    pub fn vault(&self) -> &Arc<MemoryStore> {
        &self.secrets
    }

    /// The scratch folder, the chat folder and the configuration folder.
    pub fn dirs(&self) -> (PathBuf, PathBuf, PathBuf) {
        (self.dir.clone(), self.dir.join("chats"), self.dir.join("config"))
    }

    /// A fresh chat id.
    pub fn chat(&self) -> ConversationId {
        self.service.new_conversation().expect("a chat id")
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

// ---- what the server received -------------------------------------------------------------------------------------

/// One request as the server read it.
#[derive(Clone)]
pub struct Capture {
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Capture {
    fn of(seen: &Seen) -> Capture {
        Capture { target: seen.target.clone(), headers: seen.headers.clone(), body: seen.body.clone() }
    }
}

/// Every request the server has read.
pub fn captures(fake: &FakeHttp) -> Vec<Capture> {
    fake.seen().iter().map(Capture::of).collect()
}

/// The header that carries the key of a wire.
fn key_header(wire: &str) -> &'static str {
    match wire {
        "openai_chat" | "openai_responses" | "ollama" => "authorization",
        "anthropic" => "x-api-key",
        "gemini" => "x-goog-api-key",
        other => panic!("no key header is known for the wire {other}"),
    }
}

fn flatten(requests: &[Capture], hide_key_of: Option<&str>) -> Vec<u8> {
    let mut out = Vec::new();
    for request in requests {
        out.extend_from_slice(request.target.as_bytes());
        out.push(0);
        for (name, value) in &request.headers {
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            if hide_key_of.is_none_or(|wire| key_header(wire) != name) {
                out.extend_from_slice(value.as_bytes());
            }
            out.push(0);
        }
        out.extend_from_slice(&request.body);
        out.push(0);
    }
    out
}

/// Every target, header name, header value and body the server read, joined with a separator byte.
pub fn everything(fake: &FakeHttp) -> Vec<u8> {
    flatten(&captures(fake), None)
}

/// The same as [`everything`] with the value of the one header that carries the key of `wire` left out.
pub fn everything_except_key(fake: &FakeHttp, wire: &str) -> Vec<u8> {
    flatten(&captures(fake), Some(wire))
}

/// Every string value of the JSON body of a request, found recursively.
pub fn body_strings(capture: &Capture) -> Vec<String> {
    fn walk(value: &Value, into: &mut Vec<String>) {
        match value {
            Value::String(text) => into.push(text.clone()),
            Value::Array(items) => items.iter().for_each(|item| walk(item, into)),
            Value::Object(map) => map.values().for_each(|item| walk(item, into)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    if let Ok(value) = serde_json::from_slice::<Value>(&capture.body) {
        walk(&value, &mut out);
    }
    out
}
