//! The Chat Completions adapter, proved against golden streams and an in-process server, with no outside network.
//!
//! The streams in `tests/fixtures/ai/openai_chat/` are written by hand from the documented stream format. They are not
//! recordings of a real server; recordings from real local servers are collected separately and may become new
//! fixtures. Every request in this file goes to `FakeHttp`, an HTTP/1.1 server in this process. A stream that is driven
//! directly goes through `SseDecoder` and the turn of the adapter the way the shared driver does it, so a split of the
//! bytes can be chosen at any offset. A failing case names its offset, its chunk size or its seed.

mod common;

use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

use common::Xorshift64;
use common::fake_http::{FakeHttp, Reply};
use holzwave_core::ai::agent::TRIMMED_RESULT;
use holzwave_core::ai::gate::{ApprovedContext, Destination, Gate, OfferRequest, RequestItem};
use holzwave_core::ai::http::{Frame, Http, HttpConfig, NetworkPolicy, Rejection, SecretHeader, Target};
use holzwave_core::ai::provider::{
    AiError, CancelToken, ChatRequest, Collector, Completion, ErrorClass, Flow, Provider, StreamEvent,
};
use holzwave_core::ai::redact::SourceText;
use holzwave_core::ai::sse::SseDecoder;
use holzwave_core::ai::store::NOT_STORED;
use holzwave_core::ai::types::{
    ApiKey, Capabilities, Consent, ContextKind, ContextNote, Locality, Message, MessageStatus, NoteItem, Part, Role,
    StopReason, ToolCall, ToolResult, ToolSpec, Usage,
};
use holzwave_core::ai::wire::openai_chat::{CHAT_PATH, ChatTurn, OpenAiChatAdapter};
use holzwave_core::ai::wire::{Endpoint, HttpProvider, Wire, WireAdapter, WireTurn};
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

/// What one pass over a stream delivered and how the turn ended. `before_finish` is the number of events the consumer
/// had when the stream ended, before the turn was asked to finish: tool calls are never among them.
#[derive(Clone, Debug, PartialEq)]
struct Run {
    events: Vec<Seen>,
    before_finish: usize,
    result: Result<Completion, AiError>,
}

impl Run {
    fn completion(&self) -> &Completion {
        self.result.as_ref().unwrap_or_else(|error| panic!("the turn failed: {error:?} after {:?}", self.events))
    }

    fn error(&self) -> &AiError {
        self.result.as_ref().expect_err("the turn should have failed")
    }

    fn calls(&self) -> Vec<&Seen> {
        self.events.iter().filter(|event| matches!(event, Seen::Call { .. })).collect()
    }
}

/// Drives the chunks through a decoder and a fresh turn, exactly as the shared driver does: an error from the turn is
/// kept and answered with a stop, an error from the decoder becomes a protocol error, and a kept error wins over the
/// verdict of the end. `answer` gets the number of the event and returns what the consumer says.
fn drive<T: AsRef<[u8]>>(chunks: &[T], answer: &mut dyn FnMut(usize) -> Flow) -> Run {
    let mut turn = OpenAiChatAdapter::new().turn();
    let mut decoder = SseDecoder::new();
    let events: RefCell<Vec<Seen>> = RefCell::new(Vec::new());
    let mut failure: Option<AiError> = None;
    let mut out = |event: StreamEvent| {
        let number = {
            let mut events = events.borrow_mut();
            events.push(Seen::of(&event));
            events.len() - 1
        };
        answer(number)
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
    let before_finish = events.borrow().len();
    let result = match failure {
        Some(error) => Err(error),
        None => turn.finish(end, &mut out),
    };
    Run { events: events.into_inner(), before_finish, result }
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
    let fresh: OpenAiChatAdapter = Default::default();
    let again = fresh.request(&request, &caps()).expect("a request");
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

// ---- tools, context and merging in the request -----------------------------------------------------------

fn spec(name: &str, description: &str) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema: json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
    }
}

fn tool_caps() -> Capabilities {
    Capabilities { tools: true, ..caps() }
}

fn call(id: &str, name: &str, path: &str) -> ToolCall {
    ToolCall { id: id.to_owned(), name: name.to_owned(), args: json!({ "path": path }) }
}

fn body_of(request: &ChatRequest, caps: &Capabilities) -> Value {
    json_of(&OpenAiChatAdapter::new().request(request, caps).expect("a request").body)
}

#[test]
fn declared_tools_become_function_definitions_only_when_the_model_can_use_them() {
    let mut request = request_with("hello");
    request.tools = vec![spec("read_file", "Read a text file."), spec("list_dir", "List a directory.")];

    let body = body_of(&request, &tool_caps());
    let golden = json_of(fixture!("request_tools.json"));
    assert_eq!(body["tools"], golden["tools"]);
    for tool in body["tools"].as_array().expect("a list of tools") {
        assert_eq!(tool["type"], json!("function"));
        assert!(tool.get("strict").is_none() && tool["function"].get("strict").is_none(), "{tool}");
        assert_eq!(tool["function"]["parameters"], request.tools[0].input_schema, "the schema is copied as it is");
    }
    assert_eq!(body["tools"][0]["function"]["name"], json!("read_file"), "declared in order");
    assert_eq!(body["tools"][1]["function"]["name"], json!("list_dir"));
    assert!(body.get("tool_choice").is_none(), "{body}");

    // A model that cannot use tools, or a request that declares none, has no `tools` key.
    assert!(body_of(&request, &caps()).get("tools").is_none());
    assert!(body_of(&request_with("hello"), &tool_caps()).get("tools").is_none());

    // The adapter declares tools for every model and keeps the locality it is given.
    let declared = OpenAiChatAdapter::new().capabilities("any-model", Locality::Lan);
    assert!(declared.tools && !declared.reasoning && !declared.vision);
    assert_eq!((declared.context_tokens, declared.max_output_tokens, declared.locality), (None, None, Locality::Lan));
}

/// The request of the tool exchange: the user asks, the model makes two calls, two results come back, the user thanks.
fn exchange() -> ChatRequest {
    let mut request = request_with("list files");
    request.tools = vec![spec("read_file", "Read a text file."), spec("list_dir", "List a directory.")];
    request.messages.push(Message::new(
        "a1",
        Role::Assistant,
        vec![
            Part::Text { text: "Let me look.".to_owned() },
            Part::ToolCall(call("call_1", "read_file", "a.txt")),
            Part::ToolCall(call("call_2", "list_dir", ".")),
        ],
        2,
    ));
    request.messages.push(Message::new(
        "t1",
        Role::Tool,
        vec![
            Part::ToolResult(ToolResult { call_id: "call_1".to_owned(), content: "alpha".to_owned(), is_error: false }),
            Part::ToolResult(ToolResult {
                call_id: "call_2".to_owned(),
                content: NOT_STORED.to_owned(),
                is_error: true,
            }),
        ],
        3,
    ));
    request.messages.push(Message::user_text("u2", "thanks", 4));
    request
}

#[test]
fn a_tool_exchange_is_sent_back_as_tool_calls_and_tool_messages() {
    let body = body_of(&exchange(), &tool_caps());
    assert_eq!(body, json_of(fixture!("request_tools.json")));

    // The arguments are compact JSON text that parses back to the call arguments.
    let calls = body["messages"][2]["tool_calls"].as_array().expect("calls");
    let args: Vec<Value> = calls
        .iter()
        .map(|call| serde_json::from_str(call["function"]["arguments"].as_str().expect("text")).expect("JSON"))
        .collect();
    assert_eq!(args, vec![json!({ "path": "a.txt" }), json!({ "path": "." })]);
    assert_eq!(calls[0]["function"]["arguments"], json!(r#"{"path":"a.txt"}"#), "compact");

    // The error flag has no field on the wire, and the content of a result is verbatim, whatever it says.
    let results = [&body["messages"][3], &body["messages"][4]];
    assert_eq!(results[1]["content"], json!(NOT_STORED));
    assert!(results.iter().all(|message| message.as_object().expect("an object").len() == 3), "{results:?}");

    // An assistant message with calls and no text has a null content; the agent loop's trimmed text is a plain result.
    let mut request = exchange();
    request.messages[1].parts.remove(0);
    request.messages[2].parts[0] = Part::ToolResult(ToolResult {
        call_id: "call_1".to_owned(),
        content: TRIMMED_RESULT.to_owned(),
        is_error: false,
    });
    let body = body_of(&request, &tool_caps());
    assert_eq!(body["messages"][2]["content"], Value::Null);
    assert!(body["messages"][2].get("content").is_some(), "a null content is written, not left out");
    assert_eq!(body["messages"][3]["content"], json!(TRIMMED_RESULT));
}

#[test]
fn adjacent_messages_of_one_side_are_merged_and_empty_ones_dropped() {
    let adapter = OpenAiChatAdapter::new();
    let mut request = ChatRequest::new("m");
    request.messages.push(Message::user_text("u1", "one", 1));
    request.messages.push(Message::user_text("u2", "two", 2));
    request.messages.push(Message::new("a1", Role::Assistant, vec![Part::Text { text: "x".to_owned() }], 3));
    request.messages.push(Message::new("a2", Role::Assistant, vec![Part::Text { text: "y".to_owned() }], 4));
    request.messages.push(Message::new("a3", Role::Assistant, Vec::new(), 5));
    request.messages.push(Message::new("a4", Role::Assistant, vec![Part::Text { text: "z".to_owned() }], 6));
    let result = |id: &str, content: &str| {
        Part::ToolResult(ToolResult { call_id: id.to_owned(), content: content.to_owned(), is_error: false })
    };
    request.messages.push(Message::new("t1", Role::Tool, vec![result("c1", "first")], 7));
    request.messages.push(Message::new("t2", Role::Tool, vec![result("c2", "second")], 8));
    request.messages.push(Message::user_text("u3", "three", 9));
    let body = body_of(&request, &caps());
    assert_eq!(
        body["messages"],
        json!([
            { "role": "user", "content": "one\n\ntwo" },
            { "role": "assistant", "content": "x\n\ny\n\nz" },
            { "role": "tool", "content": "first", "tool_call_id": "c1" },
            { "role": "tool", "content": "second", "tool_call_id": "c2" },
            { "role": "user", "content": "three" },
        ])
    );

    // An empty assistant message between two user messages makes them neighbours, and they merge.
    let mut request = ChatRequest::new("m");
    request.messages.push(Message::user_text("u1", "a", 1));
    request.messages.push(Message::new("a1", Role::Assistant, Vec::new(), 2));
    request.messages.push(Message::user_text("u2", "b", 3));
    assert_eq!(body_of(&request, &caps())["messages"], json!([{ "role": "user", "content": "a\n\nb" }]));

    // A request whose only message is empty has nothing to send.
    let mut request = ChatRequest::new("m");
    request.messages.push(Message::user_text("u1", "", 1));
    request.messages.push(Message::new("a1", Role::Assistant, Vec::new(), 2));
    assert_eq!(adapter.request(&request, &caps()).expect_err("nothing").class(), ErrorClass::InvalidRequest);
}

fn approved_context(provider: &str, text: &str) -> ApprovedContext {
    let gate = Gate::new();
    let destination = Destination { host: "localhost".to_owned(), locality: Locality::Loopback };
    let item = RequestItem::new(ContextKind::Screen, "build pane", SourceText::plain(text));
    let offer = gate.offer(OfferRequest::new(provider, destination, vec![item])).expect("an offer");
    gate.approve(offer.id(), &[1], provider).expect("an approval")
}

fn note_for(context: &ApprovedContext, stored: Option<&str>) -> ContextNote {
    ContextNote::for_approved(context, stored.map(str::to_owned))
}

fn older_note(items: Vec<NoteItem>, stored: Option<&str>) -> ContextNote {
    ContextNote {
        hash: "0".repeat(64),
        items,
        stored: stored.map(str::to_owned),
        consented_to: Consent { provider: "local".to_owned(), locality: Locality::Loopback },
    }
}

fn item(kind: ContextKind, label: &str, bytes: u64) -> NoteItem {
    NoteItem { kind, label: label.to_owned(), bytes }
}

#[test]
fn approved_context_is_rendered_from_its_envelope_and_an_older_note_becomes_a_marker_line() {
    let approved = approved_context("local", "error: the linker failed");
    let envelope = approved.envelope().to_owned();

    // The current message: the envelope unchanged, one blank line, then the user's text. The text the note kept is
    // never part of the body, approved or not.
    let mut request = ChatRequest::new("m");
    request.messages.push(Message::new(
        "u1",
        Role::User,
        vec![Part::Context(note_for(&approved, Some(CANARY))), Part::Text { text: "what failed?".to_owned() }],
        1,
    ));
    request.context.push(approved.clone());
    let body = body_of(&request, &caps());
    let content = body["messages"][0]["content"].as_str().expect("text");
    assert!(content.starts_with(&envelope), "{content}");
    assert_eq!(content, format!("{envelope}\nwhat failed?"));
    assert!(content.contains("error: the linker failed"));
    assert!(!body.to_string().contains(CANARY), "the kept text of a note reached the body");

    // History: a note with no approval in this request is one marker line, even when it kept its text.
    let mut request = ChatRequest::new("m");
    request.messages.push(Message::new(
        "u1",
        Role::User,
        vec![
            Part::Context(older_note(vec![item(ContextKind::Screen, "Screen of pane 1", 120)], Some(CANARY))),
            Part::Text { text: "earlier question".to_owned() },
        ],
        1,
    ));
    request.messages.push(Message::new("a1", Role::Assistant, vec![Part::Text { text: "answer".to_owned() }], 2));
    request.messages.push(Message::user_text("u2", "next", 3));
    let body = body_of(&request, &caps());
    assert_eq!(
        body["messages"][0]["content"],
        json!(
            "[terminal context from an earlier message is not sent again: screen \"Screen of pane 1\" (120 bytes)]\n\nearlier question"
        )
    );
    assert!(!body.to_string().contains(CANARY));
    assert!(!body.to_string().contains("holzwave-context"), "no envelope without an approval");

    // Several items are joined by `; `, a quote becomes a single quote and a long label is cut at 120 characters.
    let long = "a".repeat(200);
    let note =
        older_note(vec![item(ContextKind::Selection, "say \"hi\"", 5), item(ContextKind::LastCommand, &long, 9)], None);
    let mut request = ChatRequest::new("m");
    request.messages.push(Message::new("u1", Role::User, vec![Part::Context(note)], 1));
    let body = body_of(&request, &caps());
    let cut = format!("{}\u{2026}", "a".repeat(119));
    assert_eq!(
        body["messages"][0]["content"],
        json!(format!(
            "[terminal context from an earlier message is not sent again: selection \"say 'hi'\" (5 bytes); last_command \"{cut}\" (9 bytes)]"
        ))
    );

    // A note approved for the request but also kept in history by hash only still renders from its approval, and an
    // approval for another hash does not make an unrelated note an envelope.
    let other = approved_context("local", "something else");
    let mut request = ChatRequest::new("m");
    request.messages.push(Message::new(
        "u1",
        Role::User,
        vec![
            Part::Context(older_note(vec![item(ContextKind::File, "notes.md", 3)], None)),
            Part::Text { text: "q".into() },
        ],
        1,
    ));
    request.context.push(other.clone());
    let sent = body_of(&request, &caps()).to_string();
    assert!(!sent.contains("holzwave-context") && !sent.contains("something else"), "{sent}");
}

// ---- the stream: tool calls ------------------------------------------------------------------------------

const FINISH_TOOLS: &str = r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#;

/// A chunk whose delta holds the given fragments (the text of a JSON list without its brackets).
fn tool_chunk(fragments: &str) -> String {
    format!(r#"{{"choices":[{{"index":0,"delta":{{"tool_calls":[{fragments}]}}}}]}}"#)
}

fn read_call(id: &str, path: &str) -> Seen {
    Seen::Call { id: id.to_owned(), name: "read_file".to_owned(), args: json!({ "path": path }) }
}

#[test]
fn tool_call_fragments_are_assembled_by_index_and_released_only_after_the_clean_end() {
    let run = play(fixture!("tool_call.sse"));
    assert_eq!(run.before_finish, 1, "only the usage arrived before the end: {:?}", run.events);
    assert_eq!(run.events, vec![Seen::Usage(42, 17), read_call("call_9xK2", "a b.txt")]);
    let completion = run.completion();
    assert_eq!(completion.stop_reason, StopReason::ToolUse);
    assert_eq!(completion.usage, Usage { input_tokens: 42, output_tokens: 17 });
    assert!(completion.native.is_none());

    // Text before the call is delivered as it arrives; the call comes last, from the end of the turn.
    let text_chunk = r#"{"choices":[{"index":0,"delta":{"content":"Let me look."}}]}"#;
    let first = tool_chunk(r#"{"index":0,"id":"c1","function":{"name":"read_file","arguments":"{\"path\":"}}"#);
    let second = tool_chunk(r#"{"index":0,"function":{"arguments":"\"a\"}"}}"#);
    let run = play(&stream_of(&[text_chunk, &first, &second, FINISH_TOOLS, "[DONE]"]));
    assert_eq!(run.before_finish, 1);
    assert_eq!(run.events, vec![text("Let me look."), read_call("c1", "a")]);

    // Cut before the finish chunk: nothing is released.
    let run = play(&stream_of(&[text_chunk, &first, &second]));
    assert_eq!(run.before_finish, 1);
    assert!(run.calls().is_empty(), "{:?}", run.events);
    assert_eq!(run.error().class(), ErrorClass::Protocol);
}

#[test]
fn parallel_calls_whole_calls_object_arguments_missing_and_repeated_ids_are_handled() {
    // Two calls interleaved by index, both parsed, in the order opened.
    let run = play(fixture!("tool_parallel.sse"));
    assert_eq!(
        run.events,
        vec![
            Seen::Usage(60, 31),
            read_call("call_a1", "a.txt"),
            Seen::Call { id: "call_b2".to_owned(), name: "list_dir".to_owned(), args: json!({ "path": "." }) },
        ]
    );
    assert_eq!(run.completion().stop_reason, StopReason::ToolUse);

    // A whole call with an empty id: the id is made up, starts with `call_` and differs between turns.
    let one = play(fixture!("tool_whole_no_id.sse"));
    let two = play(fixture!("tool_whole_no_id.sse"));
    let minted = |run: &Run| match run.calls()[..] {
        [Seen::Call { id, name, args }] => {
            assert_eq!((name.as_str(), args), ("list_dir", &json!({ "path": "." })));
            id.clone()
        }
        ref other => panic!("one call expected: {other:?}"),
    };
    let (first_id, second_id) = (minted(&one), minted(&two));
    assert!(first_id.starts_with("call_") && second_id.starts_with("call_"), "{first_id} {second_id}");
    assert_ne!(first_id, second_id);
    assert_eq!(one.events[0], text("Let me check."));
    assert_eq!(one.completion().stop_reason, StopReason::ToolUse);

    // Without an index: a fragment with an id opens a new call, one without an id continues the last one.
    let streams = [
        tool_chunk(r#"{"id":"x1","function":{"name":"read_file","arguments":"{\"path\":"}}"#),
        tool_chunk(r#"{"function":{"arguments":"\"a\"}"}}"#),
        tool_chunk(r#"{"id":"x2","function":{"name":"list_dir","arguments":"{}"}}"#),
    ];
    let finish_stop = r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
    let run = play(&stream_of(&[&streams[0], &streams[1], &streams[2], finish_stop]));
    assert_eq!(
        run.events,
        vec![read_call("x1", "a"), Seen::Call { id: "x2".to_owned(), name: "list_dir".to_owned(), args: json!({}) }]
    );
    // A finish reason `stop` after tool fragments releases them like `tool_calls`, and the turn is a tool turn.
    assert_eq!(run.completion().stop_reason, StopReason::ToolUse);

    // Arguments given as a JSON object are the same call.
    let object = tool_chunk(r#"{"index":0,"id":"c1","function":{"name":"read_file","arguments":{"path":"a.txt"}}}"#);
    let run = play(&stream_of(&[&object, FINISH_TOOLS]));
    assert_eq!(run.events, vec![read_call("c1", "a.txt")]);

    // Two calls with the same id leave with unique ids: the first keeps it, the second gets `-2`.
    let twin = |index: u8, path: &str| {
        format!(
            r#"{{"index":{index},"id":"X","function":{{"name":"read_file","arguments":"{{\"path\":\"{path}\"}}"}}}}"#
        )
    };
    let twins = tool_chunk(&[twin(0, "a"), twin(1, "b"), twin(2, "c")].join(","));
    let run = play(&stream_of(&[&twins, FINISH_TOOLS]));
    assert_eq!(run.events, vec![read_call("X", "a"), read_call("X-2", "b"), read_call("X-3", "c")]);

    // A later fragment that repeats a different id is ignored: the call keeps the first.
    let begin = tool_chunk(r#"{"index":0,"id":"first","function":{"name":"read_file","arguments":"{\"path\":"}}"#);
    let again = tool_chunk(r#"{"index":0,"id":"second","function":{"arguments":"\"a\"}"}}"#);
    let run = play(&stream_of(&[&begin, &again, FINISH_TOOLS]));
    assert_eq!(run.events, vec![read_call("first", "a")]);

    // A finish reason that announces tool calls when none arrived is an error.
    let run = play(&stream_of(&[HI, FINISH_TOOLS]));
    assert_eq!(run.error().class(), ErrorClass::Protocol);
    assert_eq!(run.events, vec![text("Hi")]);
}

/// The ids that the server did not send are made up; compare them by their shape.
fn normalised(mut run: Run) -> Run {
    for event in &mut run.events {
        if let Seen::Call { id, .. } = event
            && id.starts_with("call_")
            && id.len() >= 22
        {
            *id = "call_*".to_owned();
        }
    }
    run
}

/// The offset just after the blank line that ends the event with the first finish reason that is not null.
fn end_of_finish_event(bytes: &[u8]) -> usize {
    let marker = br#""finish_reason":""#;
    let at = bytes.windows(marker.len()).position(|window| window == marker).expect("a finish reason");
    let blank = bytes[at..].windows(2).position(|window| window == b"\n\n").expect("a blank line");
    at + blank + 2
}

#[test]
fn every_split_of_a_tool_stream_releases_the_same_calls_and_every_cut_releases_none() {
    for (name, bytes) in [
        ("tool_call.sse", &fixture!("tool_call.sse")[..]),
        ("tool_parallel.sse", &fixture!("tool_parallel.sse")[..]),
        ("tool_whole_no_id.sse", &fixture!("tool_whole_no_id.sse")[..]),
    ] {
        let whole = normalised(play(bytes));
        assert!(!whole.calls().is_empty() && whole.result.is_ok(), "{name}: {whole:?}");
        for (label, chunks) in chunkings(bytes, 0x7001_0001) {
            let split = normalised(run_chunks(&chunks));
            assert_eq!(split, whole, "{name}, {label}");
        }

        let end = end_of_finish_event(bytes);
        for cut in 0..end {
            let run = play(&bytes[..cut]);
            assert!(run.calls().is_empty(), "{name}: a call was released for a cut at byte {cut}: {:?}", run.events);
            assert!(run.result.is_err(), "{name}: a cut at byte {cut} (before byte {end}) was a completed turn");
        }
        for cut in end..=bytes.len() {
            let run = normalised(play(&bytes[..cut]));
            // Only a cut between two events is a clean end; a cut inside an event is an error that releases nothing.
            let between_events = cut == bytes.len() || bytes[..cut].ends_with(b"\n\n");
            assert_eq!(run.result.is_ok(), between_events, "{name}: a cut at byte {cut}: {:?}", run.result);
            if between_events {
                assert_eq!(run.calls(), whole.calls(), "{name}: the calls of a cut at byte {cut} differ");
            } else {
                assert!(run.calls().is_empty(), "{name}: a failed cut at byte {cut} released a call");
            }
        }
    }
}

#[test]
fn invalid_tool_json_a_length_cut_and_conflicting_fragments_fail_the_turn_without_a_call() {
    let finish = |reason: &str| format!(r#"{{"choices":[{{"index":0,"delta":{{}},"finish_reason":"{reason}"}}]}}"#);
    let named = |args: &str| {
        tool_chunk(&format!(r#"{{"index":0,"id":"c1","function":{{"name":"read_file","arguments":{}}}}}"#, json!(args)))
    };
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("cut JSON", stream_of(&[&named(&format!("{{\"path\": \"{CANARY}")), &finish("tool_calls")])),
        ("an array", stream_of(&[&named("[1]"), &finish("tool_calls")])),
        ("a string", stream_of(&[&named("\"text\""), &finish("tool_calls")])),
        ("a number", stream_of(&[&named("7"), &finish("tool_calls")])),
        ("a cut by the length limit", stream_of(&[&named(r#"{"path":"a"}"#), &finish("length")])),
        ("a cut by a filter", stream_of(&[&named(r#"{"path":"a"}"#), &finish("content_filter")])),
        (
            "a name with a line feed",
            stream_of(&[
                &tool_chunk(&format!(
                    r#"{{"index":0,"id":"c1","function":{{"name":"read\n{CANARY}","arguments":"{{}}"}}}}"#
                )),
                &finish("tool_calls"),
            ]),
        ),
        (
            "a name that changes",
            stream_of(&[
                &tool_chunk(r#"{"index":0,"id":"c1","function":{"name":"read_file","arguments":"{"}}"#),
                &tool_chunk(r#"{"index":0,"function":{"name":"other_tool","arguments":"}"}}"#),
                &finish("tool_calls"),
            ]),
        ),
        (
            "a call without a name",
            stream_of(&[&tool_chunk(r#"{"index":0,"id":"c1","function":{"arguments":"{}"}}"#), &finish("tool_calls")]),
        ),
        (
            "a call that is changed after it is complete",
            stream_of(&[
                &tool_chunk(r#"{"index":0,"id":"c1","function":{"name":"read_file","arguments":"{}"}}"#),
                &finish("tool_calls"),
                &tool_chunk(r#"{"index":0,"function":{"arguments":"{}"}}"#),
            ]),
        ),
        (
            "a slot beyond the bound",
            stream_of(&[
                &tool_chunk(
                    &(0..65)
                        .map(|n| format!(r#"{{"index":{n},"function":{{"name":"read_file","arguments":"{{}}"}}}}"#))
                        .collect::<Vec<_>>()
                        .join(","),
                ),
                &finish("tool_calls"),
            ]),
        ),
        (
            "an index that is not a slot",
            stream_of(&[
                &tool_chunk(r#"{"index":4294967296,"function":{"name":"read_file","arguments":"{}"}}"#),
                &finish("tool_calls"),
            ]),
        ),
    ];
    for (name, stream) in &cases {
        let run = play(stream);
        assert!(run.calls().is_empty(), "{name}: a call was released: {:?}", run.events);
        assert_eq!(run.error().class(), ErrorClass::Protocol, "{name}: {:?}", run.result);
        let shown = format!("{} | {:?} | {:#?}", run.error(), run.error(), run.error());
        assert!(!shown.contains(CANARY) && !shown.contains("read_file"), "{name}: stream text in the error: {shown}");
    }

    // Empty arguments with a name are a call with an empty object, and 64 calls are the most one turn may make.
    let empty = tool_chunk(r#"{"index":0,"id":"c1","function":{"name":"read_file","arguments":""}}"#);
    let run = play(&stream_of(&[&empty, &finish("tool_calls")]));
    assert_eq!(run.calls().len(), 1);
    assert_eq!(run.events, vec![Seen::Call { id: "c1".to_owned(), name: "read_file".to_owned(), args: json!({}) }]);
    let many = tool_chunk(
        &(0..64)
            .map(|n| format!(r#"{{"index":{n},"function":{{"name":"read_file","arguments":"{{}}"}}}}"#))
            .collect::<Vec<_>>()
            .join(","),
    );
    let run = play(&stream_of(&[&many, &finish("tool_calls")]));
    assert_eq!(run.calls().len(), 64, "{:?}", run.result);
    let mut ids: Vec<&str> = run
        .events
        .iter()
        .map(|event| match event {
            Seen::Call { id, .. } => id.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 64, "the made-up ids are unique");
}

#[test]
fn reasoning_text_is_display_only_and_is_never_sent_back() {
    let run = play(fixture!("reasoning.sse"));
    assert_eq!(
        run.events,
        vec![
            Seen::Reasoning("Thinking about ".to_owned()),
            Seen::Reasoning("the greeting. ".to_owned()),
            text("Hello"),
            text(" there"),
            Seen::Usage(9, 14),
        ]
    );

    // The message the collector makes holds the answer and nothing of the reasoning, and a later request built from it
    // has neither the reasoning text nor a reasoning key.
    let fake = FakeHttp::start(vec![Reply::sse_bytes(vec![fixture!("reasoning.sse").to_vec()])]);
    let provider = provider(&fake, "/v1");
    let mut collector = Collector::new();
    let completion = provider
        .stream(&request_with("greet me"), &mut |event| collector.push(event), &CancelToken::new())
        .expect("a completed turn");
    assert_eq!(collector.reasoning(), "Thinking about the greeting. ");
    assert_eq!(collector.text(), "Hello there");
    let answer = collector.into_message("a1", 2, "test-model", MessageStatus::Complete, Some(completion));
    let mut next = request_with("greet me");
    next.messages.push(answer);
    next.messages.push(Message::user_text("u2", "again", 3));
    let body = body_of(&next, &caps());
    assert_eq!(body["messages"][2], json!({ "role": "assistant", "content": "Hello there" }));
    let sent = body.to_string();
    assert!(!sent.contains("Thinking") && !sent.contains("greeting. ") && !sent.contains("reasoning"), "{sent}");
}

// ---- errors -----------------------------------------------------------------------------------------------

fn error_after_text(data: &str) -> AiError {
    let run = play(&stream_of(&[HI, data]));
    assert_eq!(run.events, vec![text("Hi")], "{data}");
    run.error().clone()
}

fn shown(error: &AiError) -> String {
    format!("{error} | {error:?} | {error:#?}")
}

#[test]
fn a_stream_error_becomes_a_classified_error_that_never_echoes_the_server_text() {
    let run = play(fixture!("error_event.sse"));
    assert_eq!(run.events, vec![text("partial")]);
    match run.error() {
        AiError::Server { status, .. } => assert_eq!(*status, 502),
        other => panic!("a server error was expected: {other:?}"),
    }
    let line = shown(run.error());
    assert!(!line.contains(CANARY) && !line.contains("upstream provider failed"), "{line}");
    assert!(line.contains("502"), "the code token is kept: {line}");

    // The class table, by code and by type.
    let wrap = |inner: &str| format!(r#"{{"error":{inner}}}"#);
    let class = |inner: &str| error_after_text(&wrap(inner));
    assert_eq!(class(r#"{"code":"context_length_exceeded","message":"m"}"#), AiError::ContextLength);
    assert_eq!(class(r#"{"type":"exceed_context_size_error"}"#), AiError::ContextLength);
    assert_eq!(class(r#"{"code":"rate_limit_exceeded"}"#), AiError::RateLimited { retry_after: None });
    assert_eq!(class(r#"{"code":429}"#), AiError::RateLimited { retry_after: None });
    for overloaded in [r#"{"code":"overloaded_error"}"#, r#"{"code":503}"#, r#"{"code":529}"#, r#"{"code":"503"}"#] {
        assert_eq!(class(overloaded), AiError::Overloaded, "{overloaded}");
    }
    for refused in [r#"{"code":"invalid_api_key"}"#, r#"{"code":401}"#, r#"{"code":402}"#, r#"{"code":403}"#] {
        assert_eq!(class(refused).class(), ErrorClass::Auth, "{refused}");
    }
    assert_eq!(class(r#"{"code":"content_policy_violation"}"#), AiError::ContentFilter);
    for invalid in [r#"{"code":404}"#, r#"{"code":400}"#, r#"{"type":"invalid_request_error"}"#] {
        assert_eq!(class(invalid).class(), ErrorClass::InvalidRequest, "{invalid}");
    }
    for (inner, status) in [
        (r#"{"code":"weird_code"}"#, 500),
        (r#"{"code":500}"#, 500),
        (r#"{"code":599}"#, 599),
        (r#"{"message":"m"}"#, 500),
    ] {
        match class(inner) {
            AiError::Server { status: got, .. } => assert_eq!(got, status, "{inner}"),
            other => panic!("{inner}: {other:?}"),
        }
    }
    // A plain string is a server error, and its text is never shown.
    let error = error_after_text(&wrap(&format!("\"boom {CANARY}\"")));
    assert_eq!(error.class(), ErrorClass::Server);
    assert!(!shown(&error).contains(CANARY));

    // Only a plain code is shown: lower case, digits, dot, dash and underscore, 48 characters at most.
    let token = |code: &str| shown(&class(&format!(r#"{{"code":"{code}","message":"{CANARY}"}}"#)));
    assert!(token("upstream_timeout.v2-x").contains("(upstream_timeout.v2-x)"));
    assert!(!token(CANARY).contains(CANARY), "an upper-case code is not shown");
    assert!(!token("has space").contains("has space"));
    let long = "x".repeat(60);
    assert!(!token(&long).contains(&long), "a long code is not shown");
    assert!(token(&"x".repeat(48)).contains(&"x".repeat(48)));
    assert!(!token("a;b").contains("a;b"));
    // The type stands in when the code cannot be shown.
    let typed = shown(&class(&format!(r#"{{"code":"{CANARY}","type":"upstream_error"}}"#)));
    assert!(typed.contains("(upstream_error)") && !typed.contains(CANARY), "{typed}");

    // A choice that carries an error, a finish reason `error` and an event named `error` are handled the same way.
    let choice = r#"{"choices":[{"index":0,"delta":{},"finish_reason":"error","error":{"code":"server_error","message":"CANARY-SERVER-TEXT"}}]}"#;
    let error = error_after_text(choice);
    assert_eq!(error.class(), ErrorClass::Server);
    assert!(!shown(&error).contains(CANARY));
    let bare = error_after_text(r#"{"choices":[{"index":0,"delta":{},"finish_reason":"error"}]}"#);
    assert!(matches!(bare, AiError::Server { status: 500, .. }), "{bare:?}");
    let named = |data: &str| {
        let mut bytes = stream_of(&[HI]);
        bytes.extend_from_slice(format!("event: error\ndata: {data}\n\n").as_bytes());
        let run = play(&bytes);
        assert_eq!(run.events, vec![text("Hi")]);
        run.error().clone()
    };
    assert_eq!(named(&format!(r#"{{"error":{{"code":503,"message":"{CANARY}"}}}}"#)), AiError::Overloaded);
    assert_eq!(named(r#"{"code":"rate_limit_exceeded"}"#), AiError::RateLimited { retry_after: None });
    let unreadable = named(&format!("not json {CANARY}"));
    assert_eq!(unreadable.class(), ErrorClass::Server);
    assert!(!shown(&unreadable).contains(CANARY));

    // An `error` that is null, empty or an empty object on an ordinary chunk is no error.
    for quiet in [r#""error":null"#, r#""error":"""#, r#""error":{}"#] {
        let chunk = format!(r#"{{{quiet},"choices":[{{"index":0,"delta":{{"content":"ok"}}}}]}}"#);
        let run = play(&stream_of(&[&chunk, FINISH]));
        assert_eq!(run.events, vec![text("ok")], "{quiet}");
        assert!(run.result.is_ok(), "{quiet}");
    }
}

const KEY: &str = "sk-canary-0123456789abcdef";

fn keyed_endpoint(base: &str) -> Endpoint {
    let target = Target::parse(base, false).expect("a loopback base address");
    let origin = target.origin().clone();
    let secret = SecretHeader::new("authorization", "Bearer ", ApiKey::new(KEY.to_owned()), origin).expect("a header");
    Endpoint::new(Wire::OpenAiChat, target, NetworkPolicy::new(false, false)).with_secret(secret)
}

fn reject(status: u16, body: &[u8]) -> Option<AiError> {
    let endpoint = keyed_endpoint("http://127.0.0.1:9");
    OpenAiChatAdapter::new().rejection(&Rejection { status, retry_after: None, body: body.to_vec() }, &endpoint)
}

#[test]
fn error_bodies_map_to_classes_and_show_only_the_scrubbed_message() {
    // A context that is too long, whatever the status.
    assert_eq!(reject(400, fixture!("error_context_length.json")), Some(AiError::ContextLength));
    assert_eq!(reject(422, fixture!("error_context_length.json")), Some(AiError::ContextLength));
    // The class is also read from the wording of a message, which is never shown for it.
    let wording = br#"{"error":{"message":"the request exceeds the available context size, try increasing it"}}"#;
    assert_eq!(reject(400, wording), Some(AiError::ContextLength));
    // A content filter, by its code.
    let filter = br#"{"error":{"message":"blocked","type":"invalid_request_error","code":"content_filter"}}"#;
    assert_eq!(reject(400, filter), Some(AiError::ContentFilter));

    // A key that the server echoes is masked, and the status is shown.
    let echo = format!(
        r#"{{"error":{{"message":"Incorrect API key provided: {KEY}.","type":"invalid_request_error","code":"invalid_api_key"}}}}"#
    );
    let error = reject(401, echo.as_bytes()).expect("an authentication error");
    assert_eq!(error.class(), ErrorClass::Auth);
    let text = shown(&error);
    assert!(text.contains("401") && text.contains("Incorrect API key provided"), "{text}");
    assert!(!text.contains(KEY) && text.contains("<redacted>"), "{text}");
    assert_eq!(reject(403, echo.as_bytes()).expect("auth").class(), ErrorClass::Auth);
    assert_eq!(reject(402, echo.as_bytes()).expect("auth").class(), ErrorClass::Auth);

    // A string `error`, as local servers write it.
    let error = reject(404, fixture!("error_lmstudio.json")).expect("a rejected request");
    assert_eq!(error.class(), ErrorClass::InvalidRequest);
    let text = error.to_string();
    assert!(text.contains("404") && text.contains("No models loaded"), "{text}");

    // The error object of a llama.cpp-style server.
    let server = br#"{"error":{"code":500,"message":"Failed to parse grammar","type":"server_error"}}"#;
    match reject(500, server).expect("a server error") {
        AiError::Server { status, detail } => {
            assert_eq!(status, 500);
            assert!(detail.as_str().contains("Failed to parse grammar"), "{detail:?}");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(reject(502, server), Some(AiError::Server { status: 502, .. })));
    // A top-level message is read too.
    assert_eq!(reject(400, br#"{"message":"bad field"}"#).expect("invalid").class(), ErrorClass::InvalidRequest);

    // The statuses that the chokepoint maps are left to it.
    let plain = fixture!("error_lmstudio.json");
    for status in [408, 413, 429, 503, 529] {
        assert_eq!(reject(status, plain), None, "{status}");
    }
    // A body that is not an error object gives nothing, and so does a status that is not an error.
    for body in [
        &b""[..],
        b"not json",
        b"[1,2]",
        b"<html><body>Bad gateway</body></html>",
        b"{}",
        b"{\"error\":7}",
        b"\"text\"",
    ] {
        assert_eq!(reject(400, body), None, "{}", String::from_utf8_lossy(body));
        assert_eq!(reject(502, body), None, "{}", String::from_utf8_lossy(body));
    }
    assert_eq!(reject(302, plain), None);

    // The message is cut: a long one cannot fill a line.
    let long = format!(r#"{{"error":{{"message":"{}"}}}}"#, "w".repeat(5000));
    let text = reject(400, long.as_bytes()).expect("invalid").to_string();
    assert!(text.chars().count() < 400, "{}", text.chars().count());

    // Through the fake server with a real key: the key is sent once, and the answer does not show it.
    let fake = FakeHttp::start(vec![Reply::status(401).header("content-type", "application/json").body(echo.clone())]);
    let endpoint = keyed_endpoint(&format!("{}/v1", fake.base()));
    let http = Arc::new(Http::new(HttpConfig::standard()));
    let provider = HttpProvider::new("local", endpoint, Box::new(OpenAiChatAdapter::new()), http);
    let error = provider
        .stream(&request_with("hello"), &mut |_| Flow::Continue, &CancelToken::new())
        .expect_err("the server refused the key");
    assert_eq!(error.class(), ErrorClass::Auth);
    let text = shown(&error);
    assert!(!text.contains(KEY) && text.contains("401"), "{text}");
    assert_eq!(fake.accepted(), 1, "a refused key is not retried");
    let seen = fake.seen();
    assert_eq!(seen[0].header("authorization"), Some(format!("Bearer {KEY}").as_str()));
    assert!(!seen[0].body_text().contains(KEY) && !seen[0].target.contains(KEY));
}

// ---- Debug, the consumer's stop, and a tool turn through the server --------------------------------------

#[test]
fn adapter_and_turn_debug_never_show_stream_or_request_text() {
    let adapter = OpenAiChatAdapter::new();
    let mut turn = ChatTurn::new();
    let chunk = format!(
        r#"{{"choices":[{{"index":0,"delta":{{"reasoning_content":"{CANARY} thinking","content":"{CANARY} text","tool_calls":[{{"index":0,"id":"call_c","function":{{"name":"{CANARY}-tool","arguments":"{{\"x\":\"{CANARY}"}}}}]}}}}]}}"#
    );
    let more = tool_chunk(r#"{"index":0,"function":{"arguments":"\"}"}}"#);
    let bytes = stream_of(&[&chunk, &more]);
    let mut decoder = SseDecoder::new();
    let mut delivered = 0;
    decoder
        .push(&bytes, &mut |event| {
            turn.frame(Frame::Event(event), &mut |_| {
                delivered += 1;
                Flow::Continue
            })
            .expect("a readable chunk")
        })
        .expect("a framed stream");
    assert_eq!(delivered, 2, "the reasoning and the text were delivered");

    for shown in [format!("{adapter:?}"), format!("{adapter:#?}"), format!("{turn:?}"), format!("{turn:#?}")] {
        assert!(!shown.contains(CANARY), "stream text reached a Debug line: {shown}");
    }
    assert!(format!("{turn:?}").contains("ChatTurn"));

    // The request does not print either: the request type and the wire request show sizes.
    let mut request = request_with(CANARY);
    request.system = CANARY.to_owned();
    let wire = adapter.request(&request, &tool_caps()).expect("a request");
    for shown in [format!("{wire:?}"), format!("{wire:#?}")] {
        assert!(!shown.contains(CANARY), "{shown}");
    }
}

#[test]
fn a_tool_turn_through_the_fake_server_reaches_the_collector_after_the_stream_ends() {
    let body = fixture!("tool_call.sse");
    let step = body.len() / 5;
    let offsets: Vec<usize> = (1..5).map(|n| n * step).collect();
    let fake = FakeHttp::start(vec![Reply::sse_split(body, &offsets).gap(Duration::from_millis(10))]);
    let provider = provider(&fake, "/v1");
    let mut request = request_with("read a b.txt");
    request.tools = vec![spec("read_file", "Read a text file.")];

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

    assert_eq!(fake.accepted(), 1);
    let sent = json_of(&fake.seen()[0].body);
    assert_eq!(sent["tools"][0]["function"]["name"], json!("read_file"));
    assert_eq!(events, vec![Seen::Usage(42, 17), read_call("call_9xK2", "a b.txt")]);
    assert_eq!(events.last(), Some(&read_call("call_9xK2", "a b.txt")), "the call comes after everything else");
    assert_eq!(completion.stop_reason, StopReason::ToolUse);
    assert_eq!(collector.tool_calls().len(), 1);
    assert_eq!(collector.tool_calls()[0].args, json!({ "path": "a b.txt" }));
}

#[test]
fn a_stop_from_the_consumer_ends_the_turn_as_cancelled_and_nothing_more_is_delivered() {
    // On the first event.
    let run = drive(&[fixture!("text.sse")], &mut |number| if number == 0 { Flow::Stop } else { Flow::Continue });
    assert_eq!(run.events.len(), 1, "{:?}", run.events);
    assert_eq!(run.result, Err(AiError::Cancelled));

    // On the tool call, which is delivered when the turn ends.
    let run = drive(&[fixture!("tool_call.sse")], &mut |number| if number == 1 { Flow::Stop } else { Flow::Continue });
    assert_eq!(run.events.len(), 2, "{:?}", run.events);
    assert_eq!(run.result, Err(AiError::Cancelled));

    // Two calls: stopping on the first one delivers no second one.
    let run =
        drive(&[fixture!("tool_parallel.sse")], &mut |number| if number == 1 { Flow::Stop } else { Flow::Continue });
    assert_eq!(run.events.len(), 2, "{:?}", run.events);
    assert_eq!(run.result, Err(AiError::Cancelled));

    // Through the server and the shared driver.
    let fake = FakeHttp::start(vec![Reply::sse_bytes(vec![fixture!("text.sse").to_vec()])]);
    let provider = provider(&fake, "/v1");
    let mut count = 0;
    let result = provider.stream(
        &request_with("hello"),
        &mut |_| {
            count += 1;
            Flow::Stop
        },
        &CancelToken::new(),
    );
    assert_eq!(count, 1, "nothing is delivered after a stop");
    assert_eq!(result, Err(AiError::Cancelled));
}
