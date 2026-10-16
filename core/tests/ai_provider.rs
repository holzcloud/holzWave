//! The streaming path of the AI core, with no network.
//!
//! A scripted provider is driven through the `Provider` trait object into a `Collector`, the way every wire adapter and
//! the agent loop will be. The tests prove that Stop works at every step of a turn, that streamed text never reaches a
//! `Debug` line, and that the trait is usable from other threads. Every wait is bounded, so a broken token fails an
//! assertion instead of hanging the run.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use holzwave_core::ai::provider::{
    AiError, CancelToken, ChatRequest, Collector, Completion, Detail, ErrorClass, Flow, MAX_DETAIL, Provider, StreamEvent,
};
use holzwave_core::ai::scripted::{Script, ScriptedProvider};
use holzwave_core::ai::types::{
    ApiKey, Consent, ContextKind, ContextNote, Locality, Message, MessageStatus, NoteItem, Opaque, Part, Role, StopReason,
    ToolCall, ToolResult, ToolSpec, Usage,
};
use serde_json::json;

const CANARY: &str = "CANARY-7f3a";

/// A request with one user message.
fn request() -> ChatRequest {
    let mut req = ChatRequest::new("scripted-model");
    req.messages.push(Message::user_text("u1", "hello", 1));
    req
}

/// A provider that serves `script` once.
fn provider(script: Script) -> Arc<dyn Provider> {
    Arc::new(ScriptedProvider::new("scripted").with_turn(script))
}

/// Runs one turn and returns every event the closure saw with the result of the call.
fn run(
    provider: &dyn Provider,
    req: &ChatRequest,
    cancel: &CancelToken,
) -> (Vec<StreamEvent>, Result<Completion, AiError>) {
    let mut events = Vec::new();
    let result = provider.stream(
        req,
        &mut |event| {
            events.push(event);
            Flow::Continue
        },
        cancel,
    );
    (events, result)
}

fn delta(text: &str) -> StreamEvent {
    StreamEvent::TextDelta(text.to_owned())
}

#[test]
fn a_scripted_turn_streams_through_the_trait_into_a_collector() {
    let call = ToolCall { id: "call_1".into(), name: "read_file".into(), args: json!({"path": "a"}) };
    let script = Script::new()
        .text("Hel")
        .text("lo")
        .tool_call("call_1", "read_file", json!({"path": "a"}))
        .usage(12, 3)
        .stop(StopReason::ToolUse);
    let provider = provider(script);
    let cancel = CancelToken::new();
    let mut collector = Collector::new();
    let mut seen = Vec::new();

    let completion = provider
        .stream(
            &request(),
            &mut |event| {
                seen.push(event.clone());
                collector.push(event)
            },
            &cancel,
        )
        .expect("the scripted turn completes");

    assert_eq!(
        seen,
        vec![delta("Hel"), delta("lo"), StreamEvent::ToolCall(call.clone()), StreamEvent::Usage(Usage {
            input_tokens: 12,
            output_tokens: 3
        })],
        "four events, in script order"
    );
    assert_eq!(collector.text(), "Hello");
    assert_eq!(collector.tool_calls(), std::slice::from_ref(&call));
    assert_eq!(collector.usage(), Usage { input_tokens: 12, output_tokens: 3 });
    assert!(!collector.truncated());
    assert_eq!(completion.stop_reason, StopReason::ToolUse);
    assert_eq!(completion.usage, Usage { input_tokens: 12, output_tokens: 3 });

    let message = collector.into_message("a1", 9, "scripted-model", MessageStatus::Complete, Some(completion));
    assert_eq!(message.role, Role::Assistant);
    assert_eq!(message.status, MessageStatus::Complete);
    assert_eq!(message.parts, vec![Part::Text { text: "Hello".into() }, Part::ToolCall(call)]);
    assert_eq!(message.model.as_deref(), Some("scripted-model"));
    assert_eq!(message.usage, Some(Usage { input_tokens: 12, output_tokens: 3 }));
}

#[test]
fn a_cancel_inside_the_callback_stops_before_the_next_event() {
    let provider = provider(Script::new().text("one").text("two").text("three"));
    let cancel = CancelToken::new();
    let mut seen = Vec::new();

    let result = provider.stream(
        &request(),
        &mut |event| {
            seen.push(event);
            cancel.cancel();
            Flow::Continue
        },
        &cancel,
    );

    assert_eq!(result, Err(AiError::Cancelled));
    assert_eq!(seen, vec![delta("one")], "the second delta is never delivered");
}

#[test]
fn a_token_cancelled_before_the_call_delivers_nothing_and_keeps_the_script() {
    let scripted = Arc::new(ScriptedProvider::new("scripted").with_turn(Script::reply("hi")));
    let provider: Arc<dyn Provider> = scripted.clone();
    let cancelled = CancelToken::new();
    cancelled.cancel();

    let (events, result) = run(provider.as_ref(), &request(), &cancelled);

    assert_eq!(result.unwrap_err(), AiError::Cancelled);
    assert!(events.is_empty());
    assert!(scripted.requests().is_empty(), "a cancelled call records nothing");

    let (events, result) = run(provider.as_ref(), &request(), &CancelToken::new());
    assert!(result.is_ok(), "the script was not consumed by the cancelled call");
    assert_eq!(events, vec![delta("hi")]);
}

#[test]
fn a_cancel_from_another_thread_ends_a_stalled_turn_within_250_ms() {
    // The 5 s stall is a ceiling: a token that does not wake the wait fails the 250 ms assertion.
    let provider = provider(Script::new().text("a").stall(Duration::from_secs(5)).text("b"));
    let cancel = CancelToken::new();
    let remote = cancel.clone();
    let started = Instant::now();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(30));
        remote.cancel();
    });

    let (events, result) = run(provider.as_ref(), &request(), &cancel);
    let elapsed = started.elapsed();
    canceller.join().expect("the cancelling thread");

    assert_eq!(result.unwrap_err(), AiError::Cancelled);
    assert_eq!(events, vec![delta("a")], "only the text before the stall was delivered");
    assert!(elapsed < Duration::from_millis(250), "the stalled turn needed {elapsed:?} to stop");
    println!("stalled turn stopped after {elapsed:?}");
}

#[test]
fn flow_stop_ends_the_turn_like_a_cancel() {
    let provider = provider(Script::new().text("one").text("two"));
    let mut seen = Vec::new();

    let result = provider.stream(
        &request(),
        &mut |event| {
            seen.push(event);
            Flow::Stop
        },
        &CancelToken::new(),
    );

    assert_eq!(result, Err(AiError::Cancelled));
    assert_eq!(seen, vec![delta("one")], "no event after the Stop");
}

#[test]
fn a_failure_after_a_delta_is_returned_unchanged() {
    let provider = provider(Script::new().text("a").fail(AiError::Overloaded));

    let (events, result) = run(provider.as_ref(), &request(), &CancelToken::new());

    assert_eq!(events, vec![delta("a")]);
    assert_eq!(result.unwrap_err(), AiError::Overloaded);
}

#[test]
fn an_empty_script_queue_is_an_error_not_a_hang() {
    let provider: Arc<dyn Provider> = Arc::new(ScriptedProvider::new("scripted"));

    let (events, result) = run(provider.as_ref(), &request(), &CancelToken::new());

    assert!(events.is_empty());
    assert!(matches!(result, Err(AiError::Unavailable(_))), "got {result:?}");
}

#[test]
fn the_collector_stops_at_its_bound() {
    let mut collector = Collector::with_limit(8);

    assert_eq!(collector.push(delta("12345")), Flow::Continue);
    assert!(!collector.truncated());
    assert_eq!(collector.push(delta("67890")), Flow::Stop, "5 + 5 bytes pass the limit of 8");

    assert!(collector.truncated());
    assert_eq!(collector.text(), "12345", "the push that passes the bound is not appended");
    assert_eq!(collector.push(delta("x")), Flow::Stop, "a collector that stopped stays stopped");
}

#[test]
fn the_cancel_token_wakes_a_blocking_wait_and_an_async_wait() {
    // Blocking wait, cancelled from another thread.
    let token = CancelToken::new();
    let remote = token.clone();
    let started = Instant::now();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(30));
        remote.cancel();
    });
    assert!(token.wait_cancelled(Duration::from_secs(5)), "the wait reports the cancel");
    assert!(started.elapsed() < Duration::from_millis(250), "the wait needed {:?}", started.elapsed());
    canceller.join().expect("the cancelling thread");
    assert!(token.is_cancelled());

    // A fresh token times out.
    let fresh = CancelToken::new();
    let started = Instant::now();
    assert!(!fresh.wait_cancelled(Duration::from_millis(50)));
    assert!(started.elapsed() >= Duration::from_millis(40), "the wait returned after {:?}", started.elapsed());
    assert!(!fresh.is_cancelled());

    // Async wait on a current-thread runtime.
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    let token = CancelToken::new();
    let remote = token.clone();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(30));
        remote.cancel();
    });
    let started = Instant::now();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), token.cancelled()).await.expect("cancelled() completes");
    });
    assert!(started.elapsed() < Duration::from_millis(250), "the async wait needed {:?}", started.elapsed());
    canceller.join().expect("the cancelling thread");

    // An already cancelled token completes at once, also for a clone.
    let done = token.clone();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_millis(100), done.cancelled()).await.expect("completes at once");
    });
}

#[test]
fn streamed_text_never_reaches_debug_output() {
    let event = delta(CANARY);
    let message = Message::user_text("u9", CANARY, 5);
    let mut req = ChatRequest::new("scripted-model");
    req.messages.push(message.clone());
    let mut collector = Collector::new();
    collector.push(delta(CANARY));
    let size = format!("<{} bytes>", CANARY.len());

    let shown = [
        format!("{event:?}"),
        format!("{event:#?}"),
        format!("{message:?}"),
        format!("{message:#?}"),
        format!("{req:?}"),
        format!("{req:#?}"),
        format!("{collector:?}"),
        format!("{collector:#?}"),
    ];

    for text in &shown {
        assert!(!text.contains(CANARY), "the canary text reached a Debug line: {text}");
        assert!(text.contains(&size), "no byte size in {text}");
    }
}

#[test]
fn the_provider_trait_is_object_safe_send_and_sync() {
    fn assert_send_sync<T: Send + Sync + ?Sized>() {}
    assert_send_sync::<dyn Provider>();
    assert_send_sync::<Arc<dyn Provider>>();

    // And it is usable from another thread.
    let provider = provider(Script::reply("from a thread"));
    let handle = thread::spawn(move || {
        let (events, result) = run(provider.as_ref(), &request(), &CancelToken::new());
        (events, result.is_ok())
    });
    let (events, ok) = handle.join().expect("the worker thread");
    assert!(ok);
    assert_eq!(events, vec![delta("from a thread")]);
}

// ---- the message model ----------------------------------------------------------------------------------------------

const CANARY_KEY: &str = "sk-canary-0123456789abcdef";

/// A block as a provider might send it: a long signature, unicode, escapes and nested arrays.
fn native_blob() -> Opaque {
    Opaque {
        wire: "anthropic".into(),
        json: json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "step one\nstep \"two\" \\ \u{1F980} \u{00E4}\u{00F6}\u{00FC}", "signature": "Eq".repeat(900)},
                {"type": "text", "text": "tab\there"},
                {"type": "tool_use", "id": "toolu_01", "name": "read_file", "input": {"path": "a", "ranges": [[1, 2], [3, [4, 5]]]}}
            ],
            "stop_reason": "tool_use"
        }),
    }
}

fn assistant_message() -> Message {
    let mut message = Message::new(
        "m1",
        Role::Assistant,
        vec![
            Part::Text { text: "hi".into() },
            Part::ToolCall(ToolCall { id: "c1".into(), name: "read_file".into(), args: json!({"path": "a"}) }),
        ],
        7,
    );
    message.model = Some("m".into());
    message.usage = Some(Usage { input_tokens: 1, output_tokens: 2 });
    message.native = Some(Opaque { wire: "anthropic".into(), json: json!({"k": 1}) });
    message
}

#[test]
fn a_message_serialises_to_the_pinned_json_shape() {
    // These literals are a stored format. If one has to change, the conversation store changes its version; the test
    // is not the thing to fix.
    assert_eq!(
        serde_json::to_string(&assistant_message()).unwrap(),
        r#"{"id":"m1","role":"assistant","parts":[{"type":"text","text":"hi"},{"type":"tool_call","id":"c1","name":"read_file","args":{"path":"a"}}],"ts":7,"model":"m","usage":{"input_tokens":1,"output_tokens":2},"status":"complete","native":{"wire":"anthropic","json":{"k":1}}}"#
    );

    let bare = Message::user_text("m2", "x", 3);
    assert_eq!(
        serde_json::to_string(&bare).unwrap(),
        r#"{"id":"m2","role":"user","parts":[{"type":"text","text":"x"}],"ts":3,"status":"complete"}"#,
        "model, usage and native are left out when empty"
    );

    let result = Message::new(
        "m3",
        Role::Tool,
        vec![Part::ToolResult(ToolResult { call_id: "c1".into(), content: "ok".into(), is_error: false })],
        4,
    );
    assert_eq!(
        serde_json::to_string(&result).unwrap(),
        r#"{"id":"m3","role":"tool","parts":[{"type":"tool_result","call_id":"c1","content":"ok","is_error":false}],"ts":4,"status":"complete"}"#
    );

    let mut note = ContextNote {
        hash: "h".into(),
        items: vec![NoteItem { kind: ContextKind::Selection, label: "selection".into(), bytes: 5 }],
        stored: None,
        consented_to: Consent { provider: "p".into(), locality: Locality::Loopback },
    };
    assert_eq!(
        serde_json::to_string(&Part::Context(note.clone())).unwrap(),
        r#"{"type":"context","hash":"h","items":[{"kind":"selection","label":"selection","bytes":5}],"consented_to":{"provider":"p","locality":"loopback"}}"#,
        "a redacted marker has no stored key"
    );
    note.stored = Some("kept".into());
    assert_eq!(
        serde_json::to_string(&Part::Context(note)).unwrap(),
        r#"{"type":"context","hash":"h","items":[{"kind":"selection","label":"selection","bytes":5}],"stored":"kept","consented_to":{"provider":"p","locality":"loopback"}}"#
    );
}

#[test]
fn a_message_round_trips_and_keeps_the_native_turn_verbatim() {
    let blob = native_blob();
    let mut message = assistant_message();
    message.native = Some(blob.clone());

    let text = serde_json::to_string(&message).unwrap();
    let back: Message = serde_json::from_str(&text).unwrap();
    assert_eq!(back, message, "every field comes back equal");
    assert_eq!(back.native, Some(blob.clone()));

    // A message without the optional keys reads back with them empty.
    let bare: Message =
        serde_json::from_str(r#"{"id":"m2","role":"user","parts":[{"type":"text","text":"x"}],"ts":3,"status":"complete"}"#)
            .unwrap();
    assert_eq!((bare.model, bare.usage, bare.native), (None, None, None));

    // The assistant message the collector builds carries the native turn of the completion unchanged.
    let script = Script::new().text("Hello").native(blob.clone());
    let provider = provider(script);
    let mut collector = Collector::new();
    let completion =
        provider.stream(&request(), &mut |event| collector.push(event), &CancelToken::new()).expect("the turn");
    let built = collector.into_message("a1", 9, "scripted-model", MessageStatus::Complete, Some(completion));
    assert_eq!(built.native, Some(blob));
}

#[test]
fn a_native_turn_is_offered_only_to_its_own_wire() {
    let mut message = assistant_message();
    message.native = Some(native_blob());

    assert_eq!(message.native_for("anthropic"), Some(&native_blob().json));
    assert_eq!(message.native_for("gemini"), None);
    assert_eq!(message.native_for("openai_responses"), None);
    assert_eq!(message.native_for(""), None);

    let plain = Message::user_text("u1", "hello", 1);
    for wire in ["anthropic", "gemini", "openai_responses", "ollama", "openai_chat"] {
        assert_eq!(plain.native_for(wire), None, "{wire}");
    }
}

// ---- secrets and private text never reach a Debug line -------------------------------------------------------------

/// Fields read only through the derived `Debug`, which is what the test looks at.
#[derive(Debug)]
#[allow(dead_code)]
struct Holder {
    key: ApiKey,
    spare: Option<ApiKey>,
}

#[test]
fn a_canary_key_never_reaches_any_debug_output() {
    let key = ApiKey::new(CANARY_KEY.to_owned());
    assert_eq!(format!("{key:?}"), "ApiKey(<redacted>)");
    assert_eq!(format!("{key:#?}"), "ApiKey(<redacted>)");
    assert_eq!(key.expose(), CANARY_KEY);
    assert!(!key.is_empty());

    let holder = Holder { key: ApiKey::new(CANARY_KEY.to_owned()), spare: Some(ApiKey::new(CANARY_KEY.to_owned())) };
    let list = vec![ApiKey::new(CANARY_KEY.to_owned())];
    let option = Some(ApiKey::new(CANARY_KEY.to_owned()));
    for shown in [
        format!("{holder:?}"),
        format!("{holder:#?}"),
        format!("{list:?}"),
        format!("{list:#?}"),
        format!("{option:?}"),
        format!("{option:#?}"),
    ] {
        assert!(!shown.contains(CANARY_KEY), "the key reached a Debug line: {shown}");
        assert!(shown.contains("ApiKey(<redacted>)"), "no redaction mark in {shown}");
    }

    // Private text in every type that can hold some.
    let result = ToolResult { call_id: "c1".into(), content: CANARY.into(), is_error: false };
    let note = ContextNote {
        hash: "h".into(),
        items: vec![NoteItem { kind: ContextKind::Screen, label: "screen".into(), bytes: 11 }],
        stored: Some(CANARY.into()),
        consented_to: Consent { provider: "p".into(), locality: Locality::Cloud },
    };
    let call = ToolCall { id: "c2".into(), name: "run".into(), args: json!({"command": CANARY}) };
    let opaque = Opaque { wire: "anthropic".into(), json: json!({"thinking": CANARY}) };
    let spec = ToolSpec { name: "run".into(), description: CANARY.into(), input_schema: json!({"description": CANARY}) };
    let reasoning = StreamEvent::ReasoningDelta(CANARY.into());

    let mut message = Message::new(
        "m1",
        Role::Assistant,
        vec![
            Part::Text { text: CANARY.into() },
            Part::ToolCall(call.clone()),
            Part::ToolResult(result.clone()),
            Part::Context(note.clone()),
        ],
        1,
    );
    message.native = Some(opaque.clone());
    let mut req = ChatRequest::new("m");
    req.system = CANARY.into();
    req.messages.push(message.clone());
    req.tools.push(spec.clone());
    let mut collector = Collector::new();
    collector.push(reasoning.clone());
    collector.push(StreamEvent::ToolCall(call.clone()));
    collector.push(StreamEvent::TextDelta(CANARY.into()));

    let shown = [
        format!("{result:?}"),
        format!("{result:#?}"),
        format!("{note:?}"),
        format!("{note:#?}"),
        format!("{call:?}"),
        format!("{call:#?}"),
        format!("{opaque:?}"),
        format!("{opaque:#?}"),
        format!("{spec:?}"),
        format!("{spec:#?}"),
        format!("{reasoning:?}"),
        format!("{reasoning:#?}"),
        format!("{message:?}"),
        format!("{message:#?}"),
        format!("{req:?}"),
        format!("{req:#?}"),
        format!("{collector:?}"),
        format!("{collector:#?}"),
    ];
    for text in &shown {
        assert!(!text.contains(CANARY), "private text reached a Debug line: {text}");
    }
}

#[test]
fn redact_in_masks_every_occurrence_of_the_key() {
    let key = ApiKey::new(CANARY_KEY.to_owned());

    let text = format!("a {CANARY_KEY} b {CANARY_KEY}{CANARY_KEY} c");
    assert_eq!(key.redact_in(&text), "a <redacted> b <redacted><redacted> c");
    assert_eq!(key.redact_in("nothing to hide"), "nothing to hide");

    let empty = ApiKey::new(String::new());
    assert!(empty.is_empty());
    assert_eq!(empty.redact_in("abc"), "abc", "an empty key masks nothing");
}

// ---- errors ----------------------------------------------------------------------------------------------------

#[test]
fn error_text_is_scrubbed_bounded_and_free_of_control_characters() {
    let raw = format!(
        "bad\u{1b}[31m key\nline\u{202E}x\u{E0041}y\u{200B}z\u{FEFF}\u{2066}w\u{7f}\r\n{}",
        "q".repeat(500)
    );
    let detail = Detail::new(&raw);
    let text = detail.as_str();

    for c in text.chars() {
        assert!(!c.is_control(), "control character {c:?} in {text:?}");
        assert!(
            !matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}' | '\u{E0000}'..='\u{E007F}'),
            "invisible character {c:?} in {text:?}"
        );
    }
    assert!(text.starts_with("bad [31m key line x y z w q"), "{text:?}");
    assert!(text.chars().count() <= MAX_DETAIL + 1, "{} characters", text.chars().count());
    assert!(text.ends_with('\u{2026}'));

    let error = AiError::Auth(detail);
    assert!(!error.to_string().contains('\n'));
    assert!(!error.to_string().contains('\u{1b}'));

    // Exactly the limit is kept whole, one more is cut. White space is collapsed.
    assert_eq!(Detail::new(&"a".repeat(MAX_DETAIL)).as_str().chars().count(), MAX_DETAIL);
    assert!(!Detail::new(&"a".repeat(MAX_DETAIL)).as_str().ends_with('\u{2026}'));
    assert!(Detail::new(&"a".repeat(MAX_DETAIL + 1)).as_str().ends_with('\u{2026}'));
    assert_eq!(Detail::new("  a \t\t b\n\nc  ").as_str(), "a b c");

    // An empty detail adds no colon; a present one does.
    assert!(!AiError::Auth(Detail::new("")).to_string().contains(':'));
    assert!(!AiError::Policy(Detail::new("  \n ")).to_string().contains(':'));
    assert!(AiError::Auth(Detail::new("expired")).to_string().ends_with(": expired"));
    assert!(AiError::Server { status: 503, detail: Detail::new("busy") }.to_string().contains("503"));
}

#[test]
fn error_classes_have_stable_names_and_transient_ones_are_marked() {
    let detail = || Detail::new("x");
    let errors = [
        AiError::Cancelled,
        AiError::Auth(detail()),
        AiError::RateLimited { retry_after: Some(Duration::from_secs(3)) },
        AiError::Overloaded,
        AiError::ContextLength,
        AiError::ContentFilter,
        AiError::InvalidRequest(detail()),
        AiError::Unavailable(detail()),
        AiError::Network(detail()),
        AiError::Timeout,
        AiError::Protocol(detail()),
        AiError::Server { status: 500, detail: detail() },
        AiError::Policy(detail()),
    ];
    let names: Vec<&str> = errors.iter().map(|error| error.class().as_str()).collect();
    assert_eq!(
        names,
        [
            "cancelled",
            "auth",
            "rate_limit",
            "overload",
            "context_length",
            "content_filter",
            "invalid_request",
            "unavailable",
            "network",
            "timeout",
            "protocol",
            "server",
            "policy"
        ]
    );
    assert_eq!(errors[1].class(), ErrorClass::Auth);

    let transient: Vec<&str> =
        errors.iter().filter(|error| error.is_transient()).map(|error| error.class().as_str()).collect();
    assert_eq!(transient, ["rate_limit", "overload", "network", "server"]);
    assert!(!AiError::Server { status: 499, detail: detail() }.is_transient());
    assert!(!AiError::Server { status: 404, detail: detail() }.is_transient());
    assert!(AiError::Server { status: 599, detail: detail() }.is_transient());
}

#[test]
fn an_ai_error_converts_into_the_crate_error() {
    let error = holzwave_core::Error::from(AiError::Overloaded);

    assert!(matches!(error, holzwave_core::Error::Unsupported(_)), "{error:?}");
    assert!(error.to_string().starts_with("unsupported: ai: "), "{error}");
}
