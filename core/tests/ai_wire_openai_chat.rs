//! The Chat Completions adapter, proved against golden streams and an in-process server, with no outside network.
//!
//! The streams in `tests/fixtures/ai/openai_chat/` are written by hand from the documented stream format. They are not
//! recordings of a real server; recordings from real local servers are collected separately and may become new
//! fixtures. Every request in this file goes to `FakeHttp`, an HTTP/1.1 server in this process. A stream that is driven
//! directly goes through `SseDecoder` and the turn of the adapter the way the shared driver does it, so a split of the
//! bytes can be chosen at any offset. A failing case names its offset, its chunk size or its seed.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::Xorshift64;
use common::fake_http::{FakeHttp, Reply};
use holzwave_core::ai::http::{Frame, Http, HttpConfig, NetworkPolicy, Target};
use holzwave_core::ai::provider::{
    AiError, CancelToken, ChatRequest, Collector, Completion, ErrorClass, Flow, Provider, StreamEvent,
};
use holzwave_core::ai::sse::SseDecoder;
use holzwave_core::ai::types::{Capabilities, Locality, Message, Part, Role, StopReason, Usage};
use holzwave_core::ai::wire::openai_chat::{CHAT_PATH, OpenAiChatAdapter};
use holzwave_core::ai::wire::{Endpoint, HttpProvider, Wire, WireAdapter};
use serde_json::{Value, json};

/// The text that a server must never be able to put on the screen through an error or a `Debug` line.
const CANARY: &str = "CANARY-SERVER-TEXT";

/// The bytes of a fixture of this dialect.
macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!("fixtures/ai/openai_chat/", $name))
    };
}

// ---- helpers ----------------------------------------------------------------------------------------------

/// What a stream reported, as plain values that can be compared.
#[derive(Clone, Debug, PartialEq)]
enum Seen {
    Text(String),
    Reasoning(String),
    Call { id: String, name: String, args: Value },
    Usage(u32, u32),
}

impl Seen {
    fn of(event: &StreamEvent) -> Seen {
        match event {
            StreamEvent::TextDelta(text) => Seen::Text(text.clone()),
            StreamEvent::ReasoningDelta(text) => Seen::Reasoning(text.clone()),
            StreamEvent::ToolCall(call) => {
                Seen::Call { id: call.id.clone(), name: call.name.clone(), args: call.args.clone() }
            }
            StreamEvent::Usage(usage) => Seen::Usage(usage.input_tokens, usage.output_tokens),
        }
    }
}

fn text(value: &str) -> Seen {
    Seen::Text(value.to_owned())
}

/// What one pass over a stream delivered and how the turn ended.
#[derive(Debug, PartialEq)]
struct Run {
    events: Vec<Seen>,
    result: Result<Completion, AiError>,
}

impl Run {
    fn completion(&self) -> &Completion {
        self.result.as_ref().unwrap_or_else(|error| panic!("the turn failed: {error:?} after {:?}", self.events))
    }

    fn error(&self) -> &AiError {
        self.result.as_ref().expect_err("the turn should have failed")
    }
}

/// Drives the chunks through a decoder and a fresh turn, exactly as the shared driver does: an error from the turn is
/// kept and answered with a stop, an error from the decoder becomes a protocol error, and a kept error wins over the
/// verdict of the end. `answer` gets the number of the event and returns what the consumer says.
fn drive<T: AsRef<[u8]>>(chunks: &[T], answer: &mut dyn FnMut(usize) -> Flow) -> Run {
    let mut turn = OpenAiChatAdapter::new().turn();
    let mut decoder = SseDecoder::new();
    let mut events: Vec<Seen> = Vec::new();
    let mut failure: Option<AiError> = None;
    let mut out = |event: StreamEvent| {
        events.push(Seen::of(&event));
        answer(events.len() - 1)
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
    let result = match failure {
        Some(error) => Err(error),
        None => turn.finish(end, &mut out),
    };
    Run { events, result }
}

fn run_chunks<T: AsRef<[u8]>>(chunks: &[T]) -> Run {
    drive(chunks, &mut |_| Flow::Continue)
}

fn play(bytes: &[u8]) -> Run {
    run_chunks(&[bytes])
}

/// Every way of cutting `bytes` that the split tests use: at every single offset, in fixed sizes and in seeded random
/// sizes. The label names the cut.
fn chunkings(bytes: &[u8], seed: u64) -> Vec<(String, Vec<Vec<u8>>)> {
    let mut all = Vec::new();
    for at in 1..bytes.len() {
        all.push((format!("split at offset {at}"), vec![bytes[..at].to_vec(), bytes[at..].to_vec()]));
    }
    for size in [1, 2, 3, 5, 7, 64] {
        all.push((format!("chunks of {size} bytes"), bytes.chunks(size).map(<[u8]>::to_vec).collect()));
    }
    for n in 0..25 {
        let mut random = Xorshift64::new(seed + n);
        let mut chunks = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            let end = (at + 1 + random.below(40)).min(bytes.len());
            chunks.push(bytes[at..end].to_vec());
            at = end;
        }
        all.push((format!("random chunks, seed {}", seed + n), chunks));
    }
    all
}

/// Inline events: every string becomes the data of one event.
fn stream_of(data: &[&str]) -> Vec<u8> {
    data.iter().flat_map(|data| format!("data: {data}\n\n").into_bytes()).collect()
}

fn caps() -> Capabilities {
    Capabilities::conservative(Locality::Loopback)
}

fn request_with(text: &str) -> ChatRequest {
    let mut request = ChatRequest::new("test-model");
    request.system = "You are a terminal assistant.".to_owned();
    request.messages.push(Message::user_text("u1", text, 1));
    request.max_output_tokens = Some(256);
    request
}

/// A provider on the fake server; `base_path` is the path of the profile's base address.
fn provider(fake: &FakeHttp, base_path: &str) -> Arc<dyn Provider> {
    let target = Target::parse(&format!("{}{base_path}", fake.base()), false).expect("a loopback base address");
    let endpoint = Endpoint::new(Wire::OpenAiChat, target, NetworkPolicy::new(false, false));
    let http = Arc::new(Http::new(HttpConfig::standard()));
    Arc::new(HttpProvider::new("local", endpoint, Box::new(OpenAiChatAdapter::new()), http))
}

fn json_of(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("JSON")
}

// ---- the tracer -------------------------------------------------------------------------------------------

#[test]
fn a_text_turn_streams_through_the_fake_server_into_a_collector() {
    let body = fixture!("text.sse");
    let fake = FakeHttp::start(vec![
        Reply::sse_split(body, &[body.len() / 3, 2 * body.len() / 3]).gap(Duration::from_millis(20)),
    ]);
    let provider = provider(&fake, "/v1");
    let request = request_with("hello");

    let mut collector = Collector::new();
    let mut events = Vec::new();
    let completion = provider
        .stream(
            &request,
            &mut |event| {
                events.push(Seen::of(&event));
                collector.push(event)
            },
            &CancelToken::new(),
        )
        .expect("a completed turn");

    assert_eq!(events, vec![text("Hel"), text("lo \u{e9} \u{65e5}\u{672c} \u{1f600}"), Seen::Usage(11, 5)]);
    assert_eq!(completion.stop_reason, StopReason::EndTurn);
    assert_eq!(completion.usage, Usage { input_tokens: 11, output_tokens: 5 });
    assert!(completion.native.is_none(), "this dialect keeps no provider-native turn");
    assert_eq!(collector.text(), "Hello \u{e9} \u{65e5}\u{672c} \u{1f600}");

    assert_eq!(fake.accepted(), 1, "one connection for one turn");
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].target, "/v1/chat/completions");
    assert_eq!(seen[0].header("content-type"), Some("application/json"));
    assert_eq!(seen[0].header("accept"), Some("text/event-stream"));
    assert_eq!(seen[0].header("authorization"), None, "an adapter adds no credential header");
    assert_eq!(seen[0].header("accept-encoding"), None, "the stream is not compressed");
    assert_eq!(json_of(&seen[0].body), json_of(fixture!("request_basic.json")));
    println!("tracer: a_text_turn_streams_through_the_fake_server_into_a_collector ok, text {:?}", collector.text());
}

// ---- the request ------------------------------------------------------------------------------------------

#[test]
fn the_request_body_is_pinned_and_a_pure_function_of_the_request() {
    let adapter = OpenAiChatAdapter::new();
    let request = request_with("hello");
    let wire = adapter.request(&request, &caps()).expect("a request");

    assert_eq!(wire.path, CHAT_PATH);
    assert_eq!(wire.path, "/chat/completions");
    assert!(wire.headers.is_empty(), "the adapter adds no header");
    let body = json_of(&wire.body);
    assert_eq!(body, json_of(fixture!("request_basic.json")));
    for key in ["tools", "tool_choice", "temperature", "top_p", "store", "user", "n"] {
        assert!(body.get(key).is_none(), "the body must not carry {key}: {body}");
    }
    let again = OpenAiChatAdapter::default().request(&request, &caps()).expect("a request");
    assert_eq!(wire.body, again.body, "the same input gives the same bytes");

    // Optional keys are left out when they are not set.
    let mut request = request_with("hello");
    request.max_output_tokens = None;
    request.system = String::new();
    let body = json_of(&adapter.request(&request, &caps()).expect("a request").body);
    assert!(body.get("max_tokens").is_none(), "{body}");
    assert_eq!(body["messages"], json!([{ "role": "user", "content": "hello" }]));
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["stream_options"], json!({ "include_usage": true }));

    // No messages is an invalid request, with a stable class.
    let empty = ChatRequest::new("test-model");
    let error = adapter.request(&empty, &caps()).expect_err("nothing to send");
    assert_eq!(error.class(), ErrorClass::InvalidRequest);
    assert_eq!(error.class().as_str(), "invalid_request");
    let mut only_system = ChatRequest::new("test-model");
    only_system.system = "rules".to_owned();
    assert_eq!(adapter.request(&only_system, &caps()).expect_err("no message").class(), ErrorClass::InvalidRequest);

    // An assistant message with nothing in it is dropped, so the two user messages around it become one; two user
    // messages in a row are one message whose texts are joined by one blank line.
    let mut request = ChatRequest::new("test-model");
    request.messages.push(Message::user_text("u1", "first", 1));
    request.messages.push(Message::new("a1", Role::Assistant, Vec::new(), 2));
    request.messages.push(Message::new("a2", Role::Assistant, vec![Part::Text { text: String::new() }], 3));
    request.messages.push(Message::user_text("u2", "second", 4));
    request.messages.push(Message::user_text("u3", "third", 5));
    let body = json_of(&adapter.request(&request, &caps()).expect("a request").body);
    assert_eq!(body["messages"], json!([{ "role": "user", "content": "first\n\nsecond\n\nthird" }]));
}

// ---- the stream -------------------------------------------------------------------------------------------

#[test]
fn every_split_of_the_text_streams_gives_the_same_events() {
    for (name, bytes) in
        [("text.sse", &fixture!("text.sse")[..]), ("text_gateway.sse", &fixture!("text_gateway.sse")[..])]
    {
        let whole = play(bytes);
        assert!(whole.result.is_ok(), "{name} whole: {:?}", whole.result);
        for (label, chunks) in chunkings(bytes, 0x5eed_0001) {
            let split = run_chunks(&chunks);
            assert_eq!(split, whole, "{name}, {label}");
        }
    }
    let whole = play(fixture!("text.sse"));
    assert_eq!(whole.events, vec![text("Hel"), text("lo \u{e9} \u{65e5}\u{672c} \u{1f600}"), Seen::Usage(11, 5)]);
}

#[test]
fn gateway_comments_unknown_fields_and_empty_deltas_change_nothing() {
    let run = play(fixture!("text_gateway.sse"));
    assert_eq!(run.events, vec![text("Hi "), text("there"), Seen::Usage(20, 4)]);
    let completion = run.completion();
    assert_eq!(completion.stop_reason, StopReason::EndTurn);
    assert_eq!(completion.usage, Usage { input_tokens: 20, output_tokens: 4 });
    assert!(run.events.iter().all(|event| *event != text("")), "an empty delta produced an event");
}

const HI: &str = r#"{"choices":[{"index":0,"delta":{"content":"Hi"}}]}"#;
const FINISH: &str = r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;

#[test]
fn usage_comes_from_the_finish_chunk_or_a_trailing_chunk_or_not_at_all() {
    // In the finish chunk.
    let in_finish = r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#;
    let run = play(&stream_of(&[HI, in_finish, "[DONE]"]));
    assert_eq!(run.events, vec![text("Hi"), Seen::Usage(7, 3)]);
    assert_eq!(run.completion().usage, Usage { input_tokens: 7, output_tokens: 3 });

    // In a trailing chunk with an empty list of choices.
    let trailing = r#"{"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":2,"total_tokens":11}}"#;
    let run = play(&stream_of(&[HI, FINISH, trailing, "[DONE]"]));
    assert_eq!(run.events, vec![text("Hi"), Seen::Usage(9, 2)]);
    assert_eq!(run.completion().usage, Usage { input_tokens: 9, output_tokens: 2 });

    // Not reported at all: no event, and zero means not reported.
    let run = play(&stream_of(&[HI, FINISH, "[DONE]"]));
    assert_eq!(run.events, vec![text("Hi")]);
    assert_eq!(run.completion().usage, Usage::default());

    // Only the completion count.
    let only_completion = r#"{"choices":[],"usage":{"completion_tokens":4}}"#;
    let run = play(&stream_of(&[HI, FINISH, only_completion]));
    assert_eq!(run.events, vec![text("Hi"), Seen::Usage(0, 4)]);
    assert_eq!(run.completion().usage, Usage { input_tokens: 0, output_tokens: 4 });

    // A usage member that is null, or holds no count, is ignored.
    for ignored in
        [r#"{"choices":[],"usage":null}"#, r#"{"choices":[],"usage":{}}"#, r#"{"choices":[],"usage":"none"}"#]
    {
        let run = play(&stream_of(&[HI, FINISH, ignored]));
        assert_eq!(run.events, vec![text("Hi")], "{ignored}");
        assert_eq!(run.completion().usage, Usage::default(), "{ignored}");
    }

    // A count above the largest saturates instead of wrapping.
    let huge = r#"{"choices":[],"usage":{"prompt_tokens":5000000000,"completion_tokens":18446744073709551615}}"#;
    let run = play(&stream_of(&[HI, FINISH, huge]));
    assert_eq!(run.events, vec![text("Hi"), Seen::Usage(u32::MAX, u32::MAX)]);

    // Two usage chunks: the last one wins.
    let first = r#"{"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
    let second = r#"{"choices":[],"usage":{"prompt_tokens":2,"completion_tokens":6}}"#;
    let run = play(&stream_of(&[HI, first, FINISH, second]));
    assert_eq!(run.events, vec![text("Hi"), Seen::Usage(1, 1), Seen::Usage(2, 6)]);
    assert_eq!(run.completion().usage, Usage { input_tokens: 2, output_tokens: 6 });
}

#[test]
fn finish_reasons_and_the_terminal_marker_decide_how_a_stream_ends() {
    // A finish chunk without the closing marker, and a clean close, is a completed turn.
    let run = play(&stream_of(&[HI, FINISH]));
    assert_eq!(run.completion().stop_reason, StopReason::EndTurn);

    // The closing marker after text, with no finish chunk, is a completed turn of an unknown kind.
    let run = play(&stream_of(&[HI, "[DONE]"]));
    assert_eq!(run.events, vec![text("Hi")]);
    assert_eq!(run.completion().stop_reason, StopReason::Other);

    // Text and a clean close, with neither, is an error and never `Ok`.
    let run = play(&stream_of(&[HI]));
    assert_eq!(run.events, vec![text("Hi")]);
    assert_eq!(run.error().class(), ErrorClass::Protocol);
    let nothing: [&[u8]; 0] = [];
    assert_eq!(run_chunks(&nothing).error().class(), ErrorClass::Protocol);

    // A data line that is not JSON is a protocol error that does not show the line.
    let line = format!("this is not JSON {CANARY}");
    let run = play(&stream_of(&[HI, &line]));
    assert_eq!(run.error().class(), ErrorClass::Protocol);
    let shown = format!("{} | {:?}", run.error(), run.error());
    assert!(!shown.contains(CANARY) && !shown.contains("not JSON"), "{shown}");

    // Events after the closing marker are ignored, even a broken one.
    let late = r#"{"choices":[{"index":0,"delta":{"content":"late"}}]}"#;
    let run = play(&stream_of(&[HI, FINISH, "[DONE]", late, "not json", r#"{"usage":{"prompt_tokens":99}}"#]));
    assert_eq!(run.events, vec![text("Hi")]);
    assert!(run.result.is_ok(), "{:?}", run.result);

    // The finish reasons.
    for (reason, expected) in [
        ("stop", StopReason::EndTurn),
        ("length", StopReason::MaxTokens),
        ("content_filter", StopReason::Refusal),
        ("something_new", StopReason::Other),
    ] {
        let chunk = format!(r#"{{"choices":[{{"index":0,"delta":{{}},"finish_reason":"{reason}"}}]}}"#);
        let run = play(&stream_of(&[HI, &chunk]));
        assert_eq!(run.completion().stop_reason, expected, "{reason}");
    }

    // A null or empty finish reason is no finish.
    for open in [r#""finish_reason":null"#, r#""finish_reason":"""#] {
        let chunk = format!(r#"{{"choices":[{{"index":0,"delta":{{}},{open}}}]}}"#);
        assert_eq!(play(&stream_of(&[HI, &chunk])).error().class(), ErrorClass::Protocol, "{open}");
    }

    // A stream cut inside an event is an error, even after the finish chunk.
    let mut cut = stream_of(&[HI, FINISH]);
    cut.extend_from_slice(b"data: {\"choices\":[{\"ind");
    let run = play(&cut);
    assert_eq!(run.error().class(), ErrorClass::Protocol);
    let mut cut = stream_of(&[HI, FINISH]);
    cut.truncate(cut.len() - 1);
    assert_eq!(run_chunks(&[cut]).error().class(), ErrorClass::Protocol, "the finish event lost its blank line");

    // Another choice than the first is not read.
    let second = r#"{"choices":[{"index":1,"delta":{"content":"other"},"finish_reason":"stop"}]}"#;
    let run = play(&stream_of(&[HI, second, FINISH]));
    assert_eq!(run.events, vec![text("Hi")]);
}
