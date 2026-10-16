//! The Anthropic Messages adapter, proved through the real HTTP chokepoint and in memory, with no outside network.
//!
//! The streams in `tests/fixtures/ai_wire/anthropic/` are written by hand from the vendor's streaming reference. They
//! are not recordings: they are an assumed shape until the ignored live check at the end of this file has been run once
//! with a key. Every request in this file goes to `FakeHttp`, an HTTP/1.1 server in this process, except that live check.
//! A stream that is driven directly goes through `SseDecoder` and the turn of the adapter the way the shared driver does
//! it, so the bytes can be cut at any offset. A failing case names its offset, its chunk size or its seed.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::Xorshift64;
use common::fake_http::{Ending, FakeHttp, Reply};
use holzwave_core::ai::gate::{ApprovedContext, Destination, Gate, OfferRequest, RequestItem};
use holzwave_core::ai::http::{Frame, Framing, Http, HttpConfig, NetworkPolicy, SecretHeader, Target};
use holzwave_core::ai::provider::{
    AiError, CancelToken, ChatRequest, Collector, Completion, ErrorClass, Flow, Provider, StreamEvent,
};
use holzwave_core::ai::redact::SourceText;
use holzwave_core::ai::sse::SseDecoder;
use holzwave_core::ai::types::{
    ApiKey, ContextKind, ContextNote, Locality, Message, MessageStatus, Opaque, Part, Role, StopReason, ToolCall,
    ToolResult, ToolSpec,
};
use holzwave_core::ai::wire::anthropic::{
    API_VERSION, AnthropicAdapter, BINDING_BETA, DEFAULT_MAX_TOKENS, Flavor, MAX_BLOCKS, MAX_TURN_BYTES, MESSAGES_PATH,
};
use holzwave_core::ai::wire::{Endpoint, HttpProvider, Wire, WireAdapter};
use serde_json::{Value, json};

// ---- helpers ----------------------------------------------------------------------------------------------

/// The bytes of a fixture of this dialect.
fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ai_wire/anthropic").join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("fixture {name}: {error}"))
}

/// A key that no real service knows. It appears in test files only.
fn canary_key() -> &'static str {
    "sk-canary-0123456789abcdef"
}

/// An endpoint on the fake server with the key header the profile plan builds: `x-api-key`, no prefix, bound to the origin.
fn endpoint(fake: &FakeHttp) -> Endpoint {
    let target = Target::parse(&fake.base(), false).expect("a loopback base address");
    let origin = target.origin().clone();
    let secret =
        SecretHeader::new("x-api-key", "", ApiKey::new(canary_key().to_owned()), origin).expect("a key header");
    Endpoint::new(Wire::Anthropic, target, NetworkPolicy::new(false, false)).with_secret(secret)
}

/// A provider on the fake server and the chokepoint it uses.
fn make_provider(fake: &FakeHttp, flavor: Flavor) -> (Arc<dyn Provider>, Arc<Http>) {
    make_provider_with(fake, flavor, HttpConfig::standard())
}

fn make_provider_with(fake: &FakeHttp, flavor: Flavor, config: HttpConfig) -> (Arc<dyn Provider>, Arc<Http>) {
    let http = Arc::new(Http::new(config));
    let provider =
        HttpProvider::new("anthropic-test", endpoint(fake), Box::new(AnthropicAdapter::new(flavor)), http.clone());
    (Arc::new(provider), http)
}

/// A request for `model` holding one user message.
fn request(model: &str, text: &str) -> ChatRequest {
    let mut request = ChatRequest::new(model);
    request.messages.push(Message::user_text("u1", text, 1));
    request.max_output_tokens = Some(1024);
    request
}

fn json_of(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("JSON")
}

/// What a stream reported, as plain values that can be compared.
#[derive(Clone, Debug, PartialEq)]
enum Ev {
    Text(String),
    Reasoning(String),
    Tool(String, String, Value),
    Usage(u32, u32),
}

fn ev_of(event: &StreamEvent) -> Ev {
    match event {
        StreamEvent::TextDelta(text) => Ev::Text(text.clone()),
        StreamEvent::ReasoningDelta(text) => Ev::Reasoning(text.clone()),
        StreamEvent::ToolCall(call) => Ev::Tool(call.id.clone(), call.name.clone(), call.args.clone()),
        StreamEvent::Usage(usage) => Ev::Usage(usage.input_tokens, usage.output_tokens),
    }
}

fn text(value: &str) -> Ev {
    Ev::Text(value.to_owned())
}

/// What one pass over a stream delivered, with the moment of each event, and how the turn ended.
struct Run {
    events: Vec<(Instant, Ev)>,
    collector: Collector,
    result: Result<Completion, AiError>,
}

impl Run {
    fn evs(&self) -> Vec<Ev> {
        self.events.iter().map(|(_, ev)| ev.clone()).collect()
    }

    /// What two runs must share when the same bytes were cut in different places.
    fn outcome(&self) -> (Vec<Ev>, Result<Completion, AiError>) {
        (self.evs(), self.result.clone())
    }

    fn completion(&self) -> &Completion {
        self.result.as_ref().unwrap_or_else(|error| panic!("the turn failed: {error:?} after {:?}", self.evs()))
    }

    fn error(&self) -> &AiError {
        self.result.as_ref().expect_err("the turn should have failed")
    }

    fn calls(&self) -> Vec<Ev> {
        self.evs().into_iter().filter(|ev| matches!(ev, Ev::Tool(..))).collect()
    }

    /// The native turn of a completed run.
    fn native(&self) -> Option<Value> {
        let native = self.completion().native.as_ref()?;
        assert_eq!(native.wire, "anthropic");
        Some(native.json.clone())
    }

    /// The assistant message the loop would store for this run.
    fn into_message(self, id: &str, model: &str) -> Message {
        let completion = self.result.ok();
        self.collector.into_message(id, 2, model, MessageStatus::Complete, completion)
    }
}

/// Drives the chunks through a decoder and a fresh turn, exactly as the shared driver does: an error from the turn is
/// kept and answered with a stop, an error from the decoder becomes a protocol error, and a kept error wins over the
/// verdict of the end. `answer` sees every event and returns what the consumer says.
fn drive_with<T: AsRef<[u8]>>(chunks: &[T], answer: &mut dyn FnMut(&Ev) -> Flow) -> Run {
    let mut turn = AnthropicAdapter::new(Flavor::Gateway).turn();
    let mut decoder = SseDecoder::new();
    let mut events: Vec<(Instant, Ev)> = Vec::new();
    let mut collector = Collector::new();
    let mut failure: Option<AiError> = None;
    let result = {
        let mut out = |event: StreamEvent| {
            let ev = ev_of(&event);
            events.push((Instant::now(), ev.clone()));
            let flow = collector.push(event);
            if flow == Flow::Stop { flow } else { answer(&ev) }
        };
        for chunk in chunks {
            let pushed = decoder.push(chunk.as_ref(), &mut |event| {
                if failure.is_some() {
                    return Flow::Stop;
                }
                match turn.frame(Frame::Event(event), &mut out) {
                    Ok(flow) => flow,
                    Err(error) => {
                        failure = Some(error);
                        Flow::Stop
                    }
                }
            });
            match pushed {
                Err(error) => {
                    failure.get_or_insert(AiError::from(error));
                    break;
                }
                Ok(Flow::Stop) => break,
                Ok(Flow::Continue) => {}
            }
        }
        let end = decoder.finish();
        match failure {
            Some(error) => Err(error),
            None => turn.finish(end, &mut out),
        }
    };
    Run { events, collector, result }
}

fn drive<T: AsRef<[u8]>>(chunks: &[T]) -> Run {
    drive_with(chunks, &mut |_| Flow::Continue)
}

/// The whole stream in one chunk.
fn play(bytes: &[u8]) -> Run {
    drive(&[bytes])
}

/// Every single cut of a stream of `len` bytes: the offsets 1 to len-1.
fn cuts(len: usize) -> impl Iterator<Item = usize> {
    1..len
}

fn one_byte_chunks(bytes: &[u8]) -> Vec<Vec<u8>> {
    bytes.chunks(1).map(<[u8]>::to_vec).collect()
}

/// Chunks of random sizes (1 to 40 bytes) from a seed.
fn random_chunks(bytes: &[u8], seed: u64) -> Vec<Vec<u8>> {
    let mut random = Xorshift64::new(seed);
    let mut chunks = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let end = (at + 1 + random.below(40)).min(bytes.len());
        chunks.push(bytes[at..end].to_vec());
        at = end;
    }
    chunks
}

/// The offset just after the first occurrence of `needle` in `bytes`.
fn after(bytes: &[u8], needle: &[u8]) -> usize {
    let at = bytes.windows(needle.len()).position(|window| window == needle).expect("the pattern is in the stream");
    at + needle.len()
}

/// Drives `Provider::stream` and keeps every event with its moment. `stop_after_first_text` answers `Flow::Stop` to the
/// first text delta.
fn stream_once(provider: &dyn Provider, request: &ChatRequest, stop_after_first_text: bool) -> Run {
    let mut events: Vec<(Instant, Ev)> = Vec::new();
    let mut collector = Collector::new();
    let mut stopped = false;
    let result = provider.stream(
        request,
        &mut |event| {
            let ev = ev_of(&event);
            events.push((Instant::now(), ev.clone()));
            let flow = collector.push(event);
            if stop_after_first_text && !stopped && matches!(ev, Ev::Text(_)) {
                stopped = true;
                return Flow::Stop;
            }
            flow
        },
        &CancelToken::new(),
    );
    Run { events, collector, result }
}

fn stop_name(stop: StopReason) -> &'static str {
    match stop {
        StopReason::EndTurn => "end_turn",
        StopReason::ToolUse => "tool_use",
        StopReason::MaxTokens => "max_tokens",
        StopReason::Refusal => "refusal",
        StopReason::Other => "other",
    }
}

// ---- the tracer -------------------------------------------------------------------------------------------

#[test]
fn a_text_turn_streams_through_the_chokepoint_into_the_collector() {
    let body = fixture("text_turn.sse");
    // The stream is cut one byte into the two-byte character and one byte into the three-byte character.
    let two_byte = after(&body, b"\xC3");
    let three_byte = after(&body, b"\xE2");
    assert!(two_byte < three_byte);
    let fake = FakeHttp::start(vec![Reply::sse_split(&body, &[two_byte, three_byte]).gap(Duration::from_millis(10))]);
    let (provider, _) = make_provider(&fake, Flavor::Gateway);

    let run = stream_once(provider.as_ref(), &request("claude-opus-5-5", "hello"), false);

    assert_eq!(run.evs(), vec![Ev::Usage(25, 1), text("H\u{e9}"), text("llo \u{2713}"), Ev::Usage(25, 12)]);
    assert_eq!(run.collector.text(), "H\u{e9}llo \u{2713}");
    let completion = run.completion();
    assert_eq!(completion.stop_reason, StopReason::EndTurn);
    assert_eq!(stop_name(completion.stop_reason), "end_turn");
    assert_eq!((completion.usage.input_tokens, completion.usage.output_tokens), (25, 12));
    assert_eq!(run.collector.usage(), completion.usage);
    assert_eq!(run.native(), Some(json!([{ "type": "text", "text": "H\u{e9}llo \u{2713}" }])));

    // The request on the wire.
    assert_eq!(fake.accepted(), 1, "one connection for one turn");
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].target, "/v1/messages");
    assert_eq!(seen[0].header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(seen[0].header("content-type"), Some("application/json"));
    assert_eq!(seen[0].header("accept"), Some("text/event-stream"));
    assert_eq!(seen[0].header("host"), Some(format!("127.0.0.1:{}", fake.port()).as_str()));
    assert_eq!(seen[0].header("x-api-key"), Some(canary_key()));
    assert_eq!(seen[0].header("anthropic-beta"), None, "the gateway flavor sends no beta header");
    for (name, value) in &seen[0].headers {
        if name != "x-api-key" {
            assert!(!value.contains(canary_key()), "the key is in the header {name}");
        }
    }
    assert!(!seen[0].body_text().contains(canary_key()), "the key is in the body");
    let sent = json_of(&seen[0].body);
    assert_eq!(sent["model"], json!("claude-opus-5-5"));
    assert_eq!(sent["max_tokens"], json!(1024));
    assert_eq!(sent["stream"], json!(true));
    assert_eq!(sent["messages"], json!([{ "role": "user", "content": [{ "type": "text", "text": "hello" }] }]));
    for member in ["system", "tools", "thinking"] {
        assert!(sent.get(member).is_none(), "the body must not carry {member}: {sent}");
    }
    println!(
        "tracer: a_text_turn_streams_through_the_chokepoint_into_the_collector ok, text {:?}",
        run.collector.text()
    );

    // Streaming, not buffering: the first text delta is in the first chunk, the rest comes 300 ms later.
    let first_delta = after(&body, "\"text\":\"H\u{e9}\"}}\n\n".as_bytes());
    let fake = FakeHttp::start(vec![Reply::sse_split(&body, &[first_delta]).gap(Duration::from_millis(300))]);
    let (provider, _) = make_provider(&fake, Flavor::Gateway);
    let run = stream_once(provider.as_ref(), &request("claude-opus-5-5", "hello"), false);
    let returned = Instant::now();
    assert_eq!(run.completion().stop_reason, StopReason::EndTurn);
    let first_text = run.events.iter().find(|(_, ev)| matches!(ev, Ev::Text(_))).expect("a text event").0;
    let last = run.events.last().expect("events").0;
    let early = last.duration_since(first_text);
    assert!(early >= Duration::from_millis(200), "the first delta came only {early:?} before the last event");
    assert!(
        first_text + Duration::from_millis(200) <= returned,
        "the first delta did not arrive before the call ended"
    );
    println!("tracer streaming: first text delta {early:?} before the last event, call returned afterwards");

    // Stop: a consumer that answers Stop ends the turn as cancelled and the socket closes.
    let fake = FakeHttp::start(vec![Reply::sse_split(&body, &[first_delta]).ending(Ending::Hold)]);
    let (provider, _) = make_provider(&fake, Flavor::Gateway);
    let run = stream_once(provider.as_ref(), &request("claude-opus-5-5", "hello"), true);
    assert_eq!(run.result, Err(AiError::Cancelled));
    assert!(fake.wait_closed(0, Duration::from_secs(1)).is_some(), "the client did not close the socket");
}

// ---- the split property -----------------------------------------------------------------------------------

/// Every fixture of this dialect. Later parts of this file add their streams here.
const FIXTURES: [&str; 3] = ["text_turn.sse", "thinking_tool_turn.sse", "answer_turn.sse"];

#[test]
fn the_fixtures_decode_identically_at_every_split_offset() {
    for name in FIXTURES {
        let bytes = fixture(name);
        let whole = play(&bytes);
        assert!(whole.result.is_ok(), "{name} whole: {:?}", whole.result);
        assert!(!whole.events.is_empty(), "{name} delivered nothing");
        let reference = whole.outcome();
        for at in cuts(bytes.len()) {
            let split = drive(&[&bytes[..at], &bytes[at..]]);
            assert_eq!(split.outcome(), reference, "{name}, split at offset {at}");
        }
        let split = drive(&one_byte_chunks(&bytes));
        assert_eq!(split.outcome(), reference, "{name}, chunks of one byte");
        for seed in 1..=25u64 {
            let split = drive(&random_chunks(&bytes, 0x5eed_0000 + seed));
            assert_eq!(split.outcome(), reference, "{name}, random chunks, seed {}", 0x5eed_0000 + seed);
        }
    }
}

// ---- names ------------------------------------------------------------------------------------------------

#[test]
fn the_adapter_names_its_wire_path_and_version() {
    let adapter = AnthropicAdapter::new(Flavor::Gateway);
    assert_eq!(adapter.wire(), Wire::Anthropic);
    assert_eq!(Wire::Anthropic.tag(), "anthropic");
    assert_eq!(Wire::Anthropic.framing(), Framing::Sse);
    assert_eq!(adapter.flavor(), Flavor::Gateway);
    assert_eq!(AnthropicAdapter::new(Flavor::FirstParty).flavor(), Flavor::FirstParty);
    assert_eq!(Flavor::FirstParty.as_str(), "first_party");
    assert_eq!(Flavor::Gateway.as_str(), "gateway");

    assert_eq!(MESSAGES_PATH, "/v1/messages");
    assert_eq!(API_VERSION, "2023-06-01");
    assert_eq!(DEFAULT_MAX_TOKENS, 16384);
    assert_eq!(MAX_BLOCKS, 64);
    assert_eq!(MAX_TURN_BYTES, 8 * 1024 * 1024);

    let req = request("claude-opus-5-5", "hello");
    let caps = adapter.capabilities("claude-opus-5-5", Locality::Cloud);
    let wire = adapter.request(&req, &caps).expect("a request");
    assert_eq!(wire.path, "/v1/messages");
    assert_eq!(wire.path, MESSAGES_PATH);
    let names: Vec<&str> = wire.headers.iter().map(|header| header.name()).collect();
    assert_eq!(names, vec!["anthropic-version"], "the gateway flavor adds the version header only");

    assert!(caps.tools);
    assert_eq!(caps.locality, Locality::Cloud);
    assert_eq!(adapter.capabilities("claude-opus-5-5", Locality::Loopback).locality, Locality::Loopback);
}

// ---- builders for the streams that are not fixtures --------------------------------------------------------

/// A stream from events. Each value is one event; its `type` member is also its event name.
fn sse(events: &[Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for event in events {
        let name = event["type"].as_str().expect("an event has a type");
        let data = serde_json::to_string(event).expect("JSON");
        out.extend_from_slice(format!("event: {name}\ndata: {data}\n\n").as_bytes());
    }
    out
}

fn message_start(input: u64, output: u64) -> Value {
    json!({ "type": "message_start", "message": {
        "id": "msg_TEST", "type": "message", "role": "assistant", "model": "claude-opus-5-5", "content": [],
        "stop_reason": null, "stop_sequence": null,
        "usage": { "input_tokens": input, "output_tokens": output }
    } })
}

fn block_start(index: u32, block: Value) -> Value {
    json!({ "type": "content_block_start", "index": index, "content_block": block })
}

fn block_stop(index: u32) -> Value {
    json!({ "type": "content_block_stop", "index": index })
}

fn delta(index: u32, delta: Value) -> Value {
    json!({ "type": "content_block_delta", "index": index, "delta": delta })
}

fn text_block() -> Value {
    json!({ "type": "text", "text": "" })
}

fn text_delta(index: u32, text: &str) -> Value {
    delta(index, json!({ "type": "text_delta", "text": text }))
}

fn message_delta(stop: &str, output: u64) -> Value {
    json!({ "type": "message_delta", "delta": { "stop_reason": stop, "stop_sequence": null },
            "usage": { "output_tokens": output } })
}

fn message_stop() -> Value {
    json!({ "type": "message_stop" })
}

/// A complete answer of one text block: for the tests that only need the server to say something.
fn short_answer(answer: &str) -> Vec<u8> {
    sse(&[
        message_start(10, 1),
        block_start(0, text_block()),
        text_delta(0, answer),
        block_stop(0),
        message_delta("end_turn", 2),
        message_stop(),
    ])
}

// ---- builders for conversations ---------------------------------------------------------------------------

fn read_file_tool() -> ToolSpec {
    ToolSpec {
        name: "read_file".to_owned(),
        description: "Read a file".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": { "path": { "type": "string" }, "limit": { "type": "integer" } },
            "required": ["path"]
        }),
    }
}

fn said(text: &str) -> Part {
    Part::Text { text: text.to_owned() }
}

fn calling(id: &str, name: &str, args: Value) -> Part {
    Part::ToolCall(ToolCall { id: id.to_owned(), name: name.to_owned(), args })
}

fn assistant(id: &str, parts: Vec<Part>) -> Message {
    Message::new(id, Role::Assistant, parts, 2)
}

fn tool_message(id: &str, call_id: &str, content: &str, is_error: bool) -> Message {
    let result = ToolResult { call_id: call_id.to_owned(), content: content.to_owned(), is_error };
    Message::new(id, Role::Tool, vec![Part::ToolResult(result)], 3)
}

fn with_native(mut message: Message, json: Value) -> Message {
    message.native = Some(Opaque { wire: "anthropic".to_owned(), json });
    message
}

fn conversation(messages: Vec<Message>) -> ChatRequest {
    let mut request = ChatRequest::new("claude-opus-5-5");
    request.messages = messages;
    request
}

/// The body the adapter writes for `req`, with the capabilities it reports for the model of the request.
fn wire_body(flavor: Flavor, req: &ChatRequest) -> Value {
    let adapter = AnthropicAdapter::new(flavor);
    let caps = adapter.capabilities(&req.model, Locality::Cloud);
    json_of(&adapter.request(req, &caps).expect("a request").body)
}

fn wire_messages(req: &ChatRequest) -> Value {
    wire_body(Flavor::Gateway, req)["messages"].clone()
}

/// The error of a request that must be refused, by its stable class.
fn refused(req: &ChatRequest) -> ErrorClass {
    let adapter = AnthropicAdapter::new(Flavor::Gateway);
    let caps = adapter.capabilities(&req.model, Locality::Cloud);
    adapter.request(req, &caps).expect_err("the request should be refused").class()
}

fn approved_context(provider: &str, text: &str) -> ApprovedContext {
    let gate = Gate::new();
    let destination = Destination { host: "localhost".to_owned(), locality: Locality::Loopback };
    let item = RequestItem::new(ContextKind::Screen, "build pane", SourceText::plain(text));
    let offer = gate.offer(OfferRequest::new(provider, destination, vec![item])).expect("an offer");
    gate.approve(offer.id(), &[1], provider).expect("an approval")
}

// ---- the tool turn ----------------------------------------------------------------------------------------

#[test]
fn a_tool_turn_with_thinking_is_assembled_and_the_native_turn_is_verbatim() {
    let run = play(&fixture("thinking_tool_turn.sse"));

    assert_eq!(
        run.evs(),
        vec![
            Ev::Usage(532, 3),
            Ev::Reasoning("Looking at the request".to_owned()),
            Ev::Reasoning(" and choosing a tool.".to_owned()),
            text("I will read "),
            text("the file."),
            Ev::Usage(532, 87),
            Ev::Tool(
                "toolu_FIXTURE0001".to_owned(),
                "read_file".to_owned(),
                json!({ "path": "src/main.rs", "limit": 20 })
            ),
        ]
    );
    let completion = run.completion();
    assert_eq!(completion.stop_reason, StopReason::ToolUse);
    assert_eq!((completion.usage.input_tokens, completion.usage.output_tokens), (532, 87));
    assert_eq!(
        run.native(),
        Some(json!([
            { "type": "thinking", "thinking": "Looking at the request and choosing a tool.", "signature": "SIG-FIXTURE-0001==" },
            { "type": "text", "text": "I will read the file." },
            { "type": "tool_use", "id": "toolu_FIXTURE0001", "name": "read_file", "input": { "path": "src/main.rs", "limit": 20 } }
        ]))
    );
    assert_eq!(run.calls().len(), 1);
    println!(
        "tool turn: a_tool_turn_with_thinking_is_assembled_and_the_native_turn_is_verbatim ok, {} events, call {:?}",
        run.events.len(),
        run.calls()
    );
}

#[test]
fn omitted_and_redacted_thinking_are_kept_verbatim() {
    let stream = sse(&[
        message_start(30, 1),
        block_start(0, json!({ "type": "thinking", "thinking": "", "signature": "" })),
        delta(0, json!({ "type": "thinking_delta", "thinking": "" })),
        delta(0, json!({ "type": "signature_delta", "signature": "SIG-OMITTED==" })),
        block_stop(0),
        block_start(1, json!({ "type": "redacted_thinking", "data": "REDACTED-DATA==" })),
        block_stop(1),
        block_start(2, text_block()),
        text_delta(2, "Hi"),
        block_stop(2),
        message_delta("end_turn", 5),
        message_stop(),
    ]);
    let run = play(&stream);

    assert_eq!(run.evs(), vec![Ev::Usage(30, 1), text("Hi"), Ev::Usage(30, 5)], "an empty summary is not reasoning");
    assert_eq!(
        run.native(),
        Some(json!([
            { "type": "thinking", "thinking": "", "signature": "SIG-OMITTED==" },
            { "type": "redacted_thinking", "data": "REDACTED-DATA==" },
            { "type": "text", "text": "Hi" }
        ]))
    );
}

#[test]
fn the_next_request_replays_the_native_turn_and_answers_the_tool_call() {
    let fake = FakeHttp::start(vec![
        Reply::sse_bytes(vec![fixture("thinking_tool_turn.sse")]),
        Reply::sse_bytes(vec![fixture("answer_turn.sse")]),
    ]);
    let (provider, _) = make_provider(&fake, Flavor::Gateway);
    let mut req = request("claude-opus-5-5", "Read src/main.rs");
    req.tools.push(read_file_tool());

    let first = stream_once(provider.as_ref(), &req, false);
    let native = first.native().expect("a native turn");
    let message = first.into_message("a1", "claude-opus-5-5");
    assert!(message.native_for("anthropic").is_some());
    req.messages.push(message);
    req.messages.push(tool_message("t1", "toolu_FIXTURE0001", "fn main() {}", false));

    let second = stream_once(provider.as_ref(), &req, false);

    assert_eq!(second.evs(), vec![Ev::Usage(640, 1), text("The file has 20 lines."), Ev::Usage(640, 9)]);
    assert_eq!(second.completion().stop_reason, StopReason::EndTurn);
    let seen = fake.seen();
    assert_eq!(seen.len(), 2);
    let first_body = json_of(&seen[0].body);
    assert_eq!(
        first_body["tools"],
        json!([{ "name": "read_file", "description": "Read a file", "input_schema": read_file_tool().input_schema }])
    );
    let body = json_of(&seen[1].body);
    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 3, "{body}");
    assert_eq!(messages[0], json!({ "role": "user", "content": [{ "type": "text", "text": "Read src/main.rs" }] }));
    assert_eq!(messages[1]["role"], json!("assistant"));
    assert_eq!(messages[1]["content"], native, "the assistant turn is replayed element for element");
    assert_eq!(
        messages[2],
        json!({ "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": "toolu_FIXTURE0001", "content": "fn main() {}" }
        ] })
    );
    assert!(!messages[2].to_string().contains("is_error"), "{}", messages[2]);
}

// ---- rendering the messages -------------------------------------------------------------------------------

#[test]
fn tool_results_share_one_user_message_with_results_first() {
    let calls = vec![
        said("Running both."),
        calling("c1", "read_file", json!({ "path": "a" })),
        calling("c2", "read_file", json!({ "path": "b" })),
    ];
    let req = conversation(vec![
        Message::user_text("u1", "go", 1),
        assistant("a1", calls),
        tool_message("t1", "c1", "first output", false),
        tool_message("t2", "c2", "it failed", true),
        Message::user_text("u2", "thanks", 5),
    ]);
    assert_eq!(
        wire_messages(&req),
        json!([
            { "role": "user", "content": [{ "type": "text", "text": "go" }] },
            { "role": "assistant", "content": [
                { "type": "text", "text": "Running both." },
                { "type": "tool_use", "id": "c1", "name": "read_file", "input": { "path": "a" } },
                { "type": "tool_use", "id": "c2", "name": "read_file", "input": { "path": "b" } }
            ] },
            { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "c1", "content": "first output" },
                { "type": "tool_result", "tool_use_id": "c2", "content": "it failed", "is_error": true },
                { "type": "text", "text": "thanks" }
            ] }
        ])
    );

    // A user message that holds text before a result is written with the result first.
    let mixed = Message::new(
        "u2",
        Role::User,
        vec![
            said("before"),
            Part::ToolResult(ToolResult { call_id: "c1".to_owned(), content: "out".to_owned(), is_error: false }),
        ],
        3,
    );
    let req = conversation(vec![
        Message::user_text("u1", "go", 1),
        assistant("a1", vec![calling("c1", "read_file", json!({}))]),
        mixed,
    ]);
    assert_eq!(
        wire_messages(&req)[2],
        json!({ "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": "c1", "content": "out" },
            { "type": "text", "text": "before" }
        ] })
    );

    // A result with no text is sent as a fixed text: the provider refuses an empty one, also when it is an error.
    for is_error in [false, true] {
        let req = conversation(vec![
            Message::user_text("u1", "go", 1),
            assistant("a1", vec![calling("c1", "read_file", json!({}))]),
            tool_message("t1", "c1", "", is_error),
        ]);
        let block = wire_messages(&req)[2]["content"][0].clone();
        assert_eq!(block["content"], json!("(no output)"));
        assert_eq!(block.get("is_error").is_some(), is_error, "{block}");
    }

    // Arguments that are not a JSON object are sent as an empty object.
    for args in [json!("text"), json!([1, 2]), json!(null), json!(7)] {
        let req = conversation(vec![
            Message::user_text("u1", "go", 1),
            assistant("a1", vec![calling("c1", "read_file", args.clone())]),
            tool_message("t1", "c1", "ok", false),
        ]);
        assert_eq!(wire_messages(&req)[1]["content"][0]["input"], json!({}), "arguments {args}");
    }
}

#[test]
fn empty_messages_are_skipped_and_neighbours_are_merged() {
    // An assistant message with only empty text is dropped, so the two user messages around it become one.
    let req = conversation(vec![
        Message::user_text("u1", "a", 1),
        assistant("a1", vec![said("")]),
        assistant("a2", vec![]),
        Message::user_text("u2", "b", 2),
    ]);
    assert_eq!(
        wire_messages(&req),
        json!([{ "role": "user", "content": [
        { "type": "text", "text": "a" }, { "type": "text", "text": "b" }
    ] }])
    );

    // Two assistant messages in a row are one message; the order of the blocks is kept.
    let req = conversation(vec![
        Message::user_text("u1", "q", 1),
        assistant("a1", vec![said("x")]),
        assistant("a2", vec![said(""), said("y")]),
        Message::user_text("u2", "next", 3),
    ]);
    assert_eq!(
        wire_messages(&req),
        json!([
            { "role": "user", "content": [{ "type": "text", "text": "q" }] },
            { "role": "assistant", "content": [{ "type": "text", "text": "x" }, { "type": "text", "text": "y" }] },
            { "role": "user", "content": [{ "type": "text", "text": "next" }] }
        ])
    );

    // An empty text part inside a message is dropped and a message of empty parts only is no message.
    let req = conversation(vec![
        Message::new("u1", Role::User, vec![said(""), said("kept"), said("")], 1),
        Message::new("u2", Role::User, vec![said("")], 2),
    ]);
    assert_eq!(wire_messages(&req), json!([{ "role": "user", "content": [{ "type": "text", "text": "kept" }] }]));
    let only_empty = conversation(vec![Message::new("u1", Role::User, vec![said("")], 1)]);
    assert_eq!(refused(&only_empty), ErrorClass::InvalidRequest);
}

#[test]
fn a_native_turn_is_checked_and_reconciled_with_the_parts() {
    let thinking = json!({ "type": "thinking", "thinking": "T", "signature": "SIG==" });
    let words = json!({ "type": "text", "text": "visible" });
    let keep = json!({ "type": "tool_use", "id": "toolu_KEEP", "name": "read_file", "input": { "path": "k" } });
    let gone = json!({ "type": "tool_use", "id": "toolu_GONE", "name": "read_file", "input": { "path": "g" } });
    let parts = || vec![said("visible"), calling("toolu_KEEP", "read_file", json!({ "path": "k" }))];
    let rendered_from_parts = json!([words.clone(), keep.clone()]);
    let around = |assistant_message: Message| {
        conversation(vec![
            Message::user_text("u1", "go", 1),
            assistant_message,
            tool_message("t1", "toolu_KEEP", "ok", false),
        ])
    };

    // A native turn of another wire, of another shape or with an unknown block is ignored: the parts are written.
    let mut other_wire = assistant("a1", parts());
    other_wire.native = Some(Opaque { wire: "openai_chat".to_owned(), json: json!([thinking.clone()]) });
    let ignored = [
        other_wire,
        with_native(assistant("a1", parts()), json!({ "type": "thinking" })),
        with_native(assistant("a1", parts()), json!([thinking.clone(), { "type": "server_tool_use", "id": "srv_1" }])),
        with_native(assistant("a1", parts()), json!([thinking.clone(), { "no_type": true }])),
        with_native(assistant("a1", parts()), json!([thinking.clone(), "text"])),
        with_native(assistant("a1", parts()), json!([])),
    ];
    for (n, message) in ignored.into_iter().enumerate() {
        let content = wire_messages(&around(message))[1]["content"].clone();
        assert_eq!(content, rendered_from_parts, "ignored native turn {n}");
    }

    // A tool use whose id is not among the call parts is removed; every other block stays, in order.
    let message = with_native(assistant("a1", parts()), json!([thinking.clone(), words.clone(), keep.clone(), gone]));
    assert_eq!(wire_messages(&around(message))[1]["content"], json!([thinking.clone(), words.clone(), keep.clone()]));

    // A call part that is not in the native turn is appended as a tool use at the end.
    let new_call = json!({ "type": "tool_use", "id": "toolu_NEW", "name": "read_file", "input": { "path": "n" } });
    let message = with_native(
        assistant("a1", vec![said("visible"), calling("toolu_NEW", "read_file", json!({ "path": "n" }))]),
        json!([thinking.clone(), words.clone()]),
    );
    let req =
        conversation(vec![Message::user_text("u1", "go", 1), message, tool_message("t1", "toolu_NEW", "ok", false)]);
    assert_eq!(wire_messages(&req)[1]["content"], json!([thinking, words, new_call]));

    // No tool use is sent without its result and no result without its use.
    let req = conversation(vec![
        Message::user_text("u1", "q", 1),
        assistant("a1", vec![said("calling"), calling("lonely", "read_file", json!({}))]),
        Message::user_text("u2", "next", 3),
    ]);
    assert_eq!(
        wire_messages(&req),
        json!([
            { "role": "user", "content": [{ "type": "text", "text": "q" }] },
            { "role": "assistant", "content": [{ "type": "text", "text": "calling" }] },
            { "role": "user", "content": [{ "type": "text", "text": "next" }] }
        ])
    );
    let req = conversation(vec![
        Message::user_text("u1", "q", 1),
        assistant("a1", vec![said("done")]),
        tool_message("t1", "orphan", "late", false),
        Message::user_text("u2", "next", 3),
    ]);
    assert_eq!(
        wire_messages(&req),
        json!([
            { "role": "user", "content": [{ "type": "text", "text": "q" }] },
            { "role": "assistant", "content": [{ "type": "text", "text": "done" }] },
            { "role": "user", "content": [{ "type": "text", "text": "next" }] }
        ])
    );
}

// ---- what the request asks for ----------------------------------------------------------------------------

#[test]
fn thinking_and_binding_controls_go_to_first_party_adaptive_models_only() {
    assert_eq!(BINDING_BETA, "thinking-binding-controls-2026-08-01");
    let rows: [(Flavor, &str, bool); 7] = [
        (Flavor::FirstParty, "claude-opus-5-5", true),
        (Flavor::FirstParty, "anthropic/claude-sonnet-5-5", true),
        (Flavor::FirstParty, "claude-haiku-5-5", true),
        (Flavor::FirstParty, "claude-haiku-4-5", false),
        (Flavor::FirstParty, "claude-3-5-sonnet-20241022", false),
        (Flavor::FirstParty, "qwen2.5-coder", false),
        (Flavor::Gateway, "claude-opus-5-5", false),
    ];
    let fake = FakeHttp::start(vec![Reply::sse_bytes(vec![short_answer("ok")])]);
    for (flavor, model, _) in rows {
        let (provider, _) = make_provider(&fake, flavor);
        let run = stream_once(provider.as_ref(), &request(model, "hi"), false);
        assert!(run.result.is_ok(), "{flavor:?} {model}: {:?}", run.result);
    }
    let seen = fake.seen();
    assert_eq!(seen.len(), rows.len());
    for ((flavor, model, expected), seen) in rows.iter().zip(&seen) {
        let body = json_of(&seen.body);
        assert_eq!(seen.header("anthropic-version"), Some("2023-06-01"), "{flavor:?} {model}");
        if *expected {
            assert_eq!(
                body["thinking"],
                json!({ "type": "adaptive", "display": "summarized",
                        "block_binding": { "prefix_mismatch_behavior": "drop_block" } }),
                "{flavor:?} {model}"
            );
            assert_eq!(
                seen.header("anthropic-beta"),
                Some("thinking-binding-controls-2026-08-01"),
                "{flavor:?} {model}"
            );
        } else {
            assert!(body.get("thinking").is_none(), "{flavor:?} {model}: {body}");
            assert_eq!(seen.header("anthropic-beta"), None, "{flavor:?} {model}");
        }
    }

    // The flavor follows the address: the vendor's host on its default https port, and nothing else.
    let flavor_of = |url: &str| {
        let target = Target::parse(url, false).unwrap_or_else(|refusal| panic!("{url}: {refusal:?}"));
        AnthropicAdapter::for_origin(target.origin()).flavor()
    };
    assert_eq!(flavor_of("https://api.anthropic.com"), Flavor::FirstParty);
    assert_eq!(flavor_of("https://api.anthropic.com/v1"), Flavor::FirstParty);
    assert_eq!(flavor_of("https://api.anthropic.com:443"), Flavor::FirstParty);
    assert_eq!(flavor_of("https://API.Anthropic.com"), Flavor::FirstParty);
    for url in [
        "https://api.anthropic.com:8443",
        "https://api.anthropic.com.example",
        "https://xapi.anthropic.com",
        "https://anthropic.com",
        "http://127.0.0.1:8080",
        "https://ai-gateway.vercel.sh",
    ] {
        assert_eq!(flavor_of(url), Flavor::Gateway, "{url}");
    }
}

/// Whether any object inside `value` has a member called `key`.
fn has_member(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(map) => map.contains_key(key) || map.values().any(|inner| has_member(inner, key)),
        Value::Array(items) => items.iter().any(|inner| has_member(inner, key)),
        _ => false,
    }
}

#[test]
fn no_server_side_feature_is_ever_requested() {
    const BODY_MEMBERS: [&str; 7] = ["model", "max_tokens", "stream", "system", "messages", "tools", "thinking"];
    const MODELS: [&str; 7] = [
        "claude-opus-5-5",
        "anthropic/claude-sonnet-5-5",
        "claude-haiku-5-5",
        "claude-fable-5-1",
        "claude-haiku-4-5",
        "claude-3-5-sonnet-20241022",
        "qwen2.5-coder",
    ];
    let approved = approved_context("anthropic-test", "ls output");
    let mut combinations = 0;
    for flavor in [Flavor::FirstParty, Flavor::Gateway] {
        for model in MODELS {
            for with_tools in [false, true] {
                for system in ["", "be brief"] {
                    for with_context in [false, true] {
                        let mut req = ChatRequest::new(model);
                        req.system = system.to_owned();
                        req.max_output_tokens = Some(2048);
                        if with_context {
                            let note = ContextNote::for_approved(&approved, None);
                            req.messages.push(Message::new("u1", Role::User, vec![Part::Context(note), said("hi")], 1));
                            req.context.push(approved.clone());
                        } else {
                            req.messages.push(Message::user_text("u1", "hi", 1));
                        }
                        if with_tools {
                            req.tools.push(read_file_tool());
                        }
                        let label =
                            format!("{flavor:?} {model} tools={with_tools} system={system:?} context={with_context}");
                        let body = wire_body(flavor, &req);
                        for member in body.as_object().expect("an object").keys() {
                            assert!(BODY_MEMBERS.contains(&member.as_str()), "{label}: unexpected member {member}");
                        }
                        for required in ["model", "max_tokens", "stream", "messages"] {
                            assert!(body.get(required).is_some(), "{label}: {required} is missing");
                        }
                        assert_eq!(body.get("system").is_some(), !system.is_empty(), "{label}");
                        assert_eq!(body.get("tools").is_some(), with_tools, "{label}");
                        if let Some(thinking) = body.get("thinking") {
                            for member in thinking.as_object().expect("an object").keys() {
                                assert!(
                                    ["type", "display", "block_binding"].contains(&member.as_str()),
                                    "{label}: {member}"
                                );
                            }
                            assert_eq!(flavor, Flavor::FirstParty, "{label}: only the vendor's host gets thinking");
                        }
                        for tool in body.get("tools").and_then(Value::as_array).into_iter().flatten() {
                            let mut keys: Vec<&String> = tool.as_object().expect("an object").keys().collect();
                            keys.sort();
                            assert_eq!(keys, ["description", "input_schema", "name"], "{label}");
                        }
                        for word in ["cache_control", "citations"] {
                            assert!(!has_member(&body, word), "{label}: {word} in {body}");
                        }
                        combinations += 1;
                    }
                }
            }
        }
    }
    assert_eq!(combinations, 2 * 7 * 2 * 2 * 2);
}

#[test]
fn the_request_must_end_with_a_user_message_and_carry_valid_tools() {
    let user = || Message::user_text("u1", "hello", 1);
    let tool = |name: &str, schema: Value| ToolSpec {
        name: name.to_owned(),
        description: "d".to_owned(),
        input_schema: schema,
    };
    let mut cases: Vec<(&str, ChatRequest)> = Vec::new();
    cases.push(("no messages", conversation(vec![])));
    cases.push(("ends with the assistant", conversation(vec![user(), assistant("a1", vec![said("done")])])));
    cases.push(("only an assistant", conversation(vec![assistant("a1", vec![said("done")])])));
    let mut empty_model = conversation(vec![user()]);
    empty_model.model = String::new();
    cases.push(("an empty model", empty_model));
    let mut zero = conversation(vec![user()]);
    zero.max_output_tokens = Some(0);
    cases.push(("an output bound of zero", zero));
    for (label, bad) in [
        ("a tool with an empty name", tool("", json!({ "type": "object" }))),
        ("a schema of another type", tool("t", json!({ "type": "string" }))),
        ("a schema that is an array", tool("t", json!([]))),
        ("a schema without a type", tool("t", json!({ "properties": {} }))),
        ("a schema that is text", tool("t", json!("object"))),
    ] {
        let mut req = conversation(vec![user()]);
        req.tools.push(read_file_tool());
        req.tools.push(bad);
        cases.push((label, req));
    }

    let fake = FakeHttp::start(vec![Reply::sse_bytes(vec![short_answer("never sent")])]);
    let (provider, _) = make_provider(&fake, Flavor::Gateway);
    for (label, req) in &cases {
        assert_eq!(refused(req), ErrorClass::InvalidRequest, "{label}");
        let run = stream_once(provider.as_ref(), req, false);
        assert_eq!(run.error().class(), ErrorClass::InvalidRequest, "{label}: {:?}", run.result);
    }
    assert_eq!(fake.accepted(), 0, "a refused request opens no connection");

    // A valid tool next to a request that ends with a user message goes through.
    let mut good = conversation(vec![user()]);
    good.tools.push(tool("t", json!({ "type": "object", "properties": {} })));
    assert!(wire_body(Flavor::Gateway, &good).get("tools").is_some());
}

#[test]
fn max_tokens_is_always_sent_and_never_above_the_known_limit() {
    let sent = |model: &str, limit: Option<u32>| {
        let mut req = request(model, "hi");
        req.max_output_tokens = limit;
        wire_body(Flavor::Gateway, &req)["max_tokens"].clone()
    };
    assert_eq!(sent("claude-opus-5-5", None), json!(16384));
    assert_eq!(sent("qwen2.5-coder", None), json!(16384));
    assert_eq!(sent("claude-opus-5-5", Some(8192)), json!(8192));
    assert_eq!(sent("claude-opus-5-5", Some(500_000)), json!(128_000));
    assert_eq!(sent("anthropic/claude-sonnet-5-5", Some(500_000)), json!(128_000));
    assert_eq!(sent("qwen2.5-coder", Some(500_000)), json!(500_000));
    assert_eq!(sent("claude-haiku-4-5", Some(500_000)), json!(500_000));
}
