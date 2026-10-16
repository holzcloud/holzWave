//! The streaming path of the AI core, with no network.
//!
//! A scripted provider is driven through the `Provider` trait object into a `Collector`, the way every wire adapter and
//! the agent loop will be. The tests prove that Stop works at every step of a turn, that streamed text never reaches a
//! `Debug` line, and that the trait is usable from other threads. Every wait is bounded, so a broken token fails an
//! assertion instead of hanging the run.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use holzwave_core::ai::provider::{AiError, CancelToken, ChatRequest, Collector, Completion, Flow, Provider, StreamEvent};
use holzwave_core::ai::scripted::{Script, ScriptedProvider};
use holzwave_core::ai::types::{Message, MessageStatus, Part, Role, StopReason, ToolCall, Usage};
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
