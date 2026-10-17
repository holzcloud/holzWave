//! Tests against the real system model. They are ignored by default: they need a Mac with macOS 26 or later and Apple
//! Intelligence switched on, and they run only with `HOLZWAVE_TEST_APPLE=1`:
//!
//! `HOLZWAVE_TEST_APPLE=1 cargo test -p holzwave-apple-ai --test live -- --ignored --nocapture`
#![cfg(target_os = "macos")]

mod common;

use std::time::{Duration, Instant};

use holzwave_apple_ai::{AppleError, AppleProvider, Availability, Backend, GenerateRequest, SystemBackend, native, probe};
use holzwave_core::ai::provider::{AiError, CancelToken, Collector, Flow, Provider, StreamEvent};
use holzwave_core::ai::types::StopReason;

use common::user_request;

/// Whether the live tests were asked for. With the variable set and the model not available a live test fails with the
/// key of the state, so a run that was asked for never passes by doing nothing.
fn live_requested() -> bool {
    if std::env::var("HOLZWAVE_TEST_APPLE").as_deref() != Ok("1") {
        println!("live test skipped: HOLZWAVE_TEST_APPLE is not 1");
        return false;
    }
    let state = probe();
    assert_eq!(state, Availability::Available, "the system model is not available: {}", state.as_str());
    true
}

#[test]
#[ignore = "needs macOS 26 with Apple Intelligence and HOLZWAVE_TEST_APPLE=1"]
fn a_real_text_turn_streams_from_the_system_model() {
    if !live_requested() {
        return;
    }
    let provider = AppleProvider::system();
    let provider: &dyn Provider = &provider;
    let req = user_request("Answer in one short sentence.", "Say hello.");
    let started = Instant::now();
    let mut first_delta = None;
    let mut deltas = 0usize;
    let mut collector = Collector::new();

    let completion = provider
        .stream(
            &req,
            &mut |event| {
                if matches!(event, StreamEvent::TextDelta(_)) {
                    deltas += 1;
                    first_delta.get_or_insert_with(|| started.elapsed());
                }
                collector.push(event)
            },
            &CancelToken::new(),
        )
        .expect("the real turn succeeds");

    println!("live first delta ms {}", first_delta.map_or(0, |d| d.as_millis()));
    println!("live total ms {}", started.elapsed().as_millis());
    println!("live deltas {deltas}");
    println!("live reply bytes {}", collector.text().len());
    println!("live late events {}", native::late_events());
    assert!(deltas >= 1, "at least one delta");
    assert!(!collector.text().trim().is_empty(), "a non-empty reply");
    assert_eq!(completion.stop_reason, StopReason::EndTurn);
    assert!(completion.usage.input_tokens > 0);
    assert_eq!(native::late_events(), 0);
}

#[test]
#[ignore = "needs macOS 26 with Apple Intelligence and HOLZWAVE_TEST_APPLE=1"]
fn a_real_turn_can_be_cancelled_mid_stream() {
    if !live_requested() {
        return;
    }
    let provider = AppleProvider::system();
    let late_before = native::late_events();
    let cancel = CancelToken::new();
    let mut cancelled_at = None;
    let req = user_request("", "Write the numbers 1 to 300, one per line, and nothing else.");

    let result = provider.stream(
        &req,
        &mut |event| {
            if matches!(event, StreamEvent::TextDelta(_)) && cancelled_at.is_none() {
                cancelled_at = Some(Instant::now());
                cancel.cancel();
            }
            Flow::Continue
        },
        &cancel,
    );
    let stopped_after = cancelled_at.expect("the reply started").elapsed();

    println!("live cancel ms {}", stopped_after.as_millis());
    assert_eq!(result.unwrap_err(), AiError::Cancelled);
    assert!(stopped_after < Duration::from_millis(500), "the cancel took {stopped_after:?}");
    assert_eq!(native::late_events(), late_before, "an event arrived after the terminal one");

    // The provider serves the next turn normally.
    let mut collector = Collector::new();
    provider
        .stream(&user_request("Answer in one short sentence.", "Say hello."), &mut |e| collector.push(e), &CancelToken::new())
        .expect("a normal turn after a cancel");
    assert!(!collector.text().trim().is_empty());
}

#[test]
#[ignore = "needs macOS 26 with Apple Intelligence and HOLZWAVE_TEST_APPLE=1"]
fn the_framework_context_overflow_is_a_typed_error() {
    if !live_requested() {
        return;
    }
    // About 48 KiB goes straight to the backend, past the budget of the provider.
    let request = GenerateRequest {
        instructions: String::new(),
        prompt: "word ".repeat(9800),
        max_response_tokens: 64,
    };
    let backend = SystemBackend::new();
    let late_before = native::late_events();

    let result = backend.generate(&request, &mut |_| Flow::Continue, &CancelToken::new());

    println!("live overflow result {result:?}");
    assert_eq!(result, Err(AppleError::ContextExceeded));
    assert_eq!(native::late_events(), late_before);
}

#[test]
#[ignore = "needs macOS 26 with Apple Intelligence and HOLZWAVE_TEST_APPLE=1"]
fn two_real_turns_at_once_both_finish() {
    if !live_requested() {
        return;
    }
    let provider = AppleProvider::system();
    let started = Instant::now();

    let replies: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = ["Say hello.", "Say goodbye."]
            .into_iter()
            .map(|text| {
                let provider = &provider;
                scope.spawn(move || {
                    let mut collector = Collector::new();
                    provider
                        .stream(&user_request("Answer in one short sentence.", text), &mut |e| collector.push(e), &CancelToken::new())
                        .expect("both turns finish");
                    collector.text().to_owned()
                })
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().unwrap()).collect()
    });

    println!("live two at once ms {}", started.elapsed().as_millis());
    assert!(replies.iter().all(|reply| !reply.trim().is_empty()), "{replies:?}");
}
