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
use holzwave_core::ai::http::{Http, HttpConfig, NetworkPolicy, SecretHeader, Target};
use holzwave_core::ai::provider::{CancelToken, ChatRequest, Collector, Completion, Provider, StreamEvent};
use holzwave_core::ai::types::{
    ApiKey, Capabilities, Locality, Message, MessageStatus, Opaque, Part, Role, StopReason, ToolResult, ToolSpec, Usage,
};
use holzwave_core::ai::wire::openai_responses::{INCLUDE_ENCRYPTED, OpenAiResponsesAdapter, PATH};
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
