//! Errors, cancel and serialisation of the provider, against the scripted backend. Nothing here needs a Mac.

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use holzwave_apple_ai::{AppleError, AppleProvider, BackendScript, ScriptedBackend};
use holzwave_core::ai::provider::{AiError, CancelToken, Flow, Provider, StreamEvent};

use common::{run, user_request};

fn provider_over(backend: ScriptedBackend) -> (AppleProvider, Arc<ScriptedBackend>) {
    let backend = Arc::new(backend);
    (AppleProvider::new(backend.clone()), backend)
}

fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_failure_after_some_text_keeps_the_text_that_was_delivered() {
    let (provider, backend) = provider_over(
        ScriptedBackend::new()
            .with_turn(BackendScript::new().delta("partial").fail(AppleError::Guardrail))
            .with_turn(BackendScript::new().delta("some text").fail(AppleError::Other)),
    );

    let (result, collector) = run(&provider, &user_request("", "hi"), &CancelToken::new());
    assert_eq!(result.unwrap_err(), AiError::ContentFilter);
    assert_eq!(collector.text(), "partial");
    assert_eq!(backend.calls(), 1);

    // A reply that was delivered and then ended with an unknown error is a protocol error that names no framework text.
    let (result, collector) = run(&provider, &user_request("", "hi"), &CancelToken::new());
    match result {
        Err(AiError::Protocol(detail)) => assert_eq!(detail.as_str(), "other"),
        other => panic!("expected a protocol error, got {other:?}"),
    }
    assert_eq!(collector.text(), "some text");
}

#[test]
fn a_guardrail_error_is_a_content_filter_error_and_is_not_retried() {
    for kind in [AppleError::Guardrail, AppleError::Refusal] {
        let (provider, backend) = provider_over(
            ScriptedBackend::new()
                .with_turn(BackendScript::new().fail(kind))
                .with_turn(BackendScript::reply("a second attempt would find this")),
        );
        let (result, collector) = run(&provider, &user_request("", "hi"), &CancelToken::new());
        assert_eq!(result.unwrap_err(), AiError::ContentFilter, "{kind:?}");
        assert_eq!(collector.text(), "");
        assert_eq!(backend.calls(), 1, "{kind:?} was retried");
    }
}

#[test]
fn a_framework_context_overflow_is_a_context_length_error() {
    let (provider, backend) = provider_over(
        ScriptedBackend::new()
            .with_turn(BackendScript::new().fail(AppleError::ContextExceeded))
            .with_turn(BackendScript::reply("never")),
    );
    let (result, _) = run(&provider, &user_request("", "hi"), &CancelToken::new());
    assert_eq!(result.unwrap_err(), AiError::ContextLength);
    assert_eq!(backend.calls(), 1);
}

#[test]
fn a_rate_limit_is_a_rate_limit_error_with_no_retry_hint() {
    let (provider, backend) = provider_over(
        ScriptedBackend::new()
            .with_turn(BackendScript::new().fail(AppleError::RateLimited))
            .with_turn(BackendScript::new().fail(AppleError::Concurrent))
            .with_turn(BackendScript::reply("never")),
    );
    let (result, _) = run(&provider, &user_request("", "hi"), &CancelToken::new());
    assert_eq!(result.unwrap_err(), AiError::RateLimited { retry_after: None });
    let (result, _) = run(&provider, &user_request("", "hi"), &CancelToken::new());
    assert_eq!(result.unwrap_err(), AiError::Overloaded);
    assert_eq!(backend.calls(), 2, "one backend call per turn and nothing in between");
}

#[test]
fn a_cancel_raised_before_the_call_returns_cancelled_and_the_backend_is_not_called() {
    let (provider, backend) = provider_over(ScriptedBackend::new().with_turn(BackendScript::reply("never")));
    let cancel = CancelToken::new();
    cancel.cancel();

    let (result, collector) = run(&provider, &user_request("", "hi"), &cancel);

    assert_eq!(result.unwrap_err(), AiError::Cancelled);
    assert_eq!(collector.text(), "");
    assert_eq!(backend.calls(), 0);
}

#[test]
fn a_cancel_during_a_stall_returns_cancelled_within_250_ms() {
    let (provider, backend) =
        provider_over(ScriptedBackend::new().with_turn(BackendScript::new().delta("start").stall(Duration::from_secs(10))));
    let cancel = CancelToken::new();
    let cancelled_at = std::sync::Mutex::new(None);

    let (result, finished) = std::thread::scope(|scope| {
        scope.spawn(|| {
            wait_until("the stalled call", || backend.calls() == 1);
            std::thread::sleep(Duration::from_millis(50));
            *cancelled_at.lock().unwrap() = Some(Instant::now());
            cancel.cancel();
        });
        let (result, _) = run(&provider, &user_request("", "hi"), &cancel);
        (result, Instant::now())
    });

    assert_eq!(result.unwrap_err(), AiError::Cancelled);
    let waited = finished.duration_since(cancelled_at.lock().unwrap().expect("the cancel was raised"));
    assert!(waited < Duration::from_millis(250), "the stalled turn took {waited:?} to stop");
}

#[test]
fn a_stop_from_the_consumer_ends_the_turn_as_cancelled() {
    let (provider, _) = provider_over(
        ScriptedBackend::new().with_turn(BackendScript::new().delta("one").delta("two").delta("three")),
    );
    let mut seen = Vec::new();

    let result = provider.stream(
        &user_request("", "hi"),
        &mut |event| {
            seen.push(event);
            Flow::Stop
        },
        &CancelToken::new(),
    );

    assert_eq!(result.unwrap_err(), AiError::Cancelled);
    assert_eq!(seen, [StreamEvent::TextDelta("one".to_owned())], "no event follows the stop, not even usage");
}

#[test]
fn two_turns_at_once_run_one_after_the_other() {
    let script = || BackendScript::new().delta("a").stall(Duration::from_millis(100)).delta("b");
    let (provider, backend) = provider_over(ScriptedBackend::new().with_turn(script()).with_turn(script()));

    let texts: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|_| {
                scope.spawn(|| {
                    let (result, collector) = run(&provider, &user_request("", "hi"), &CancelToken::new());
                    result.expect("both turns finish");
                    collector.text().to_owned()
                })
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().unwrap()).collect()
    });

    assert_eq!(texts, ["ab", "ab"]);
    assert_eq!(backend.calls(), 2);
    assert_eq!(backend.max_concurrent(), 1, "the two backend calls overlapped");
}

#[test]
fn a_waiting_turn_can_be_cancelled_without_reaching_the_backend() {
    let (provider, backend) = provider_over(
        ScriptedBackend::new()
            .with_turn(BackendScript::new().delta("first").stall(Duration::from_secs(10)))
            .with_turn(BackendScript::reply("never")),
    );
    let first_cancel = CancelToken::new();
    let second_cancel = CancelToken::new();
    let second_started = AtomicBool::new(false);

    let (first, second, waited) = std::thread::scope(|scope| {
        let first = scope.spawn(|| run(&provider, &user_request("", "one"), &first_cancel).0);
        wait_until("the first call", || backend.calls() == 1);
        let second = scope.spawn(|| {
            second_started.store(true, Ordering::SeqCst);
            run(&provider, &user_request("", "two"), &second_cancel).0
        });
        wait_until("the second turn to start", || second_started.load(Ordering::SeqCst));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(backend.calls(), 1, "the waiting turn reached the backend");
        let cancelled_at = Instant::now();
        second_cancel.cancel();
        let second = second.join().unwrap();
        let waited = cancelled_at.elapsed();
        first_cancel.cancel();
        (first.join().unwrap(), second, waited)
    });

    assert_eq!(second.unwrap_err(), AiError::Cancelled);
    assert!(waited < Duration::from_millis(250), "the waiting turn took {waited:?} to stop");
    assert_eq!(first.unwrap_err(), AiError::Cancelled);
    assert_eq!(backend.calls(), 1, "the cancelled turn never reached the backend");
}

#[test]
fn a_panic_inside_a_turn_does_not_wedge_later_turns() {
    let (provider, backend) = provider_over(
        ScriptedBackend::new().with_turn(BackendScript::new().panic()).with_turn(BackendScript::reply("recovered")),
    );

    let crashed = catch_unwind(AssertUnwindSafe(|| run(&provider, &user_request("", "hi"), &CancelToken::new())));
    assert!(crashed.is_err(), "the scripted panic goes through");

    let (result, collector) = run(&provider, &user_request("", "again"), &CancelToken::new());
    result.expect("the next turn runs");
    assert_eq!(collector.text(), "recovered");
    assert_eq!(backend.calls(), 2);
}
