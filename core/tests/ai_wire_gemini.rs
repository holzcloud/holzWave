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

use common::fake_http::{Ending, FakeHttp, Reply};
use holzwave_core::ai::gate::{ApprovedContext, Destination, Gate, OfferRequest, RequestItem};
use holzwave_core::ai::http::{Frame, Http, HttpConfig, NetworkPolicy, Rejection, RetryPolicy, SecretHeader, Target};
use holzwave_core::ai::provider::{
    AiError, CancelToken, ChatRequest, Collector, Completion, ErrorClass, Flow, Provider, StreamEvent,
};
use holzwave_core::ai::redact::SourceText;
use holzwave_core::ai::sse::{Limits, SseDecoder};
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
        "parallel_tools_stream.sse" => include_bytes!("fixtures/ai_wire_gemini/parallel_tools_stream.sse"),
        "thinking_stream.sse" => include_bytes!("fixtures/ai_wire_gemini/thinking_stream.sse"),
        "max_tokens_stream.sse" => include_bytes!("fixtures/ai_wire_gemini/max_tokens_stream.sse"),
        "blocked_stream.sse" => include_bytes!("fixtures/ai_wire_gemini/blocked_stream.sse"),
        "error_stream.sse" => include_bytes!("fixtures/ai_wire_gemini/error_stream.sse"),
        "unknown_fields_stream.sse" => include_bytes!("fixtures/ai_wire_gemini/unknown_fields_stream.sse"),
        "safety_stream.sse" => include_bytes!("fixtures/ai_wire_gemini/safety_stream.sse"),
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

// ---- the stream: a driver without a socket ----------------------------------------------------------------

/// What one stream delivered when it was driven without a socket.
struct Driven {
    log: Vec<String>,
    calls: Vec<ToolCall>,
    result: Result<Completion, AiError>,
}

impl Driven {
    fn completion(&self) -> &Completion {
        self.result.as_ref().unwrap_or_else(|error| panic!("the stream failed: {error:?} after {:?}", self.log))
    }

    fn error(&self) -> &AiError {
        self.result.as_ref().expect_err("the stream should have failed")
    }

    fn text(&self) -> String {
        self.log.iter().filter_map(|entry| entry.strip_prefix("text:")).collect()
    }

    fn native_parts(&self) -> Value {
        self.completion().native.as_ref().expect("a native turn").json["parts"].clone()
    }
}

/// Does what the chokepoint's pump does, without a socket: a fresh turn, a decoder, every chunk pushed, each decoded
/// event handed to the turn, the first error remembered and answered with a stop, then the verdicts of both.
fn drive_with(chunks: &[Vec<u8>], limits: Limits) -> Driven {
    let mut turn = GeminiAdapter::new().turn();
    let mut decoder = SseDecoder::with_limits(limits);
    let mut log = Vec::new();
    let mut calls = Vec::new();
    let mut out = |event: StreamEvent| {
        log.push(log_entry(&event));
        if let StreamEvent::ToolCall(call) = event {
            calls.push(call);
        }
        Flow::Continue
    };
    let mut failure: Option<AiError> = None;
    for chunk in chunks {
        let pushed = decoder.push(chunk, &mut |event| match turn.frame(Frame::Event(event), &mut out) {
            Ok(flow) => flow,
            Err(error) => {
                failure.get_or_insert(error);
                Flow::Stop
            }
        });
        match pushed {
            Ok(Flow::Continue) => {}
            Ok(Flow::Stop) => break,
            Err(error) => {
                failure.get_or_insert(AiError::from(error));
                break;
            }
        }
    }
    let end = decoder.finish();
    let result = match failure {
        Some(error) => Err(error),
        None => turn.finish(end, &mut out),
    };
    Driven { log, calls, result }
}

fn drive(chunks: &[Vec<u8>]) -> Driven {
    drive_with(chunks, Limits::DEFAULT)
}

fn drive_whole(bytes: &[u8]) -> Driven {
    drive(&[bytes.to_vec()])
}

/// The outcome of a driven stream as one string, so a comparison failure prints both sides.
fn summary(driven: &Driven) -> String {
    let calls: Vec<String> = driven
        .calls
        .iter()
        .map(|call| {
            format!("{}:{}:{}", if call.id.starts_with("call_") { "<minted>" } else { &call.id }, call.name, call.args)
        })
        .collect();
    let outcome = match &driven.result {
        Ok(completion) => format!(
            "ok {:?} usage {}/{} native {}",
            completion.stop_reason,
            completion.usage.input_tokens,
            completion.usage.output_tokens,
            completion.native.as_ref().map_or_else(|| "none".to_owned(), |native| native.json.to_string())
        ),
        Err(error) => format!("err {} | {error}", error.class().as_str()),
    };
    format!("{:?} calls {calls:?} => {outcome}", driven.log)
}

/// `bytes` with every line feed turned into a carriage return and a line feed.
fn crlf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 8);
    for &byte in bytes {
        if byte == b'\n' {
            out.push(b'\r');
        }
        out.push(byte);
    }
    out
}

/// One event whose data is `json`.
fn event_of(json: &str) -> Vec<u8> {
    format!("data: {json}\n\n").into_bytes()
}

/// One candidate with the given parts and finish reason, as an event.
fn parts_event(parts: &str, finish: Option<&str>) -> Vec<u8> {
    let finish = finish.map_or_else(String::new, |reason| format!(r#","finishReason":"{reason}""#));
    event_of(&format!(r#"{{"candidates":[{{"content":{{"parts":{parts},"role":"model"}}{finish},"index":0}}]}}"#))
}

/// Several events as one stream.
fn stream_of(events: &[Vec<u8>]) -> Vec<u8> {
    events.concat()
}

const GOLDEN: [&str; 9] = [
    "text_stream.sse",
    "tool_stream.sse",
    "parallel_tools_stream.sse",
    "thinking_stream.sse",
    "max_tokens_stream.sse",
    "blocked_stream.sse",
    "error_stream.sse",
    "unknown_fields_stream.sse",
    "safety_stream.sse",
];

const THINKING_SIGNATURE: &str = "dHJhaWxpbmctZml4dHVyZS1zaWduYXR1cmUtMDAwMy1ub3QtYS1jcmVkZW50aWFs";

// ---- the stream: goldens and splits -----------------------------------------------------------------------

#[test]
fn every_golden_stream_decodes_the_same_at_every_split_point() {
    let mut split_points = 0;
    let mut runs = 0;
    for name in GOLDEN {
        let written = fixture(name);
        let expected = summary(&drive_whole(&written));
        for (form, bytes) in [("as written", written.clone()), ("with CRLF", crlf(&written))] {
            assert_eq!(summary(&drive_whole(&bytes)), expected, "{name} {form}, unsplit");
            for offset in 1..bytes.len() {
                let got = summary(&drive(&[bytes[..offset].to_vec(), bytes[offset..].to_vec()]));
                assert_eq!(got, expected, "{name} {form}, split at offset {offset}");
                split_points += 1;
            }
            for size in [1, 2, 3, 7, 64] {
                let chunks: Vec<Vec<u8>> = bytes.chunks(size).map(<[u8]>::to_vec).collect();
                assert_eq!(summary(&drive(&chunks)), expected, "{name} {form}, chunks of {size} bytes");
                runs += 1;
            }
        }
    }
    println!(
        "split property: {} fixtures, 2 forms (LF, CRLF), {split_points} single-offset splits, {runs} chunked runs of 1, 2, 3, 7 and 64 bytes",
        GOLDEN.len()
    );
}

#[test]
fn golden_streams_decode_to_the_expected_events_usage_and_native_turn() {
    let unicode = "lo \u{e9} \u{65e5}\u{672c} \u{1f600}";

    let text = drive_whole(&fixture("text_stream.sse"));
    assert_eq!(text.log, vec!["text:Hel", "usage:9/0", &format!("text:{unicode}"), "usage:9/4", "text:!", "usage:9/5"]);
    assert_eq!(text.completion().stop_reason, StopReason::EndTurn);
    assert_eq!(text.native_parts(), json!([{ "text": format!("Hel{unicode}!") }]));

    let tool = drive_whole(&fixture("tool_stream.sse"));
    assert_eq!(tool.log, vec!["text:I will read the file.", "usage:42/0", "usage:42/68", "call:read_file"]);
    assert_eq!(tool.completion().stop_reason, StopReason::ToolUse);
    assert_eq!(tool.calls.len(), 1);
    assert!(tool.calls[0].id.starts_with("call_"), "the server sent no id");
    assert_eq!(
        tool.native_parts(),
        json!([
            { "text": "I will read the file." },
            { "functionCall": { "name": "read_file", "args": { "path": "notes/a b.txt" } }, "thoughtSignature": TOOL_SIGNATURE },
        ])
    );

    let parallel = drive_whole(&fixture("parallel_tools_stream.sse"));
    assert_eq!(parallel.log, vec!["usage:50/20", "usage:50/50", "call:read_file", "call:list_dir"]);
    assert_eq!(parallel.completion().stop_reason, StopReason::ToolUse);
    assert_eq!(parallel.calls[0].args, json!({ "path": "a.txt" }));
    assert_eq!(parallel.calls[1].args, json!({ "path": "." }));
    assert_ne!(parallel.calls[0].id, parallel.calls[1].id, "each call has its own id");
    assert_eq!(
        parallel.native_parts(),
        json!([
            { "functionCall": { "name": "read_file", "args": { "path": "a.txt" } }, "thoughtSignature": TOOL_SIGNATURE },
            { "functionCall": { "name": "list_dir", "args": { "path": "." } } },
        ]),
        "the two call parts in order, only the first with a signature"
    );

    let thinking = drive_whole(&fixture("thinking_stream.sse"));
    assert_eq!(
        thinking.log,
        vec!["reasoning:Weighing the options.", "usage:12/0", "text:The answer ", "text:is four.", "usage:12/46"]
    );
    assert_eq!(thinking.text(), "The answer is four.");
    assert_eq!(thinking.completion().stop_reason, StopReason::EndTurn);
    assert_eq!(
        thinking.native_parts(),
        json!([
            { "text": "Weighing the options.", "thought": true },
            { "text": "The answer is four." },
            { "text": "", "thoughtSignature": THINKING_SIGNATURE },
        ]),
        "the plain parts are joined, the part with a signature never is"
    );

    let max_tokens = drive_whole(&fixture("max_tokens_stream.sse"));
    assert_eq!(max_tokens.log, vec!["usage:30/8190"]);
    assert_eq!(max_tokens.completion().stop_reason, StopReason::MaxTokens);
    assert!(max_tokens.completion().native.is_none(), "no parts, no native turn");

    let blocked = drive_whole(&fixture("blocked_stream.sse"));
    assert!(matches!(blocked.error(), AiError::ContentFilter), "{:?}", blocked.error());
    assert_eq!(blocked.error().class(), ErrorClass::ContentFilter);

    let failed = drive_whole(&fixture("error_stream.sse"));
    assert_eq!(failed.log, vec!["text:Partial "]);
    assert!(matches!(failed.error(), AiError::Overloaded), "{:?}", failed.error());
    let shown = format!("{} {:?}", failed.error(), failed.error());
    assert!(!shown.contains("FIXTURE-SERVER-TEXT"), "the server's text reached the error: {shown}");

    let unknown = drive_whole(&fixture("unknown_fields_stream.sse"));
    assert_eq!(unknown.log, vec!["text:Hi", "usage:5/0", "text: there", "usage:5/3"]);
    assert_eq!(unknown.text(), "Hi there");
    assert_eq!(unknown.completion().stop_reason, StopReason::EndTurn);
    assert_eq!(unknown.completion().usage, Usage { input_tokens: 5, output_tokens: 3 });
    assert_eq!(unknown.native_parts(), json!([{ "text": "Hi there" }]), "unknown keys are not stored");

    let safety = drive_whole(&fixture("safety_stream.sse"));
    assert_eq!(safety.log, vec!["text:I can start, but", "usage:9/5"]);
    assert_eq!(safety.completion().stop_reason, StopReason::Refusal);
    assert_eq!(safety.native_parts(), json!([{ "text": "I can start, but" }]));
}

// ---- the stream: finish reasons and calls -----------------------------------------------------------------

/// How a finished turn is expected to look: a stop reason, or an error class.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Want {
    Stop(StopReason),
    Fail(ErrorClass),
}

fn outcome(driven: &Driven) -> Want {
    match &driven.result {
        Ok(completion) => Want::Stop(completion.stop_reason),
        Err(error) => Want::Fail(error.class()),
    }
}

#[test]
fn finish_reasons_map_to_stable_stop_reasons_and_errors() {
    use ErrorClass::{InvalidRequest, Protocol};
    use StopReason::{EndTurn, MaxTokens, Other, Refusal, ToolUse};
    let mut rows: Vec<(Option<&str>, Want, Want)> = vec![
        (Some("STOP"), Want::Stop(EndTurn), Want::Stop(ToolUse)),
        (Some("MAX_TOKENS"), Want::Stop(MaxTokens), Want::Fail(Protocol)),
        (Some("CONTINUATION"), Want::Stop(MaxTokens), Want::Fail(Protocol)),
        (Some("OTHER"), Want::Stop(Other), Want::Fail(Protocol)),
        (Some("A_REASON_FROM_THE_FUTURE"), Want::Stop(Other), Want::Fail(Protocol)),
        (Some("MALFORMED_FUNCTION_CALL"), Want::Fail(Protocol), Want::Fail(Protocol)),
        (Some("UNEXPECTED_TOOL_CALL"), Want::Fail(Protocol), Want::Fail(Protocol)),
        (Some("TOO_MANY_TOOL_CALLS"), Want::Fail(Protocol), Want::Fail(Protocol)),
        (Some("MALFORMED_RESPONSE"), Want::Fail(Protocol), Want::Fail(Protocol)),
        (Some("MISSING_THOUGHT_SIGNATURE"), Want::Fail(InvalidRequest), Want::Fail(InvalidRequest)),
        (Some("FINISH_REASON_UNSPECIFIED"), Want::Fail(Protocol), Want::Fail(Protocol)),
        (Some(""), Want::Fail(Protocol), Want::Fail(Protocol)),
        (None, Want::Fail(Protocol), Want::Fail(Protocol)),
    ];
    for reason in
        ["SAFETY", "RECITATION", "LANGUAGE", "BLOCKLIST", "PROHIBITED_CONTENT", "SPII", "IMAGE_SAFETY", "ESCALATION"]
    {
        rows.push((Some(reason), Want::Stop(Refusal), Want::Fail(Protocol)));
    }
    for (reason, plain, with_call) in rows {
        let text = drive_whole(&parts_event(r#"[{"text":"hi"}]"#, reason));
        assert_eq!(outcome(&text), plain, "reason {reason:?} without a call: {}", summary(&text));
        let call = drive_whole(&parts_event(r#"[{"functionCall":{"name":"read_file","args":{"path":"a"}}}]"#, reason));
        assert_eq!(outcome(&call), with_call, "reason {reason:?} with a call: {}", summary(&call));
        if matches!(with_call, Want::Fail(_)) {
            assert!(
                call.calls.is_empty() && !call.log.iter().any(|entry| entry.starts_with("call:")),
                "{reason:?}: a call ran"
            );
        } else {
            assert_eq!(call.calls.len(), 1, "{reason:?}: the call is released once");
        }
    }
    // The errors name no server text and are stable.
    let missing = drive_whole(&parts_event(r#"[{"text":"hi"}]"#, Some("MISSING_THOUGHT_SIGNATURE")));
    assert!(matches!(missing.error(), AiError::InvalidRequest(_)));
}

#[test]
fn tool_calls_are_released_only_after_a_clean_stop() {
    for (name, expected_calls) in [("tool_stream.sse", 1), ("parallel_tools_stream.sse", 2)] {
        let bytes = fixture(name);
        for cut in 0..bytes.len() {
            let driven = drive_whole(&bytes[..cut]);
            assert!(driven.calls.is_empty(), "{name} cut at byte {cut}: a call was released");
            assert!(
                !driven.log.iter().any(|entry| entry.starts_with("call:")),
                "{name} cut at byte {cut}: a call event"
            );
            assert!(driven.result.is_err(), "{name} cut at byte {cut}: a cut stream ended well");
        }
        let whole = drive_whole(&bytes);
        assert_eq!(whole.calls.len(), expected_calls, "{name}: the whole stream releases its calls once");
        assert_eq!(
            whole.log.iter().filter(|entry| entry.starts_with("call:")).count(),
            expected_calls,
            "{name}: each call is delivered once"
        );
        let tail = &whole.log[whole.log.len() - expected_calls..];
        assert!(tail.iter().all(|entry| entry.starts_with("call:")), "{name}: the calls come last: {:?}", whole.log);
    }

    // A call event followed by a clean end and no finish reason releases nothing.
    let no_finish = drive_whole(&parts_event(r#"[{"functionCall":{"name":"read_file","args":{}}}]"#, None));
    assert!(no_finish.calls.is_empty());
    assert!(matches!(no_finish.error(), AiError::Protocol(_)), "{:?}", no_finish.error());
}

#[test]
fn a_blocked_prompt_is_a_content_filter_error() {
    let blocked = drive_whole(&fixture("blocked_stream.sse"));
    assert!(matches!(blocked.error(), AiError::ContentFilter));

    // The unspecified value and an empty one are not a block.
    for reason in ["BLOCK_REASON_UNSPECIFIED", ""] {
        let feedback = event_of(&format!(
            r#"{{"promptFeedback":{{"blockReason":"{reason}"}},"usageMetadata":{{"promptTokenCount":3}}}}"#
        ));
        let stream = stream_of(&[feedback, parts_event(r#"[{"text":"fine"}]"#, Some("STOP"))]);
        let driven = drive_whole(&stream);
        assert_eq!(driven.completion().stop_reason, StopReason::EndTurn, "block reason {reason:?}");
        assert_eq!(driven.text(), "fine");
    }

    // The same member next to a candidate with content is ignored.
    let next_to = event_of(
        r#"{"promptFeedback":{"blockReason":"OTHER"},"candidates":[{"content":{"parts":[{"text":"fine"}],"role":"model"},"finishReason":"STOP","index":0}]}"#,
    );
    let driven = drive_whole(&next_to);
    assert_eq!(driven.completion().stop_reason, StopReason::EndTurn);
    assert_eq!(driven.text(), "fine");

    // Next to a candidate without content it blocks.
    let empty =
        event_of(r#"{"promptFeedback":{"blockReason":"OTHER"},"candidates":[{"content":{"parts":[]},"index":0}]}"#);
    assert!(matches!(drive_whole(&empty).error(), AiError::ContentFilter));
}

#[test]
fn a_mid_stream_error_object_maps_to_a_fixed_phrase_error_without_server_text() {
    let server_text = "SERVER-TEXT-7f3a";
    let event = |status: Option<&str>, code: Option<u64>| {
        let status = status.map_or_else(String::new, |name| format!(r#","status":"{name}""#));
        let code = code.map_or_else(String::new, |code| format!(r#""code":{code},"#));
        event_of(&format!(r#"{{"error":{{{code}"message":"{server_text}"{status}}}}}"#))
    };
    let mut rows: Vec<(&str, Option<u64>, ErrorClass)> = vec![
        ("UNAUTHENTICATED", Some(401), ErrorClass::Auth),
        ("PERMISSION_DENIED", Some(403), ErrorClass::Auth),
        ("RESOURCE_EXHAUSTED", Some(429), ErrorClass::RateLimit),
        ("UNAVAILABLE", Some(503), ErrorClass::Overload),
        ("DEADLINE_EXCEEDED", Some(504), ErrorClass::Timeout),
        ("WEIRD_NAME", Some(500), ErrorClass::Protocol),
    ];
    for name in ["INTERNAL", "UNKNOWN", "ABORTED", "DATA_LOSS", "CANCELLED"] {
        rows.push((name, Some(500), ErrorClass::Server));
    }
    for name in
        ["INVALID_ARGUMENT", "NOT_FOUND", "FAILED_PRECONDITION", "OUT_OF_RANGE", "ALREADY_EXISTS", "UNIMPLEMENTED"]
    {
        rows.push((name, Some(400), ErrorClass::InvalidRequest));
    }
    for (name, code, class) in rows {
        let driven = drive_whole(&event(Some(name), code));
        assert_eq!(driven.error().class(), class, "status {name}: {:?}", driven.error());
        let shown = format!("{} | {:?}", driven.error(), driven.error());
        assert!(!shown.contains(server_text), "status {name}: the server's text reached the error: {shown}");
    }
    let missing = drive_whole(&event(None, Some(500)));
    assert_eq!(missing.error().class(), ErrorClass::Protocol, "a missing status name");
    assert!(!format!("{:?}", missing.error()).contains(server_text));
    assert!(matches!(
        drive_whole(&event(Some("RESOURCE_EXHAUSTED"), Some(429))).error(),
        AiError::RateLimited { retry_after: None }
    ));

    // The numeric code of a server error is kept when it is a status code, else 500.
    for (code, status) in
        [(Some(503), 503), (Some(100), 100), (Some(599), 599), (Some(99), 500), (Some(600), 500), (None, 500)]
    {
        let driven = drive_whole(&event(Some("INTERNAL"), code));
        match driven.error() {
            AiError::Server { status: got, .. } => assert_eq!(*got, status, "code {code:?}"),
            other => panic!("code {code:?}: {other:?}"),
        }
    }

    // A text event first keeps its text; the error still ends the turn.
    let stream = stream_of(&[parts_event(r#"[{"text":"Partial"}]"#, None), event(Some("UNAVAILABLE"), Some(503))]);
    let driven = drive_whole(&stream);
    assert_eq!(driven.log, vec!["text:Partial"]);
    assert!(matches!(driven.error(), AiError::Overloaded));
}

// ---- the stream: strictness and bounds --------------------------------------------------------------------

#[test]
fn unrequested_part_kinds_and_malformed_events_are_protocol_errors() {
    let with_stop = |parts: &str| parts_event(parts, Some("STOP"));
    let mut rows: Vec<(String, Vec<u8>)> = vec![
        ("data that is not JSON".to_owned(), event_of("this is not json")),
        ("data that is a JSON array".to_owned(), event_of("[1, 2]")),
        ("data that is a JSON string".to_owned(), event_of("\"text\"")),
        ("candidates that is not an array".to_owned(), event_of(r#"{"candidates":"x"}"#)),
        ("a candidate that is not an object".to_owned(), event_of(r#"{"candidates":[5]}"#)),
        ("content that is not an object".to_owned(), event_of(r#"{"candidates":[{"content":5}]}"#)),
        ("parts that is not an array".to_owned(), event_of(r#"{"candidates":[{"content":{"parts":{}}}]}"#)),
        ("a part that is not an object".to_owned(), with_stop("[5]")),
        ("a text that is not a string".to_owned(), with_stop(r#"[{"text":5}]"#)),
        ("a call without a name".to_owned(), with_stop(r#"[{"functionCall":{"args":{}}}]"#)),
        ("a call whose name is not a string".to_owned(), with_stop(r#"[{"functionCall":{"name":3,"args":{}}}]"#)),
        ("a call that is not an object".to_owned(), with_stop(r#"[{"functionCall":"x"}]"#)),
        ("a signature that is not a string".to_owned(), with_stop(r#"[{"text":"a","thoughtSignature":5}]"#)),
        (
            "text and a call in one part".to_owned(),
            with_stop(r#"[{"text":"a","functionCall":{"name":"a","args":{}}}]"#),
        ),
        ("call arguments that are an array".to_owned(), with_stop(r#"[{"functionCall":{"name":"a","args":[1]}}]"#)),
        ("call arguments that are a string".to_owned(), with_stop(r#"[{"functionCall":{"name":"a","args":"x"}}]"#)),
        ("a call name with a line feed".to_owned(), with_stop(r#"[{"functionCall":{"name":"a\nb","args":{}}}]"#)),
        ("an error that is not an object".to_owned(), event_of(r#"{"error":"x"}"#)),
    ];
    for kind in [
        "inlineData",
        "fileData",
        "functionResponse",
        "executableCode",
        "codeExecutionResult",
        "toolCall",
        "toolResponse",
    ] {
        rows.push((
            format!("a part holding {kind}"),
            with_stop(&format!(r#"[{{"{kind}":{{"data":"LEAK-CANARY"}}}}]"#)),
        ));
        rows.push((format!("a part with text and {kind}"), with_stop(&format!(r#"[{{"text":"a","{kind}":{{}}}}]"#))));
    }
    for (row, stream) in &rows {
        let driven = drive_whole(stream);
        assert_eq!(driven.error().class(), ErrorClass::Protocol, "{row}: {}", summary(&driven));
        assert!(driven.calls.is_empty(), "{row}: a call ran");
        let shown = format!("{} {:?}", driven.error(), driven.error());
        assert!(!shown.contains("LEAK-CANARY"), "{row}: the error echoes the stream: {shown}");
    }

    // A line frame is not an event.
    let mut turn = GeminiAdapter::new().turn();
    let error = turn.frame(Frame::Line(b"{}"), &mut |_| Flow::Continue).expect_err("a line frame");
    assert_eq!(error.class(), ErrorClass::Protocol);

    // These are not errors.
    let skipped = drive_whole(&with_stop(r#"[{"futureField":1}]"#));
    assert_eq!(skipped.completion().stop_reason, StopReason::EndTurn, "a part with only unknown keys is skipped");
    assert!(skipped.completion().native.is_none());
    let done =
        stream_of(&[parts_event(r#"[{"text":"a"}]"#, None), event_of("[DONE]"), parts_event("[]", Some("STOP"))]);
    assert_eq!(drive_whole(&done).text(), "a", "a done marker is ignored");
    let empty_data = stream_of(&[b"data:\n\n".to_vec(), parts_event(r#"[{"text":"a"}]"#, Some("STOP"))]);
    assert_eq!(
        drive_whole(&empty_data).completion().stop_reason,
        StopReason::EndTurn,
        "an empty data field is ignored"
    );
    let null_args = drive_whole(&with_stop(r#"[{"functionCall":{"name":"a","args":null}}]"#));
    assert_eq!(null_args.calls[0].args, json!({}), "absent or null arguments are an empty object");
    assert_eq!(null_args.completion().stop_reason, StopReason::ToolUse);
}

#[test]
fn signatures_text_runs_and_sizes_are_enforced_in_the_native_turn() {
    let leak = "BAD-SIGNATURE-TEXT";
    let signed = |count: usize| -> String {
        (0..count).map(|_| r#"{"text":"t","thoughtSignature":"AAAA"}"#).collect::<Vec<_>>().join(",")
    };

    // Parts per event.
    let plain = |count: usize| -> String { (0..count).map(|_| r#"{"text":"a"}"#).collect::<Vec<_>>().join(",") };
    let at_limit = drive_whole(&parts_event(&format!("[{}]", plain(256)), Some("STOP")));
    assert_eq!(at_limit.text().len(), 256, "256 parts in one event pass");
    let over = drive_whole(&parts_event(&format!("[{}]", plain(257)), Some("STOP")));
    assert_eq!(over.error().class(), ErrorClass::Protocol, "257 parts in one event");

    // Parts that cannot be joined, over several events.
    let in_events = |total: usize| -> Vec<u8> {
        let mut events = Vec::new();
        let mut left = total;
        while left > 0 {
            let now = left.min(100);
            events.push(parts_event(&format!("[{}]", signed(now)), None));
            left -= now;
        }
        events.push(parts_event("[]", Some("STOP")));
        stream_of(&events)
    };
    let ok = drive_whole(&in_events(1024));
    assert_eq!(ok.native_parts().as_array().map(Vec::len), Some(1024), "1024 parts pass");
    let too_many = drive_whole(&in_events(1025));
    assert_eq!(too_many.error().class(), ErrorClass::Protocol, "1025 parts that cannot be joined");

    // Signatures.
    let signature_stream =
        |signature: &str| parts_event(&format!(r#"[{{"text":"a","thoughtSignature":"{signature}"}}]"#), Some("STOP"));
    let big = drive_whole(&signature_stream(&"A".repeat(262_144)));
    assert_eq!(
        big.native_parts()[0]["thoughtSignature"].as_str().map(str::len),
        Some(262_144),
        "262144 characters pass"
    );
    let too_big = drive_whole(&signature_stream(&"A".repeat(262_145)));
    assert_eq!(too_big.error().class(), ErrorClass::Protocol, "262145 characters");
    let bad_char = drive_whole(&signature_stream(&format!("{leak}!")));
    assert_eq!(bad_char.error().class(), ErrorClass::Protocol, "a character outside the base64 set");
    assert!(!format!("{} {:?}", bad_char.error(), bad_char.error()).contains(leak), "the signature is not echoed");
    let alphabets = drive_whole(&signature_stream("Ab09+/=_-"));
    assert_eq!(alphabets.native_parts()[0]["thoughtSignature"], "Ab09+/=_-", "both base64 alphabets pass");
    let empty = drive_whole(&signature_stream(""));
    assert_eq!(empty.native_parts(), json!([{ "text": "a" }]), "an empty signature is no signature");

    // A signature-only part is kept, and a plain part after a signed one is not joined to it.
    let kept = drive_whole(&stream_of(&[parts_event(
        r#"[{"thoughtSignature":"AAAA"},{"text":"x"},{"text":"y","thoughtSignature":"BBBB"},{"text":"z"},{"text":"w"}]"#,
        Some("STOP"),
    )]));
    assert_eq!(
        kept.native_parts(),
        json!([
            { "thoughtSignature": "AAAA" },
            { "text": "x" },
            { "text": "y", "thoughtSignature": "BBBB" },
            { "text": "zw" },
        ])
    );
    // Thought parts and plain parts are not joined to each other.
    let mixed = drive_whole(&parts_event(
        r#"[{"text":"a","thought":true},{"text":"b","thought":true},{"text":"c"}]"#,
        Some("STOP"),
    ));
    assert_eq!(mixed.native_parts(), json!([{ "text": "ab", "thought": true }, { "text": "c" }]));

    // The stored turn: 5 MiB of plain text in events of 1 MiB passes 4 MiB and is refused; 4 MiB passes.
    let limits = Limits { line: 8 * 1024 * 1024, event: 8 * 1024 * 1024 };
    let megabyte = "x".repeat(1024 * 1024);
    let chunk = |last: bool| parts_event(&format!(r#"[{{"text":"{megabyte}"}}]"#), last.then_some("STOP"));
    let four = drive_with(&[stream_of(&[chunk(false), chunk(false), chunk(false), chunk(true)])], limits);
    assert_eq!(four.completion().stop_reason, StopReason::EndTurn, "4 MiB of text is within the bound");
    let five = drive_with(&[stream_of(&[chunk(false), chunk(false), chunk(false), chunk(false), chunk(true)])], limits);
    assert_eq!(five.error().class(), ErrorClass::Protocol, "5 MiB of text is over the bound");
    assert!(!format!("{} {:?}", five.error(), five.error()).contains("xxxx"), "the text is not echoed");
}

// ---- the rejection ----------------------------------------------------------------------------------------

/// An endpoint that holds the canary key, for the tests that read error bodies.
fn key_endpoint() -> Endpoint {
    let target = Target::parse("http://127.0.0.1:9/v1beta", false).expect("a loopback address");
    let secret = SecretHeader::new(API_KEY_HEADER, "", ApiKey::new(CANARY_KEY.to_owned()), target.origin().clone())
        .expect("a key header");
    Endpoint::new(Wire::Gemini, target, NetworkPolicy::new(false, false)).with_secret(secret)
}

fn error_body(code: u16, status: &str, message: &str, details: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({ "error": { "code": code, "message": message, "status": status, "details": details } }))
        .expect("JSON")
}

fn reject(status: u16, body: Vec<u8>, retry_after: Option<Duration>) -> Option<AiError> {
    GeminiAdapter::new().rejection(&Rejection { status, retry_after, body }, &key_endpoint())
}

fn key_info(reason: &str) -> Value {
    json!([{ "@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": reason, "domain": "googleapis.com", "metadata": { "x": "DETAIL-CANARY" } }])
}

fn retry_info(delay: &str) -> Value {
    json!([{ "@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": delay }])
}

#[test]
fn error_bodies_map_to_stable_classes_and_never_carry_the_key() {
    let no_details = json!([]);
    let invalid_key =
        reject(400, error_body(400, "INVALID_ARGUMENT", "API key not valid.", key_info("API_KEY_INVALID")), None);
    assert_eq!(invalid_key.expect("an error").class(), ErrorClass::Auth, "a key reason on a 400");
    let credentials =
        reject(400, error_body(400, "FAILED_PRECONDITION", "No credentials.", key_info("CREDENTIALS_MISSING")), None);
    assert_eq!(credentials.expect("an error").class(), ErrorClass::Auth, "a credentials reason");
    for (status, name) in [(401, "UNAUTHENTICATED"), (403, "PERMISSION_DENIED")] {
        let error = reject(status, error_body(status, name, "denied", no_details.clone()), None).expect("an error");
        assert_eq!(error.class(), ErrorClass::Auth, "{name}");
    }
    let not_found =
        reject(404, error_body(404, "NOT_FOUND", "models/x is not found for this version", no_details.clone()), None)
            .expect("an error");
    match &not_found {
        AiError::InvalidRequest(detail) => assert_eq!(detail.as_str(), "models/x is not found for this version"),
        other => panic!("{other:?}"),
    }

    // The wait of a rate limit.
    for (delay, want) in [
        ("34s", Some(34)),
        ("0.5s", Some(1)),
        ("7200000s", Some(86_400)),
        ("1.5s", Some(2)),
        ("soon", None),
        ("34", None),
    ] {
        let error = reject(
            429,
            error_body(429, "RESOURCE_EXHAUSTED", "slow down", retry_info(delay)),
            Some(Duration::from_secs(7)),
        )
        .expect("an error");
        let want = want.map_or(Some(Duration::from_secs(7)), |secs| Some(Duration::from_secs(secs)));
        assert_eq!(error, AiError::RateLimited { retry_after: want }, "retry delay {delay:?}");
    }
    let own = reject(
        429,
        error_body(429, "RESOURCE_EXHAUSTED", "slow down", no_details.clone()),
        Some(Duration::from_secs(9)),
    );
    assert_eq!(
        own,
        Some(AiError::RateLimited { retry_after: Some(Duration::from_secs(9)) }),
        "the rejection's own wait"
    );
    let none = reject(429, error_body(429, "RESOURCE_EXHAUSTED", "slow down", no_details.clone()), None);
    assert_eq!(none, Some(AiError::RateLimited { retry_after: None }));

    assert_eq!(reject(503, error_body(503, "UNAVAILABLE", "m", no_details.clone()), None), Some(AiError::Overloaded));
    assert_eq!(
        reject(504, error_body(504, "DEADLINE_EXCEEDED", "m", no_details.clone()), None),
        Some(AiError::Timeout)
    );
    match reject(500, error_body(500, "INTERNAL", "m", no_details.clone()), None).expect("an error") {
        AiError::Server { status, .. } => assert_eq!(status, 500),
        other => panic!("{other:?}"),
    }
    let long_input = "The input token count (5000000) exceeds the maximum number of tokens allowed (1048576).";
    assert_eq!(
        reject(400, error_body(400, "INVALID_ARGUMENT", long_input, no_details.clone()), None),
        Some(AiError::ContextLength)
    );
    let other_invalid =
        reject(400, error_body(400, "INVALID_ARGUMENT", "unknown field", no_details.clone()), None).expect("an error");
    assert_eq!(other_invalid.class(), ErrorClass::InvalidRequest, "an argument error that is not about length");
    assert_eq!(
        reject(400, error_body(400, "FAILED_PRECONDITION", "billing", no_details.clone()), None)
            .expect("an error")
            .class(),
        ErrorClass::InvalidRequest
    );

    // A body of any other shape falls through to the chokepoint's table.
    let others: Vec<(&str, Vec<u8>)> = vec![
        ("an empty body", Vec::new()),
        ("a body that is not JSON", b"not json".to_vec()),
        ("an HTML body", b"<html><body>Bad gateway</body></html>".to_vec()),
        ("JSON without an error object", br#"{"foo":1}"#.to_vec()),
        ("an error that is a string", br#"{"error":"x"}"#.to_vec()),
        ("an unknown status name", error_body(400, "SOMETHING_NEW", "m", no_details.clone())),
        ("a missing status name", br#"{"error":{"code":400,"message":"m"}}"#.to_vec()),
        ("an empty array", b"[]".to_vec()),
    ];
    for (row, body) in others {
        assert_eq!(reject(400, body, None), None, "{row}");
    }
    let wrapped = {
        let inner = error_body(401, "UNAUTHENTICATED", "denied", no_details.clone());
        let mut body = b"[".to_vec();
        body.extend_from_slice(&inner);
        body.push(b']');
        body
    };
    assert_eq!(
        reject(401, wrapped, None).expect("an error").class(),
        ErrorClass::Auth,
        "an error in a one-element array"
    );

    // The key is masked, the text is bounded, and the details are never copied.
    let echo = format!("bad key {CANARY_KEY} in the request");
    for (status, name, details) in [
        (400, "INVALID_ARGUMENT", no_details.clone()),
        (401, "UNAUTHENTICATED", key_info("ACCESS_TOKEN_EXPIRED")),
        (400, "INVALID_ARGUMENT", key_info("API_KEY_INVALID")),
    ] {
        let error = reject(status, error_body(status, name, &echo, details), None).expect("an error");
        let shown = format!("{error} | {error:?} | {error:#?}");
        assert!(!shown.contains(CANARY_KEY), "{name}: the key is in the error: {shown}");
        assert!(shown.contains("<redacted>"), "{name}: the key is masked: {shown}");
        assert!(!shown.contains("DETAIL-CANARY"), "{name}: the details are copied: {shown}");
    }
    let huge = "m".repeat(8 * 1024);
    match reject(400, error_body(400, "NOT_FOUND", &huge, no_details), None).expect("an error") {
        AiError::InvalidRequest(detail) => {
            assert!(detail.as_str().chars().count() <= 241, "{} characters", detail.as_str().chars().count())
        }
        other => panic!("{other:?}"),
    }
}

// ---- the stream over the wire -----------------------------------------------------------------------------

#[test]
fn an_unfinished_stream_over_the_wire_is_an_error_and_delivers_no_call() {
    let tool_events = events_of(&fixture("tool_stream.sse"));
    let request = || {
        let mut req = request("gemini-test", vec![Message::user_text("u1", "read it", 1)]);
        req.tools.push(read_file_spec());
        req
    };

    // The first event only, ended properly: no finish reason.
    let fake = FakeHttp::start(vec![Reply::sse_bytes(vec![tool_events[0].clone()])]);
    let (service, _http) = provider(&fake);
    let driven = run(service.as_ref(), &request());
    assert_eq!(driven.log, vec!["text:I will read the file.", "usage:42/0"]);
    assert!(matches!(driven.result, Err(AiError::Protocol(_))), "{:?}", driven.result);

    // A call event with no finish reason releases nothing.
    let call_only = parts_event(r#"[{"functionCall":{"name":"read_file","args":{"path":"a"}}}]"#, None);
    let fake = FakeHttp::start(vec![Reply::sse_bytes(vec![call_only])]);
    let (service, _http) = provider(&fake);
    let driven = run(service.as_ref(), &request());
    assert!(!driven.log.iter().any(|entry| entry.starts_with("call:")), "{:?}", driven.log);
    assert!(matches!(driven.result, Err(AiError::Protocol(_))), "{:?}", driven.result);

    // Cut in the middle of the second event but ended with a proper terminating chunk.
    let cut = tool_events[1].len() / 2;
    let fake = FakeHttp::start(vec![Reply::sse_bytes(vec![tool_events[0].clone(), tool_events[1][..cut].to_vec()])]);
    let (service, _http) = provider(&fake);
    let driven = run(service.as_ref(), &request());
    assert!(!driven.log.iter().any(|entry| entry.starts_with("call:")), "{:?}", driven.log);
    assert!(matches!(driven.result, Err(AiError::Network(_))), "{:?}", driven.result);

    // The connection dropped in the middle of the body.
    let text = events_of(&fixture("text_stream.sse"));
    let fake = FakeHttp::start(vec![Reply::sse_bytes(vec![text[0].clone()]).ending(Ending::Drop)]);
    let (service, _http) = provider(&fake);
    let driven = run(service.as_ref(), &request());
    assert_eq!(driven.log, vec!["text:Hel", "usage:9/0"]);
    let error = driven.result.as_ref().expect_err("a dropped connection is never a success");
    assert!(matches!(error.class(), ErrorClass::Network | ErrorClass::Timeout), "{error:?}");

    // A 401 with the body of an invalid key is an authentication error after exactly one connection.
    let body = error_body(
        400,
        "INVALID_ARGUMENT",
        "API key not valid. Please pass a valid API key.",
        key_info("API_KEY_INVALID"),
    );
    let fake = FakeHttp::start(vec![Reply::status(401).body(body)]);
    let (service, _http) = provider(&fake);
    let driven = run(service.as_ref(), &request());
    assert!(matches!(driven.result, Err(AiError::Auth(_))), "{:?}", driven.result);
    assert_eq!(fake.accepted(), 1, "an authentication error is not retried");

    // A 400 whose message repeats the key is an error without the key.
    let body = error_body(400, "INVALID_ARGUMENT", &format!("the key {CANARY_KEY} is not accepted"), json!([]));
    let fake = FakeHttp::start(vec![Reply::status(400).body(body)]);
    let (service, _http) = provider(&fake);
    let driven = run(service.as_ref(), &request());
    let error = driven.result.expect_err("a bad request");
    assert_eq!(error.class(), ErrorClass::InvalidRequest);
    assert!(!format!("{error} {error:?}").contains(CANARY_KEY), "the key is in the error");
}

#[test]
fn a_rejected_request_over_the_wire_carries_the_scrubbed_message_only() {
    let request = || request("gemini-test", vec![Message::user_text("u1", "hello", 1)]);

    // A message with a control sequence and the key: both are gone.
    let message = format!("models/x is not found \u{1b}[31m{CANARY_KEY}\u{1b}[0m \u{202e}reversed");
    let fake = FakeHttp::start(vec![Reply::status(404).body(error_body(404, "NOT_FOUND", &message, json!([])))]);
    let (service, _http) = provider(&fake);
    let error = run(service.as_ref(), &request()).result.expect_err("a rejection");
    assert_eq!(error.class(), ErrorClass::InvalidRequest);
    let shown = error.to_string();
    assert!(shown.contains("models/x is not found"), "{shown}");
    assert!(!shown.contains(CANARY_KEY) && !shown.contains('\u{1b}') && !shown.contains('\u{202e}'), "{shown:?}");
    assert_eq!(fake.accepted(), 1);

    // A rate limit carries the server's retry delay after the chokepoint's retries.
    let fake = FakeHttp::start(vec![Reply::status(429).body(error_body(
        429,
        "RESOURCE_EXHAUSTED",
        "slow down",
        retry_info("2s"),
    ))]);
    let (service, _http) = provider(&fake);
    let error = run(service.as_ref(), &request()).result.expect_err("a rate limit");
    assert_eq!(error, AiError::RateLimited { retry_after: Some(Duration::from_secs(2)) });
    assert_eq!(fake.accepted(), 3, "the request and two retries");

    // An overload and a body of another shape.
    let fake = FakeHttp::start(vec![Reply::status(503).body(error_body(503, "UNAVAILABLE", "busy", json!([])))]);
    let (service, _http) = provider(&fake);
    assert_eq!(run(service.as_ref(), &request()).result.expect_err("an overload"), AiError::Overloaded);
    let fake = FakeHttp::start(vec![Reply::status(404).body(b"<html>not found</html>".to_vec())]);
    let (service, _http) = provider(&fake);
    let error = run(service.as_ref(), &request()).result.expect_err("a rejection");
    assert_eq!(error.class(), ErrorClass::InvalidRequest, "the chokepoint's table answers");
    assert!(!error.to_string().contains("html"), "the body is not echoed: {error}");
}

// ---- the real service -------------------------------------------------------------------------------------

#[test]
#[ignore = "needs a real key in HOLZWAVE_TEST_GEMINI_KEY and network access"]
fn live_text_and_tool_turn_against_the_real_service() {
    let key = std::env::var("HOLZWAVE_TEST_GEMINI_KEY")
        .unwrap_or_else(|_| panic!("set HOLZWAVE_TEST_GEMINI_KEY to a Gemini API key to run this test"));
    let model = std::env::var("HOLZWAVE_TEST_GEMINI_MODEL").unwrap_or_else(|_| "gemini-3.8-flash".to_owned());
    let target = Target::parse("https://generativelanguage.googleapis.com/v1beta", false).expect("the service address");
    let secret =
        SecretHeader::new(API_KEY_HEADER, "", ApiKey::new(key), target.origin().clone()).expect("a key header");
    let endpoint = Endpoint::new(Wire::Gemini, target, NetworkPolicy::new(false, false)).with_secret(secret);
    let http = Arc::new(Http::new(HttpConfig::standard()));
    let provider = HttpProvider::new("gemini-live", endpoint, Box::new(GeminiAdapter::new()), http);

    // A text turn.
    let mut text_request = request(&model, vec![Message::user_text("u1", "Reply with the single word: ready", 1)]);
    text_request.max_output_tokens = Some(2048);
    let text_turn = run(&provider, &text_request);
    let completion = text_turn.completion().clone();
    assert!(
        !text_turn.collector.text().is_empty() || completion.stop_reason == StopReason::MaxTokens,
        "the text turn returned no text and did not stop at the limit: {:?}",
        text_turn.log
    );
    println!(
        "live text: model {model}, {} characters, stop {:?}",
        text_turn.collector.text().chars().count(),
        completion.stop_reason
    );

    // A tool turn, then the echo of the model's own content.
    let mut tool_request =
        request(&model, vec![Message::user_text("u2", "Use the read_file tool to read notes.txt.", 1)]);
    tool_request.tools.push(read_file_spec());
    tool_request.max_output_tokens = Some(2048);
    let tool_turn = run(&provider, &tool_request);
    let completion = tool_turn.completion().clone();
    let calls = tool_turn.collector.tool_calls().to_vec();
    assert_eq!(
        calls.first().map(|call| call.name.as_str()),
        Some("read_file"),
        "the declared call: {:?}",
        tool_turn.log
    );
    let signature_parts = completion
        .native
        .as_ref()
        .and_then(|native| native.json["parts"].as_array())
        .map_or(0, |parts| parts.iter().filter(|part| part.get("thoughtSignature").is_some()).count());
    println!("live tool: model {model}, call {}, stop {:?}", calls[0].name, completion.stop_reason);
    println!("live signature parts: {signature_parts}");
    println!("live usage: {}/{}", completion.usage.input_tokens, completion.usage.output_tokens);
    let call_id = calls[0].id.clone();
    let assistant = tool_turn.collector.into_message("m2", 2, &model, MessageStatus::Complete, Some(completion));
    let mut echo = request(
        &model,
        vec![
            Message::user_text("u2", "Use the read_file tool to read notes.txt.", 1),
            assistant,
            tool_message("m3", &call_id, "notes: all good"),
        ],
    );
    echo.tools.push(read_file_spec());
    echo.max_output_tokens = Some(2048);
    let echoed = run(&provider, &echo);
    assert!(
        !matches!(echoed.result, Err(AiError::InvalidRequest(_))),
        "the echo request was refused: {:?}",
        echoed.result
    );
    println!("live echo: accepted, log {:?}", echoed.log);
}
