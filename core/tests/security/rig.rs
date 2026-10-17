//! The service rig of the release gate: the service built the way the shell builds it, over the in-process server.
//!
//! The rig makes a scratch directory with a chat folder and a configuration folder, starts the server, builds the
//! service through the builder with a memory vault and a scripted context source, saves one profile and keys it. The
//! capture helpers read what the server received from the outside. Test code only.

use std::fs;
use std::net::IpAddr;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use holzwave_core::ai::agent::Limits;
use holzwave_core::ai::http::{Http, HttpConfig, Resolver};
use holzwave_core::ai::profiles::{KeySlot, KeyVault, Preset, ProfileInput};
use holzwave_core::ai::redact::SourceText;
use holzwave_core::ai::scripted::{Script, ScriptedProvider};
use holzwave_core::ai::service::context::{ContextSource, Fetched, ReadRefusal};
use holzwave_core::ai::service::events::AiEvent;
use holzwave_core::ai::service::{AiService, AiSink, AskEvent, AskLimits, AskNotify, AskOut, Asks};
use holzwave_core::ai::store::{ConversationId, JsonDirStore};
use holzwave_core::ai::tools::{DenyPolicy, ToolHost};
use holzwave_core::ai::types::{ApiKey, Capabilities, ContextKind, Locality};
use holzwave_core::ai::wire::Wire;
use holzwave_core::layout::PaneId;
use holzwave_core::secrets::MemoryStore;
use holzwave_core::settings::{AiLocalContext, Settings};
use serde_json::{Value, json};

use crate::common::fake_http::{FakeHttp, Reply, Seen};
use crate::common::{scratch_dir, wait_until};
use crate::security::canaries::{api_key, wire_key};

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

/// A Chat Completions answer that asks for one tool call: a role chunk, one tool call (the arguments as one JSON text),
/// a finish chunk with the reason `tool_calls`, usage, the marker.
pub fn tool_call_reply(call_id: &str, name: &str, args: &Value) -> Reply {
    let call = json!({ "tool_calls": [{
        "index": 0, "id": call_id, "type": "function",
        "function": { "name": name, "arguments": args.to_string() }
    }] });
    let mut chunks = vec![
        chunk(&json!({ "role": "assistant", "content": "" }), None),
        chunk(&call, None),
        chunk(&json!({}), Some("tool_calls")),
    ];
    chunks.extend(closing());
    Reply::sse_bytes(chunks.into_iter().map(String::into_bytes).collect())
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

// ---- the resolver and the ask doubles -----------------------------------------------------------------------------

/// A resolver that records every host it is asked to look up and answers one documentation-range address.
#[derive(Default)]
pub struct RecordingResolver {
    hosts: Mutex<Vec<String>>,
}

impl RecordingResolver {
    /// The hosts looked up so far, in order.
    pub fn lookups(&self) -> Vec<String> {
        lock(&self.hosts).clone()
    }
}

impl Resolver for RecordingResolver {
    fn lookup(&self, host: &str) -> std::io::Result<Vec<IpAddr>> {
        lock(&self.hosts).push(host.to_owned());
        Ok(vec!["203.0.113.7".parse::<IpAddr>().expect("an address")])
    }
}

/// The window of the desk: keeps every event with the pane it was for.
#[derive(Default)]
pub struct Notes {
    log: Mutex<Vec<(PaneId, AskEvent)>>,
}

impl AskNotify for Notes {
    fn notify(&self, pane: PaneId, event: &AskEvent) {
        lock(&self.log).push((pane, event.clone()));
    }
}

impl Notes {
    /// Every event with its pane.
    pub fn all(&self) -> Vec<(PaneId, AskEvent)> {
        lock(&self.log).clone()
    }

    /// The `waiting` cards shown so far, as JSON.
    pub fn waiting(&self) -> Vec<Value> {
        self.all()
            .into_iter()
            .filter_map(|(_, event)| match event {
                AskEvent::Waiting { .. } => serde_json::to_value(&event).ok(),
                AskEvent::Closed { .. } => None,
            })
            .collect()
    }

    /// The first `waiting` card, waiting up to `timeout` for it.
    pub fn wait_waiting(&self, timeout: Duration) -> Option<Value> {
        wait_until(timeout, || !self.waiting().is_empty());
        self.waiting().into_iter().next()
    }

    /// The `closed` events as (ask id, reason).
    pub fn closed(&self) -> Vec<(String, String)> {
        self.all()
            .into_iter()
            .filter_map(|(_, event)| match serde_json::to_value(&event).ok()? {
                value if value["kind"] == "closed" => {
                    Some((value["ask"].as_str()?.to_owned(), value["why"].as_str()?.to_owned()))
                }
                _ => None,
            })
            .collect()
    }
}

/// What the caller of an ask received: the pieces of the answer and the notes of status.
#[derive(Default)]
pub struct AskOutput {
    pub texts: Vec<String>,
    pub notes: Vec<String>,
}

impl AskOut for AskOutput {
    fn text(&mut self, text: &str) -> std::io::Result<()> {
        self.texts.push(text.to_owned());
        Ok(())
    }

    fn note(&mut self, text: &str) -> std::io::Result<()> {
        self.notes.push(text.to_owned());
        Ok(())
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
    /// One profile for every preset, each with the key its preset needs.
    pub profile_per_preset: bool,
    /// The resolver the chokepoint asks for every host name.
    pub resolver: Option<Arc<dyn Resolver>>,
    /// The tools the shell would offer.
    pub tool_host: Option<Arc<dyn ToolHost>>,
    /// The places the file tools must stay away from.
    pub deny: Option<DenyPolicy>,
    /// The limits of a turn.
    pub limits: Option<Limits>,
    /// Put the ask desk in front of the sink of the service, with these limits.
    pub asks: Option<AskLimits>,
    /// Save the profile `local` on the server (default). The ask tests with a scripted provider switch it off so the
    /// desk finds exactly one candidate.
    pub local_profile: bool,
}

impl Default for RigOptions {
    fn default() -> Self {
        RigOptions {
            replies: vec![chat_reply("ok.")],
            source: Vec::new(),
            persist_context: false,
            local_context_auto: false,
            profile_per_preset: false,
            resolver: None,
            tool_host: None,
            deny: None,
            limits: None,
            asks: None,
            local_profile: true,
        }
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
    asks: Option<Arc<Asks>>,
    notes: Arc<Notes>,
    provider: Option<Arc<ScriptedProvider>>,
    profile_ids: Vec<String>,
}

impl Rig {
    /// Builds the service the way the shell does, saves the profile `local` on the server and keys it.
    pub fn new(options: RigOptions) -> Rig {
        let dir = scratch_dir("gate");
        let (chats, config) = (dir.join("chats"), dir.join("config"));
        fs::create_dir(&config).expect("a configuration folder");
        let fake = FakeHttp::start(options.replies);
        let sink = Arc::new(CollectSink::default());
        let notes = Arc::new(Notes::default());
        let asks = options
            .asks
            .map(|limits| Asks::with_limits(sink.clone() as Arc<dyn AiSink>, notes.clone() as Arc<dyn AskNotify>, limits));
        let service_sink: Arc<dyn AiSink> = match &asks {
            Some(asks) => asks.clone() as Arc<dyn AiSink>,
            None => sink.clone() as Arc<dyn AiSink>,
        };
        let secrets = Arc::new(MemoryStore::default());
        let source = Arc::new(ScriptedSource::new(options.source));
        let mut builder = AiService::builder(Arc::new(JsonDirStore::new(chats)), service_sink, config)
            .vault(KeyVault::new(secrets.clone()))
            .context_source(source.clone() as Arc<dyn ContextSource>);
        if let Some(resolver) = options.resolver {
            builder = builder.http(Arc::new(Http::with_resolver(HttpConfig::standard(), resolver)));
        }
        if let Some(host) = options.tool_host {
            builder = builder.tool_host(host);
        }
        if let Some(policy) = options.deny {
            builder = builder.deny_policy(policy);
        }
        if let Some(limits) = options.limits {
            builder = builder.limits(limits);
        }
        let service = builder.build();
        let settings = Settings {
            ai_persist_context: options.persist_context,
            ai_local_context: if options.local_context_auto { AiLocalContext::Auto } else { AiLocalContext::Ask },
            ..Settings::default()
        };
        service.apply_settings(&settings);
        let mut rig = Rig {
            dir,
            fake,
            sink,
            secrets,
            source,
            service: Arc::new(service),
            asks,
            notes,
            provider: None,
            profile_ids: Vec::new(),
        };
        if options.local_profile {
            rig.save_local_profile();
        }
        if options.profile_per_preset {
            rig.save_preset_profiles();
        }
        rig
    }

    /// A rig whose only provider is a scripted one registered under the id `scripted` with the class `locality`. The
    /// desk is in front of the sink and knows the provider as a candidate. `turns` are the scripts it plays.
    pub fn scripted(locality: Locality, turns: Vec<Script>) -> Rig {
        Rig::scripted_with(locality, turns, RigOptions::default())
    }

    /// Like [`Rig::scripted`] with the options of the caller (the desk limits, the tool host).
    pub fn scripted_with(locality: Locality, turns: Vec<Script>, options: RigOptions) -> Rig {
        let options = RigOptions {
            local_profile: false,
            asks: Some(options.asks.unwrap_or_default()),
            ..options
        };
        let mut rig = Rig::new(options);
        let mut provider = ScriptedProvider::new("scripted").with_capabilities(Capabilities {
            tools: true,
            ..Capabilities::conservative(locality)
        });
        for turn in turns {
            provider = provider.with_turn(turn);
        }
        let provider = Arc::new(provider);
        rig.service.register_provider("scripted", provider.clone()).expect("the provider is registered");
        rig.asks.as_ref().expect("a desk").add_candidate("scripted", "m");
        rig.provider = Some(provider);
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

    /// Saves one profile for every preset and the key each of them needs. The Custom one is on the server of the rig.
    fn save_preset_profiles(&mut self) {
        for preset in Preset::ALL {
            let mut input = ProfileInput::new(preset.tag(), preset);
            input.model = "m".to_owned();
            input.acknowledge_gateway = true;
            if preset == Preset::Custom {
                input.wire = Some(Wire::OpenAiChat);
                input.base_url = format!("{}/v1", self.fake.base());
            }
            if preset == Preset::CloudflareGateway {
                input.base_url = "https://gateway.ai.cloudflare.com/v1/account1/gateway1/anthropic".to_owned();
            }
            let id = self.service.save_profile(input).expect("the profile is saved");
            self.service.set_key(&id, KeySlot::Provider, ApiKey::new(wire_key(preset.tag()))).expect("the key is saved");
            if preset.spec().gateway_token {
                let token = ApiKey::new(wire_key("gateway"));
                self.service.set_key(&id, KeySlot::Gateway, token).expect("the gateway token is saved");
            }
            self.profile_ids.push(id);
        }
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

    /// The ask desk, when the rig has one.
    pub fn asks(&self) -> &Arc<Asks> {
        self.asks.as_ref().expect("this rig has a desk")
    }

    /// The window of the desk.
    pub fn notes(&self) -> &Arc<Notes> {
        &self.notes
    }

    /// The scripted provider of a scripted rig.
    pub fn provider(&self) -> &Arc<ScriptedProvider> {
        self.provider.as_ref().expect("this rig has a scripted provider")
    }

    /// The ids of the profiles made for the presets, in the order of the presets.
    pub fn preset_profiles(&self) -> &[String] {
        &self.profile_ids
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
