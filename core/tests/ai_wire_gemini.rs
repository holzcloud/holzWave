//! The Gemini adapter, proved through the chokepoint and the event decoder against an in-process server, with no outside
//! network.
//!
//! The streams in `tests/fixtures/ai_wire_gemini/` are written by hand from the documented response shape; they are not
//! recordings of a real account. Every request that goes through a provider goes to `FakeHttp`, an HTTP/1.1 server in
//! this process, so a test reads the exact address, headers and body that left the adapter. A stream that is driven
//! directly goes through `SseDecoder` and the turn of the adapter the way the shared driver does it, so a split of the
//! bytes can be chosen at any offset. A failing case names its row.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::fake_http::{FakeHttp, Reply};
use holzwave_core::ai::gate::{ApprovedContext, Destination, Gate, OfferRequest, RequestItem};
use holzwave_core::ai::http::{Http, HttpConfig, NetworkPolicy, RetryPolicy, SecretHeader, Target};
use holzwave_core::ai::provider::{
    AiError, CancelToken, ChatRequest, Collector, Completion, ErrorClass, Provider, StreamEvent,
};
use holzwave_core::ai::redact::SourceText;
use holzwave_core::ai::types::{
    ApiKey, Consent, ContextKind, ContextNote, Locality, Message, MessageStatus, NoteItem, Opaque, Part, Role,
    StopReason, ToolCall, ToolResult, ToolSpec, Usage,
};
use holzwave_core::ai::wire::gemini::{
    API_KEY_HEADER, CONTEXT_TOKENS, GeminiAdapter, MAX_MODEL_ID, OUTPUT_TOKENS, stream_path,
};
use holzwave_core::ai::wire::{Endpoint, HttpProvider, Wire, WireAdapter};
use serde_json::{Value, json};

/// The key the tests bind to the origin of the fake server.
const CANARY_KEY: &str = "sk-canary-0123456789abcdef";

/// The thought signature of the tool fixture: base64 of a sentence that says it is not a credential.
const TOOL_SIGNATURE: &str = "c2lnbmF0dXJlLWZpeHR1cmUtMDAwMS1ub3QtYS1jcmVkZW50aWFs";

// ---- helpers ----------------------------------------------------------------------------------------------

/// `bytes` with every CRLF turned into LF: a checkout on Windows may convert the fixtures.
fn lf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'\r' && bytes.get(at + 1) == Some(&b'\n') {
            at += 1;
            continue;
        }
        out.push(bytes[at]);
        at += 1;
    }
    out
}

/// The bytes of a fixture, with LF line ends.
fn fixture(name: &str) -> Vec<u8> {
    let bytes: &[u8] = match name {
        "text_stream.sse" => include_bytes!("fixtures/ai_wire_gemini/text_stream.sse"),
        "tool_stream.sse" => include_bytes!("fixtures/ai_wire_gemini/tool_stream.sse"),
        other => panic!("no fixture called {other}"),
    };
    lf(bytes)
}

/// The events of a stream, each with its terminating empty line.
fn events_of(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut events = Vec::new();
    let mut start = 0;
    let mut at = 0;
    while at + 1 < bytes.len() {
        if bytes[at] == b'\n' && bytes[at + 1] == b'\n' {
            events.push(bytes[start..at + 2].to_vec());
            start = at + 2;
            at += 2;
        } else {
            at += 1;
        }
    }
    if start < bytes.len() {
        events.push(bytes[start..].to_vec());
    }
    events
}

/// A reply that sends the events of a fixture one chunk each, 20 ms apart.
fn sse_reply(name: &str) -> Reply {
    Reply::sse_bytes(events_of(&fixture(name))).gap(Duration::from_millis(20))
}

/// A reply that sends each of the given JSON texts as one event.
fn inline_reply(events: &[&str]) -> Reply {
    Reply::sse_bytes(events.iter().map(|event| format!("data: {event}\n\n").into_bytes()).collect())
}

/// An endpoint on the fake server: the base path of the profile, the key in the header of the dialect, bound to the
/// origin of the server.
fn endpoint(fake: &FakeHttp, base_path: &str) -> Endpoint {
    let target = Target::parse(&format!("{}{base_path}", fake.base()), false).expect("a loopback base address");
    let secret = SecretHeader::new(API_KEY_HEADER, "", ApiKey::new(CANARY_KEY.to_owned()), target.origin().clone())
        .expect("a key header");
    Endpoint::new(Wire::Gemini, target, NetworkPolicy::new(false, false)).with_secret(secret)
}

/// A provider on the fake server, and the client it uses (to read its attempt counter).
fn provider(fake: &FakeHttp) -> (Arc<dyn Provider>, Arc<Http>) {
    let standard = HttpConfig::standard();
    let config =
        HttpConfig { retry: RetryPolicy { base_delay: Duration::from_millis(5), ..standard.retry }, ..standard };
    let http = Arc::new(Http::new(config));
    let provider =
        HttpProvider::new("gemini-test", endpoint(fake, "/v1beta"), Box::new(GeminiAdapter::new()), http.clone());
    (Arc::new(provider), http)
}

fn read_file_spec() -> ToolSpec {
    ToolSpec {
        name: "read_file".to_owned(),
        description: "Read a text file".to_owned(),
        input_schema: read_file_schema(),
    }
}

fn read_file_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": ["path"],
        "additionalProperties": false
    })
}

/// A request for `model` holding `messages`.
fn request(model: &str, messages: Vec<Message>) -> ChatRequest {
    let mut request = ChatRequest::new(model);
    request.messages = messages;
    request
}

/// What one turn through a provider delivered. `log` holds `text:<s>`, `reasoning:<s>`, `call:<name>` and
/// `usage:<in>/<out>` entries in arrival order.
struct Run {
    log: Vec<String>,
    collector: Collector,
    result: Result<Completion, AiError>,
}

impl Run {
    fn completion(&self) -> &Completion {
        self.result.as_ref().unwrap_or_else(|error| panic!("the turn failed: {error:?} after {:?}", self.log))
    }
}

fn log_entry(event: &StreamEvent) -> String {
    match event {
        StreamEvent::TextDelta(text) => format!("text:{text}"),
        StreamEvent::ReasoningDelta(text) => format!("reasoning:{text}"),
        StreamEvent::ToolCall(call) => format!("call:{}", call.name),
        StreamEvent::Usage(usage) => format!("usage:{}/{}", usage.input_tokens, usage.output_tokens),
    }
}

fn run(provider: &dyn Provider, request: &ChatRequest) -> Run {
    let mut collector = Collector::new();
    let mut log = Vec::new();
    let result = provider.stream(
        request,
        &mut |event| {
            log.push(log_entry(&event));
            collector.push(event)
        },
        &CancelToken::new(),
    );
    Run { log, collector, result }
}

fn json_of(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("JSON")
}

fn caps(model: &str) -> holzwave_core::ai::types::Capabilities {
    GeminiAdapter::new().capabilities(model, Locality::Cloud)
}

fn tool_message(id: &str, call_id: &str, content: &str) -> Message {
    Message::new(
        id,
        Role::Tool,
        vec![Part::ToolResult(ToolResult {
            call_id: call_id.to_owned(),
            content: content.to_owned(),
            is_error: false,
        })],
        3,
    )
}

// ---- the tracer -------------------------------------------------------------------------------------------

#[test]
fn a_text_turn_and_a_tool_turn_with_a_signature_echo_run_through_the_provider() {
    let done = r#"{"candidates":[{"content":{"parts":[{"text":"Done."}],"role":"model"},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":80,"candidatesTokenCount":2,"totalTokenCount":82}}"#;
    let fake = FakeHttp::start(vec![sse_reply("text_stream.sse"), sse_reply("tool_stream.sse"), inline_reply(&[done])]);
    let (provider, _http) = provider(&fake);

    // Turn 1: a text turn. The deltas arrive chunk by chunk, usage and the stop reason come from the last chunk.
    let mut first = request("gemini-test", vec![Message::user_text("u1", "hello", 1)]);
    first.system = "be brief".to_owned();
    first.max_output_tokens = Some(256);
    let text_turn = run(provider.as_ref(), &first);
    assert_eq!(
        text_turn.log,
        vec!["text:Hel", "usage:9/0", "text:lo \u{e9} \u{65e5}\u{672c} \u{1f600}", "usage:9/4", "text:!", "usage:9/5"],
        "text turn, closure log"
    );
    assert_eq!(text_turn.collector.text(), "Hello \u{e9} \u{65e5}\u{672c} \u{1f600}!");
    let completion = text_turn.completion();
    assert_eq!(completion.stop_reason, StopReason::EndTurn);
    assert_eq!(completion.usage, Usage { input_tokens: 9, output_tokens: 5 });
    let native = completion.native.as_ref().expect("a native turn");
    assert_eq!(native.wire, "gemini");
    assert_eq!(
        native.json,
        json!({ "role": "model", "parts": [{ "text": "Hello \u{e9} \u{65e5}\u{672c} \u{1f600}!" }] }),
        "the three text parts are joined"
    );
    let seen = fake.seen();
    assert_eq!(seen.len(), 1, "one request for the first turn");
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].target, "/v1beta/models/gemini-test:streamGenerateContent?alt=sse");
    assert_eq!(seen[0].header("content-type"), Some("application/json"));
    assert_eq!(seen[0].header("accept"), Some("text/event-stream"));
    assert_eq!(seen[0].header("x-goog-api-key"), Some(CANARY_KEY));
    assert_eq!(
        json_of(&seen[0].body),
        json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hello" }] }],
            "systemInstruction": { "parts": [{ "text": "be brief" }] },
            "generationConfig": { "maxOutputTokens": 256 },
        }),
        "first request body"
    );

    // Turn 2: a tool turn. The call arrives last, after the whole stream, and carries the signature in the native turn.
    let mut second = request("gemini-test", vec![Message::user_text("u2", "read notes/a b.txt", 1)]);
    second.tools.push(read_file_spec());
    let tool_turn = run(provider.as_ref(), &second);
    assert_eq!(
        tool_turn.log,
        vec!["text:I will read the file.", "usage:42/0", "usage:42/68", "call:read_file"],
        "tool turn, closure log"
    );
    let completion = tool_turn.completion().clone();
    assert_eq!(completion.stop_reason, StopReason::ToolUse);
    assert_eq!(
        completion.usage,
        Usage { input_tokens: 42, output_tokens: 68 },
        "68 is 11 candidate and 57 thought tokens"
    );
    let call = tool_turn.collector.tool_calls()[0].clone();
    assert!(call.id.starts_with("call_"), "the server sent no id, so one was minted: {}", call.id);
    assert_eq!(call.args, json!({ "path": "notes/a b.txt" }));
    let native = completion.native.clone().expect("a native turn");
    let native_parts = json!([
        { "text": "I will read the file." },
        {
            "functionCall": { "name": "read_file", "args": { "path": "notes/a b.txt" } },
            "thoughtSignature": TOOL_SIGNATURE,
        },
    ]);
    assert_eq!(native.json, json!({ "role": "model", "parts": native_parts.clone() }));
    let seen = fake.seen();
    assert_eq!(
        json_of(&seen[1].body),
        json!({
            "contents": [{ "role": "user", "parts": [{ "text": "read notes/a b.txt" }] }],
            "tools": [{ "functionDeclarations": [{
                "name": "read_file",
                "description": "Read a text file",
                "parametersJsonSchema": read_file_schema(),
            }] }],
        }),
        "second request body"
    );

    // Turn 3: the echo. The model's own content goes back byte for byte, followed by the function response.
    let assistant = tool_turn.collector.into_message("m2", 2, "gemini-test", MessageStatus::Complete, Some(completion));
    let third = {
        let mut third = request(
            "gemini-test",
            vec![
                Message::user_text("u2", "read notes/a b.txt", 1),
                assistant,
                tool_message("m3", &call.id, "file text"),
            ],
        );
        third.tools.push(read_file_spec());
        third
    };
    let answer = run(provider.as_ref(), &third);
    assert_eq!(answer.log, vec!["text:Done.", "usage:80/2"], "echo turn, closure log");
    assert_eq!(answer.completion().stop_reason, StopReason::EndTurn);
    assert_eq!(fake.accepted(), 3, "exactly three connections");
    let seen = fake.seen();
    let body = json_of(&seen[2].body);
    assert_eq!(
        body["contents"],
        json!([
            { "role": "user", "parts": [{ "text": "read notes/a b.txt" }] },
            { "role": "model", "parts": native_parts },
            { "role": "user", "parts": [{ "functionResponse": { "name": "read_file", "response": { "result": "file text" } } }] },
        ]),
        "third request contents"
    );
    assert_eq!(seen[2].body_text().matches(TOOL_SIGNATURE).count(), 1, "the signature occurs exactly once");
    println!(
        "tracer: a_text_turn_and_a_tool_turn_with_a_signature_echo_run_through_the_provider ok, text {:?}, tool {:?}, echo {:?}",
        text_turn.log.len(),
        tool_turn.log.len(),
        answer.log
    );
}

// ---- the key ----------------------------------------------------------------------------------------------

#[test]
fn the_key_travels_only_in_its_header_never_in_the_address_or_body() {
    let done =
        r#"{"candidates":[{"content":{"parts":[{"text":"ok"}],"role":"model"},"finishReason":"STOP","index":0}]}"#;
    let fake = FakeHttp::start(vec![sse_reply("text_stream.sse"), sse_reply("tool_stream.sse"), inline_reply(&[done])]);
    let (provider, http) = provider(&fake);

    let first = request("gemini-test", vec![Message::user_text("u1", "hello", 1)]);
    run(provider.as_ref(), &first).completion();
    let mut second = request("models/gemini-test", vec![Message::user_text("u2", "read notes/a b.txt", 1)]);
    second.tools.push(read_file_spec());
    let tool_turn = run(provider.as_ref(), &second);
    let completion = tool_turn.completion().clone();
    let call_id = tool_turn.collector.tool_calls()[0].id.clone();
    let assistant = tool_turn.collector.into_message("m2", 2, "gemini-test", MessageStatus::Complete, Some(completion));
    let mut third = request(
        "gemini-test",
        vec![Message::user_text("u2", "read notes/a b.txt", 1), assistant, tool_message("m3", &call_id, "file text")],
    );
    third.tools.push(read_file_spec());
    run(provider.as_ref(), &third).completion();

    let seen = fake.seen();
    assert_eq!(seen.len(), 3);
    for (index, request) in seen.iter().enumerate() {
        assert!(!request.target.contains(CANARY_KEY), "request {index}: the key is in the address");
        assert!(!request.target.contains("key="), "request {index}: a key parameter is in the address");
        assert!(!request.body_text().contains(CANARY_KEY), "request {index}: the key is in the body");
        assert!(!request.body_text().contains("key="), "request {index}: a key parameter is in the body");
        let named: Vec<&str> =
            request.headers.iter().map(|(name, _)| name.as_str()).filter(|n| *n == API_KEY_HEADER).collect();
        assert_eq!(named.len(), 1, "request {index}: the key header is sent once");
        assert_eq!(request.header(API_KEY_HEADER), Some(CANARY_KEY), "request {index}: the exact value");
        for (name, _) in &request.headers {
            let credential = ["auth", "key", "token", "secret", "password", "cookie"].iter().any(|f| name.contains(f));
            assert!(!credential || name == API_KEY_HEADER, "request {index}: another credential header {name}");
        }
    }

    // The adapter itself puts no header on a request.
    let adapter = GeminiAdapter::new();
    let wire = adapter.request(&first, &caps("gemini-test")).expect("a request");
    assert!(wire.headers.is_empty(), "the adapter's request carries no header");
    assert_eq!(format!("{adapter:?}"), "GeminiAdapter");

    // A model id equal to the key is refused by the chokepoint before any connection.
    let before = fake.accepted();
    let attempts = http.attempts();
    let hostile = request(CANARY_KEY, vec![Message::user_text("u1", "hello", 1)]);
    let result = run(provider.as_ref(), &hostile).result;
    let error = result.expect_err("a model id equal to the key is refused");
    assert_eq!(error.class(), ErrorClass::Policy, "{error:?}");
    assert!(!error.to_string().contains(CANARY_KEY) && !format!("{error:?}").contains(CANARY_KEY));
    assert_eq!(fake.accepted(), before, "no connection was opened");
    assert_eq!(http.attempts(), attempts, "nothing was attempted");
}

// ---- the model id -----------------------------------------------------------------------------------------

#[test]
fn the_model_id_is_checked_before_it_reaches_the_address() {
    let long_ok = format!("m{}", "a".repeat(MAX_MODEL_ID - 1));
    assert_eq!(long_ok.len(), 64);
    let accepted: Vec<(String, String)> = vec![
        ("gemini-3.8-flash".to_owned(), "gemini-3.8-flash".to_owned()),
        ("models/gemini-4-argon".to_owned(), "gemini-4-argon".to_owned()),
        ("gemini-2.5-pro-preview-06-05".to_owned(), "gemini-2.5-pro-preview-06-05".to_owned()),
        ("a".to_owned(), "a".to_owned()),
        ("gemini_x.1".to_owned(), "gemini_x.1".to_owned()),
        (long_ok.clone(), long_ok),
    ];
    for (model, bare) in &accepted {
        let path = stream_path(model).unwrap_or_else(|error| panic!("accepted row {model:?}: {error:?}"));
        assert_eq!(path, format!("/models/{bare}:streamGenerateContent?alt=sse"), "accepted row {model:?}");
    }

    let too_long = "a".repeat(MAX_MODEL_ID + 1);
    let refused: Vec<(&str, String)> = vec![
        ("the empty string", String::new()),
        ("the prefix alone", "models/".to_owned()),
        ("a doubled prefix", "models/models/x".to_owned()),
        ("a parent segment", "../x".to_owned()),
        ("a slash", "a/b".to_owned()),
        ("a query", "a?b".to_owned()),
        ("a fragment", "a#b".to_owned()),
        ("a colon", "a:b".to_owned()),
        ("a space", "a b".to_owned()),
        ("an encoded dot", "a%2e".to_owned()),
        ("a backslash", "a\\b".to_owned()),
        ("a leading dash", "-a".to_owned()),
        ("a leading dot", ".a".to_owned()),
        ("two dots", "a..b".to_owned()),
        ("a non-ASCII letter", "gemini-\u{e9}".to_owned()),
        ("65 characters", too_long),
        ("a trailing line feed", "x\n".to_owned()),
        ("a tuned-model prefix", "tunedModels/x".to_owned()),
    ];
    for (row, model) in &refused {
        let error = stream_path(model).expect_err(row);
        assert!(matches!(error, AiError::InvalidRequest(_)), "refused row {row}: {error:?}");
        let adapter_error = GeminiAdapter::new()
            .request(&request(model, vec![Message::user_text("u1", "hi", 1)]), &caps("gemini-test"))
            .expect_err(row);
        assert_eq!(adapter_error.class(), ErrorClass::InvalidRequest, "refused row {row} through the adapter");
    }

    let hostile = stream_path("x?key=INPUT-TEXT").expect_err("a hostile id");
    assert!(!hostile.to_string().contains("INPUT-TEXT"), "the phrase does not echo the input: {hostile}");
    assert!(!format!("{hostile:?}").contains("INPUT-TEXT"));
}

// ---- the capabilities -------------------------------------------------------------------------------------

#[test]
fn the_adapter_names_its_wire_and_describes_its_capabilities() {
    let adapter = GeminiAdapter::new();
    assert_eq!(adapter.wire(), Wire::Gemini);
    assert_eq!(adapter.wire().tag(), "gemini");
    assert_eq!(API_KEY_HEADER, "x-goog-api-key");
    for locality in [Locality::Cloud, Locality::Lan] {
        for model in ["gemini-3.8-flash", "models/gemini-3.8-flash"] {
            let caps = adapter.capabilities(model, locality);
            assert!(caps.tools, "{model}: tools");
            assert!(!caps.reasoning, "{model}: reasoning");
            assert!(!caps.vision, "{model}: vision");
            assert_eq!(caps.context_tokens, Some(1_048_576));
            assert_eq!(caps.context_tokens, Some(CONTEXT_TOKENS));
            assert_eq!(caps.max_output_tokens, Some(8192));
            assert_eq!(caps.max_output_tokens, Some(OUTPUT_TOKENS));
            assert_eq!(caps.locality, locality, "{model}: the locality that was passed in");
        }
    }
    assert!(!adapter.capabilities("gemma-3-27b-it", Locality::Cloud).tools, "gemma takes no tools");
    assert!(!adapter.capabilities("models/gemma-3-27b-it", Locality::Cloud).tools, "gemma with a prefix");
}

// ---- request helpers --------------------------------------------------------------------------------------

/// Signatures used by the replay tests. Letters only, so they are valid in the base64 alphabet.
const SIG_ONE: &str = "SIGNATUREONEaaaa";
const SIG_TWO: &str = "SIGNATURETWObbbb";
const SIG_THREE: &str = "SIGNATURETHREEcccc";

/// A marker that must never reach a request body when it sits in a part that is dropped.
const MARKER: &str = "TAMPERED-MARKER";

/// The request the adapter builds, for a model that takes tools.
fn build(req: &ChatRequest) -> Result<holzwave_core::ai::wire::WireRequest, AiError> {
    GeminiAdapter::new().request(req, &caps("gemini-test"))
}

/// The body the adapter builds, as JSON.
fn body_of(req: &ChatRequest) -> Value {
    json_of(&build(req).expect("a request").body)
}

/// The `contents` the adapter builds for `messages`.
fn contents_of(messages: Vec<Message>) -> Vec<Value> {
    body_of(&request("gemini-test", messages))["contents"].as_array().expect("contents").clone()
}

/// The error the adapter builds for `req`, which must be an invalid request.
fn refusal(req: &ChatRequest, row: &str) -> AiError {
    let error = build(req).expect_err(row);
    assert_eq!(error.class(), ErrorClass::InvalidRequest, "{row}: {error:?}");
    error
}

fn user(id: &str, text: &str) -> Message {
    Message::user_text(id, text, 1)
}

fn text_part(text: &str) -> Part {
    Part::Text { text: text.to_owned() }
}

fn call_part(id: &str, name: &str, args: Value) -> Part {
    Part::ToolCall(ToolCall { id: id.to_owned(), name: name.to_owned(), args })
}

fn assistant(id: &str, parts: Vec<Part>, native: Option<Value>) -> Message {
    let mut message = Message::new(id, Role::Assistant, parts, 2);
    message.native = native.map(|json| Opaque { wire: "gemini".to_owned(), json });
    message
}

fn result_message(id: &str, call_id: &str, content: &str, is_error: bool) -> Message {
    Message::new(
        id,
        Role::Tool,
        vec![Part::ToolResult(ToolResult { call_id: call_id.to_owned(), content: content.to_owned(), is_error })],
        3,
    )
}

fn tool(name: &str, description: &str, schema: Value) -> ToolSpec {
    ToolSpec { name: name.to_owned(), description: description.to_owned(), input_schema: schema }
}

/// An object schema whose serialised size is exactly `total` bytes.
fn schema_of_size(total: usize) -> Value {
    let base = serde_json::to_vec(&json!({ "type": "object", "description": "" })).expect("JSON").len();
    let schema = json!({ "type": "object", "description": "x".repeat(total - base) });
    assert_eq!(serde_json::to_vec(&schema).expect("JSON").len(), total);
    schema
}

/// The sorted member names of an object.
fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value.as_object().expect("an object").keys().cloned().collect();
    keys.sort();
    keys
}

/// A context approved for `provider` through the public gate, with one screen item.
fn approved_context(provider: &str, label: &str, text: &str) -> ApprovedContext {
    let gate = Gate::new();
    let destination = Destination { host: "generativelanguage.example.test".to_owned(), locality: Locality::Cloud };
    let item = RequestItem::new(ContextKind::Screen, label, SourceText::plain(text));
    let offer = gate.offer(OfferRequest::new(provider, destination, vec![item])).expect("an offer");
    gate.approve(offer.id(), &[1], provider).expect("an approval")
}

/// `text` as it is written inside a JSON string.
fn escaped(text: &str) -> String {
    let json = serde_json::to_string(text).expect("JSON");
    json[1..json.len() - 1].to_owned()
}

/// A note that names no approval of any request.
fn history_note(items: Vec<NoteItem>, stored: Option<&str>) -> ContextNote {
    ContextNote {
        hash: "0".repeat(64),
        items,
        stored: stored.map(str::to_owned),
        consented_to: Consent { provider: "gemini-test".to_owned(), locality: Locality::Cloud },
    }
}

fn note_item(kind: ContextKind, label: &str, bytes: u64) -> NoteItem {
    NoteItem { kind, label: label.to_owned(), bytes }
}

const FORBIDDEN_FIELDS: [&str; 15] = [
    "temperature",
    "topP",
    "topK",
    "safetySettings",
    "toolConfig",
    "thinkingConfig",
    "cachedContent",
    "candidateCount",
    "stopSequences",
    "responseMimeType",
    "labels",
    "googleSearch",
    "codeExecution",
    "urlContext",
    "fileSearch",
];

// ---- the request: body shape ------------------------------------------------------------------------------

#[test]
fn system_text_limit_and_tools_are_encoded_and_nothing_else_is_sent() {
    for with_system in [false, true] {
        for with_tools in [false, true] {
            for limit in [None, Some(256)] {
                let row = format!("system {with_system}, tools {with_tools}, limit {limit:?}");
                let mut req = request("gemini-test", vec![user("u1", "hello")]);
                if with_system {
                    req.system = "be brief".to_owned();
                }
                if with_tools {
                    req.tools.push(read_file_spec());
                }
                req.max_output_tokens = limit;
                let wire = build(&req).expect(&row);
                let body = json_of(&wire.body);
                let mut want = vec!["contents"];
                if with_system {
                    want.push("systemInstruction");
                }
                if with_tools {
                    want.push("tools");
                }
                if limit.is_some() {
                    want.push("generationConfig");
                }
                want.sort_unstable();
                assert_eq!(keys(&body), want, "top-level keys, {row}");
                let text = String::from_utf8(wire.body).expect("UTF-8");
                for field in FORBIDDEN_FIELDS {
                    assert!(!text.contains(field), "{row}: the body names {field}");
                }
            }
        }
    }

    // The members themselves.
    let mut req = request("gemini-test", vec![user("u1", "hello")]);
    req.system = "be brief".to_owned();
    req.max_output_tokens = Some(256);
    req.tools.push(read_file_spec());
    req.tools.push(tool("list_dir", "List a directory", json!({ "type": "object", "properties": {} })));
    let body = body_of(&req);
    assert_eq!(body["systemInstruction"], json!({ "parts": [{ "text": "be brief" }] }));
    assert_eq!(keys(&body["generationConfig"]), vec!["maxOutputTokens"], "generationConfig holds one member");
    let tools = body["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 1, "one tools entry");
    assert_eq!(keys(&tools[0]), vec!["functionDeclarations"]);
    let declarations = tools[0]["functionDeclarations"].as_array().expect("declarations");
    assert_eq!(declarations.len(), 2, "two tools, two declarations in order");
    assert_eq!(declarations[0]["name"], "read_file");
    assert_eq!(declarations[1]["name"], "list_dir");
    assert_eq!(declarations[0]["parametersJsonSchema"], read_file_schema(), "the schema is passed through unchanged");

    // The limit is clamped to the capability, zero is refused, none sends nothing.
    for (limit, sent) in [(100_000, 8192), (1, 1), (8192, 8192), (8193, 8192)] {
        let mut req = request("gemini-test", vec![user("u1", "hello")]);
        req.max_output_tokens = Some(limit);
        assert_eq!(body_of(&req)["generationConfig"]["maxOutputTokens"], json!(sent), "limit {limit}");
    }
    let mut zero = request("gemini-test", vec![user("u1", "hello")]);
    zero.max_output_tokens = Some(0);
    refusal(&zero, "a zero limit");
    assert!(body_of(&request("gemini-test", vec![user("u1", "hello")])).get("generationConfig").is_none());

    // A model that takes no tools gets none, and an empty list sends none.
    let mut gemma = request("gemma-3-27b-it", vec![user("u1", "hello")]);
    gemma.tools.push(read_file_spec());
    let wire = GeminiAdapter::new().request(&gemma, &caps("gemma-3-27b-it")).expect("a request");
    assert!(json_of(&wire.body).get("tools").is_none(), "no tools for a model that cannot use them");
    let mut empty = request("gemini-test", vec![user("u1", "hello")]);
    empty.tools.clear();
    assert!(body_of(&empty).get("tools").is_none(), "an empty list sends no tools");
}

#[test]
fn tool_declarations_are_validated_before_anything_is_sent() {
    let ok_schema = read_file_schema;
    let long_name = "n".repeat(128);
    let too_long_name = "n".repeat(129);
    let description_ok = "d".repeat(4096);
    let description_long = "d".repeat(4097);
    let many = |count: usize| -> Vec<ToolSpec> {
        (0..count).map(|n| tool(&format!("tool_{n}"), "A tool", ok_schema())).collect()
    };
    let rows: Vec<(&str, Vec<ToolSpec>, bool)> = vec![
        ("an empty name", vec![tool("", "d", ok_schema())], false),
        ("a 129-character name", vec![tool(&too_long_name, "d", ok_schema())], false),
        ("a 128-character name", vec![tool(&long_name, "d", ok_schema())], true),
        ("a name starting with a digit", vec![tool("1tool", "d", ok_schema())], false),
        ("a name with a space", vec![tool("a b", "d", ok_schema())], false),
        ("a name with a dot", vec![tool("a.b", "d", ok_schema())], false),
        ("a name with a slash", vec![tool("a/b", "d", ok_schema())], false),
        ("a name starting with a dash", vec![tool("-a", "d", ok_schema())], false),
        ("a name starting with an underscore", vec![tool("_a-b_c", "d", ok_schema())], true),
        ("two tools with one name", vec![tool("a", "d", ok_schema()), tool("a", "e", ok_schema())], false),
        ("an empty description", vec![tool("a", "", ok_schema())], false),
        ("a 4097-byte description", vec![tool("a", &description_long, ok_schema())], false),
        ("a 4096-byte description", vec![tool("a", &description_ok, ok_schema())], true),
        ("an array schema", vec![tool("a", "d", json!([]))], false),
        ("a string schema", vec![tool("a", "d", json!("object"))], false),
        ("a null schema", vec![tool("a", "d", Value::Null)], false),
        ("an object schema without a type", vec![tool("a", "d", json!({ "properties": {} }))], false),
        ("an object schema of type string", vec![tool("a", "d", json!({ "type": "string" }))], false),
        ("a schema of 32769 bytes", vec![tool("a", "d", schema_of_size(32_769))], false),
        ("a schema of 32768 bytes", vec![tool("a", "d", schema_of_size(32_768))], true),
        ("65 tools", many(65), false),
        ("64 tools", many(64), true),
    ];
    for (row, tools, allowed) in rows {
        let mut req = request("gemini-test", vec![user("u1", "hello")]);
        req.tools = tools;
        match (build(&req), allowed) {
            (Ok(_), true) => {}
            (Err(error), false) => assert_eq!(error.class(), ErrorClass::InvalidRequest, "{row}: {error:?}"),
            (Ok(_), false) => panic!("{row}: the declaration was accepted"),
            (Err(error), true) => panic!("{row}: the declaration was refused: {error:?}"),
        }
    }

    // The refusal comes before any connection, and the hostile name is not echoed.
    let fake = FakeHttp::start(vec![Reply::Close]);
    let (provider, http) = provider(&fake);
    let mut hostile = request("gemini-test", vec![user("u1", "hello")]);
    hostile.tools.push(tool("x\"; DROP", "d", ok_schema()));
    let error = run(provider.as_ref(), &hostile).result.expect_err("a hostile tool name");
    assert_eq!(error.class(), ErrorClass::InvalidRequest, "{error:?}");
    assert!(!error.to_string().contains("DROP") && !format!("{error:?}").contains("DROP"), "the name is not echoed");
    assert_eq!(fake.accepted(), 0, "no connection was opened");
    assert_eq!(http.attempts(), 0, "nothing was attempted");
}

// ---- the request: contents --------------------------------------------------------------------------------

#[test]
fn roles_map_adjacent_messages_merge_and_empty_ones_vanish() {
    let user_content = |texts: &[&str]| json!({ "role": "user", "parts": texts.iter().map(|text| json!({ "text": text })).collect::<Vec<Value>>() });
    let model_content = |texts: &[&str]| json!({ "role": "model", "parts": texts.iter().map(|text| json!({ "text": text })).collect::<Vec<Value>>() });
    let rows: Vec<(&str, Vec<Message>, Vec<Value>)> = vec![
        ("two user messages are one content", vec![user("u1", "a"), user("u2", "b")], vec![user_content(&["a", "b"])]),
        (
            "two assistant messages are one content",
            vec![
                user("u1", "q"),
                assistant("a1", vec![text_part("a1")], None),
                assistant("a2", vec![text_part("a2")], None),
                user("u2", "next"),
            ],
            vec![user_content(&["q"]), model_content(&["a1", "a2"]), user_content(&["next"])],
        ),
        (
            "an empty assistant message vanishes and the user messages around it merge",
            vec![user("u1", "a"), assistant("a1", vec![], None), user("u2", "b")],
            vec![user_content(&["a", "b"])],
        ),
        (
            "an assistant message with only an empty text is skipped",
            vec![user("u1", "a"), assistant("a1", vec![text_part("")], None), user("u2", "b")],
            vec![user_content(&["a", "b"])],
        ),
        (
            "empty text parts inside a message are dropped",
            vec![Message::new("u1", Role::User, vec![text_part(""), text_part("x"), text_part("")], 1)],
            vec![user_content(&["x"])],
        ),
        (
            "a tool message is the user role and merges with the user message after it",
            vec![
                user("u1", "q"),
                assistant("a1", vec![call_part("c1", "read_file", json!({ "path": "p" }))], None),
                result_message("t1", "c1", "r", false),
                user("u2", "thanks"),
            ],
            vec![
                user_content(&["q"]),
                json!({ "role": "model", "parts": [{ "functionCall": { "name": "read_file", "args": { "path": "p" } } }] }),
                json!({ "role": "user", "parts": [
                    { "functionResponse": { "name": "read_file", "response": { "result": "r" } } },
                    { "text": "thanks" },
                ] }),
            ],
        ),
    ];
    for (row, messages, want) in rows {
        assert_eq!(contents_of(messages), want, "{row}");
    }
}

// ---- the request: native replay ---------------------------------------------------------------------------

/// The stored turn of the replay tests: text with a signature, a call with an id and a signature, and an empty text
/// part that holds a signature.
fn base_blob() -> Value {
    json!({
        "role": "model",
        "parts": [
            { "text": "A", "thoughtSignature": SIG_ONE },
            { "functionCall": { "name": "read_file", "args": { "path": "p" }, "id": "fc-1" }, "thoughtSignature": SIG_TWO },
            { "text": "", "thoughtSignature": SIG_THREE },
        ],
    })
}

/// A conversation whose assistant message carries `native`: the message holds the text `A` and one `read_file` call.
fn replay_conversation(native: Option<Opaque>) -> ChatRequest {
    let mut message =
        assistant("a1", vec![text_part("A"), call_part("call-x", "read_file", json!({ "path": "p" }))], None);
    message.native = native;
    request("gemini-test", vec![user("u1", "q"), message, result_message("t1", "call-x", "ok", false)])
}

fn gemini_blob(json: Value) -> Option<Opaque> {
    Some(Opaque { wire: "gemini".to_owned(), json })
}

/// The parts of an assistant message that has no stored turn.
fn rebuilt_parts() -> Value {
    json!([{ "text": "A" }, { "functionCall": { "name": "read_file", "args": { "path": "p" } } }])
}

#[test]
fn a_native_turn_is_replayed_verbatim_to_gemini_only() {
    // A good blob is replayed part for part, each signature once, and the call's own id goes on the response.
    let blob = base_blob();
    let wire = build(&replay_conversation(gemini_blob(blob.clone()))).expect("a request");
    let text = String::from_utf8(wire.body.clone()).expect("UTF-8");
    let contents = json_of(&wire.body)["contents"].clone();
    assert_eq!(contents[1], json!({ "role": "model", "parts": blob["parts"] }), "the blob's parts, unchanged");
    assert_eq!(
        contents[2],
        json!({ "role": "user", "parts": [{ "functionResponse": { "name": "read_file", "id": "fc-1", "response": { "result": "ok" } } }] }),
        "the response carries the provider's id"
    );
    for signature in [SIG_ONE, SIG_TWO, SIG_THREE] {
        assert_eq!(text.matches(signature).count(), 1, "{signature} occurs once");
    }

    // A signature of 262144 characters passes.
    let mut big = base_blob();
    big["parts"][0]["thoughtSignature"] = json!("A".repeat(262_144));
    let contents = body_of(&replay_conversation(gemini_blob(big.clone())))["contents"].clone();
    assert_eq!(contents[1]["parts"], big["parts"], "a signature of 262144 characters is replayed");

    // A blob of another wire is not offered: the turn is rebuilt, with no id and no signature anywhere.
    let foreign = Some(Opaque { wire: "anthropic".to_owned(), json: base_blob() });
    let wire = build(&replay_conversation(foreign)).expect("a request");
    let text = String::from_utf8(wire.body.clone()).expect("UTF-8");
    let contents = json_of(&wire.body)["contents"].clone();
    assert_eq!(contents[1]["parts"], rebuilt_parts(), "the other wire's blob is dropped");
    assert!(!text.contains("thoughtSignature") && !text.contains("\"id\""), "nothing of the blob is left: {text}");
    assert!(!text.contains(SIG_ONE) && !text.contains(SIG_TWO) && !text.contains(SIG_THREE));

    // Each tampered blob is dropped and the turn rebuilt, and the marker never appears.
    let mut rows: Vec<(&str, Value)> = Vec::new();
    let mut row = |name: &'static str, edit: &dyn Fn(&mut Value)| {
        let mut blob = base_blob();
        edit(&mut blob);
        rows.push((name, blob));
    };
    row("an extra key fileData on a part", &|b| b["parts"][0]["fileData"] = json!({ "fileUri": MARKER }));
    row("a part with inlineData", &|b| b["parts"][2] = json!({ "inlineData": { "data": MARKER } }));
    row("a functionResponse part in the model content", &|b| {
        b["parts"]
            .as_array_mut()
            .expect("parts")
            .push(json!({ "functionResponse": { "name": MARKER, "response": {} } }));
    });
    row("a signature with a space", &|b| b["parts"][0]["thoughtSignature"] = json!(format!("{MARKER} x")));
    row("a signature of 262145 characters", &|b| b["parts"][0]["thoughtSignature"] = json!("A".repeat(262_145)));
    row("a call whose name differs from the message", &|b| b["parts"][1]["functionCall"]["name"] = json!("list_dir"));
    row("two calls in the blob and one in the message", &|b| {
        b["parts"]
            .as_array_mut()
            .expect("parts")
            .push(json!({ "functionCall": { "name": "read_file", "args": { "marker": MARKER } } }));
    });
    row("parts that is not an array", &|b| b["parts"] = json!({ "text": MARKER }));
    row("parts that is empty", &|b| b["parts"] = json!([]));
    row("a role of user", &|b| b["role"] = json!("user"));
    row("a part with both text and functionCall", &|b| b["parts"][1]["text"] = json!(MARKER));
    row("a blob larger than 4 MiB", &|b| b["parts"][0]["text"] = json!("x".repeat(4 * 1024 * 1024)));
    row("a thought flag that is not a boolean", &|b| b["parts"][0]["thought"] = json!(MARKER));
    row("a text that is not a string", &|b| b["parts"][0]["text"] = json!({ "a": MARKER }));
    row("an extra key on the call", &|b| b["parts"][1]["functionCall"]["fileUri"] = json!(MARKER));
    row("call arguments that are not an object", &|b| b["parts"][1]["functionCall"]["args"] = json!(MARKER));
    row("a call id that is not a string", &|b| b["parts"][1]["functionCall"]["id"] = json!(7));
    row("a part with nothing in it", &|b| b["parts"][2] = json!({ "thought": true }));
    row("a blob that is not an object", &|b| *b = json!([MARKER]));
    for (name, blob) in rows {
        let wire = build(&replay_conversation(gemini_blob(blob))).expect(name);
        let text = String::from_utf8(wire.body.clone()).expect("UTF-8");
        let contents = json_of(&wire.body)["contents"].clone();
        assert_eq!(contents[1]["parts"], rebuilt_parts(), "{name}: the turn is rebuilt");
        assert_eq!(
            contents[2],
            json!({ "role": "user", "parts": [{ "functionResponse": { "name": "read_file", "response": { "result": "ok" } } }] }),
            "{name}: the response has no provider id"
        );
        assert!(
            !text.contains(MARKER) && !text.contains("thoughtSignature"),
            "{name}: the tampered blob reached the body"
        );
        assert!(text.len() < 10_000, "{name}: the body is small ({} bytes)", text.len());
    }
}

#[test]
fn assistant_turns_without_a_native_turn_are_rebuilt_from_text_and_calls() {
    let messages = vec![
        user("u1", "q"),
        assistant(
            "a1",
            vec![
                text_part("see"),
                call_part("c1", "read_file", json!({ "p": 1 })),
                call_part("c2", "list_dir", json!({})),
            ],
            None,
        ),
        Message::new(
            "t1",
            Role::Tool,
            vec![
                Part::ToolResult(ToolResult { call_id: "c1".to_owned(), content: "r1".to_owned(), is_error: false }),
                Part::ToolResult(ToolResult { call_id: "c2".to_owned(), content: "r2".to_owned(), is_error: false }),
            ],
            3,
        ),
    ];
    let wire = build(&request("gemini-test", messages)).expect("a request");
    let text = String::from_utf8(wire.body.clone()).expect("UTF-8");
    let contents = json_of(&wire.body)["contents"].clone();
    assert_eq!(
        contents[1]["parts"],
        json!([
            { "text": "see" },
            { "functionCall": { "name": "read_file", "args": { "p": 1 } } },
            { "functionCall": { "name": "list_dir", "args": {} } },
        ])
    );
    assert_eq!(
        contents[2]["parts"],
        json!([
            { "functionResponse": { "name": "read_file", "response": { "result": "r1" } } },
            { "functionResponse": { "name": "list_dir", "response": { "result": "r2" } } },
        ])
    );
    assert!(!text.contains("\"id\"") && !text.contains("thoughtSignature"), "no id and no signature: {text}");
}

#[test]
fn parallel_results_are_answered_in_call_order_in_one_content() {
    let blob = json!({
        "role": "model",
        "parts": [
            { "functionCall": { "name": "read_file", "args": { "path": "a" }, "id": "fc-1" }, "thoughtSignature": SIG_ONE },
            { "functionCall": { "name": "list_dir", "args": { "path": "." } } },
        ],
    });
    let messages = vec![
        user("u1", "q"),
        assistant(
            "a1",
            vec![
                call_part("fc-1", "read_file", json!({ "path": "a" })),
                call_part("minted-2", "list_dir", json!({ "path": "." })),
            ],
            Some(blob),
        ),
        // The results arrive in the opposite order of the calls.
        result_message("t2", "minted-2", "boom", true),
        result_message("t1", "fc-1", "r1", false),
    ];
    let contents = contents_of(messages);
    assert_eq!(contents.len(), 3, "user, model, one user content for both results");
    assert_eq!(
        contents[2],
        json!({ "role": "user", "parts": [
            { "functionResponse": { "name": "read_file", "id": "fc-1", "response": { "result": "r1" } } },
            { "functionResponse": { "name": "list_dir", "response": { "error": "boom" } } },
        ] }),
        "call order, the tool name, the provider id where there was one, failures under error"
    );
}

#[test]
fn tool_pairing_errors_are_refused_without_a_connection() {
    let canary = "PAIR-CANARY-91";
    let one_call = |id: &str| assistant("a1", vec![call_part(id, "read_file", json!({ "path": canary }))], None);
    let rows: Vec<(&str, Vec<Message>)> = vec![
        (
            "a result for a call of no reply",
            vec![user("u1", canary), one_call("c1"), result_message("t1", "c9", canary, false)],
        ),
        (
            "a result for a call two replies back",
            vec![
                user("u1", canary),
                one_call("c1"),
                result_message("t1", "c1", "r", false),
                assistant("a2", vec![text_part("done")], None),
                user("u2", "again"),
                result_message("t2", "c1", canary, false),
            ],
        ),
        (
            "a result given twice",
            vec![
                user("u1", canary),
                one_call("c1"),
                result_message("t1", "c1", "r", false),
                result_message("t2", "c1", canary, false),
            ],
        ),
        (
            "a reply with two calls and one result",
            vec![
                user("u1", canary),
                assistant(
                    "a1",
                    vec![call_part("c1", "read_file", json!({})), call_part("c2", "list_dir", json!({}))],
                    None,
                ),
                result_message("t1", "c1", canary, false),
            ],
        ),
        (
            "a tool message after a user message with no calls",
            vec![user("u1", canary), result_message("t1", "c1", canary, false)],
        ),
        (
            "a call followed by a user message and no result",
            vec![user("u1", canary), one_call("c1"), user("u2", "next")],
        ),
        (
            "a conversation that ends with an assistant message",
            vec![user("u1", canary), assistant("a1", vec![text_part(canary)], None)],
        ),
        (
            "a conversation that starts with an assistant message",
            vec![assistant("a1", vec![text_part(canary)], None), user("u1", canary)],
        ),
        ("no message at all", vec![]),
        ("messages that are all empty", vec![Message::new("u1", Role::User, vec![text_part("")], 1)]),
        (
            "a tool call part in a user message",
            vec![Message::new("u1", Role::User, vec![call_part("c1", "read_file", json!({ "p": canary }))], 1)],
        ),
        (
            "a tool result part in an assistant message",
            vec![
                user("u1", canary),
                Message::new(
                    "a1",
                    Role::Assistant,
                    vec![Part::ToolResult(ToolResult {
                        call_id: "c1".to_owned(),
                        content: canary.to_owned(),
                        is_error: false,
                    })],
                    2,
                ),
                user("u2", "x"),
            ],
        ),
        (
            "a text part in a tool message",
            vec![user("u1", canary), one_call("c1"), Message::new("t1", Role::Tool, vec![text_part(canary)], 3)],
        ),
        (
            "a context note in an assistant message",
            vec![
                user("u1", canary),
                Message::new("a1", Role::Assistant, vec![Part::Context(history_note(vec![], Some(canary)))], 2),
                user("u2", "x"),
            ],
        ),
        (
            "call arguments that are not an object",
            vec![
                user("u1", canary),
                assistant("a1", vec![call_part("c1", "read_file", json!(canary))], None),
                result_message("t1", "c1", "r", false),
            ],
        ),
    ];
    for (row, messages) in &rows {
        let error = refusal(&request("gemini-test", messages.clone()), row);
        let shown = format!("{error} {error:?}");
        assert!(!shown.contains(canary), "{row}: the error echoes the request: {shown}");
    }

    // Through the provider the first row opens no connection.
    let fake = FakeHttp::start(vec![Reply::Close]);
    let (provider, http) = provider(&fake);
    for (row, messages) in &rows {
        let error = run(provider.as_ref(), &request("gemini-test", messages.clone())).result.expect_err(row);
        assert_eq!(error.class(), ErrorClass::InvalidRequest, "{row} through the provider: {error:?}");
    }
    assert_eq!(fake.accepted(), 0, "no refused request opened a connection");
    assert_eq!(http.attempts(), 0);
}

// ---- the request: context ---------------------------------------------------------------------------------

#[test]
fn approved_context_is_rendered_from_the_envelope_and_stored_text_never_leaves() {
    let context = approved_context("gemini-test", "visible screen", "SCREEN-TEXT");
    let note = ContextNote::for_approved(&context, Some("STORED-TEXT".to_owned()));
    let mut req = request(
        "gemini-test",
        vec![Message::new("u1", Role::User, vec![Part::Context(note), text_part("explain this")], 1)],
    );
    req.context.push(context.clone());
    let wire = build(&req).expect("a request");
    let text = String::from_utf8(wire.body.clone()).expect("UTF-8");
    assert_eq!(
        json_of(&wire.body)["contents"],
        json!([{ "role": "user", "parts": [{ "text": context.envelope() }, { "text": "explain this" }] }]),
        "the envelope, then the text"
    );
    assert!(!text.contains("STORED-TEXT"), "the stored text never leaves");
    assert!(context.envelope().contains("SCREEN-TEXT"), "the envelope holds the approved text");

    // Through the provider the same request reaches the server.
    let fake = FakeHttp::start(vec![inline_reply(&[
        r#"{"candidates":[{"content":{"parts":[{"text":"ok"}],"role":"model"},"finishReason":"STOP","index":0}]}"#,
    ])]);
    let (provider, _http) = provider(&fake);
    run(provider.as_ref(), &req).completion();
    assert_eq!(fake.accepted(), 1);
    let sent = json_of(&fake.seen()[0].body);
    assert_eq!(sent["contents"][0]["parts"][0]["text"], json!(context.envelope()));
    assert!(!fake.seen()[0].body_text().contains("STORED-TEXT"));

    // A note whose approval is not in this request is history: one marker line, never the text.
    let items = vec![
        note_item(ContextKind::Selection, "chosen text", 11),
        note_item(ContextKind::Screen, "bad\nlabel\u{1b}[31m", 123),
        note_item(ContextKind::LastCommand, "make", 4),
        note_item(ContextKind::File, "notes.txt", 5),
        note_item(ContextKind::Preview, "page", 6),
    ];
    let history = Message::new(
        "u0",
        Role::User,
        vec![Part::Context(history_note(items, Some("STORED-TEXT"))), text_part("earlier")],
        1,
    );
    let later = Message::new("a0", Role::Assistant, vec![text_part("seen")], 2);
    let mut both = request("gemini-test", vec![history, later, req.messages[0].clone()]);
    both.context.push(context.clone());
    let wire = build(&both).expect("a request");
    let text = String::from_utf8(wire.body.clone()).expect("UTF-8");
    let contents = json_of(&wire.body)["contents"].clone();
    let marker = contents[0]["parts"][0]["text"].as_str().expect("a marker line").to_owned();
    assert!(marker.starts_with("[terminal context shared earlier"), "{marker}");
    for word in [
        "selection \"chosen text\" (11 bytes)",
        "last command \"make\" (4 bytes)",
        "file \"notes.txt\" (5 bytes)",
        "preview \"page\" (6 bytes)",
    ] {
        assert!(marker.contains(word), "the marker names {word}: {marker}");
    }
    assert!(marker.contains("(123 bytes)") && marker.contains("screen \"bad label"), "label and size: {marker}");
    assert!(!marker.contains('\n') && !marker.contains('\u{1b}'), "no line feed and no control character: {marker:?}");
    assert!(!marker.contains("STORED-TEXT") && !marker.contains(context.envelope()));
    let escaped = escaped(context.envelope());
    assert_eq!(
        text.matches(escaped.as_str()).count(),
        1,
        "the envelope occurs once in the body, in the message that names its approval"
    );
    assert!(!text.contains("STORED-TEXT"));

    // A note without items is a bare marker, and the same approval is never sent twice.
    let bare = Message::new("u5", Role::User, vec![Part::Context(history_note(vec![], None))], 1);
    assert_eq!(contents_of(vec![bare])[0]["parts"][0]["text"], "[terminal context shared earlier and not repeated]");
    let twice = request(
        "gemini-test",
        vec![req.messages[0].clone(), assistant("a1", vec![text_part("x")], None), req.messages[0].clone()],
    );
    let mut twice = twice;
    twice.context.push(context.clone());
    let text = String::from_utf8(build(&twice).expect("a request").body).expect("UTF-8");
    assert_eq!(text.matches(escaped.as_str()).count(), 1, "one approval, one envelope");

    // A context approved for another provider is refused with no connection.
    let foreign = approved_context("someone-else", "visible screen", "SCREEN-TEXT");
    let mut wrong = request(
        "gemini-test",
        vec![Message::new("u1", Role::User, vec![Part::Context(ContextNote::for_approved(&foreign, None))], 1)],
    );
    wrong.context.push(foreign);
    let before = fake.accepted();
    let error = run(provider.as_ref(), &wrong).result.expect_err("a foreign context");
    assert_eq!(error.class(), ErrorClass::Policy, "{error:?}");
    assert_eq!(fake.accepted(), before, "no connection for a foreign context");
}

// ---- the request: errors and debug ------------------------------------------------------------------------

#[test]
fn request_errors_and_debug_output_never_echo_request_text() {
    let canary = "REQ-CANARY-7f3a";
    let spaced = format!("bad name {canary}");

    // A valid request with the canary in every place it may sit: no error, and no Debug line shows it.
    let mut good = request(
        "gemini-test",
        vec![
            user("u1", canary),
            assistant("a1", vec![call_part("c1", "read_file", json!({ "path": canary }))], None),
            result_message("t1", "c1", canary, false),
        ],
    );
    good.system = canary.to_owned();
    good.tools.push(tool("read_file", canary, read_file_schema()));
    let wire = build(&good).expect("a request");
    assert!(String::from_utf8_lossy(&wire.body).contains(canary), "the canary is in the body, as it should be");
    assert!(!format!("{wire:?}").contains(canary) && !format!("{wire:#?}").contains(canary), "WireRequest Debug");
    assert!(!format!("{good:?}").contains(canary), "ChatRequest Debug");

    // Every error is free of it.
    let with_tool = |tools: Vec<ToolSpec>| {
        let mut req = request("gemini-test", vec![user("u1", canary)]);
        req.system = canary.to_owned();
        req.tools = tools;
        req
    };
    let rows: Vec<(&str, ChatRequest)> = vec![
        ("an invalid model id", request(&spaced, vec![user("u1", canary)])),
        ("a tool name", with_tool(vec![tool(&spaced, "d", read_file_schema())])),
        (
            "a tool description that is too long",
            with_tool(vec![tool("a", &format!("{canary}{}", "d".repeat(4097)), read_file_schema())]),
        ),
        ("a schema that is not an object schema", with_tool(vec![tool("a", "d", json!({ "type": canary }))])),
        (
            "a result with no call",
            request("gemini-test", vec![user("u1", canary), result_message("t1", canary, canary, false)]),
        ),
        (
            "call arguments that are not an object",
            request(
                "gemini-test",
                vec![user("u1", canary), assistant("a1", vec![call_part(canary, "read_file", json!(canary))], None)],
            ),
        ),
        (
            "a conversation that ends with the model",
            request("gemini-test", vec![assistant("a1", vec![text_part(canary)], None)]),
        ),
    ];
    for (row, req) in rows {
        let error = build(&req).expect_err(row);
        let shown = format!("{error} | {error:?} | {error:#?}");
        assert!(!shown.contains(canary), "{row}: {shown}");
    }
}

#[test]
fn the_request_is_a_pure_function_of_its_inputs() {
    let fake = FakeHttp::start(vec![Reply::Close]);
    let context = approved_context("gemini-test", "visible screen", "SCREEN-TEXT");
    let mut req = request(
        "gemini-test",
        vec![
            Message::new(
                "u1",
                Role::User,
                vec![Part::Context(ContextNote::for_approved(&context, None)), text_part("read it")],
                1,
            ),
            assistant(
                "a1",
                vec![text_part("A"), call_part("call-x", "read_file", json!({ "path": "p" }))],
                Some(base_blob()),
            ),
            result_message("t1", "call-x", "ok", false),
        ],
    );
    req.system = "be brief".to_owned();
    req.max_output_tokens = Some(1000);
    req.tools.push(read_file_spec());
    req.context.push(context);

    let first = build(&req).expect("a request");
    let second = build(&req).expect("a request");
    let other = GeminiAdapter::new().request(&req, &caps("gemini-test")).expect("a request");
    assert_eq!(first.path, second.path);
    assert_eq!(first.body, second.body, "the same inputs give the same bytes");
    assert_eq!(first.path, other.path);
    assert_eq!(first.body, other.body, "a second adapter gives the same bytes");
    assert_eq!(fake.accepted(), 0, "building a request opens no connection");
}
