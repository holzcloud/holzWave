//! The native Ollama adapter, proved against golden streams and an in-process server, with no outside network.
//!
//! The streams in `tests/fixtures/ai/ollama/` are written by hand from the documented stream format. They are not
//! recordings of a real server; recordings from real local servers are collected separately and may become new
//! fixtures. Every request in this file goes to `FakeHttp`, an HTTP/1.1 server in this process. A stream that is driven
//! directly goes through `NdjsonDecoder` and the turn of the adapter the way the shared driver does it, so a split of the
//! bytes can be chosen at any offset. A failing case names its offset, its chunk size or its seed.

mod common;

use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

use common::Xorshift64;
use common::fake_http::{FakeHttp, Reply};
use holzwave_core::ai::gate::{ApprovedContext, Destination, Gate, OfferRequest, RequestItem};
use holzwave_core::ai::http::{Frame, Http, HttpConfig, NetworkPolicy, Rejection, SecretHeader, Target};
use holzwave_core::ai::provider::{
    AiError, CancelToken, ChatRequest, Collector, Completion, ErrorClass, Flow, Provider, StreamEvent,
};
use holzwave_core::ai::redact::SourceText;
use holzwave_core::ai::sse::NdjsonDecoder;
use holzwave_core::ai::store::NOT_STORED;
use holzwave_core::ai::types::{
    ApiKey, Capabilities, Consent, ContextKind, ContextNote, Locality, Message, MessageStatus, NoteItem, Part, Role,
    StopReason, ToolCall, ToolResult, ToolSpec, Usage,
};
use holzwave_core::ai::wire::ollama::{
    CHAT_PATH, DEFAULT_NUM_CTX, MAX_NUM_CTX, MIN_NUM_CTX, OllamaAdapter, OllamaTurn,
};
use holzwave_core::ai::wire::{Endpoint, HttpProvider, Wire, WireAdapter, WireTurn};
use serde_json::{Value, json};

/// The text that a server must never be able to put on the screen through an error or a `Debug` line.
const CANARY: &str = "CANARY-SERVER-TEXT";
/// A key that a server may echo in an error body.
const KEY: &str = "sk-canary-0123456789abcdef";

/// The bytes of a fixture of this dialect.
macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!("fixtures/ai/ollama/", $name))
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

fn read_call(id: &str, path: &str) -> Seen {
    Seen::Call { id: id.to_owned(), name: "read_file".to_owned(), args: json!({ "path": path }) }
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

/// Drives the chunks through a decoder and a fresh turn of `adapter`, exactly as the shared driver does: an error from
/// the turn is kept and answered with a stop, an error from the decoder becomes a protocol error, and a kept error wins
/// over the verdict of the end. `answer` gets the number of the event and returns what the consumer says.
fn drive<T: AsRef<[u8]>>(adapter: &OllamaAdapter, chunks: &[T], answer: &mut dyn FnMut(usize) -> Flow) -> Run {
    let mut turn = adapter.turn();
    let mut decoder = NdjsonDecoder::new();
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
        let pushed = decoder.push(chunk.as_ref(), &mut |line| {
            if failure.is_some() {
                return Flow::Stop;
            }
            match turn.frame(Frame::Line(line), &mut out) {
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
    drive(&OllamaAdapter::new(), chunks, &mut |_| Flow::Continue)
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

/// Inline records: every string is one line.
fn lines_of(records: &[&str]) -> Vec<u8> {
    records.iter().flat_map(|record| format!("{record}\n").into_bytes()).collect()
}

/// A record that carries text.
fn content(value: &str) -> String {
    format!(r#"{{"message":{{"role":"assistant","content":{}}},"done":false}}"#, json!(value))
}

/// A final record with the given reason and counters.
fn done_line(reason: &str, prompt: u64, produced: u64) -> String {
    format!(
        r#"{{"message":{{"role":"assistant","content":""}},"done":true,"done_reason":"{reason}","prompt_eval_count":{prompt},"eval_count":{produced}}}"#
    )
}

/// A record that carries the given calls (the text of a JSON list without its brackets).
fn calls_line(calls: &str) -> String {
    format!(r#"{{"message":{{"role":"assistant","content":"","tool_calls":[{calls}]}},"done":false}}"#)
}

fn caps() -> Capabilities {
    Capabilities::conservative(Locality::Loopback)
}

fn tool_caps() -> Capabilities {
    Capabilities { tools: true, ..caps() }
}

fn request_with(text: &str) -> ChatRequest {
    let mut request = ChatRequest::new("test-model");
    request.system = "You are a terminal assistant.".to_owned();
    request.messages.push(Message::user_text("u1", text, 1));
    request.max_output_tokens = Some(256);
    request
}

fn json_of(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("JSON")
}

fn body_of(request: &ChatRequest, caps: &Capabilities) -> Value {
    json_of(&OllamaAdapter::new().request(request, caps).expect("a request").body)
}

/// A provider on the fake server.
fn provider(fake: &FakeHttp, adapter: OllamaAdapter) -> Arc<dyn Provider> {
    let target = Target::parse(&fake.base(), false).expect("a loopback base address");
    let endpoint = Endpoint::new(Wire::Ollama, target, NetworkPolicy::new(false, false));
    let http = Arc::new(Http::new(HttpConfig::standard()));
    Arc::new(HttpProvider::new("local", endpoint, Box::new(adapter), http))
}

fn spec(name: &str, description: &str) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema: json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
    }
}

fn call(id: &str, name: &str, path: &str) -> ToolCall {
    ToolCall { id: id.to_owned(), name: name.to_owned(), args: json!({ "path": path }) }
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

fn approved_context(provider: &str, text: &str) -> ApprovedContext {
    let gate = Gate::new();
    let destination = Destination { host: "localhost".to_owned(), locality: Locality::Loopback };
    let item = RequestItem::new(ContextKind::Screen, "build pane", SourceText::plain(text));
    let offer = gate.offer(OfferRequest::new(provider, destination, vec![item])).expect("an offer");
    gate.approve(offer.id(), &[1], provider).expect("an approval")
}

fn older_note(items: Vec<NoteItem>, stored: Option<&str>) -> ContextNote {
    ContextNote {
        hash: "0".repeat(64),
        items,
        stored: stored.map(str::to_owned),
        consented_to: Consent { provider: "local".to_owned(), locality: Locality::Loopback },
    }
}

fn shown(error: &AiError) -> String {
    format!("{error} | {error:?} | {error:#?}")
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

// ---- the tracer -------------------------------------------------------------------------------------------

#[test]
fn a_text_turn_streams_through_the_fake_server_as_ndjson_lines() {
    let body = fixture!("text.ndjson");
    let fake = FakeHttp::start(vec![
        Reply::sse_split(body, &[11, body.len() / 2 + 5])
            .content_type("application/x-ndjson")
            .gap(Duration::from_millis(20)),
        Reply::sse_bytes(vec![body.to_vec()]).content_type("application/x-ndjson"),
    ]);
    let provider = provider(&fake, OllamaAdapter::new());
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

    assert_eq!(events, vec![text("Hel"), text("lo \u{e9} \u{65e5}\u{672c} \u{1f600}"), Seen::Usage(26, 3)]);
    assert_eq!(completion.stop_reason, StopReason::EndTurn);
    assert_eq!(completion.usage, Usage { input_tokens: 26, output_tokens: 3 });
    assert!(completion.native.is_none());
    assert_eq!(collector.text(), "Hello \u{e9} \u{65e5}\u{672c} \u{1f600}");

    assert_eq!(fake.accepted(), 1, "one connection for one turn");
    let seen = fake.seen();
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].target, "/api/chat");
    assert_eq!(seen[0].header("content-type"), Some("application/json"));
    assert_eq!(seen[0].header("accept"), Some("application/x-ndjson"));
    assert_eq!(seen[0].header("authorization"), None);
    assert_eq!(
        json_of(&seen[0].body),
        json!({
            "model": "test-model",
            "messages": [
                { "role": "system", "content": "You are a terminal assistant." },
                { "role": "user", "content": "hello" },
            ],
            "stream": true,
            "options": { "num_ctx": 16384, "num_predict": 256 },
        })
    );

    // A consumer that answers `Stop` ends the turn as cancelled and gets nothing more.
    let mut count = 0;
    let result = provider.stream(
        &request,
        &mut |_| {
            count += 1;
            Flow::Stop
        },
        &CancelToken::new(),
    );
    assert_eq!(count, 1, "nothing is delivered after a stop");
    assert_eq!(result, Err(AiError::Cancelled));
    let stopped = drive(&OllamaAdapter::new(), &[fixture!("text.ndjson")], &mut |number| {
        if number == 0 { Flow::Stop } else { Flow::Continue }
    });
    assert_eq!(stopped.events.len(), 1, "{:?}", stopped.events);
    assert_eq!(stopped.result, Err(AiError::Cancelled));

    // Records without content produce no event.
    let quiet = play(&lines_of(&[&content(""), &content("x"), &content(""), &done_line("stop", 5, 1)]));
    assert_eq!(quiet.events, vec![text("x"), Seen::Usage(5, 1)]);
    println!("tracer: a_text_turn_streams_through_the_fake_server_as_ndjson_lines ok, text {:?}", collector.text());
}

// ---- the request ------------------------------------------------------------------------------------------

#[test]
fn the_request_body_is_pinned_with_the_context_window_and_no_extras() {
    let adapter = OllamaAdapter::new();
    let wire = adapter.request(&exchange(), &tool_caps()).expect("a request");
    assert_eq!(wire.path, CHAT_PATH);
    assert_eq!(wire.path, "/api/chat");
    assert!(wire.headers.is_empty(), "the adapter adds no header");
    let body = json_of(&wire.body);
    assert_eq!(body, json_of(fixture!("request_tools.json")));
    let again = OllamaAdapter::new().request(&exchange(), &tool_caps()).expect("a request");
    assert_eq!(wire.body, again.body, "the same input gives the same bytes");
    for key in ["think", "keep_alive", "format", "stream_options", "tool_choice", "temperature", "top_p", "max_tokens"]
    {
        assert!(body.get(key).is_none(), "the body must not carry {key}: {body}");
    }
    assert_eq!(body["options"].as_object().expect("options").len(), 2, "the window and the output bound only");

    // The output bound is left out when the request sets none; the window is always there.
    let mut request = request_with("hello");
    request.max_output_tokens = None;
    request.system = String::new();
    let body = body_of(&request, &caps());
    assert_eq!(body["options"], json!({ "num_ctx": 16384 }));
    assert_eq!(body["messages"], json!([{ "role": "user", "content": "hello" }]));
    assert!(body.get("tools").is_none(), "no tools without a declaration");
    // A model that cannot use tools gets none, and neither does a request that declares none.
    assert!(body_of(&exchange(), &caps()).get("tools").is_none());

    // No messages is an invalid request.
    assert_eq!(
        adapter.request(&ChatRequest::new("m"), &caps()).expect_err("nothing").class(),
        ErrorClass::InvalidRequest
    );

    // The window: a default, a floor, a ceiling, and what is set is what is sent.
    assert_eq!(DEFAULT_NUM_CTX, 16384);
    assert_eq!((MIN_NUM_CTX, MAX_NUM_CTX), (2048, 1_048_576));
    assert_eq!(OllamaAdapter::new().num_ctx(), 16384);
    assert_eq!(OllamaAdapter::default().num_ctx(), 16384);
    assert_eq!(OllamaAdapter::new().with_num_ctx(0).num_ctx(), 2048);
    assert_eq!(OllamaAdapter::new().with_num_ctx(10_000_000).num_ctx(), 1_048_576);
    assert_eq!(OllamaAdapter::new().with_num_ctx(4096).num_ctx(), 4096);
    let small = OllamaAdapter::new().with_num_ctx(4096);
    let sent = json_of(&small.request(&request_with("hello"), &caps()).expect("a request").body);
    assert_eq!(sent["options"]["num_ctx"], json!(4096));

    // The capabilities declare tools and the window; everything else is conservative.
    let declared = small.capabilities("any-model", Locality::Lan);
    assert!(declared.tools && !declared.reasoning && !declared.vision);
    assert_eq!(declared.context_tokens, Some(4096));
    assert_eq!((declared.max_output_tokens, declared.locality), (None, Locality::Lan));
    assert_eq!(adapter.capabilities("m", Locality::Loopback).context_tokens, Some(DEFAULT_NUM_CTX));
    assert_eq!(adapter.wire(), Wire::Ollama);
}

#[test]
fn a_tool_exchange_is_sent_back_with_object_arguments_and_tool_names() {
    let body = body_of(&exchange(), &tool_caps());
    // Arguments are objects, calls carry no id, and a result names the tool that it answers.
    let calls = body["messages"][2]["tool_calls"].as_array().expect("calls");
    assert_eq!(calls[0], json!({ "function": { "name": "read_file", "arguments": { "path": "a.txt" } } }));
    assert!(calls.iter().all(|call| call.get("id").is_none()));
    assert_eq!(body["messages"][3], json!({ "role": "tool", "content": "alpha", "tool_name": "read_file" }));
    assert_eq!(body["messages"][4]["tool_name"], json!("list_dir"));
    assert_eq!(body["messages"][4]["content"], json!(NOT_STORED));

    // An assistant message with calls and no text has an empty string as its content.
    let mut request = exchange();
    request.messages[1].parts.remove(0);
    let body = body_of(&request, &tool_caps());
    assert_eq!(body["messages"][2]["content"], json!(""));
    assert!(body["messages"][2]["tool_calls"].is_array());

    // A result whose call is not in the request has no tool name.
    let mut request = request_with("hi");
    request.messages.push(Message::new(
        "t1",
        Role::Tool,
        vec![Part::ToolResult(ToolResult { call_id: "unknown".to_owned(), content: "x".to_owned(), is_error: false })],
        2,
    ));
    let body = body_of(&request, &caps());
    assert_eq!(body["messages"][2], json!({ "role": "tool", "content": "x" }));

    // Context is rendered exactly as in the compatible dialect: the envelope of an approval the request carries, or one
    // marker line, and never the text a note kept.
    let approved = approved_context("local", "error: the linker failed");
    let envelope = approved.envelope().to_owned();
    let mut request = ChatRequest::new("m");
    request.messages.push(Message::new(
        "u0",
        Role::User,
        vec![
            Part::Context(older_note(
                vec![NoteItem { kind: ContextKind::Screen, label: "Screen of pane 1".to_owned(), bytes: 120 }],
                Some(CANARY),
            )),
            Part::Text { text: "earlier".to_owned() },
        ],
        1,
    ));
    request.messages.push(Message::new("a0", Role::Assistant, Vec::new(), 2));
    request.messages.push(Message::new(
        "u1",
        Role::User,
        vec![
            Part::Context(ContextNote::for_approved(&approved, Some(CANARY.to_owned()))),
            Part::Text { text: "what failed?".to_owned() },
        ],
        3,
    ));
    request.context.push(approved);
    let body = body_of(&request, &caps());
    // The empty assistant message disappears, so the two user messages are neighbours and merge.
    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 1, "{messages:?}");
    let merged = messages[0]["content"].as_str().expect("text");
    let marker =
        "[terminal context from an earlier message is not sent again: screen \"Screen of pane 1\" (120 bytes)]";
    assert_eq!(merged, format!("{marker}\n\nearlier\n\n{envelope}\nwhat failed?"));
    assert!(!body.to_string().contains(CANARY), "the kept text of a note reached the body");
}

// ---- the stream -------------------------------------------------------------------------------------------

#[test]
fn every_split_of_the_ndjson_streams_gives_the_same_events() {
    let fixtures: [(&str, &[u8]); 5] = [
        ("text.ndjson", fixture!("text.ndjson")),
        ("tool_call.ndjson", fixture!("tool_call.ndjson")),
        ("thinking.ndjson", fixture!("thinking.ndjson")),
        ("error_line.ndjson", fixture!("error_line.ndjson")),
        ("prompt_truncated.ndjson", fixture!("prompt_truncated.ndjson")),
    ];
    for (name, bytes) in fixtures {
        let whole = normalised(play(bytes));
        for (label, chunks) in chunkings(bytes, 0x0a11_a001) {
            let split = normalised(run_chunks(&chunks));
            assert_eq!(split, whole, "{name}, {label}");
        }

        // A line ending of carriage return and line feed is tolerated, and blank lines are skipped.
        let crlf: Vec<u8> = bytes.iter().flat_map(|&b| if b == b'\n' { vec![b'\r', b'\n'] } else { vec![b] }).collect();
        assert_eq!(normalised(play(&crlf)), whole, "{name} with CRLF");
        let spaced: Vec<u8> =
            bytes.iter().flat_map(|&b| if b == b'\n' { vec![b'\n', b'\n'] } else { vec![b] }).collect();
        assert_eq!(normalised(play(&spaced)), whole, "{name} with blank lines");
        for (label, chunks) in chunkings(&crlf, 0x0a11_a002).into_iter().step_by(7) {
            assert_eq!(normalised(run_chunks(&chunks)), whole, "{name} with CRLF, {label}");
        }
    }
}

#[test]
fn tool_calls_arrive_whole_with_or_without_ids_and_are_released_after_the_done_line() {
    let bytes = fixture!("tool_call.ndjson");
    let run = play(bytes);
    // The usage arrived with the done line; the call comes after it, from the end of the turn.
    assert_eq!(run.before_finish, 1, "{:?}", run.events);
    match &run.events[..] {
        [Seen::Usage(30, 12), Seen::Call { id, name, args }] => {
            assert!(id.starts_with("call_"), "{id}");
            assert_eq!((name.as_str(), args), ("read_file", &json!({ "path": "a b.txt" })));
        }
        other => panic!("{other:?}"),
    }
    // The server said `stop`, but the turn is a tool turn.
    assert_eq!(run.completion().stop_reason, StopReason::ToolUse);
    let other = play(bytes);
    assert_ne!(run.calls(), other.calls(), "made-up ids are unique between turns");

    // An id that the server sends is kept, with a function-level index beside it.
    let with_id = calls_line(r#"{"id":"call_x1","function":{"index":0,"name":"read_file","arguments":{"path":"a"}}}"#);
    let run = play(&lines_of(&[&with_id, &done_line("stop", 3, 1)]));
    assert_eq!(run.calls(), vec![&read_call("call_x1", "a")]);

    // Two calls on one line leave in order.
    let two = calls_line(
        r#"{"function":{"name":"read_file","arguments":{"path":"a"}}},{"function":{"name":"list_dir","arguments":{"path":"."}}}"#,
    );
    let run = normalised(play(&lines_of(&[&two, &done_line("stop", 3, 1)])));
    assert_eq!(
        run.calls(),
        vec![
            &Seen::Call { id: "call_*".to_owned(), name: "read_file".to_owned(), args: json!({ "path": "a" }) },
            &Seen::Call { id: "call_*".to_owned(), name: "list_dir".to_owned(), args: json!({ "path": "." }) },
        ]
    );

    // Calls with the same id leave with unique ids: the first keeps it, the next gets `-2`, then `-3`.
    let twin =
        |path: &str| format!(r#"{{"id":"X","function":{{"name":"read_file","arguments":{{"path":"{path}"}}}}}}"#);
    let twins = calls_line(&[twin("a"), twin("b")].join(","));
    let more = calls_line(&twin("c"));
    let run = play(&lines_of(&[&twins, &more, &done_line("stop", 3, 1)]));
    assert_eq!(run.calls(), vec![&read_call("X", "a"), &read_call("X-2", "b"), &read_call("X-3", "c")]);

    // A call whose arguments are missing is a call with an empty object.
    let bare = calls_line(r#"{"function":{"name":"list_dir"}}"#);
    let run = normalised(play(&lines_of(&[&bare, &done_line("stop", 3, 1)])));
    assert_eq!(
        run.calls(),
        vec![&Seen::Call { id: "call_*".to_owned(), name: "list_dir".to_owned(), args: json!({}) }]
    );

    // No call is released before the done line has been read: cut at every byte, none arrives; only the whole stream
    // completes, and then the call is the one of the whole stream.
    let whole = normalised(play(bytes));
    for cut in 0..bytes.len() {
        let run = play(&bytes[..cut]);
        assert!(run.calls().is_empty(), "a call was released for a cut at byte {cut}: {:?}", run.events);
        assert!(run.result.is_err(), "a cut at byte {cut} was a completed turn");
    }
    assert_eq!(normalised(play(bytes)), whole);

    // The done line and then a cut inside another record: the end is not clean, so the call is not released.
    let mut tail = bytes.to_vec();
    tail.extend_from_slice(b"{\"message\"");
    let run = play(&tail);
    assert!(run.calls().is_empty(), "{:?}", run.events);
    assert_eq!(run.error().class(), ErrorClass::Protocol);

    // A call and a clean end without the done line: no call.
    let run = play(&lines_of(&[&with_id]));
    assert!(run.calls().is_empty());
    assert_eq!(run.error().class(), ErrorClass::Protocol);
}

#[test]
fn thinking_is_display_only_and_done_reasons_and_counters_become_stop_reasons_and_usage() {
    let run = play(fixture!("thinking.ndjson"));
    assert_eq!(
        run.events,
        vec![
            Seen::Reasoning("Let me ".to_owned()),
            Seen::Reasoning("think.".to_owned()),
            text("Hi"),
            text(" there"),
            Seen::Usage(20, 9),
        ]
    );

    // The thinking is not part of any later request: the message the collector makes holds the answer only.
    let fake = FakeHttp::start(vec![
        Reply::sse_bytes(vec![fixture!("thinking.ndjson").to_vec()]).content_type("application/x-ndjson"),
    ]);
    let provider = provider(&fake, OllamaAdapter::new());
    let mut collector = Collector::new();
    let completion = provider
        .stream(&request_with("greet me"), &mut |event| collector.push(event), &CancelToken::new())
        .expect("a completed turn");
    assert_eq!(collector.reasoning(), "Let me think.");
    let answer = collector.into_message("a1", 2, "test-model", MessageStatus::Complete, Some(completion));
    let mut next = request_with("greet me");
    next.messages.push(answer);
    next.messages.push(Message::user_text("u2", "again", 3));
    let body = body_of(&next, &caps());
    assert_eq!(body["messages"][2], json!({ "role": "assistant", "content": "Hi there" }));
    assert!(!body.to_string().contains("think"), "{body}");

    // The done reasons.
    let reason = |line: &str| play(&lines_of(&[&content("x"), line])).completion().stop_reason;
    assert_eq!(reason(&done_line("stop", 5, 1)), StopReason::EndTurn);
    assert_eq!(reason(r#"{"done":true,"prompt_eval_count":5,"eval_count":1}"#), StopReason::EndTurn);
    assert_eq!(reason(&done_line("length", 5, 1)), StopReason::MaxTokens);
    assert_eq!(reason(&done_line("load", 5, 1)), StopReason::Other);
    assert_eq!(reason(&done_line("unload", 5, 1)), StopReason::Other);
    assert_eq!(reason(&done_line("something_new", 5, 1)), StopReason::Other);

    // Missing counters are zero and produce no usage event; a huge one saturates.
    let run = play(&lines_of(&[&content("x"), r#"{"done":true,"done_reason":"stop"}"#]));
    assert_eq!(run.events, vec![text("x")]);
    assert_eq!(run.completion().usage, Usage::default());
    let run = play(&lines_of(&[&content("x"), r#"{"done":true,"eval_count":7}"#]));
    assert_eq!(run.events, vec![text("x"), Seen::Usage(0, 7)]);
    let run = play(&lines_of(&[&content("x"), r#"{"done":true,"prompt_eval_count":12,"eval_count":5000000000}"#]));
    assert_eq!(run.events, vec![text("x"), Seen::Usage(12, u32::MAX)]);
    assert_eq!(run.completion().usage, Usage { input_tokens: 12, output_tokens: u32::MAX });
    // A prompt counter beyond every window is a conversation that is too long, not a wrapped number.
    let run = play(&lines_of(&[&content("x"), &done_line("stop", 99_999_999_999, 1)]));
    assert_eq!(run.error().class(), ErrorClass::ContextLength);
}

#[test]
fn a_prompt_that_fills_the_window_is_a_context_length_error() {
    let small = OllamaAdapter::new().with_num_ctx(4096);
    let bytes = fixture!("prompt_truncated.ndjson");
    let run = drive(&small, &[bytes], &mut |_| Flow::Continue);
    assert_eq!(run.events[0], text("An answer about the end of a long prompt."), "the text was delivered first");
    assert_eq!(run.result, Err(AiError::ContextLength));

    // One token less than the window is fine, and so is the same counter with the default window.
    let almost = lines_of(&[&content("ok"), &done_line("stop", 4095, 8)]);
    let run = drive(&small, &[&almost], &mut |_| Flow::Continue);
    assert!(run.result.is_ok(), "{:?}", run.result);
    let run = play(bytes);
    assert!(run.result.is_ok(), "the default window is larger: {:?}", run.result);
    assert_eq!(run.completion().usage, Usage { input_tokens: 4096, output_tokens: 8 });

    // The default window itself is the edge for the default adapter, and the floor is the edge for the smallest one.
    let at_default = lines_of(&[&content("ok"), &done_line("stop", u64::from(DEFAULT_NUM_CTX), 8)]);
    assert_eq!(play(&at_default).result, Err(AiError::ContextLength));
    let below_default = lines_of(&[&content("ok"), &done_line("stop", u64::from(DEFAULT_NUM_CTX) - 1, 8)]);
    assert!(play(&below_default).result.is_ok());
    let smallest = OllamaAdapter::new().with_num_ctx(1);
    let at_floor = lines_of(&[&content("ok"), &done_line("stop", u64::from(MIN_NUM_CTX), 8)]);
    assert_eq!(drive(&smallest, &[&at_floor], &mut |_| Flow::Continue).result, Err(AiError::ContextLength));

    // A final record without a prompt counter is not judged.
    let uncounted = lines_of(&[&content("ok"), r#"{"done":true,"done_reason":"stop","eval_count":3}"#]);
    assert!(drive(&small, &[&uncounted], &mut |_| Flow::Continue).result.is_ok());

    // The window of the request is the window of the check: an adapter built for 4096 sends 4096 and judges against it.
    let fake = FakeHttp::start(vec![Reply::sse_bytes(vec![bytes.to_vec()]).content_type("application/x-ndjson")]);
    let provider = provider(&fake, small);
    let error = provider
        .stream(&request_with("a long prompt"), &mut |_| Flow::Continue, &CancelToken::new())
        .expect_err("a prompt that fills the window");
    assert_eq!(error, AiError::ContextLength);
    assert_eq!(json_of(&fake.seen()[0].body)["options"]["num_ctx"], json!(4096));
    assert_eq!(
        provider.capabilities("test-model").context_tokens,
        Some(4096),
        "the loop budgets against the same number"
    );
}

#[test]
fn an_error_line_a_missing_done_line_and_an_unterminated_last_line_fail_the_turn() {
    // An error record after some text: the text arrives, then a server error that shows nothing of the message.
    let run = play(fixture!("error_line.ndjson"));
    assert_eq!(run.events, vec![text("partial")]);
    match run.error() {
        AiError::Server { status, .. } => assert_eq!(*status, 500),
        other => panic!("a server error was expected: {other:?}"),
    }
    let line = shown(run.error());
    assert!(!line.contains(CANARY) && !line.contains("llama runner"), "{line}");

    // An error object is classified by its code, like in the compatible dialect.
    let classified = play(&lines_of(&[&content("x"), r#"{"error":{"code":"context_length_exceeded","message":"m"}}"#]));
    assert_eq!(classified.error(), &AiError::ContextLength);

    // A clean end without the done line is an error, and so is an empty stream.
    let run = play(&lines_of(&[&content("x")]));
    assert_eq!(run.events, vec![text("x")]);
    assert_eq!(run.error().class(), ErrorClass::Protocol);
    let nothing: [&[u8]; 0] = [];
    assert_eq!(run_chunks(&nothing).error().class(), ErrorClass::Protocol);

    // A last line without its line feed is never delivered: the done line that lost its line feed is an error, and the
    // text before it was delivered.
    let mut bytes = lines_of(&[&content("x"), &done_line("stop", 5, 1)]);
    bytes.pop();
    let run = play(&bytes);
    assert_eq!(run.events, vec![text("x")]);
    assert_eq!(run.error().class(), ErrorClass::Protocol);

    // A line that is not JSON is a protocol error that does not show the line.
    let broken = format!("this is not JSON {CANARY}");
    let run = play(&lines_of(&[&content("x"), &broken, &done_line("stop", 5, 1)]));
    assert_eq!(run.events, vec![text("x")]);
    assert_eq!(run.error().class(), ErrorClass::Protocol);
    let line = shown(run.error());
    assert!(!line.contains(CANARY) && !line.contains("not JSON"), "{line}");

    // Lines after the done line are ignored, even a broken one.
    let run = play(&lines_of(&[&content("x"), &done_line("stop", 5, 1), "not json", &content("late")]));
    assert_eq!(run.events, vec![text("x"), Seen::Usage(5, 1)]);
    assert!(run.result.is_ok());

    // An event frame is not what this dialect speaks.
    let mut turn = OllamaAdapter::new().turn();
    let mut decoder = holzwave_core::ai::sse::SseDecoder::new();
    let mut failure = None;
    decoder
        .push(b"data: {}\n\n", &mut |event| {
            failure = turn.frame(Frame::Event(event), &mut |_| Flow::Continue).err();
            Flow::Stop
        })
        .expect("a framed event");
    assert_eq!(failure.expect("an error").class(), ErrorClass::Protocol);
}

#[test]
fn string_arguments_and_unnamed_calls_fail_the_turn_without_a_call() {
    let one = |function: &str| calls_line(&format!(r#"{{"function":{function}}}"#));
    let cases: Vec<(&str, String)> = vec![
        ("arguments as a string", one(&format!(r#"{{"name":"read_file","arguments":"{{\"path\":\"{CANARY}\"}}"}}"#))),
        ("arguments as an array", one(r#"{"name":"read_file","arguments":[1]}"#)),
        ("arguments as a number", one(r#"{"name":"read_file","arguments":7}"#)),
        ("no name", one(r#"{"arguments":{"path":"a"}}"#)),
        ("an empty name", one(r#"{"name":"","arguments":{}}"#)),
        ("no function", calls_line(r#"{"id":"c1"}"#)),
        ("a control character in the name", one(&format!(r#"{{"name":"read\n{CANARY}","arguments":{{}}}}"#))),
        (
            "a 65th call",
            calls_line(
                &(0..65).map(|_| r#"{"function":{"name":"read_file","arguments":{}}}"#).collect::<Vec<_>>().join(","),
            ),
        ),
    ];
    for (name, line) in &cases {
        let run = play(&lines_of(&[line, &done_line("stop", 3, 1)]));
        assert!(run.calls().is_empty(), "{name}: a call was released: {:?}", run.events);
        assert_eq!(run.error().class(), ErrorClass::Protocol, "{name}: {:?}", run.result);
        let line = shown(run.error());
        assert!(!line.contains(CANARY) && !line.contains("read_file"), "{name}: stream text in the error: {line}");
    }

    // Missing arguments are an empty object, and 64 calls are the most one turn may make.
    let run = normalised(play(&lines_of(&[&one(r#"{"name":"read_file"}"#), &done_line("stop", 3, 1)])));
    assert_eq!(run.calls().len(), 1);
    let many = calls_line(
        &(0..64).map(|_| r#"{"function":{"name":"read_file","arguments":{}}}"#).collect::<Vec<_>>().join(","),
    );
    let run = play(&lines_of(&[&many, &done_line("stop", 3, 1)]));
    assert_eq!(run.calls().len(), 64, "{:?}", run.result);
}

// ---- errors -----------------------------------------------------------------------------------------------

fn keyed_endpoint(base: &str) -> Endpoint {
    let target = Target::parse(base, false).expect("a loopback base address");
    let origin = target.origin().clone();
    let secret = SecretHeader::new("authorization", "Bearer ", ApiKey::new(KEY.to_owned()), origin).expect("a header");
    Endpoint::new(Wire::Ollama, target, NetworkPolicy::new(false, false)).with_secret(secret)
}

fn reject(status: u16, body: &[u8]) -> Option<AiError> {
    let endpoint = keyed_endpoint("http://127.0.0.1:9");
    OllamaAdapter::new().rejection(&Rejection { status, retry_after: None, body: body.to_vec() }, &endpoint)
}

#[test]
fn error_bodies_show_the_scrubbed_message_and_a_missing_model_is_an_invalid_request() {
    // A missing model: the server's message is shown, with the status.
    let error = reject(404, fixture!("error_model_not_found.json")).expect("a rejected request");
    assert_eq!(error.class(), ErrorClass::InvalidRequest);
    let line = error.to_string();
    assert!(line.contains("404") && line.contains("not found") && line.contains("try pulling it first"), "{line}");

    // A body that echoes the key through an endpoint that holds it is masked.
    let echo = format!(r#"{{"error":"unauthorized: the key {KEY} is not valid"}}"#);
    let error = reject(401, echo.as_bytes()).expect("an authentication error");
    assert_eq!(error.class(), ErrorClass::Auth);
    let line = shown(&error);
    assert!(!line.contains(KEY) && line.contains("<redacted>") && line.contains("401"), "{line}");

    // The statuses that the chokepoint maps are left to it, and so is a body that is not an error.
    for status in [429, 503] {
        assert_eq!(reject(status, fixture!("error_model_not_found.json")), None, "{status}");
    }
    assert_eq!(reject(500, b"not json"), None);
    assert_eq!(reject(500, b""), None);

    // A context that is too long is named by an object-style error, like in the compatible dialect.
    let long = br#"{"error":{"message":"input exceeds the context window","code":"context_length_exceeded"}}"#;
    assert_eq!(reject(400, long), Some(AiError::ContextLength));

    // Through the server and the shared driver: the error reaches the caller with the status and without the key.
    let fake = FakeHttp::start(vec![Reply::status(401).header("content-type", "application/json").body(echo.clone())]);
    let endpoint = keyed_endpoint(&fake.base());
    let http = Arc::new(Http::new(HttpConfig::standard()));
    let provider = HttpProvider::new("local", endpoint, Box::new(OllamaAdapter::new()), http);
    let error = provider
        .stream(&request_with("hello"), &mut |_| Flow::Continue, &CancelToken::new())
        .expect_err("the server refused the key");
    assert_eq!(error.class(), ErrorClass::Auth);
    assert!(!shown(&error).contains(KEY));
    assert_eq!(fake.seen()[0].header("authorization"), Some(format!("Bearer {KEY}").as_str()));
}

#[test]
fn adapter_and_turn_debug_never_show_stream_or_request_text() {
    let adapter = OllamaAdapter::new().with_num_ctx(4096);
    let mut turn = OllamaTurn::new(4096);
    let thinking = format!(
        r#"{{"message":{{"role":"assistant","content":"{CANARY} text","thinking":"{CANARY} thinking"}},"done":false}}"#
    );
    let call = calls_line(&format!(r#"{{"function":{{"name":"{CANARY}-tool","arguments":{{"x":"{CANARY}"}}}}}}"#));
    let bytes = lines_of(&[&thinking, &call]);
    let mut decoder = NdjsonDecoder::new();
    let mut delivered = 0;
    decoder
        .push(&bytes, &mut |line| {
            turn.frame(Frame::Line(line), &mut |_| {
                delivered += 1;
                Flow::Continue
            })
            .expect("a readable record")
        })
        .expect("a framed stream");
    assert_eq!(delivered, 2, "the thinking and the text were delivered");

    for shown in [format!("{adapter:?}"), format!("{adapter:#?}"), format!("{turn:?}"), format!("{turn:#?}")] {
        assert!(!shown.contains(CANARY), "stream text reached a Debug line: {shown}");
    }
    assert!(format!("{turn:?}").contains("OllamaTurn"));

    // The request does not print either.
    let mut request = request_with(CANARY);
    request.system = CANARY.to_owned();
    let wire = adapter.request(&request, &tool_caps()).expect("a request");
    for shown in [format!("{wire:?}"), format!("{wire:#?}")] {
        assert!(!shown.contains(CANARY), "{shown}");
    }
}
