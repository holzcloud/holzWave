//! The Responses adapter, proved against recorded-shape streams and an in-process server, with no outside network.
//!
//! The streams in `tests/fixtures/ai_wire/openai_responses/` are written by hand from the documented event shapes; they
//! are not recordings of a real account. Every request that goes through a provider goes to `FakeHttp`, an HTTP/1.1
//! server in this process. A stream that is driven directly goes through `SseDecoder` and the turn of the adapter the
//! way the shared driver does it, so a split of the bytes can be chosen at any offset. A failing case names its offset,
//! its chunk size or its seed.

mod common;

use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::Xorshift64;
use common::fake_http::{FakeHttp, Reply};
use holzwave_core::ai::agent::TRIMMED_RESULT;
use holzwave_core::ai::gate::{ApprovedContext, Destination, Gate, OfferRequest, RequestItem};
use holzwave_core::ai::http::{Frame, Http, HttpConfig, NetworkPolicy, RetryPolicy, SecretHeader, Target};
use holzwave_core::ai::provider::{
    AiError, CancelToken, ChatRequest, Collector, Completion, ErrorClass, Flow, Provider, StreamEvent,
};
use holzwave_core::ai::redact::SourceText;
use holzwave_core::ai::sse::{Limits, SseDecoder};
use holzwave_core::ai::store::NOT_STORED;
use holzwave_core::ai::types::{
    ApiKey, Capabilities, Consent, ContextKind, ContextNote, Locality, Message, MessageStatus, NoteItem, Opaque, Part,
    Role, StopReason, ToolCall, ToolResult, ToolSpec, Usage,
};
use holzwave_core::ai::wire::openai_responses::{
    INCLUDE_ENCRYPTED, MAX_CODE_BYTES, MAX_NATIVE_BYTES, MAX_OUTPUT_ITEMS, MAX_TOOLS,
    OpenAiResponsesAdapter, PATH,
};
use holzwave_core::ai::wire::{Endpoint, HttpProvider, Wire, WireAdapter};
use serde_json::{Value, json};

/// The key the tests bind to the origin of the fake server.
const CANARY_KEY: &str = "sk-canary-0123456789abcdef";

/// The bytes of a fixture of this dialect, with line ends normalised: a checkout on Windows may convert them.
macro_rules! fixture {
    ($name:literal) => {
        lf(include_bytes!(concat!("fixtures/ai_wire/openai_responses/", $name)))
    };
}

// ---- helpers ----------------------------------------------------------------------------------------------

/// `bytes` with every CRLF turned into LF.
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

/// The events of a fixture as `(event name, data)` pairs.
fn events_of(bytes: &[u8]) -> Vec<(String, Value)> {
    let text = String::from_utf8(bytes.to_vec()).expect("a fixture is UTF-8");
    text.split("\n\n")
        .filter(|block| !block.trim().is_empty())
        .map(|block| {
            let name = block.lines().find_map(|line| line.strip_prefix("event: ")).expect("an event name");
            let data = block.lines().find_map(|line| line.strip_prefix("data: ")).expect("event data");
            (name.to_owned(), serde_json::from_str(data).expect("event data is JSON"))
        })
        .collect()
}

/// The items of the `response.output_item.done` events of a fixture, in the order of their output index.
fn fixture_items(bytes: &[u8]) -> Vec<Value> {
    let mut items: Vec<(u64, Value)> = events_of(bytes)
        .into_iter()
        .filter(|(name, _)| name == "response.output_item.done")
        .map(|(_, data)| (data["output_index"].as_u64().expect("an index"), data["item"].clone()))
        .collect();
    items.sort_by_key(|(index, _)| *index);
    items.into_iter().map(|(_, item)| item).collect()
}

/// `bytes` cut into pieces of `size` bytes.
fn chunks_of(bytes: &[u8], size: usize) -> Vec<Vec<u8>> {
    bytes.chunks(size).map(<[u8]>::to_vec).collect()
}

fn caps(model: &str) -> Capabilities {
    OpenAiResponsesAdapter::new().capabilities(model, Locality::Cloud)
}

fn json_of(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("JSON")
}

/// A request for `model` holding `messages`, with the system text, the output limit and the tool of these tests.
fn chat(model: &str, messages: Vec<Message>) -> ChatRequest {
    let mut request = ChatRequest::new(model);
    request.system = "You are concise.".to_owned();
    request.messages = messages;
    request.max_output_tokens = Some(2048);
    request.tools.push(read_file_tool());
    request
}

fn read_file_tool() -> ToolSpec {
    ToolSpec {
        name: "read_file".to_owned(),
        description: "Read a file.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
            "additionalProperties": false
        }),
    }
}

/// An endpoint on the fake server: base path `/v1`, the key bound to the origin of the server.
fn endpoint(fake: &FakeHttp) -> Endpoint {
    let target = Target::parse(&format!("{}/v1", fake.base()), false).expect("a loopback base address");
    let secret =
        SecretHeader::new("authorization", "Bearer ", ApiKey::new(CANARY_KEY.to_owned()), target.origin().clone())
            .expect("a key header");
    Endpoint::new(Wire::OpenAiResponses, target, NetworkPolicy::new(false, false)).with_secret(secret)
}

/// A provider on the fake server.
fn provider(fake: &FakeHttp) -> HttpProvider {
    let http = Arc::new(Http::new(HttpConfig::standard()));
    HttpProvider::new("openai", endpoint(fake), Box::new(OpenAiResponsesAdapter::new()), http)
}

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

/// What one turn through a provider delivered.
struct Run {
    events: Vec<Seen>,
    collector: Collector,
    completion: Completion,
}

/// Runs one turn and expects it to complete.
fn run(provider: &dyn Provider, request: &ChatRequest) -> Run {
    let mut collector = Collector::new();
    let mut events = Vec::new();
    let completion = provider
        .stream(
            request,
            &mut |event| {
                events.push(Seen::of(&event));
                collector.push(event)
            },
            &CancelToken::new(),
        )
        .unwrap_or_else(|error| panic!("the turn failed: {error:?} after {events:?}"));
    Run { events, collector, completion }
}

fn user(id: &str, text: &str) -> Message {
    Message::user_text(id, text, 1)
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

/// The sorted member names of an object.
fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value.as_object().expect("an object").keys().cloned().collect();
    keys.sort();
    keys
}

// ---- the tracer -------------------------------------------------------------------------------------------

#[test]
fn a_text_turn_streams_through_the_provider_and_keeps_the_output_items() {
    let body = fixture!("text_turn.sse");
    let fake = FakeHttp::start(vec![Reply::sse_bytes(chunks_of(&body, 7))]);
    let provider = provider(&fake);
    let request = chat("gpt-5", vec![user("u1", "hello")]);

    let run = run(&provider, &request);

    assert_eq!(run.events, vec![text("Hel"), text("lo caf\u{e9}"), Seen::Usage(12, 3)]);
    assert_eq!(run.collector.text(), "Hello caf\u{e9}");
    assert_eq!(run.completion.stop_reason, StopReason::EndTurn);
    assert_eq!(run.completion.usage, Usage { input_tokens: 12, output_tokens: 3 });
    let native = run.completion.native.expect("a native turn");
    assert_eq!(native.wire, "openai_responses");
    assert_eq!(native.json, Value::Array(fixture_items(&body)));
    assert_eq!(native.json.as_array().map(Vec::len), Some(1), "exactly the message item");

    assert_eq!(fake.accepted(), 1, "one connection for one turn");
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].target, "/v1/responses");
    assert_eq!(seen[0].header("authorization"), Some(format!("Bearer {CANARY_KEY}").as_str()));
    assert_eq!(seen[0].header("content-type"), Some("application/json"));
    assert_eq!(seen[0].header("accept"), Some("text/event-stream"));
    assert_eq!(seen[0].header("host"), Some(fake.addr().to_string().as_str()));
    assert_eq!(seen[0].header("accept-encoding"), None, "the stream is not compressed");
    assert_eq!(json_of(&seen[0].body)["store"], json!(false));
    println!(
        "tracer: a_text_turn_streams_through_the_provider_and_keeps_the_output_items ok, text {:?}",
        run.collector.text()
    );
}

#[test]
fn a_tool_turn_with_reasoning_is_replayed_verbatim_in_the_next_request() {
    let first = fixture!("tool_turn_reasoning.sse");
    let second = fixture!("text_after_tool.sse");
    let fake = FakeHttp::start(vec![Reply::sse_bytes(chunks_of(&first, 7)), Reply::sse_bytes(chunks_of(&second, 5))]);
    let provider = provider(&fake);

    let first_request = chat("gpt-5", vec![user("u1", "read a.txt")]);
    let turn = run(&provider, &first_request);

    assert_eq!(
        turn.events,
        vec![
            Seen::Reasoning("Checking the file.".to_owned()),
            Seen::Usage(40, 20),
            Seen::Call { id: "call_b1".to_owned(), name: "read_file".to_owned(), args: json!({ "path": "a.txt" }) },
        ],
        "the call is delivered once, after the usage, from the end of the turn"
    );
    assert_eq!(turn.completion.stop_reason, StopReason::ToolUse);
    assert_eq!(turn.completion.usage, Usage { input_tokens: 40, output_tokens: 20 });
    let stored_items = fixture_items(&first);
    assert_eq!(stored_items.len(), 2);
    assert_eq!(stored_items[0]["type"], "reasoning", "the reasoning item comes first");
    let native = turn.completion.native.clone().expect("a native turn");
    assert_eq!(native, Opaque { wire: "openai_responses".to_owned(), json: Value::Array(stored_items.clone()) });

    let assistant = turn.collector.into_message("m2", 2, "gpt-5", MessageStatus::Complete, Some(turn.completion));
    let second_request =
        chat("gpt-5", vec![user("u1", "read a.txt"), assistant, tool_message("m3", "call_b1", "hello from a.txt")]);
    let wire = OpenAiResponsesAdapter::new().request(&second_request, &caps("gpt-5")).expect("a request");
    let input = json_of(&wire.body)["input"].clone();
    assert_eq!(
        input,
        json!([
            { "role": "user", "content": [{ "type": "input_text", "text": "read a.txt" }] },
            stored_items[0],
            stored_items[1],
            { "type": "function_call_output", "call_id": "call_b1", "output": "hello from a.txt" },
        ]),
        "the stored items are sent back item for item, with their ids and their encrypted content"
    );

    let answer = run(&provider, &second_request);
    assert_eq!(answer.collector.text(), "a.txt says hello.");
    assert_eq!(answer.completion.stop_reason, StopReason::EndTurn);
    assert_eq!(answer.completion.usage, Usage { input_tokens: 70, output_tokens: 8 });

    assert_eq!(fake.accepted(), 2);
    let seen = fake.seen();
    assert_eq!(seen.len(), 2);
    for request in &seen {
        assert_eq!(json_of(&request.body)["store"], json!(false), "no request leaves the conversation on the server");
    }
    assert_eq!(json_of(&seen[1].body)["input"], input, "the server received the replayed items");
    println!(
        "tracer: a_tool_turn_with_reasoning_is_replayed_verbatim_in_the_next_request ok, first body {:?}, second body {:?}",
        keys(&json_of(&seen[0].body)),
        keys(&json_of(&seen[1].body))
    );
}

// ---- the request ------------------------------------------------------------------------------------------

#[test]
fn the_request_never_asks_the_server_to_keep_anything() {
    let adapter = OpenAiResponsesAdapter::new();
    let request = chat("gpt-5", vec![user("u1", "hello")]);
    let wire = adapter.request(&request, &caps("gpt-5")).expect("a request");

    assert_eq!(wire.path, PATH);
    assert_eq!(wire.path, "/responses");
    assert!(wire.headers.is_empty(), "the adapter adds no header");
    assert_eq!(
        json_of(&wire.body),
        json!({
            "model": "gpt-5",
            "instructions": "You are concise.",
            "input": [{ "role": "user", "content": [{ "type": "input_text", "text": "hello" }] }],
            "store": false,
            "stream": true,
            "include": [INCLUDE_ENCRYPTED],
            "max_output_tokens": 2048,
            "tools": [{
                "type": "function",
                "name": "read_file",
                "description": "Read a file.",
                "parameters": read_file_tool().input_schema,
                "strict": false
            }]
        })
    );
    assert_eq!(
        keys(&json_of(&wire.body)).join(","),
        "include,input,instructions,max_output_tokens,model,store,stream,tools"
    );

    // A model that does not reason, with no system text, no tool and no limit.
    let mut plain = ChatRequest::new("gpt-4.1");
    plain.messages.push(user("u1", "hello"));
    let wire = adapter.request(&plain, &caps("gpt-4.1")).expect("a request");
    let body = json_of(&wire.body);
    assert_eq!(keys(&body).join(","), "input,model,store,stream");
    assert_eq!(body["store"], json!(false));
    assert_eq!(body["stream"], json!(true));

    // A reasoning model without a tool and a limit.
    let mut bare = ChatRequest::new("gpt-5");
    bare.messages.push(user("u1", "hello"));
    let wire = adapter.request(&bare, &caps("gpt-5")).expect("a request");
    let body = json_of(&wire.body);
    assert_eq!(keys(&body).join(","), "include,input,model,store,stream");
    assert_eq!(body["store"], json!(false));
    assert_eq!(body["stream"], json!(true));

    let again = adapter.request(&bare, &caps("gpt-5")).expect("a request");
    assert_eq!(wire.body, again.body, "the same request gives the same bytes");
}

#[test]
fn capabilities_follow_the_model_family_and_the_locality() {
    let adapter = OpenAiResponsesAdapter::new();
    for model in ["gpt-5", "gpt-5-mini", "GPT-5", "o3", "o4-mini", "openai/gpt-5", "gpt-6"] {
        let found = adapter.capabilities(model, Locality::Cloud);
        assert!(found.reasoning, "{model} reasons");
    }
    for model in ["gpt-4.1", "gpt-4o", "gpt-5-chat-latest", "llama-3", ""] {
        let found = adapter.capabilities(model, Locality::Cloud);
        assert!(!found.reasoning, "{model:?} does not reason");
    }
    for locality in [Locality::Cloud, Locality::Loopback] {
        for model in ["gpt-5", "gpt-4.1", "llama-3"] {
            let found = adapter.capabilities(model, locality);
            assert!(found.tools, "{model} takes tools");
            assert!(!found.vision);
            assert_eq!(found.context_tokens, None);
            assert_eq!(found.max_output_tokens, None);
            assert_eq!(found.locality, locality, "the locality comes back unchanged");
        }
    }
    assert_eq!(adapter.wire(), Wire::OpenAiResponses);
    assert_eq!(adapter.wire().tag(), "openai_responses");
}

// ---- the request, as tables -------------------------------------------------------------------------------

/// The `input` array of the request for `messages` to `model`, or the error.
fn input_of(model: &str, messages: Vec<Message>) -> Result<Vec<Value>, AiError> {
    let request = chat(model, messages);
    let wire = OpenAiResponsesAdapter::new().request(&request, &caps(model))?;
    Ok(json_of(&wire.body)["input"].as_array().expect("an input array").clone())
}

/// A request error must have the class `invalid_request` and never echo any of `forbidden`.
fn assert_invalid(result: Result<Vec<Value>, AiError>, forbidden: &[&str], case: &str) {
    let error = result.expect_err(case);
    assert_eq!(error.class(), ErrorClass::InvalidRequest, "{case}: {error:?}");
    assert_eq!(error.class().as_str(), "invalid_request", "{case}");
    let shown = format!("{error} {error:?}");
    for text in forbidden {
        assert!(!shown.contains(text), "{case}: the error shows {text}: {shown}");
    }
}

fn assistant(id: &str, parts: Vec<Part>) -> Message {
    Message::new(id, Role::Assistant, parts, 2)
}

fn call(id: &str) -> Part {
    Part::ToolCall(ToolCall { id: id.to_owned(), name: "read_file".to_owned(), args: json!({ "path": "a" }) })
}

fn result_part(call_id: &str, content: &str, is_error: bool) -> Part {
    Part::ToolResult(ToolResult { call_id: call_id.to_owned(), content: content.to_owned(), is_error })
}

fn tool_results(id: &str, results: Vec<Part>) -> Message {
    Message::new(id, Role::Tool, results, 3)
}

/// An approval for the provider `openai` of one screen item whose text holds `text`.
fn approval(label: &str, text: &str) -> ApprovedContext {
    let gate = Gate::new();
    let destination = Destination { host: "api.example.test".to_owned(), locality: Locality::Cloud };
    let item = RequestItem::new(ContextKind::Screen, label, SourceText::plain(text));
    let offer = gate.offer(OfferRequest::new("openai", destination, vec![item])).expect("an offer");
    gate.approve(offer.id(), &[1], "openai").expect("an approval")
}

/// A note that names no approval of any request.
fn history_note(items: Vec<NoteItem>, stored: Option<&str>) -> ContextNote {
    ContextNote {
        hash: "0".repeat(64),
        items,
        stored: stored.map(str::to_owned),
        consented_to: Consent { provider: "openai".to_owned(), locality: Locality::Cloud },
    }
}

fn note_item(label: &str, bytes: u64) -> NoteItem {
    NoteItem { kind: ContextKind::Screen, label: label.to_owned(), bytes }
}

fn user_with(parts: Vec<Part>) -> Message {
    Message::new("u1", Role::User, parts, 1)
}

/// The text of the first `input_text` part of the first input item.
fn first_part_text(input: &[Value]) -> String {
    input[0]["content"][0]["text"].as_str().expect("a text part").to_owned()
}

#[test]
fn approved_context_is_sent_as_user_text_and_a_missing_approval_sends_only_a_marker() {
    const CONTEXT_CANARY: &str = "CTX-CANARY-4d2";
    const STORED_CANARY: &str = "STORED-CANARY-9a7";
    let approved = approval("pane 2 screen", &format!("error: {CONTEXT_CANARY} not found"));
    let note = ContextNote::for_approved(&approved, Some(STORED_CANARY.to_owned()));
    let parts = vec![Part::Context(note.clone()), Part::Text { text: "what failed?".to_owned() }];

    // With the approval in the request: the exact envelope, in the user role, in its own text part.
    let mut request = chat("gpt-5", vec![user_with(parts.clone())]);
    request.context.push(approved.clone());
    let wire = OpenAiResponsesAdapter::new().request(&request, &caps("gpt-5")).expect("a request");
    let body = json_of(&wire.body);
    let input = body["input"].as_array().expect("input");
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["role"], "user");
    let content = input[0]["content"].as_array().expect("content");
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "input_text");
    assert_eq!(content[0]["text"].as_str(), Some(approved.envelope()), "the envelope, byte for byte");
    assert_eq!(content[1]["text"], "what failed?");
    let body_text = String::from_utf8(wire.body.clone()).expect("UTF-8");
    assert!(!body["instructions"].as_str().expect("instructions").contains(CONTEXT_CANARY));
    assert!(!body_text.contains(STORED_CANARY), "the stored text of a note is never sent");
    assert_eq!(
        body_text.matches(CONTEXT_CANARY).count(),
        1,
        "the context text appears in the one part and nowhere else"
    );

    // Without the approval the note is history: one marker line, built from the label and the size.
    let history = chat("gpt-5", vec![user_with(parts)]);
    let wire = OpenAiResponsesAdapter::new().request(&history, &caps("gpt-5")).expect("a request");
    let body_text = String::from_utf8(wire.body.clone()).expect("UTF-8");
    let input = json_of(&wire.body)["input"].as_array().expect("input").clone();
    let bytes = note.items[0].bytes;
    assert_eq!(
        first_part_text(&input),
        format!("[terminal context from an earlier message is not sent again: screen \"pane 2 screen\" ({bytes} bytes)]")
    );
    assert!(!body_text.contains(CONTEXT_CANARY) && !body_text.contains(STORED_CANARY), "{body_text}");

    // The marker forms.
    let marker = |items: Vec<NoteItem>| {
        let message = user_with(vec![
            Part::Context(history_note(items, Some(STORED_CANARY))),
            Part::Text { text: "q".to_owned() },
        ]);
        let input = input_of("gpt-5", vec![message]).expect("a request");
        let first = first_part_text(&input);
        assert!(!first.contains(STORED_CANARY), "{first}");
        first
    };
    assert_eq!(
        marker(vec![note_item("A", 1), note_item("B", 2)]),
        "[terminal context from an earlier message is not sent again: screen \"A\" (1 bytes); screen \"B\" (2 bytes)]"
    );
    assert_eq!(marker(Vec::new()), "[terminal context from an earlier message is not sent again]");
    let long = "x".repeat(200);
    assert_eq!(
        marker(vec![note_item(&long, 3)]),
        format!("[terminal context from an earlier message is not sent again: screen \"{}\u{2026}\" (3 bytes)]", "x".repeat(119))
    );
    assert_eq!(
        marker(vec![note_item("pane\none\ttwo", 5)]),
        "[terminal context from an earlier message is not sent again: screen \"pane one two\" (5 bytes)]"
    );
}

#[test]
fn an_assistant_message_without_a_usable_native_turn_is_rendered_from_its_parts() {
    let messages = vec![
        user("u1", "read it"),
        assistant("m2", vec![Part::Text { text: "I will read it".to_owned() }, call("call_1")]),
        tool_results("m3", vec![result_part("call_1", "ok", false)]),
    ];
    let input = input_of("gpt-5", messages).expect("a request");
    assert_eq!(
        input,
        vec![
            json!({ "role": "user", "content": [{ "type": "input_text", "text": "read it" }] }),
            json!({ "role": "assistant", "content": "I will read it" }),
            json!({ "type": "function_call", "call_id": "call_1", "name": "read_file", "arguments": "{\"path\":\"a\"}" }),
            json!({ "type": "function_call_output", "call_id": "call_1", "output": "ok" }),
        ]
    );
    let text = serde_json::to_string(&input).expect("JSON");
    assert!(
        !text.contains("\"id\"") && !text.contains("reasoning"),
        "a plain replay carries no id and no reasoning: {text}"
    );

    // Two text parts give two items, empty parts are skipped, and a message that renders nothing is no error.
    let messages = vec![
        user("u1", "q"),
        assistant(
            "m2",
            vec![
                Part::Text { text: "one".to_owned() },
                Part::Text { text: String::new() },
                Part::Text { text: "two".to_owned() },
            ],
        ),
        user("u3", "q2"),
        assistant("m4", vec![Part::Text { text: String::new() }]),
        user("u5", "q3"),
    ];
    let input = input_of("gpt-5", messages).expect("a request");
    assert_eq!(input.len(), 5);
    assert_eq!(input[1], json!({ "role": "assistant", "content": "one" }));
    assert_eq!(input[2], json!({ "role": "assistant", "content": "two" }));
    assert_eq!(input[3]["role"], "user");
    assert_eq!(input[4]["role"], "user");

    // A user message with no part adds nothing.
    let input = input_of("gpt-5", vec![user_with(Vec::new()), user("u2", "hi")]).expect("a request");
    assert_eq!(input.len(), 1);
}

#[test]
fn a_native_turn_is_replayed_only_when_it_is_whole_and_belongs_to_this_wire_and_model() {
    let reasoning = json!({ "id": "rs_1", "type": "reasoning", "summary": [], "encrypted_content": "gAAAA-enc-1" });
    let function_call = json!({
        "id": "fc_1", "type": "function_call", "status": "completed", "call_id": "call_1", "name": "read_file",
        "arguments": "{\"path\":\"a\"}"
    });
    let message_item = json!({
        "id": "msg_1", "type": "message", "status": "completed", "role": "assistant",
        "content": [{ "type": "output_text", "annotations": [], "text": "reading" }]
    });
    let plain = [
        json!({ "type": "function_call", "call_id": "call_1", "name": "read_file", "arguments": "{\"path\":\"a\"}" }),
        json!({ "type": "function_call_output", "call_id": "call_1", "output": "ok" }),
    ];
    // (case, wire, message model, stored json, replayed)
    let rows: Vec<(&str, &str, Option<&str>, Value, bool)> = vec![
        ("whole turn", "openai_responses", Some("gpt-5"), json!([reasoning, function_call]), true),
        (
            "whole turn with a message",
            "openai_responses",
            Some("gpt-5"),
            json!([reasoning, message_item, function_call]),
            true,
        ),
        ("another wire", "anthropic", Some("gpt-5"), json!([reasoning, function_call]), false),
        ("another model", "openai_responses", Some("gpt-5-mini"), json!([reasoning, function_call]), false),
        ("no model", "openai_responses", None, json!([reasoning, function_call]), false),
        ("an object", "openai_responses", Some("gpt-5"), json!({ "output": [reasoning] }), false),
        ("an empty array", "openai_responses", Some("gpt-5"), json!([]), false),
        ("a number element", "openai_responses", Some("gpt-5"), json!([reasoning, 7, function_call]), false),
        (
            "a remote tool element",
            "openai_responses",
            Some("gpt-5"),
            json!([reasoning, { "type": "web_search_call", "id": "ws_1" }, function_call]),
            false,
        ),
        (
            "reasoning without encrypted content",
            "openai_responses",
            Some("gpt-5"),
            json!([{ "id": "rs_1", "type": "reasoning", "summary": [] }, function_call]),
            false,
        ),
        (
            "reasoning with empty encrypted content",
            "openai_responses",
            Some("gpt-5"),
            json!([{ "id": "rs_1", "type": "reasoning", "summary": [], "encrypted_content": "" }, function_call]),
            false,
        ),
        (
            "reasoning with a number as encrypted content",
            "openai_responses",
            Some("gpt-5"),
            json!([{ "id": "rs_1", "type": "reasoning", "summary": [], "encrypted_content": 5 }, function_call]),
            false,
        ),
        (
            "a call without a call id",
            "openai_responses",
            Some("gpt-5"),
            json!([reasoning, { "id": "fc_1", "type": "function_call", "name": "read_file", "arguments": "{}" }]),
            false,
        ),
        (
            "a message of the user role",
            "openai_responses",
            Some("gpt-5"),
            json!([reasoning, { "type": "message", "role": "user", "content": "x" }, function_call]),
            false,
        ),
        (
            "a message without a role",
            "openai_responses",
            Some("gpt-5"),
            json!([reasoning, { "type": "message", "content": "x" }, function_call]),
            false,
        ),
    ];
    for (case, wire, model, stored, replayed) in rows {
        let mut assistant_message = assistant("m2", vec![call("call_1")]);
        assistant_message.model = model.map(str::to_owned);
        assistant_message.native = Some(Opaque { wire: wire.to_owned(), json: stored.clone() });
        let messages =
            vec![user("u1", "read"), assistant_message, tool_results("m3", vec![result_part("call_1", "ok", false)])];
        let input = input_of("gpt-5", messages).unwrap_or_else(|error| panic!("{case}: {error:?}"));
        let user_item = json!({ "role": "user", "content": [{ "type": "input_text", "text": "read" }] });
        let expected: Vec<Value> = if replayed {
            let mut items = vec![user_item];
            items.extend(stored.as_array().expect("an array").iter().cloned());
            items.push(plain[1].clone());
            items
        } else {
            let mut items = vec![user_item];
            items.extend(plain.iter().cloned());
            items
        };
        assert_eq!(input, expected, "{case}");
    }

    // A stored turn over the item bound is not replayed either.
    let item = |n: usize| json!({ "id": format!("msg_{n}"), "type": "message", "status": "completed", "role": "assistant", "content": [] });
    let render = |count: usize| {
        let mut message = assistant("m2", vec![Part::Text { text: "plain text".to_owned() }]);
        message.model = Some("gpt-5".to_owned());
        message.native =
            Some(Opaque { wire: "openai_responses".to_owned(), json: Value::Array((0..count).map(item).collect()) });
        input_of("gpt-5", vec![user("u1", "q"), message]).expect("a request")
    };
    assert_eq!(render(MAX_OUTPUT_ITEMS).len(), 1 + MAX_OUTPUT_ITEMS, "a stored turn at the bound is replayed");
    let over = render(MAX_OUTPUT_ITEMS + 1);
    assert_eq!(over.len(), 2, "a stored turn over the bound is written again from its parts");
    assert_eq!(over[1], json!({ "role": "assistant", "content": "plain text" }));
}

#[test]
fn tool_results_must_pair_with_earlier_tool_calls() {
    let answered = |id: &str| result_part(id, "ok", false);
    let invalid_rows: Vec<(&str, Vec<Message>)> = vec![
        ("a result for a call nobody made", vec![user("u1", "q"), tool_results("m2", vec![answered("call_x")])]),
        (
            "two results for one call",
            vec![
                user("u1", "q"),
                assistant("m2", vec![call("call_1")]),
                tool_results("m3", vec![answered("call_1"), answered("call_1")]),
            ],
        ),
        (
            "a second result in a later message",
            vec![
                user("u1", "q"),
                assistant("m2", vec![call("call_1")]),
                tool_results("m3", vec![answered("call_1")]),
                tool_results("m4", vec![answered("call_1")]),
            ],
        ),
        (
            "a call followed by a user message",
            vec![user("u1", "q"), assistant("m2", vec![call("call_1")]), user("u3", "again")],
        ),
        ("a call at the end of the input", vec![user("u1", "q"), assistant("m2", vec![call("call_1")])]),
        (
            "a result before its call",
            vec![user("u1", "q"), tool_results("m2", vec![answered("call_1")]), assistant("m3", vec![call("call_1")])],
        ),
        (
            "one of two calls unanswered",
            vec![
                user("u1", "q"),
                assistant("m2", vec![call("call_1"), call("call_2")]),
                tool_results("m3", vec![answered("call_2")]),
                user("u4", "next"),
            ],
        ),
        (
            "two calls with one id and one result",
            vec![
                user("u1", "q"),
                assistant("m2", vec![call("call_1"), call("call_1")]),
                tool_results("m3", vec![answered("call_1")]),
            ],
        ),
        (
            "a result that comes after a user message",
            vec![
                user("u1", "q"),
                assistant("m2", vec![call("call_1")]),
                user("u3", "again"),
                tool_results("m4", vec![answered("call_1")]),
            ],
        ),
        (
            "a call without an id",
            vec![user("u1", "q"), assistant("m2", vec![call("")]), tool_results("m3", vec![answered("")])],
        ),
    ];
    for (case, messages) in invalid_rows {
        assert_invalid(input_of("gpt-5", messages), &[], case);
    }

    // Two calls answered in the opposite order are accepted and rendered in the order of the results.
    let messages = vec![
        user("u1", "q"),
        assistant("m2", vec![call("call_1"), call("call_2")]),
        tool_results("m3", vec![answered("call_2"), answered("call_1")]),
    ];
    let input = input_of("gpt-5", messages).expect("a request");
    let outputs: Vec<&str> = input
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| item["call_id"].as_str().expect("id"))
        .collect();
    assert_eq!(outputs, ["call_2", "call_1"]);

    // The fixed texts of the loop and the store are ordinary content, and the error flag has no member.
    let messages = vec![
        user("u1", "q"),
        assistant("m2", vec![call("call_1"), call("call_2"), call("call_3")]),
        tool_results(
            "m3",
            vec![
                result_part("call_1", NOT_STORED, false),
                result_part("call_2", TRIMMED_RESULT, false),
                result_part("call_3", "boom", true),
            ],
        ),
    ];
    let input = input_of("gpt-5", messages).expect("a request");
    let outputs: Vec<&Value> = input.iter().filter(|item| item["type"] == "function_call_output").collect();
    assert_eq!(outputs[0]["output"], NOT_STORED);
    assert_eq!(outputs[1]["output"], TRIMMED_RESULT);
    assert_eq!(outputs[2]["output"], "boom");
    assert_eq!(keys(outputs[2]).join(","), "call_id,output,type", "the error flag has no member in this dialect");

    // A call of a replayed turn is answered in the next message.
    let mut replayed = assistant("m2", vec![call("call_1")]);
    replayed.model = Some("gpt-5".to_owned());
    replayed.native = Some(Opaque {
        wire: "openai_responses".to_owned(),
        json: json!([{ "id": "fc_1", "type": "function_call", "call_id": "call_1", "name": "read_file", "arguments": "{}" }]),
    });
    let input = input_of("gpt-5", vec![user("u1", "q"), replayed, tool_results("m3", vec![answered("call_1")])])
        .expect("a request");
    assert_eq!(input.len(), 3);
    assert_eq!(input[1]["id"], "fc_1", "the stored item is replayed");
}

#[test]
fn roles_and_parts_must_agree_and_request_errors_never_echo_text() {
    const ROLE_CANARY: &str = "ROLE-CANARY-3b8";
    let canary_call = Part::ToolCall(ToolCall {
        id: ROLE_CANARY.to_owned(),
        name: ROLE_CANARY.to_owned(),
        args: json!({ "path": ROLE_CANARY }),
    });
    let canary_result = result_part(ROLE_CANARY, ROLE_CANARY, false);
    let canary_text = Part::Text { text: ROLE_CANARY.to_owned() };
    let canary_context = Part::Context(history_note(vec![note_item(ROLE_CANARY, 1)], Some(ROLE_CANARY)));
    let rows: Vec<(&str, Message)> = vec![
        ("a user message with a tool call", Message::new("m", Role::User, vec![canary_call.clone()], 1)),
        ("a user message with a tool result", Message::new("m", Role::User, vec![canary_result.clone()], 1)),
        ("an assistant message with a tool result", Message::new("m", Role::Assistant, vec![canary_result.clone()], 1)),
        ("an assistant message with a context", Message::new("m", Role::Assistant, vec![canary_context.clone()], 1)),
        ("a tool message with a text", Message::new("m", Role::Tool, vec![canary_text.clone()], 1)),
        ("a tool message with a context", Message::new("m", Role::Tool, vec![canary_context.clone()], 1)),
    ];
    for (case, message) in rows {
        assert_invalid(input_of("gpt-5", vec![user("u1", "q"), message]), &[ROLE_CANARY], case);
    }
    assert_invalid(input_of("gpt-5", Vec::new()), &[ROLE_CANARY], "a request with no messages");
    assert_invalid(input_of("gpt-5", vec![user_with(Vec::new())]), &[ROLE_CANARY], "a request that renders nothing");

    // Pairing errors name the kind of problem and nothing of the request.
    assert_invalid(
        input_of("gpt-5", vec![user("u1", "q"), tool_results("m2", vec![canary_result.clone()])]),
        &[ROLE_CANARY],
        "an orphan result with a canary id",
    );
    assert_invalid(
        input_of("gpt-5", vec![user("u1", "q"), assistant("m2", vec![canary_call])]),
        &[ROLE_CANARY],
        "an unanswered call with a canary id and name",
    );

    // The tool bound.
    let mut request = chat("gpt-5", vec![user("u1", "q")]);
    request.tools = (0..=MAX_TOOLS)
        .map(|n| ToolSpec {
            name: format!("{ROLE_CANARY}-{n}"),
            description: ROLE_CANARY.to_owned(),
            input_schema: json!({}),
        })
        .collect();
    let error = OpenAiResponsesAdapter::new().request(&request, &caps("gpt-5")).expect_err("129 tools");
    assert_eq!(error.class(), ErrorClass::InvalidRequest);
    let shown = format!("{error} {error:?}");
    assert!(!shown.contains(ROLE_CANARY), "{shown}");
}

#[test]
fn tools_are_declared_as_non_strict_functions_and_only_when_the_model_can_use_them() {
    let adapter = OpenAiResponsesAdapter::new();
    let schema = json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "minLength": 1 },
            "options": { "type": "object", "properties": { "limit": { "type": "integer", "minimum": 1 } } }
        },
        "required": ["path"],
        "additionalProperties": false
    });
    let spec = |n: usize| ToolSpec {
        name: format!("tool_{n}"),
        description: format!("Does thing {n}."),
        input_schema: schema.clone(),
    };

    let mut request = chat("gpt-5", vec![user("u1", "q")]);
    request.tools = (0..MAX_TOOLS).map(spec).collect();
    let wire = adapter.request(&request, &caps("gpt-5")).expect("128 tools are accepted");
    let body = json_of(&wire.body);
    let tools = body["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), MAX_TOOLS);
    assert_eq!(MAX_TOOLS, 128);
    for (n, tool) in tools.iter().enumerate() {
        assert_eq!(keys(tool).join(","), "description,name,parameters,strict,type");
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["name"], format!("tool_{n}"));
        assert_eq!(tool["description"], format!("Does thing {n}."));
        assert_eq!(tool["parameters"], schema, "the schema is sent unchanged");
        assert_eq!(tool["strict"], json!(false));
    }

    request.tools.push(spec(MAX_TOOLS));
    let error = adapter.request(&request, &caps("gpt-5")).expect_err("129 tools are refused");
    assert_eq!(error.class(), ErrorClass::InvalidRequest);

    // A model that cannot use tools gets none, whatever the request lists.
    request.tools.truncate(2);
    let blind = Capabilities { tools: false, ..caps("gpt-5") };
    let wire = adapter.request(&request, &blind).expect("a request");
    assert!(json_of(&wire.body).get("tools").is_none());

    // No tool, no member.
    request.tools.clear();
    let wire = adapter.request(&request, &caps("gpt-5")).expect("a request");
    assert!(json_of(&wire.body).get("tools").is_none());
}

#[test]
fn the_request_body_is_deterministic_and_carries_none_of_our_bookkeeping() {
    let mut earlier = assistant("m2", vec![Part::Text { text: "I will read it".to_owned() }, call("call_1")]);
    earlier.model = Some("gpt-5-mini".to_owned());
    earlier.usage = Some(Usage { input_tokens: 123, output_tokens: 456 });
    earlier.status = MessageStatus::Interrupted;
    let messages = vec![
        user("m1", "read a"),
        earlier,
        tool_results("m3", vec![result_part("call_1", "ok", false)]),
        user("m4", "thanks"),
    ];
    let request = chat("gpt-5", messages);
    let adapter = OpenAiResponsesAdapter::new();
    let first = adapter.request(&request, &caps("gpt-5")).expect("a request");
    let second = adapter.request(&request.clone(), &caps("gpt-5")).expect("a request");
    assert_eq!(first.body, second.body, "the same request gives the same bytes");

    let body_text = String::from_utf8(first.body).expect("UTF-8");
    for id in ["m1", "m2", "m3", "m4"] {
        assert!(!body_text.contains(&format!("\"{id}\"")), "the message id {id} is sent: {body_text}");
    }
    for member in ["ts", "status", "usage", "id"] {
        assert!(!body_text.contains(&format!("\"{member}\"")), "the member {member} is sent: {body_text}");
    }
    assert!(!body_text.contains("gpt-5-mini"), "the model of an earlier message is not sent");
    assert!(!body_text.contains("123") && !body_text.contains("456"), "no usage value is sent");
}

// ---- the stream, as tables and properties -----------------------------------------------------------------

/// What one pass over a stream delivered and how the turn ended.
struct Driven {
    events: Vec<Seen>,
    result: Result<Completion, AiError>,
}

impl Driven {
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

/// Drives the chunks through a decoder with `limits` and a fresh turn, exactly as the shared driver does: an error from
/// the turn is kept and answered with a stop, an error from the decoder becomes a protocol error, and a kept error wins
/// over the verdict of the end. `answer` gets the number of the event and returns what the consumer says.
fn drive_with<T: AsRef<[u8]>>(chunks: &[T], limits: Limits, answer: &mut dyn FnMut(usize) -> Flow) -> Driven {
    let mut turn = OpenAiResponsesAdapter::new().turn();
    let mut decoder = SseDecoder::with_limits(limits);
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
    let result = match failure {
        Some(error) => Err(error),
        None => turn.finish(end, &mut out),
    };
    Driven { events: events.into_inner(), result }
}

fn drive_chunks<T: AsRef<[u8]>>(chunks: &[T]) -> Driven {
    drive_with(chunks, Limits::DEFAULT, &mut |_| Flow::Continue)
}

fn play(bytes: &[u8]) -> Driven {
    drive_chunks(&[bytes])
}

/// One comparable string for a pass: the events, then the completion or the error class and text.
fn outcome(driven: &Driven) -> String {
    let mut lines: Vec<String> = driven
        .events
        .iter()
        .map(|event| match event {
            Seen::Text(text) => format!("text {text}"),
            Seen::Reasoning(text) => format!("reasoning {text}"),
            Seen::Call { id, name, args } => format!("call {id}|{name}|{args}"),
            Seen::Usage(input, output) => format!("usage {input} {output}"),
        })
        .collect();
    lines.push(match &driven.result {
        Ok(completion) => format!(
            "ok {:?} {:?} {}",
            completion.stop_reason,
            completion.usage,
            completion.native.as_ref().map_or_else(|| "no native".to_owned(), |native| native.json.to_string())
        ),
        Err(error) => format!("error {}: {error}", error.class().as_str()),
    });
    lines.join("\n")
}

/// The bytes with every line feed written as CRLF.
fn crlf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        if byte == b'\n' {
            out.push(b'\r');
        }
        out.push(byte);
    }
    out
}

/// One event as the server writes it: its name, its compact data and the blank line.
fn block(name: &str, data: &Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

/// One event with data that is not necessarily JSON.
fn raw_block(name: &str, data: &str) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn sse(blocks: &[String]) -> Vec<u8> {
    blocks.concat().into_bytes()
}

fn delta_block(text: &str) -> String {
    block(
        "response.output_text.delta",
        &json!({ "type": "response.output_text.delta", "item_id": "msg_x", "output_index": 0, "content_index": 0, "delta": text }),
    )
}

fn message_item(index: usize, text: &str) -> Value {
    json!({
        "id": format!("msg_{index}"), "type": "message", "status": "completed", "role": "assistant",
        "content": [{ "type": "output_text", "annotations": [], "text": text }]
    })
}

fn item_done_block(index: usize, item: &Value) -> String {
    block(
        "response.output_item.done",
        &json!({ "type": "response.output_item.done", "output_index": index, "item": item }),
    )
}

fn completed_block(output: Option<Vec<Value>>, usage: Option<Value>) -> String {
    let mut response = json!({ "id": "resp_x", "status": "completed" });
    if let Some(output) = output {
        response["output"] = Value::Array(output);
    }
    if let Some(usage) = usage {
        response["usage"] = usage;
    }
    block("response.completed", &json!({ "type": "response.completed", "response": response }))
}

fn call_item(call_id: Option<&str>, name: &str, arguments: &str) -> Value {
    let mut item =
        json!({ "id": "fc_x", "type": "function_call", "status": "completed", "name": name, "arguments": arguments });
    if let Some(call_id) = call_id {
        item["call_id"] = json!(call_id);
    }
    item
}

fn call_added_block(index: usize, item: &Value) -> String {
    let mut open = item.clone();
    open["arguments"] = json!("");
    open["status"] = json!("in_progress");
    block(
        "response.output_item.added",
        &json!({ "type": "response.output_item.added", "output_index": index, "item": open }),
    )
}

fn args_delta_block(index: usize, delta: &str) -> String {
    block(
        "response.function_call_arguments.delta",
        &json!({ "type": "response.function_call_arguments.delta", "output_index": index, "delta": delta }),
    )
}

fn args_done_block(index: usize, arguments: &str) -> String {
    block(
        "response.function_call_arguments.done",
        &json!({ "type": "response.function_call_arguments.done", "output_index": index, "arguments": arguments }),
    )
}

/// The blocks of a fixture, one string per event.
fn blocks_of(bytes: &[u8]) -> Vec<String> {
    String::from_utf8(bytes.to_vec()).expect("UTF-8").split_inclusive("\n\n").map(str::to_owned).collect()
}

/// A stream around one call `call_s`: the arguments as deltas, as an arguments-done event and as the text of the
/// finished item, each optional.
fn call_stream(deltas: &[&str], args_done: Option<&str>, item_args: &str, call_id: Option<&str>) -> Vec<u8> {
    let item = call_item(call_id, "read_file", item_args);
    let mut blocks = vec![call_added_block(0, &item)];
    blocks.extend(deltas.iter().map(|delta| args_delta_block(0, delta)));
    if let Some(done) = args_done {
        blocks.push(args_done_block(0, done));
    }
    blocks.push(item_done_block(0, &item));
    blocks.push(completed_block(Some(vec![item]), Some(json!({ "input_tokens": 1, "output_tokens": 2 }))));
    sse(&blocks)
}

fn path_call(id: &str, path: &str) -> Seen {
    Seen::Call { id: id.to_owned(), name: "read_file".to_owned(), args: json!({ "path": path }) }
}

#[test]
fn every_golden_stream_gives_the_same_result_at_every_split() {
    const SEED: u64 = 0x5eed_0016;
    let streams: Vec<(&str, Vec<u8>)> = vec![
        ("text_turn", fixture!("text_turn.sse")),
        ("tool_turn_reasoning", fixture!("tool_turn_reasoning.sse")),
        ("text_after_tool", fixture!("text_after_tool.sse")),
        ("refusal", fixture!("refusal.sse")),
    ];
    for (name, bytes) in streams {
        let whole = play(&bytes);
        let baseline = outcome(&whole);
        assert!(whole.result.is_ok(), "{name}: the golden stream completes: {baseline}");
        let mut runs = 0;
        let mut check = |label: String, chunks: Vec<Vec<u8>>| {
            runs += 1;
            assert_eq!(outcome(&drive_chunks(&chunks)), baseline, "{name}: {label}");
        };
        for at in 1..bytes.len() {
            check(format!("split at offset {at}"), vec![bytes[..at].to_vec(), bytes[at..].to_vec()]);
        }
        for size in [1, 2, 3, 5, 7, 64] {
            check(format!("chunks of {size} bytes"), chunks_of(&bytes, size));
        }
        check("the CRLF version in one chunk".to_owned(), vec![crlf(&bytes)]);
        check("the CRLF version in chunks of 3 bytes".to_owned(), chunks_of(&crlf(&bytes), 3));
        for n in 0..200 {
            let seed = SEED + n;
            let mut random = Xorshift64::new(seed);
            let mut chunks = Vec::new();
            let mut at = 0;
            while at < bytes.len() {
                let end = (at + 1 + random.below(40)).min(bytes.len());
                chunks.push(bytes[at..end].to_vec());
                at = end;
            }
            check(format!("random chunks, seed {seed}"), chunks);
        }
        assert!(runs > bytes.len(), "{name}: every split offset was run");
    }

    // The baselines are not vacuous: the outcomes are what the fixtures say.
    let text = play(&fixture!("text_turn.sse"));
    assert_eq!(text.events, vec![self::text("Hel"), self::text("lo caf\u{e9}"), Seen::Usage(12, 3)]);
    let tool = play(&fixture!("tool_turn_reasoning.sse"));
    assert_eq!(tool.calls(), vec![&path_call("call_b1", "a.txt")]);
}

#[test]
fn a_stream_cut_at_any_byte_never_releases_a_tool_call() {
    let bytes = fixture!("tool_turn_reasoning.sse");
    for cut in 0..bytes.len() {
        let driven = play(&bytes[..cut]);
        assert!(driven.calls().is_empty(), "a call left a stream cut at byte {cut}: {:?}", driven.events);
        assert!(driven.result.is_err(), "a stream cut at byte {cut} completed");
    }
    // The whole stream does release it, once.
    assert_eq!(play(&bytes).calls().len(), 1);

    // A clean end at an event boundary, with the completed event missing.
    let blocks = blocks_of(&bytes);
    let without_completed = sse(&blocks[..blocks.len() - 1]);
    let driven = play(&without_completed);
    assert!(driven.calls().is_empty(), "{:?}", driven.events);
    assert_eq!(driven.error().class(), ErrorClass::Protocol);

    // The call is left open: no arguments-done event and no finished item for it.
    let open: Vec<String> = blocks
        .iter()
        .filter(|block| {
            !block.starts_with("event: response.function_call_arguments.done")
                && !(block.starts_with("event: response.output_item.done") && block.contains("\"function_call\""))
        })
        .cloned()
        .collect();
    assert!(open.len() + 2 == blocks.len(), "two events were removed");
    let driven = play(&sse(&open));
    assert!(driven.calls().is_empty(), "{:?}", driven.events);
    assert_eq!(driven.error().class(), ErrorClass::Protocol);
}

#[test]
fn failed_and_error_events_end_the_turn_with_a_stable_class_and_no_provider_text() {
    const CANARY: &str = "FAIL-CANARY-r5e1";
    let long = "c".repeat(MAX_CODE_BYTES + 1);
    assert_eq!(MAX_CODE_BYTES, 64);
    let at_limit = "c".repeat(MAX_CODE_BYTES);
    // (code, expected class, expected status of a server error)
    let table: Vec<(&str, ErrorClass, Option<u16>)> = vec![
        ("server_error", ErrorClass::Server, Some(500)),
        ("internal_error", ErrorClass::Server, Some(500)),
        ("context_length_exceeded", ErrorClass::ContextLength, None),
        ("rate_limit_exceeded", ErrorClass::RateLimit, None),
        ("insufficient_quota", ErrorClass::Auth, None),
        ("overloaded", ErrorClass::Overload, None),
        ("server_is_overloaded", ErrorClass::Overload, None),
        ("slow_down", ErrorClass::Overload, None),
        ("service_unavailable", ErrorClass::Overload, None),
        ("invalid_prompt", ErrorClass::ContentFilter, None),
        ("content_filter", ErrorClass::ContentFilter, None),
        ("content_policy_violation", ErrorClass::ContentFilter, None),
        ("invalid_encrypted_content", ErrorClass::InvalidRequest, None),
        ("model_not_found", ErrorClass::InvalidRequest, None),
        ("invalid_request_error", ErrorClass::InvalidRequest, None),
        ("invalid_value", ErrorClass::InvalidRequest, None),
        ("unsupported_parameter", ErrorClass::InvalidRequest, None),
        ("unsupported_value", ErrorClass::InvalidRequest, None),
        ("missing_required_parameter", ErrorClass::InvalidRequest, None),
        ("weird_code", ErrorClass::Protocol, None),
    ];
    for (code, class, status) in &table {
        for shape in ["response.failed", "error"] {
            let failure = if shape == "error" {
                json!({ "type": "error", "code": code, "message": CANARY, "param": null })
            } else {
                json!({ "type": "response.failed", "response": { "status": "failed", "error": { "code": code, "message": CANARY } } })
            };
            let bytes = sse(&[delta_block("Par"), block(shape, &failure)]);
            let driven = play(&bytes);
            let case = format!("{shape} with {code}");
            assert_eq!(driven.events, vec![text("Par")], "{case}: the text before the failure was delivered");
            let error = driven.error();
            assert_eq!(error.class(), *class, "{case}: {error:?}");
            if let Some(status) = status {
                assert!(matches!(error, AiError::Server { status: found, .. } if found == status), "{case}: {error:?}");
            }
            let shown = format!("{error} {error:?}");
            assert!(!shown.contains(CANARY), "{case}: the provider message is shown: {shown}");
        }
    }

    // A code that is unknown shows only when it is safe; the message is never read.
    let unknown = |code: Value, shape: &str| {
        let failure = if shape == "error" {
            json!({ "type": "error", "code": code, "message": CANARY })
        } else {
            json!({ "type": "response.failed", "response": { "error": { "code": code, "message": CANARY } } })
        };
        let driven = play(&sse(&[block(shape, &failure)]));
        let error = driven.error().clone();
        assert_eq!(error.class(), ErrorClass::Protocol, "{error:?}");
        format!("{error} {error:?}")
    };
    for shape in ["response.failed", "error"] {
        let shown = unknown(json!("weird_code"), shape);
        assert!(shown.contains("weird_code") && !shown.contains(CANARY), "{shown}");
        let shown = unknown(json!(at_limit), shape);
        assert!(shown.contains(&at_limit), "a code at the limit may be shown: {shown}");
        for hostile in [json!("Bad Code<b>"), json!(long), json!("code\u{1b}[2J"), json!(5), json!(null), json!(["x"])]
        {
            let shown = unknown(hostile.clone(), shape);
            assert!(
                !shown.contains("Bad Code") && !shown.contains("<b>") && !shown.contains(&long),
                "{shape} {hostile}: {shown}"
            );
            assert!(!shown.contains(CANARY) && !shown.contains('\u{1b}'), "{shape} {hostile}: {shown}");
        }
    }
    // No code at all.
    let driven = play(&sse(&[block("error", &json!({ "type": "error", "message": CANARY }))]));
    assert_eq!(driven.error().class(), ErrorClass::Protocol);
    assert!(!format!("{}", driven.error()).contains(CANARY));
    let driven = play(&sse(&[block("response.failed", &json!({ "type": "response.failed" }))]));
    assert_eq!(driven.error().class(), ErrorClass::Protocol);

    // The recorded shape: text, then a failed response with the code of a server error.
    let driven = play(&fixture!("failed_midstream.sse"));
    assert_eq!(driven.events, vec![text("Par")]);
    assert!(matches!(driven.error(), AiError::Server { status: 500, .. }), "{:?}", driven.error());
    let shown = format!("{} {:?}", driven.error(), driven.error());
    assert!(!shown.contains(CANARY), "{shown}");
}

#[test]
fn an_incomplete_response_keeps_its_text_but_never_releases_a_tool_call() {
    let driven = play(&fixture!("incomplete_max_tokens.sse"));
    assert_eq!(driven.events, vec![text("The answer is"), Seen::Usage(30, 2048)]);
    let completion = driven.completion();
    assert_eq!(completion.stop_reason, StopReason::MaxTokens);
    assert_eq!(completion.usage, Usage { input_tokens: 30, output_tokens: 2048 });
    assert_eq!(completion.native, None, "a cut reply is not kept for replay");

    // The reason decides the verdict.
    let incomplete = |details: Value| {
        let mut response =
            json!({ "id": "resp_x", "status": "incomplete", "usage": { "input_tokens": 1, "output_tokens": 2 } });
        if !details.is_null() {
            response["incomplete_details"] = details;
        }
        let end = block("response.incomplete", &json!({ "type": "response.incomplete", "response": response }));
        play(&sse(&[delta_block("Hi"), end]))
    };
    let filtered = incomplete(json!({ "reason": "content_filter" }));
    assert!(matches!(filtered.error(), AiError::ContentFilter), "{:?}", filtered.error());
    assert_eq!(filtered.events, vec![text("Hi"), Seen::Usage(1, 2)]);
    for details in [json!({ "reason": "something_else" }), json!({}), json!(null), json!({ "reason": 7 })] {
        let driven = incomplete(details.clone());
        assert_eq!(driven.completion().stop_reason, StopReason::Other, "{details}");
        assert_eq!(driven.completion().native, None);
    }
    assert_eq!(incomplete(json!({ "reason": "max_output_tokens" })).completion().stop_reason, StopReason::MaxTokens);

    // Anything after the terminal event that is not a whole event is an unclean end.
    let mut bytes = fixture!("text_turn.sse");
    bytes.extend_from_slice(b"event: response.created\ndata: {");
    let driven = play(&bytes);
    assert_eq!(driven.error().class(), ErrorClass::Protocol);
    assert!(driven.error().to_string().contains("before the response was complete"), "{}", driven.error());

    // The tool stream with its completed event replaced by an incomplete one: the call is pending, so the turn fails.
    let blocks = blocks_of(&fixture!("tool_turn_reasoning.sse"));
    let mut cut = blocks[..blocks.len() - 1].to_vec();
    cut.push(block(
        "response.incomplete",
        &json!({
            "type": "response.incomplete",
            "response": { "status": "incomplete", "incomplete_details": { "reason": "max_output_tokens" } }
        }),
    ));
    let driven = play(&sse(&cut));
    assert!(driven.calls().is_empty(), "{:?}", driven.events);
    assert_eq!(driven.error().class(), ErrorClass::Protocol);
    assert_eq!(driven.events, vec![Seen::Reasoning("Checking the file.".to_owned())], "what came before is kept");
}

#[test]
fn a_refusal_is_shown_and_ends_with_the_refusal_stop_reason() {
    let driven = play(&fixture!("refusal.sse"));
    assert_eq!(driven.events, vec![text("I can't help "), text("with that."), Seen::Usage(10, 5)]);
    let completion = driven.completion();
    assert_eq!(completion.stop_reason, StopReason::Refusal);
    assert_eq!(completion.usage, Usage { input_tokens: 10, output_tokens: 5 });
    let native = completion.native.as_ref().expect("a completed reply is kept");
    assert_eq!(native.json[0]["content"][0]["type"], "refusal");
}

#[test]
fn tool_arguments_come_from_deltas_then_the_done_event_then_the_item_and_must_be_an_object() {
    let cases: Vec<(&str, Vec<u8>, Seen)> = vec![
        ("deltas only", call_stream(&["{\"path\":", "\"d\"}"], None, "", Some("call_s")), path_call("call_s", "d")),
        (
            "no deltas, an arguments-done event",
            call_stream(&[], Some("{\"path\":\"e\"}"), "", Some("call_s")),
            path_call("call_s", "e"),
        ),
        (
            "no deltas and no done event, the item",
            call_stream(&[], None, "{\"path\":\"f\"}", Some("call_s")),
            path_call("call_s", "f"),
        ),
        (
            "deltas win over the item",
            call_stream(&["{\"path\":\"g\"}"], None, "{\"path\":\"other\"}", Some("call_s")),
            path_call("call_s", "g"),
        ),
        (
            "deltas win over the done event",
            call_stream(&["{\"path\":\"h\"}"], Some("{\"path\":\"other\"}"), "{\"path\":\"other2\"}", Some("call_s")),
            path_call("call_s", "h"),
        ),
        (
            "the done event wins over the item",
            call_stream(&[], Some("{\"path\":\"i\"}"), "{\"path\":\"other\"}", Some("call_s")),
            path_call("call_s", "i"),
        ),
        (
            "nothing anywhere is an empty object",
            call_stream(&[], None, "", Some("call_s")),
            Seen::Call { id: "call_s".to_owned(), name: "read_file".to_owned(), args: json!({}) },
        ),
    ];
    for (case, bytes, expected) in cases {
        let driven = play(&bytes);
        assert_eq!(driven.calls(), vec![&expected], "{case}: {:?}", driven.events);
        assert_eq!(driven.completion().stop_reason, StopReason::ToolUse, "{case}");
    }

    // Arguments that are not one JSON object end the turn without a call, whatever the source.
    for bad in ["{\"path\":", "[1]", "null", "3", "\"text\"", "{\"path\":\"x\"} trailing"] {
        for (source, bytes) in [
            ("deltas", call_stream(&[bad], None, "", Some("call_s"))),
            ("done event", call_stream(&[], Some(bad), "", Some("call_s"))),
            ("item", call_stream(&[], None, bad, Some("call_s"))),
        ] {
            let driven = play(&bytes);
            assert!(driven.calls().is_empty(), "{source} {bad}: {:?}", driven.events);
            assert_eq!(driven.error().class(), ErrorClass::Protocol, "{source} {bad}");
        }
    }

    // A finished item whose name differs from the added one.
    let added = call_item(Some("call_s"), "read_file", "");
    let done = call_item(Some("call_s"), "write_file", "{}");
    let bytes = sse(&[call_added_block(0, &added), item_done_block(0, &done), completed_block(None, None)]);
    let driven = play(&bytes);
    assert!(driven.calls().is_empty());
    assert_eq!(driven.error().class(), ErrorClass::Protocol);

    // A call item without a call id gets a minted one; the stored item stays as received; the next request writes the
    // call plainly with the minted id, and the result pairs with it.
    let bytes = call_stream(&[], None, "{\"path\":\"m\"}", None);
    let driven = play(&bytes);
    let Seen::Call { id, name, args } = driven.calls()[0].clone() else { panic!("a call") };
    assert!(id.starts_with("call_") && id.len() > "call_".len(), "a minted id: {id}");
    assert_eq!((name.as_str(), &args), ("read_file", &json!({ "path": "m" })));
    let native = driven.completion().native.clone().expect("a native turn");
    assert!(native.json[0].get("call_id").is_none(), "the item is kept as received");
    let mut stored =
        assistant("m2", vec![Part::ToolCall(ToolCall { id: id.clone(), name: name.clone(), args: args.clone() })]);
    stored.model = Some("gpt-5".to_owned());
    stored.native = Some(native);
    let input =
        input_of("gpt-5", vec![user("u1", "q"), stored, tool_results("m3", vec![result_part(&id, "ok", false)])])
            .expect("a request");
    assert_eq!(
        input[1],
        json!({ "type": "function_call", "call_id": id, "name": "read_file", "arguments": "{\"path\":\"m\"}" })
    );
    assert_eq!(input[2], json!({ "type": "function_call_output", "call_id": id, "output": "ok" }));
}

#[test]
fn the_final_response_must_agree_with_the_calls_seen_in_the_stream() {
    let item = call_item(Some("call_s"), "read_file", "{\"path\":\"a\"}");
    let other = call_item(Some("call_t"), "read_file", "{\"path\":\"b\"}");
    let stream = |output: Option<Vec<Value>>| {
        sse(&[call_added_block(0, &item), item_done_block(0, &item), completed_block(output, None)])
    };
    // Two listed while the stream completed one, and none listed while it completed one.
    for (case, output) in [("two listed", vec![item.clone(), other.clone()]), ("none listed", Vec::new())] {
        let driven = play(&stream(Some(output)));
        assert!(driven.calls().is_empty(), "{case}: {:?}", driven.events);
        assert_eq!(driven.error().class(), ErrorClass::Protocol, "{case}");
    }
    // Equal counts, and a response without an output member, are accepted.
    for (case, output) in [("equal", Some(vec![item.clone()])), ("absent", None)] {
        let driven = play(&stream(output));
        assert_eq!(driven.calls(), vec![&path_call("call_s", "a")], "{case}");
    }
    // Items of another type in the final list do not count as calls.
    let driven = play(&stream(Some(vec![message_item(1, "hi"), item.clone()])));
    assert_eq!(driven.calls().len(), 1);
}

#[test]
fn hostile_events_are_ignored_or_fail_closed() {
    let plain =
        sse(&[delta_block("Hi"), completed_block(None, Some(json!({ "input_tokens": 1, "output_tokens": 2 })))]);
    let expected = outcome(&play(&plain));
    assert!(expected.contains("text Hi") && expected.contains("ok EndTurn"), "{expected}");

    // Unknown types, the end marker of other dialects, and anything after the terminal event deliver nothing.
    let hostile = sse(&[
        raw_block(
            "response.shiny",
            "{\"type\":\"response.shiny\",\"delta\":5,\"item\":3,\"response\":[1],\"output_index\":\"x\"}",
        ),
        raw_block("response.queued", "{\"type\":\"response.queued\",\"delta\":{\"a\":[1,2]},\"arguments\":null}"),
        raw_block("message", "[DONE]"),
        delta_block("Hi"),
        completed_block(None, Some(json!({ "input_tokens": 1, "output_tokens": 2 }))),
        delta_block("late"),
        raw_block("message", "[DONE]"),
        raw_block("response.failed", "{\"type\":\"response.failed\"}"),
        raw_block("error", "not json at all"),
    ]);
    assert_eq!(outcome(&play(&hostile)), expected);

    // Data that is not a JSON object, with one fixed phrase.
    let phrase = |data: &str| {
        let driven = play(&sse(&[raw_block("response.created", data)]));
        assert_eq!(driven.error().class(), ErrorClass::Protocol, "{data}");
        driven.error().to_string()
    };
    let first = phrase("this is not JSON");
    for data in ["[1,2,3]", "[\"response.created\"]", "\"text\"", "42", "null", "{\"type\":", ""] {
        assert_eq!(phrase(data), first, "{data}");
    }
    assert!(!first.contains("this is not JSON"));

    // A member of another type: a protocol error where the event needs it, ignored where it does not.
    let driven = play(&sse(&[block(
        "response.output_text.delta",
        &json!({ "type": "response.output_text.delta", "delta": 7 }),
    )]));
    assert_eq!(driven.error().class(), ErrorClass::Protocol);
    let driven = play(&sse(&[
        block("response.unknown", &json!({ "type": "response.unknown", "delta": 7 })),
        delta_block("ok"),
        completed_block(None, None),
    ]));
    assert_eq!(driven.events, vec![text("ok")]);
    for missing in [
        json!({ "type": "response.output_text.delta" }),
        json!({ "type": "response.reasoning_text.delta" }),
        json!({ "type": "response.completed" }),
        json!({ "type": "response.function_call_arguments.delta", "delta": "x" }),
        json!({ "type": "response.output_item.done", "output_index": 0 }),
    ] {
        let name = missing["type"].as_str().expect("type").to_owned();
        let driven = play(&sse(&[block(&name, &missing)]));
        assert_eq!(driven.error().class(), ErrorClass::Protocol, "{name}");
    }

    // The JSON type wins over the event name, and the event name stands in for a missing type.
    let driven = play(&sse(&[
        raw_block("response.output_text.delta", "{\"type\":\"response.created\",\"delta\":\"no\"}"),
        raw_block("response.shiny", "{\"type\":\"response.output_text.delta\",\"delta\":\"yes\"}"),
        raw_block("response.output_text.delta", "{\"delta\":\"named\"}"),
        raw_block("response.completed", "{\"response\":{\"status\":\"completed\"}}"),
    ]));
    assert_eq!(driven.events, vec![text("yes"), text("named")]);
    assert_eq!(driven.completion().stop_reason, StopReason::EndTurn);

    // A call item without an output index.
    let item = call_item(Some("call_s"), "read_file", "");
    let driven = play(&sse(&[block(
        "response.output_item.added",
        &json!({ "type": "response.output_item.added", "item": item }),
    )]));
    assert_eq!(driven.error().class(), ErrorClass::Protocol);
    // Argument text for a call that was never started.
    let driven = play(&sse(&[args_delta_block(3, "{}")]));
    assert_eq!(driven.error().class(), ErrorClass::Protocol);

    // A line frame is not what this dialect uses.
    let mut turn = OpenAiResponsesAdapter::new().turn();
    let error = turn.frame(Frame::Line(b"{}"), &mut |_| Flow::Continue).expect_err("a line");
    assert_eq!(error.class(), ErrorClass::Protocol);

    // An empty delta delivers nothing.
    let driven = play(&sse(&[delta_block(""), delta_block("x"), completed_block(None, None)]));
    assert_eq!(driven.events, vec![text("x")]);

    // Usage: saturated above the largest count, zero for a negative, text or missing count.
    let usage = |value: Value| play(&sse(&[completed_block(None, Some(value))])).events;
    assert_eq!(
        usage(json!({ "input_tokens": 99_999_999_999u64, "output_tokens": -5 })),
        vec![Seen::Usage(u32::MAX, 0)]
    );
    assert_eq!(usage(json!({ "input_tokens": "12", "output_tokens": 7 })), vec![Seen::Usage(0, 7)]);
    assert_eq!(usage(json!({ "output_tokens": 1.0e30 })), vec![Seen::Usage(0, u32::MAX)]);
    assert_eq!(usage(json!({ "input_tokens": null, "output_tokens": [1] })), vec![Seen::Usage(0, 0)]);
    assert!(usage(json!("text")).is_empty(), "usage that is not an object is no usage");
    assert!(play(&sse(&[completed_block(None, None)])).events.is_empty(), "no usage member, no usage event");

    // An answer of Stop ends the turn: nothing comes after it, even from a later frame.
    let stream = sse(&[delta_block("one"), delta_block("two"), completed_block(None, None)]);
    let driven = drive_with(&[&stream], Limits::DEFAULT, &mut |_| Flow::Stop);
    assert_eq!(driven.events, vec![text("one")]);
    assert!(matches!(driven.error(), AiError::Cancelled), "{:?}", driven.error());
    let mut turn = OpenAiResponsesAdapter::new().turn();
    let mut delivered = Vec::new();
    let mut stopping = |event: StreamEvent| {
        delivered.push(Seen::of(&event));
        Flow::Stop
    };
    for (n, piece) in [delta_block("one"), delta_block("two")].iter().enumerate() {
        let mut decoder = SseDecoder::new();
        let mut flow = Flow::Continue;
        decoder
            .push(piece.as_bytes(), &mut |event| {
                flow = turn.frame(Frame::Event(event), &mut stopping).expect("a frame");
                flow
            })
            .expect("framed");
        assert_eq!(flow, Flow::Stop, "event {n}");
    }
    assert_eq!(delivered, vec![text("one")], "the second event was not delivered");
}

#[test]
fn output_items_and_the_native_turn_are_bounded() {
    let done_blocks = |count: usize| -> Vec<String> {
        (0..count).map(|index| item_done_block(index, &message_item(index, "x"))).collect()
    };
    // At the bound, and one finished item over it.
    let mut blocks = done_blocks(MAX_OUTPUT_ITEMS);
    blocks.push(completed_block(None, None));
    let driven = play(&sse(&blocks));
    let native = driven.completion().native.clone().expect("a native turn");
    assert_eq!(native.json.as_array().map(Vec::len), Some(MAX_OUTPUT_ITEMS));
    assert_eq!(MAX_OUTPUT_ITEMS, 256);
    let mut blocks = done_blocks(MAX_OUTPUT_ITEMS + 1);
    blocks.push(completed_block(None, None));
    let driven = play(&sse(&blocks));
    assert_eq!(driven.error().class(), ErrorClass::Protocol);
    assert!(driven.error().to_string().contains("more output items"), "{}", driven.error());

    // The native turn is ordered by output index, not by arrival.
    let blocks = [
        item_done_block(1, &message_item(1, "second")),
        item_done_block(0, &message_item(0, "first")),
        completed_block(None, None),
    ];
    let driven = play(&sse(&blocks));
    let native = driven.completion().native.clone().expect("a native turn");
    assert_eq!(native.json[0]["id"], "msg_0");
    assert_eq!(native.json[1]["id"], "msg_1");

    // A second finished item for one index.
    let blocks = [
        item_done_block(0, &message_item(0, "a")),
        item_done_block(0, &message_item(0, "b")),
        completed_block(None, None),
    ];
    let driven = play(&sse(&blocks));
    assert_eq!(driven.error().class(), ErrorClass::Protocol);
    assert!(driven.error().to_string().contains("twice"), "{}", driven.error());

    // Calls: 64 are the most, the 65th is refused by the assembler.
    let calls = |count: usize| -> Vec<u8> {
        let mut blocks = Vec::new();
        let mut items = Vec::new();
        for index in 0..count {
            let item = call_item(Some(&format!("call_{index}")), "read_file", "{}");
            blocks.push(call_added_block(index, &item));
            blocks.push(item_done_block(index, &item));
            items.push(item);
        }
        blocks.push(completed_block(Some(items), None));
        sse(&blocks)
    };
    assert_eq!(play(&calls(64)).calls().len(), 64);
    let driven = play(&calls(65));
    assert!(driven.calls().is_empty());
    assert_eq!(driven.error().class(), ErrorClass::Protocol);
    assert!(driven.error().to_string().contains("more tool calls"), "{}", driven.error());

    // Bytes: two reasoning items of 700,000 bytes fit, a third passes the bound.
    let reasoning = |index: usize| json!({ "id": format!("rs_{index}"), "type": "reasoning", "summary": [], "encrypted_content": "A".repeat(700_000) });
    const { assert!(2 * 700_000 < MAX_NATIVE_BYTES && 3 * 700_000 > MAX_NATIVE_BYTES) };
    assert_eq!(MAX_NATIVE_BYTES, 2_097_152);
    let two = sse(&[item_done_block(0, &reasoning(0)), item_done_block(1, &reasoning(1)), completed_block(None, None)]);
    assert!(play(&two).result.is_ok());
    let three = sse(&[
        item_done_block(0, &reasoning(0)),
        item_done_block(1, &reasoning(1)),
        item_done_block(2, &reasoning(2)),
        completed_block(None, None),
    ]);
    let driven = play(&three);
    assert_eq!(driven.error().class(), ErrorClass::Protocol);
    assert!(driven.error().to_string().contains("larger than one reply"), "{}", driven.error());

    // A single text delta of 3 MiB is delivered whole, when the decoder is allowed events of that size.
    let big = "a".repeat(3 * 1024 * 1024);
    let bytes = sse(&[delta_block(&big), completed_block(None, None)]);
    let limits = Limits { line: 4 * 1024 * 1024, event: 4 * 1024 * 1024 };
    let driven = drive_with(&chunks_of(&bytes, 64 * 1024), limits, &mut |_| Flow::Continue);
    assert!(driven.result.is_ok());
    assert_eq!(driven.events.len(), 1);
    assert!(matches!(&driven.events[0], Seen::Text(found) if found.len() == big.len()));
    // With the default limits the same event is refused by the decoder, as a protocol error.
    let driven = drive_chunks(&chunks_of(&bytes, 64 * 1024));
    assert_eq!(driven.error().class(), ErrorClass::Protocol);
}

/// A provider on the fake server with a short retry wait.
fn quick_provider(fake: &FakeHttp) -> HttpProvider {
    let config = HttpConfig {
        retry: RetryPolicy { max_retries: 2, base_delay: Duration::from_millis(5), max_wait: Duration::from_secs(2) },
        ..HttpConfig::standard()
    };
    HttpProvider::new("openai", endpoint(fake), Box::new(OpenAiResponsesAdapter::new()), Arc::new(Http::new(config)))
}

#[test]
fn rejected_requests_map_to_stable_classes_and_never_echo_a_key() {
    let body = |code: &str, message: &str| {
        serde_json::to_vec(
            &json!({ "error": { "message": message, "type": "invalid_request_error", "param": null, "code": code } }),
        )
        .expect("JSON")
    };
    struct Row {
        case: &'static str,
        reply: Reply,
        class: ErrorClass,
        connections: usize,
        shown: &'static [&'static str],
        hidden: &'static [&'static str],
    }
    let echoed = format!("Incorrect API key provided: {CANARY_KEY}. Find your key on the account page.");
    let leaking = format!("Unknown parameter: 'foo' for key {CANARY_KEY}. See the reference.");
    let rows = vec![
        Row {
            case: "401 echoing the key",
            reply: Reply::status(401).body(body("invalid_api_key", &echoed)),
            class: ErrorClass::Auth,
            connections: 1,
            shown: &["status 401"],
            hidden: &["sk-canary", "Incorrect API key", "account page"],
        },
        Row {
            case: "401 with a code that is known for other statuses",
            reply: Reply::status(401).body(body("invalid_request_error", &echoed)),
            class: ErrorClass::Auth,
            connections: 1,
            shown: &["status 401"],
            hidden: &["sk-canary", "Incorrect API key", "account page"],
        },
        Row {
            case: "403 with a message",
            reply: Reply::status(403).body(body("unsupported_country_region_territory", "Country not supported")),
            class: ErrorClass::Auth,
            connections: 1,
            shown: &["status 403"],
            hidden: &["Country not supported"],
        },
        Row {
            case: "400 unsupported parameter with the key in the message",
            reply: Reply::status(400).body(body("unsupported_parameter", &leaking)),
            class: ErrorClass::InvalidRequest,
            connections: 1,
            shown: &["Unknown parameter: 'foo' for key", "<redacted>"],
            hidden: &["sk-canary", CANARY_KEY],
        },
        Row {
            case: "400 context length",
            reply: Reply::status(400)
                .body(body("context_length_exceeded", "This model's maximum context length is 1 tokens")),
            class: ErrorClass::ContextLength,
            connections: 1,
            shown: &[],
            hidden: &["maximum context"],
        },
        Row {
            case: "400 stale encrypted reasoning",
            reply: Reply::status(400).body(body(
                "invalid_encrypted_content",
                "The encrypted content could not be verified. SECRET-PROVIDER-TEXT",
            )),
            class: ErrorClass::InvalidRequest,
            connections: 1,
            shown: &["new chat", "another model"],
            hidden: &["SECRET-PROVIDER-TEXT", "verified"],
        },
        Row {
            case: "404 model not found",
            reply: Reply::status(404)
                .body(body("model_not_found", "The model `gpt-9` does not exist. SECRET-PROVIDER-TEXT")),
            class: ErrorClass::InvalidRequest,
            connections: 1,
            shown: &["model"],
            hidden: &["SECRET-PROVIDER-TEXT", "gpt-9"],
        },
        Row {
            case: "429 insufficient quota",
            reply: Reply::status(429)
                .body(body("insufficient_quota", "You exceeded your current quota. SECRET-PROVIDER-TEXT")),
            class: ErrorClass::Auth,
            connections: 3,
            shown: &["no quota"],
            hidden: &["SECRET-PROVIDER-TEXT"],
        },
        Row {
            case: "429 rate limit with a long wait",
            reply: Reply::status(429)
                .header("retry-after", "3600")
                .body(body("rate_limit_exceeded", "Slow down. SECRET-PROVIDER-TEXT")),
            class: ErrorClass::RateLimit,
            connections: 1,
            shown: &["retry after 3600 s"],
            hidden: &["SECRET-PROVIDER-TEXT"],
        },
        Row {
            case: "500 without a body",
            reply: Reply::status(500),
            class: ErrorClass::Server,
            connections: 3,
            shown: &["status 500"],
            hidden: &[],
        },
        Row {
            case: "500 with a server error code",
            reply: Reply::status(500).body(body("server_error", "SECRET-PROVIDER-TEXT")),
            class: ErrorClass::Server,
            connections: 3,
            shown: &["status 500"],
            hidden: &["SECRET-PROVIDER-TEXT"],
        },
        Row {
            case: "400 with an HTML body",
            reply: Reply::status(400).body("<html><body>Bad request</body></html>"),
            class: ErrorClass::InvalidRequest,
            connections: 1,
            shown: &["status 400"],
            hidden: &["<html>", "Bad request"],
        },
    ];
    for row in rows {
        let fake = FakeHttp::start(vec![row.reply]);
        let provider = quick_provider(&fake);
        let request = chat("gpt-5", vec![user("u1", "hello")]);
        let started = Instant::now();
        let error = provider
            .stream(&request, &mut |_| Flow::Continue, &CancelToken::new())
            .expect_err(&format!("{}: a rejection", row.case));
        assert!(started.elapsed() < Duration::from_secs(10), "{}: the retry wait is bounded", row.case);
        assert_eq!(error.class(), row.class, "{}: {error:?}", row.case);
        let shown = format!("{error} | {error:?}");
        for expected in row.shown {
            assert!(shown.contains(expected), "{}: {expected:?} is missing from {shown}", row.case);
        }
        for secret in row.hidden {
            assert!(!shown.contains(secret), "{}: {secret:?} is shown in {shown}", row.case);
        }
        assert_eq!(fake.accepted(), row.connections, "{}: the number of connections", row.case);
    }
    // The wait of a rate limit comes from the answer.
    let fake =
        FakeHttp::start(vec![Reply::status(429).header("retry-after", "3600").body(body("rate_limit_exceeded", "x"))]);
    let error = quick_provider(&fake)
        .stream(&chat("gpt-5", vec![user("u1", "hello")]), &mut |_| Flow::Continue, &CancelToken::new())
        .expect_err("a rejection");
    assert!(
        matches!(error, AiError::RateLimited { retry_after: Some(wait) } if wait == Duration::from_secs(3600)),
        "{error:?}"
    );
}

#[test]
#[ignore = "a measurement for release builds: cargo test --release -- --ignored --nocapture"]
fn a_hundred_thousand_deltas_cost_microseconds_each() {
    const EVENTS: usize = 100_000;
    let filler = "word ".repeat(12);
    let mut bytes = Vec::with_capacity(EVENTS * 200);
    for number in 0..EVENTS {
        bytes.extend_from_slice(
            format!(
                "event: response.output_text.delta\ndata: {{\"type\":\"response.output_text.delta\",\"sequence_number\":{number},\"item_id\":\"msg_x\",\"output_index\":0,\"content_index\":0,\"delta\":\"{filler}\"}}\n\n"
            )
            .as_bytes(),
        );
    }
    bytes.extend_from_slice(completed_block(None, Some(json!({ "input_tokens": 1, "output_tokens": 2 }))).as_bytes());
    assert!(bytes.len() > EVENTS * 150, "about 150 bytes per event: {}", bytes.len() / EVENTS);
    let chunks = chunks_of(&bytes, 16 * 1024);
    let started = Instant::now();
    let mut seen = 0usize;
    let driven = drive_with(&chunks, Limits::DEFAULT, &mut |_| {
        seen += 1;
        Flow::Continue
    });
    let elapsed = started.elapsed();
    assert!(driven.result.is_ok(), "{:?}", driven.result);
    assert_eq!(seen, EVENTS + 1, "every delta and the usage reached the consumer");
    let per_event = elapsed.as_nanos() / EVENTS as u128;
    println!("ns per delta event: {per_event}");
    assert!(per_event < 50_000, "{per_event} ns per event");
}
