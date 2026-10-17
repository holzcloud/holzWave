#![allow(dead_code)]
//! The shared part of the provider matrix.
//!
//! The matrix runs every wire adapter, every preset and every wire a preset offers through the same neutral scenarios
//! and pins every difference between them that must not exist. Each test crate that includes this file declares it with
//! a path attribute, because a plain module declaration would look for a second test target.
//!
//! A provider is built here only through the public profile, dispatch and chokepoint API, in the order the service uses:
//! a profile is saved with `AiConfig::upsert`, its key is stored with `AiConfig::set_key` in a test secret store, the
//! profile is resolved, the adapter of the wire is picked with `adapter_for` and the pair is boxed with
//! `Resolved::into_provider`. No provider is ever built from an endpoint by hand.

use std::collections::{BTreeSet, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use holzwave_core::ai::http::{Http, HttpConfig, Origin, Resolver, RetryPolicy};
use holzwave_core::ai::profiles::{AiConfig, KeySlot, KeyVault, Preset, ProfileInput, ResolveError};
use holzwave_core::ai::provider::{AiError, CancelToken, ChatRequest, Completion, Flow, Provider, StreamEvent};
use holzwave_core::ai::service::dispatch::adapter_for;
use holzwave_core::ai::types::{ApiKey, Message, StopReason, ToolCall, Usage};
use holzwave_core::ai::wire::{HttpProvider, Wire};
use holzwave_core::secrets::MemoryStore;
use serde_json::Value;

use crate::common::fake_http::{FakeHttp, Reply, Seen};

// ---- canaries ---------------------------------------------------------------------------------------------

/// The provider key of every row. It is test data: a hit anywhere but its own header is a leak.
pub fn canary_key() -> &'static str {
    "sk-canary-0123456789abcdef"
}

/// The gateway token of the Cloudflare rows, a second and different canary.
pub fn canary_gateway_key() -> &'static str {
    "gw-canary-fedcba9876543210"
}

/// Text a server puts into an error message. No fixture and no phrase of the code contains it.
pub const CANARY_TEXT: &str = "CANARY-SERVER-TEXT-91c4e7";

/// Text a message may have kept as the stored text of a context note.
pub const CANARY_STORED: &str = "CANARY-STORED-TEXT-5b2d08";

// ---- the cells --------------------------------------------------------------------------------------------

/// Whether a cell needs a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyRule {
    /// Nothing is sent without a key.
    Required,
    /// A key may be saved and a cell without one works.
    Optional,
}

/// One preset on one wire, with everything the matrix pins about it.
#[derive(Clone, Debug)]
pub struct Cell {
    /// The name of the row.
    pub name: &'static str,
    /// The tag of the preset.
    pub preset_tag: &'static str,
    /// The wire the cell speaks.
    pub wire: Wire,
    /// The base path of the profile address (the part after the host).
    pub base_suffix: &'static str,
    /// The path the adapter appends; for Gemini the model is written as `{model}`.
    pub wire_path: &'static str,
    /// The model of the cell.
    pub model: &'static str,
    /// Whether a key is needed.
    pub key: KeyRule,
    /// Whether the wire signs its reasoning (Anthropic, Responses, Gemini).
    pub signs: bool,
    /// Whether the cell is a local server preset.
    pub local: bool,
    /// Whether the cell is a gateway, a router or a custom server.
    pub gateway: bool,
}

impl Cell {
    /// The target the fake must see: the base path of the address and the path of the wire.
    pub fn path(&self) -> String {
        format!("{}{}", self.base_suffix, self.wire_path.replace("{model}", self.model))
    }

    /// The preset of the cell.
    pub fn preset(&self) -> Preset {
        Preset::from_tag(self.preset_tag).expect("a known preset tag")
    }

    /// Whether the cell is on a Cloudflare gateway (the one preset with a second token).
    pub fn cloudflare(&self) -> bool {
        self.preset_tag == "cloudflare_gateway"
    }

    /// The scheme and host the preset really has, for the rows that hold the destination rules.
    pub fn real_origin(&self) -> &'static str {
        match self.preset_tag {
            "anthropic" => "https://api.anthropic.com",
            "openai" => "https://api.openai.com",
            "gemini" => "https://generativelanguage.googleapis.com",
            "openrouter" => "https://openrouter.ai",
            "vercel_gateway" => "https://ai-gateway.vercel.sh",
            "cloudflare_gateway" => "https://gateway.ai.cloudflare.com",
            "litellm" => "https://litellm.example.net",
            _ => "https://gateway.example.net",
        }
    }
}

const CHAT: &str = "/chat/completions";
const OLLAMA: &str = "/api/chat";
const MESSAGES: &str = "/v1/messages";
const RESPONSES: &str = "/responses";
const GEMINI: &str = "/models/{model}:streamGenerateContent?alt=sse";

#[allow(clippy::too_many_arguments)]
fn cell_of(
    name: &'static str,
    preset_tag: &'static str,
    wire: Wire,
    base_suffix: &'static str,
    key: KeyRule,
    model: &'static str,
) -> Cell {
    let wire_path = match wire {
        Wire::OpenAiChat => CHAT,
        Wire::Ollama => OLLAMA,
        Wire::Anthropic => MESSAGES,
        Wire::OpenAiResponses => RESPONSES,
        Wire::Gemini => GEMINI,
    };
    let signs = matches!(wire, Wire::Anthropic | Wire::OpenAiResponses | Wire::Gemini);
    let local = matches!(name, "ollama" | "lm_studio" | "llama_cpp" | "litellm" | "custom_chat" | "custom_ollama");
    let gateway = matches!(
        name,
        "openrouter"
            | "vercel_chat"
            | "vercel_anthropic"
            | "cloudflare_anthropic"
            | "cloudflare_responses"
            | "cloudflare_chat"
            | "cloudflare_gemini"
            | "litellm"
            | "custom_chat"
            | "custom_responses"
            | "custom_anthropic"
            | "custom_gemini"
            | "custom_ollama"
    );
    Cell { name, preset_tag, wire, base_suffix, wire_path, model, key, signs, local, gateway }
}

/// The 19 cells: the eleven presets, each on every wire it offers, Custom on all five. The data is the pinned preset
/// table with fixed model names; a preset change fails the table test and is one edit here.
pub fn cells() -> Vec<Cell> {
    use KeyRule::{Optional, Required};
    vec![
        cell_of("anthropic", "anthropic", Wire::Anthropic, "", Required, "claude-test-1"),
        cell_of("openai", "openai", Wire::OpenAiResponses, "/v1", Required, "gpt-test-1"),
        cell_of("gemini", "gemini", Wire::Gemini, "/v1beta", Required, "gemini-test-1"),
        cell_of("ollama", "ollama", Wire::Ollama, "", Optional, "llama-test"),
        cell_of("lm_studio", "lm_studio", Wire::OpenAiChat, "/v1", Optional, "local-model"),
        cell_of("llama_cpp", "llama_cpp", Wire::OpenAiChat, "/v1", Optional, "local-model"),
        cell_of("openrouter", "openrouter", Wire::OpenAiChat, "/api/v1", Required, "vendor/model-test"),
        cell_of("vercel_chat", "vercel_gateway", Wire::OpenAiChat, "/v1", Required, "vendor/model-test"),
        cell_of("vercel_anthropic", "vercel_gateway", Wire::Anthropic, "", Required, "claude-test-1"),
        cell_of(
            "cloudflare_anthropic",
            "cloudflare_gateway",
            Wire::Anthropic,
            "/v1/acct/gw/anthropic",
            Optional,
            "claude-test-1",
        ),
        cell_of(
            "cloudflare_responses",
            "cloudflare_gateway",
            Wire::OpenAiResponses,
            "/v1/acct/gw/openai",
            Optional,
            "gpt-test-1",
        ),
        cell_of("cloudflare_chat", "cloudflare_gateway", Wire::OpenAiChat, "/v1/acct/gw/openai", Optional, "gpt-test-1"),
        cell_of(
            "cloudflare_gemini",
            "cloudflare_gateway",
            Wire::Gemini,
            "/v1/acct/gw/google-ai-studio/v1beta",
            Optional,
            "gemini-test-1",
        ),
        cell_of("litellm", "litellm", Wire::OpenAiChat, "/v1", Optional, "local-model"),
        cell_of("custom_chat", "custom", Wire::OpenAiChat, "/v1", Optional, "local-model"),
        cell_of("custom_responses", "custom", Wire::OpenAiResponses, "/v1", Optional, "gpt-test-1"),
        cell_of("custom_anthropic", "custom", Wire::Anthropic, "", Optional, "claude-test-1"),
        cell_of("custom_gemini", "custom", Wire::Gemini, "/v1beta", Optional, "gemini-test-1"),
        cell_of("custom_ollama", "custom", Wire::Ollama, "", Optional, "llama-test"),
    ]
}

/// The cell with this name.
pub fn cell(name: &str) -> Cell {
    cells().into_iter().find(|cell| cell.name == name).unwrap_or_else(|| panic!("there is no cell called {name}"))
}

// ---- headers and bodies -----------------------------------------------------------------------------------

/// The header names every request carries: nothing else is sent unless a key, a version or a gateway token asks for it.
pub const BASELINE: [&str; 5] = ["accept", "content-length", "content-type", "host", "user-agent"];

/// The pinned header that carries a key, and its prefix, by wire.
pub const KEY_HEADERS: [(Wire, &str, &str); 5] = [
    (Wire::OpenAiChat, "authorization", "Bearer "),
    (Wire::OpenAiResponses, "authorization", "Bearer "),
    (Wire::Anthropic, "x-api-key", ""),
    (Wire::Gemini, "x-goog-api-key", ""),
    (Wire::Ollama, "authorization", "Bearer "),
];

/// The version header of the Anthropic dialect.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The second header of a Cloudflare gateway.
pub const GATEWAY_HEADER: &str = "cf-aig-authorization";

/// The key header of a wire: the name and the prefix.
pub fn key_header(wire: Wire) -> (&'static str, &'static str) {
    KEY_HEADERS.iter().find(|(known, _, _)| *known == wire).map(|(_, name, prefix)| (*name, *prefix)).expect("a wire")
}

/// The lower-case names of the headers of a captured request.
pub fn header_names(seen: &Seen) -> BTreeSet<String> {
    seen.headers.iter().map(|(name, _)| name.clone()).collect()
}

/// The captured body as JSON. A body that is not JSON fails the row and names the cell.
pub fn json_body(cell: &str, seen: &Seen) -> Value {
    serde_json::from_slice(&seen.body).unwrap_or_else(|error| panic!("{cell}: the request body is not JSON: {error}"))
}

/// The top-level member names of a JSON object.
pub fn members(value: &Value) -> BTreeSet<String> {
    value.as_object().map(|object| object.keys().cloned().collect()).unwrap_or_default()
}

/// Every string of a JSON value, depth first.
pub fn strings_of(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| strings_of(item, out)),
        Value::Object(map) => map.values().for_each(|item| strings_of(item, out)),
        _ => {}
    }
}

// ---- the rig ----------------------------------------------------------------------------------------------

/// Which keys a rig stores before it resolves the profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keys {
    /// None.
    None,
    /// The provider key.
    Provider,
    /// The provider key and the gateway token (a Cloudflare cell only).
    Both,
    /// The gateway token alone (a Cloudflare cell only).
    GatewayOnly,
}

/// What a rig changes about the default way a cell is built.
#[derive(Clone)]
pub struct RigOptions {
    /// The keys to store.
    pub keys: Keys,
    /// A chokepoint to use instead of a standard one.
    pub http: Option<Arc<Http>>,
    /// The scheme, host and port that replace the address of the fake server; the base path of the cell is added.
    pub origin: Option<String>,
    /// Resolve with the local-only switch on.
    pub local_only: bool,
    /// The profile allows plain http to the local network.
    pub allow_lan_http: bool,
    /// The address is acknowledged when the profile is saved.
    pub acknowledge: bool,
}

impl Default for RigOptions {
    fn default() -> Self {
        RigOptions { keys: Keys::None, http: None, origin: None, local_only: false, allow_lan_http: false, acknowledge: false }
    }
}

impl RigOptions {
    /// Default options with these keys.
    pub fn keyed(keys: Keys) -> Self {
        RigOptions { keys, ..RigOptions::default() }
    }
}

/// A provider built the way the service builds it, and what it was built from.
pub struct Rig {
    /// The server the profile points at (unused by the rows that name another origin).
    pub fake: FakeHttp,
    /// The provider.
    pub provider: HttpProvider,
    /// The chokepoint it sends through.
    pub http: Arc<Http>,
    /// The profile store.
    pub config: AiConfig,
    /// The key vault over the test secret store.
    pub vault: KeyVault,
    /// The id of the profile, which is also the id a context is approved for.
    pub id: String,
}

/// A secret store in memory behind a vault.
pub fn test_vault() -> KeyVault {
    KeyVault::new(Arc::new(MemoryStore::default()))
}

/// Saves the profile of `cell` in `config` and stores the keys `keys` asks for. Returns the id.
pub fn save_profile(config: &mut AiConfig, vault: &KeyVault, cell: &Cell, address: &str, options: &RigOptions) -> String {
    let mut input = ProfileInput::new(cell.name, cell.preset());
    input.wire = Some(cell.wire);
    input.base_url = address.to_owned();
    input.model = cell.model.to_owned();
    input.allow_lan_http = options.allow_lan_http;
    input.acknowledge_gateway = options.acknowledge;
    let id = config.upsert(input).unwrap_or_else(|error| panic!("{}: the profile was refused: {error}", cell.name));
    store_keys(config, vault, &id, options.keys, cell);
    id
}

/// Stores the keys a rig asks for.
pub fn store_keys(config: &mut AiConfig, vault: &KeyVault, id: &str, keys: Keys, cell: &Cell) {
    if matches!(keys, Keys::Provider | Keys::Both) {
        config
            .set_key(vault, id, KeySlot::Provider, ApiKey::new(canary_key().to_owned()))
            .unwrap_or_else(|error| panic!("{}: the key was refused: {error}", cell.name));
    }
    if matches!(keys, Keys::Both | Keys::GatewayOnly) {
        config
            .set_key(vault, id, KeySlot::Gateway, ApiKey::new(canary_gateway_key().to_owned()))
            .unwrap_or_else(|error| panic!("{}: the gateway token was refused: {error}", cell.name));
    }
}

/// Builds the provider of `cell` the way the service does: save, store the keys, resolve, pick the adapter, box.
pub fn try_rig_with(cell: &Cell, replies: Vec<Reply>, options: RigOptions) -> Result<Rig, ResolveError> {
    let fake = FakeHttp::start(replies);
    let origin = options.origin.clone().unwrap_or_else(|| fake.base());
    let address = format!("{origin}{}", cell.base_suffix);
    let vault = test_vault();
    let mut config = AiConfig::default();
    let id = save_profile(&mut config, &vault, cell, &address, &options);
    let http = options.http.clone().unwrap_or_else(|| Arc::new(Http::new(HttpConfig::standard())));
    let resolved = config.resolve(&id, &vault, options.local_only)?;
    let scope = Origin::from_scope(resolved.scope()).expect("a resolved scope is an origin");
    let adapter = adapter_for(resolved.wire(), &scope);
    let provider = resolved.into_provider(adapter, Arc::clone(&http));
    Ok(Rig { fake, provider, http, config, vault, id })
}

/// As [`try_rig_with`] with the given keys and no other option.
pub fn try_rig(cell: &Cell, replies: Vec<Reply>, keys: Keys) -> Result<Rig, ResolveError> {
    try_rig_with(cell, replies, RigOptions::keyed(keys))
}

/// A rig that must build, over a chokepoint with these settings.
pub fn rig(cell: &Cell, replies: Vec<Reply>, keys: Keys, config: HttpConfig) -> Rig {
    let options = RigOptions { keys, http: Some(Arc::new(Http::new(config))), ..RigOptions::default() };
    try_rig_with(cell, replies, options).unwrap_or_else(|error| panic!("{}: the rig was refused: {error}", cell.name))
}

/// The keys a cell is run with when a row does not care: the provider key where one is needed, and the provider key
/// where it is optional too, so every row of every cell goes through its key header.
pub fn default_keys(_cell: &Cell) -> Keys {
    Keys::Provider
}

// ---- the scenarios ----------------------------------------------------------------------------------------

/// The neutral scenarios every dialect is asked to tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    /// An answer in two pieces.
    Text,
}

/// The constants of the scenarios.
pub struct Neutral;

impl Neutral {
    /// The pieces of the neutral answer.
    pub const TEXT_PARTS: [&'static str; 2] = ["Hel", "lo \u{e9} \u{65e5}\u{672c} \u{1f600}"];
    /// The neutral answer.
    pub const TEXT: &'static str = "Hello \u{e9} \u{65e5}\u{672c} \u{1f600}";
    /// The usage of the text scenario.
    pub const TEXT_USAGE: Usage = Usage { input_tokens: 11, output_tokens: 5 };
}

// ---- requests and runs ------------------------------------------------------------------------------------

/// The system text of every request.
pub const SYSTEM_TEXT: &str = "You are a terminal assistant.";

/// A request for `cell` with the given user text, the system text, 1024 output tokens, no tools and no context.
pub fn chat_request(cell: &Cell, text: &str) -> ChatRequest {
    let mut request = ChatRequest::new(cell.model);
    request.system = SYSTEM_TEXT.to_owned();
    request.messages.push(Message::user_text("u1", text, 1));
    request.max_output_tokens = Some(1024);
    request
}

/// What a run delivered and how it ended.
pub struct Run {
    /// The events, in order.
    pub events: Vec<StreamEvent>,
    /// The completion, or the error.
    pub result: Result<Completion, AiError>,
}

/// Runs one turn with a fresh token and a callback that records and continues.
pub fn run(provider: &dyn Provider, request: &ChatRequest) -> Run {
    let mut events = Vec::new();
    let result = provider.stream(
        request,
        &mut |event| {
            events.push(event);
            Flow::Continue
        },
        &CancelToken::new(),
    );
    Run { events, result }
}

/// What a turn said, in plain values.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    /// The answer text.
    pub text: String,
    /// The reasoning text.
    pub reasoning: String,
    /// The tool calls, in order.
    pub calls: Vec<ToolCall>,
    /// The last usage.
    pub usage: Usage,
    /// Why the turn ended; `None` for an error.
    pub stop: Option<StopReason>,
}

/// The outcome of a run.
pub fn outcome(run: &Run) -> Outcome {
    let mut out = Outcome {
        text: String::new(),
        reasoning: String::new(),
        calls: Vec::new(),
        usage: Usage::default(),
        stop: None,
    };
    for event in &run.events {
        match event {
            StreamEvent::TextDelta(text) => out.text.push_str(text),
            StreamEvent::ReasoningDelta(text) => out.reasoning.push_str(text),
            StreamEvent::ToolCall(call) => out.calls.push(call.clone()),
            StreamEvent::Usage(usage) => out.usage = *usage,
        }
    }
    if let Ok(done) = &run.result {
        out.stop = Some(done.stop_reason);
    }
    out
}

/// The side of a request a text is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// The system, instruction or system-instruction member.
    System,
    /// The messages of the user.
    User,
}

fn texts_in(content: &Value, out: &mut Vec<String>) {
    match content {
        Value::String(text) => out.push(text.clone()),
        Value::Array(parts) => {
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    out.push(text.to_owned());
                }
            }
        }
        _ => {}
    }
}

/// The text strings of one side of a request body, in the shape of the dialect.
pub fn texts_of(wire: Wire, body: &Value, side: Side) -> Vec<String> {
    let mut out = Vec::new();
    match wire {
        Wire::OpenAiChat | Wire::Ollama => {
            let wanted = if side == Side::System { "system" } else { "user" };
            for message in body.get("messages").and_then(Value::as_array).into_iter().flatten() {
                if message.get("role").and_then(Value::as_str) == Some(wanted) {
                    if let Some(content) = message.get("content") {
                        texts_in(content, &mut out);
                    }
                }
            }
        }
        Wire::Anthropic => match side {
            Side::System => {
                if let Some(system) = body.get("system") {
                    texts_in(system, &mut out);
                }
            }
            Side::User => {
                for message in body.get("messages").and_then(Value::as_array).into_iter().flatten() {
                    if message.get("role").and_then(Value::as_str) == Some("user") {
                        if let Some(content) = message.get("content") {
                            texts_in(content, &mut out);
                        }
                    }
                }
            }
        },
        Wire::OpenAiResponses => match side {
            Side::System => {
                if let Some(text) = body.get("instructions").and_then(Value::as_str) {
                    out.push(text.to_owned());
                }
            }
            Side::User => {
                for item in body.get("input").and_then(Value::as_array).into_iter().flatten() {
                    if item.get("role").and_then(Value::as_str) == Some("user") {
                        if let Some(content) = item.get("content") {
                            texts_in(content, &mut out);
                        }
                    }
                }
            }
        },
        Wire::Gemini => match side {
            Side::System => {
                if let Some(parts) = body.get("systemInstruction").and_then(|system| system.get("parts")) {
                    texts_in(parts, &mut out);
                }
            }
            Side::User => {
                for content in body.get("contents").and_then(Value::as_array).into_iter().flatten() {
                    if content.get("role").and_then(Value::as_str) == Some("user") {
                        if let Some(parts) = content.get("parts") {
                            texts_in(parts, &mut out);
                        }
                    }
                }
            }
        },
    }
    out
}

// ---- timing and chunking ----------------------------------------------------------------------------------

static TIMING: Mutex<()> = Mutex::new(());

/// Held by a row whose assertion measures time, so rows of one crate do not run their clocks over each other.
pub fn timing_guard() -> MutexGuard<'static, ()> {
    TIMING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Three chunks: one cut one byte after the first byte of the first two-byte character and one cut one byte after the
/// first byte of the first four-byte character, so neither cut falls on a character boundary.
pub fn split_inside_characters(bytes: &[u8]) -> Vec<Vec<u8>> {
    let two = bytes.iter().position(|byte| (0xC0..0xE0).contains(byte)).expect("a two-byte character") + 1;
    let four = bytes.iter().position(|byte| (0xF0..0xF8).contains(byte)).expect("a four-byte character") + 1;
    assert!(two < four, "the two-byte character comes first");
    vec![bytes[..two].to_vec(), bytes[two..four].to_vec(), bytes[four..].to_vec()]
}

/// The reply of a wire for these chunks: Ollama says so in its content type.
pub fn reply_for(wire: Wire, chunks: Vec<Vec<u8>>) -> Reply {
    let reply = Reply::sse_bytes(chunks);
    if wire == Wire::Ollama { reply.content_type("application/x-ndjson") } else { reply }
}

/// How many resolver calls a stub has seen, and for which names.
#[derive(Default)]
struct StubState {
    answers: VecDeque<Vec<IpAddr>>,
    last: Vec<IpAddr>,
    names: Vec<String>,
}

/// A resolver that answers from a queue, repeats its last answer and remembers every name it was asked for.
pub struct StubResolver {
    state: Mutex<StubState>,
    calls: AtomicUsize,
}

impl StubResolver {
    /// A stub with these answers in order.
    pub fn new(answers: Vec<Vec<IpAddr>>) -> Arc<StubResolver> {
        let last = answers.last().cloned().unwrap_or_default();
        Arc::new(StubResolver {
            state: Mutex::new(StubState { answers: answers.into(), last, names: Vec::new() }),
            calls: AtomicUsize::new(0),
        })
    }

    /// How many names were looked up.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// The names that were looked up, in order.
    pub fn names(&self) -> Vec<String> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).names.clone()
    }
}

impl Resolver for StubResolver {
    fn lookup(&self, host: &str) -> std::io::Result<Vec<IpAddr>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.names.push(host.to_owned());
        let answer = state.answers.pop_front().unwrap_or_else(|| state.last.clone());
        Ok(answer)
    }
}

/// A chokepoint whose names are looked up by `stub`.
pub fn http_with_stub(config: HttpConfig, stub: &Arc<StubResolver>) -> Arc<Http> {
    Arc::new(Http::with_resolver(config, Arc::clone(stub) as Arc<dyn Resolver>))
}

/// The standard chokepoint settings with a short retry: 5 ms of base delay and 1 s of longest wait, so a `retry-after`
/// of seven seconds is returned and not slept.
pub fn fast_retry() -> HttpConfig {
    HttpConfig {
        retry: RetryPolicy { max_retries: 2, base_delay: Duration::from_millis(5), max_wait: Duration::from_secs(1) },
        ..HttpConfig::standard()
    }
}

/// The standard settings with this idle limit in milliseconds and a short retry.
pub fn short_idle(milliseconds: u64) -> HttpConfig {
    HttpConfig { idle_timeout: Duration::from_millis(milliseconds), ..fast_retry() }
}
