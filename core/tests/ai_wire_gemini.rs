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
use holzwave_core::ai::http::{Http, HttpConfig, NetworkPolicy, RetryPolicy, SecretHeader, Target};
use holzwave_core::ai::provider::{
    AiError, CancelToken, ChatRequest, Collector, Completion, ErrorClass, Provider, StreamEvent,
};
use holzwave_core::ai::types::{
    ApiKey, Locality, Message, MessageStatus, Part, Role, StopReason, ToolResult, ToolSpec, Usage,
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
