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
use holzwave_core::ai::http::{Frame, Framing, Http, HttpConfig, NetworkPolicy, SecretHeader, Target};
use holzwave_core::ai::provider::{
    AiError, CancelToken, ChatRequest, Collector, Completion, Flow, Provider, StreamEvent,
};
use holzwave_core::ai::sse::SseDecoder;
use holzwave_core::ai::types::{ApiKey, Locality, Message, MessageStatus, StopReason};
use holzwave_core::ai::wire::anthropic::{
    API_VERSION, AnthropicAdapter, DEFAULT_MAX_TOKENS, Flavor, MAX_BLOCKS, MAX_TURN_BYTES, MESSAGES_PATH,
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
const FIXTURES: [&str; 1] = ["text_turn.sse"];

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
