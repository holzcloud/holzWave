//! Tests against the real system model. They are ignored by default: they need a Mac with macOS 26 or later and Apple
//! Intelligence switched on, and they run only with `HOLZWAVE_TEST_APPLE=1`:
//!
//! `HOLZWAVE_TEST_APPLE=1 cargo test -p holzwave-apple-ai --test live -- --ignored --nocapture`
#![cfg(target_os = "macos")]

mod common;

use std::time::Instant;

use holzwave_apple_ai::{AppleProvider, Availability, native, probe};
use holzwave_core::ai::provider::{CancelToken, Collector, Provider, StreamEvent};
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
