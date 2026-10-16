//! The Responses adapter, proved against recorded-shape streams and an in-process server, with no outside network.
//!
//! The streams in `tests/fixtures/ai_wire/openai_responses/` are written by hand from the documented event shapes; they
//! are not recordings of a real account. Every request that goes through a provider goes to `FakeHttp`, an HTTP/1.1
//! server in this process. A stream that is driven directly goes through `SseDecoder` and the turn of the adapter the
//! way the shared driver does it, so a split of the bytes can be chosen at any offset. A failing case names its offset,
//! its chunk size or its seed.

mod common;

use std::sync::Arc;

use common::fake_http::{FakeHttp, Reply};
use holzwave_core::ai::agent::TRIMMED_RESULT;
use holzwave_core::ai::gate::{ApprovedContext, Destination, Gate, OfferRequest, RequestItem};
use holzwave_core::ai::http::{Http, HttpConfig, NetworkPolicy, SecretHeader, Target};
use holzwave_core::ai::provider::{
    AiError, CancelToken, ChatRequest, Collector, Completion, ErrorClass, Provider, StreamEvent,
};
use holzwave_core::ai::redact::SourceText;
use holzwave_core::ai::store::NOT_STORED;
use holzwave_core::ai::types::{
    ApiKey, Capabilities, Consent, ContextKind, ContextNote, Locality, Message, MessageStatus, NoteItem, Opaque, Part,
    Role, StopReason, ToolCall, ToolResult, ToolSpec, Usage,
};
use holzwave_core::ai::wire::openai_responses::{
    INCLUDE_ENCRYPTED, MARKER_LABEL_CHARS, MAX_OUTPUT_ITEMS, MAX_TOOLS, OpenAiResponsesAdapter, PATH,
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
    assert_eq!(first_part_text(&input), format!("[context not sent: pane 2 screen ({bytes} bytes)]"));
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
    assert_eq!(marker(vec![note_item("A", 1), note_item("B", 2)]), "[context not sent: A (1 bytes), B (2 bytes)]");
    assert_eq!(marker(Vec::new()), "[context not sent]");
    let long = "x".repeat(200);
    assert_eq!(
        marker(vec![note_item(&long, 3)]),
        format!("[context not sent: {} (3 bytes)]", "x".repeat(MARKER_LABEL_CHARS))
    );
    assert_eq!(MARKER_LABEL_CHARS, 80);
    assert_eq!(marker(vec![note_item("pane\none\u{1b}two", 5)]), "[context not sent: pane one two (5 bytes)]");
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
