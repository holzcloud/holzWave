//! The cost of the whole stream path, measured: from the call of `Provider::stream` through the chokepoint, the framing,
//! the adapter and the callback, on every wire.
//!
//! One test runs on every `cargo test` and proves the flood harness: a long reply of one-character events is counted
//! event by event on every wire and ends cleanly, so a bound that counts what a stream sends instead of what an adapter
//! stores is found here. Two tests are ignored because a debug build is meant to be slower than the budget. Run them in a
//! release build and quote the printed numbers:
//!
//! ```text
//! cargo test --release -p holzwave-core --no-default-features --test ai_matrix_budget -- --ignored --nocapture
//! ```
//!
//! The reference is the bare HTTP client on the same kind of stream, measured on Linux x86_64 (an Intel Xeon 2.1 GHz
//! virtual machine, 4 cores) in a release build with thin LTO: time to first delta 0.32 ms median and 0.75 ms at most with
//! a warm client, 0.77 ms median and 1.53 ms at most with a fresh client each time, and 1.17 microseconds per event
//! (854,845 events per second) on a stream of 100,000 events. The difference between those numbers and the numbers printed
//! here is the cost of the decoder, the adapter and the callback.
//!
//! The ceilings are wall-clock proxies measured on one machine, not guarantees. A value above its ceiling fails the test
//! and is a finding: the ceiling is never raised to make it pass.

mod common;
#[path = "ai_matrix/streams.rs"]
mod streams;
#[path = "ai_matrix/support.rs"]
mod support;

use std::cell::Cell as StdCell;
use std::time::{Duration, Instant};

use common::fake_http::{Ending, Reply};
use holzwave_core::ai::http::HttpConfig;
use holzwave_core::ai::provider::{CancelToken, Flow, Provider, StreamEvent};
use holzwave_core::ai::types::StopReason;
use holzwave_core::ai::wire::Wire;
use streams::{flood, stream};
use support::{Cell, Keys, Scenario, cell, chat_request, reply_for, rig, timing_guard};

/// The events of the per-event test.
const EVENTS: usize = 100_000;
/// The most microseconds one event may cost on the whole provider path. Started at 100 and tightened to the larger of
/// three times the worst of three release runs (0.86 us) and the floor of 10 us.
const PER_EVENT_US: f64 = 10.0;
/// The most milliseconds the median time to the first delta may take with a warm provider. Started at 20 and tightened
/// to the larger of three times the worst median of three release runs (0.63 ms) and the floor of 2 ms.
const FIRST_DELTA_WARM_MS: f64 = 2.0;
/// The most milliseconds the median time to the first delta may take with a fresh chokepoint and provider each time.
/// Started at 100 and tightened to the larger of three times the worst median of three release runs (0.41 ms) and the
/// floor of 2 ms.
const FIRST_DELTA_COLD_MS: f64 = 2.0;

/// The five Custom cells, one per wire.
fn custom_cells() -> Vec<Cell> {
    ["custom_chat", "custom_ollama", "custom_anthropic", "custom_responses", "custom_gemini"].map(cell).to_vec()
}

/// The middle value of a series.
fn median(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[times.len() / 2]
}

/// The operating system and the architecture of this build, for the printed lines.
fn machine() -> String {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "other"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        "other"
    };
    format!("{os} {arch}")
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

#[test]
fn the_flood_harness_counts_every_event_on_every_wire() {
    for cell in custom_cells() {
        let built = rig(&cell, vec![reply_for(cell.wire, flood(cell.wire, 2_000))], Keys::None, HttpConfig::standard());
        let result = support::run(&built.provider, &chat_request(&cell, "hello"));
        let done = result
            .result
            .as_ref()
            .unwrap_or_else(|error| panic!("{}: a long reply of one-character events failed: {error}", cell.name));
        assert_eq!(done.stop_reason, StopReason::EndTurn, "{}: the stop reason", cell.name);
        let texts: Vec<&String> = result
            .events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::TextDelta(text) => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(texts.len(), 2_000, "{}: every event is delivered once", cell.name);
        assert_eq!(texts.iter().map(|text| text.chars().count()).sum::<usize>(), 2_000, "{}: the joined length", cell.name);
        assert_eq!(built.fake.accepted(), 1, "{}: one connection", cell.name);
    }
}

#[test]
#[ignore = "a measurement for release builds: cargo test --release -p holzwave-core --no-default-features --test ai_matrix_budget -- --ignored --nocapture"]
fn stream_overhead_per_event_stays_inside_the_budget_on_every_wire() {
    let _guard = timing_guard();
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    println!("stream overhead on {}, {profile} build", machine());
    for cell in custom_cells() {
        // The flood is built before the clock starts.
        let chunks = flood(cell.wire, EVENTS);
        let built = rig(&cell, vec![reply_for(cell.wire, chunks)], Keys::None, HttpConfig::standard());
        let request = chat_request(&cell, "hello");
        let counted = StdCell::new(0usize);
        let started = Instant::now();
        let result = built.provider.stream(
            &request,
            &mut |event| {
                if matches!(event, StreamEvent::TextDelta(_)) {
                    counted.set(counted.get() + 1);
                }
                Flow::Continue
            },
            &CancelToken::new(),
        );
        let elapsed = started.elapsed();
        result.unwrap_or_else(|error| panic!("{}: the flood failed: {error}", cell.name));
        let per_event = elapsed.as_secs_f64() * 1e6 / EVENTS as f64;
        println!(
            "per event {}: {} events, {:.1} ms, {per_event:.2} us per event, {:.0} events/s",
            cell.wire.tag(),
            counted.get(),
            ms(elapsed),
            EVENTS as f64 / elapsed.as_secs_f64()
        );
        assert_eq!(counted.get(), EVENTS, "{}: events were lost or counted twice", cell.name);
        if !cfg!(debug_assertions) {
            assert!(per_event <= PER_EVENT_US, "{}: {per_event:.2} us per event, the ceiling is {PER_EVENT_US}", cell.name);
        }
    }
    if cfg!(debug_assertions) {
        println!("budgets: not asserted in a debug build");
    }
}

/// The time from the call of `stream` to the first text delta, which cancels the turn.
fn first_delta(provider: &dyn Provider, request: &holzwave_core::ai::provider::ChatRequest) -> Duration {
    let token = CancelToken::new();
    let started = Instant::now();
    let at: StdCell<Option<Duration>> = StdCell::new(None);
    let _ = provider.stream(
        request,
        &mut |event| {
            if matches!(event, StreamEvent::TextDelta(_)) && at.get().is_none() {
                at.set(Some(started.elapsed()));
                token.cancel();
            }
            Flow::Continue
        },
        &token,
    );
    at.get().expect("no text delta arrived")
}

/// A server that sends the stream up to its first delta and holds the socket.
fn prefix_reply(wire: Wire) -> Reply {
    let streamed = stream(wire, Scenario::Text);
    reply_for(wire, vec![streamed.bytes[..streamed.first_delta_end].to_vec()]).ending(Ending::Hold)
}

#[test]
#[ignore = "a measurement for release builds: cargo test --release -p holzwave-core --no-default-features --test ai_matrix_budget -- --ignored --nocapture"]
fn time_to_first_delta_through_the_provider_stays_inside_the_budget_on_every_wire() {
    let _guard = timing_guard();
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    println!("time to first delta on {}, {profile} build", machine());
    for cell in custom_cells() {
        let request = chat_request(&cell, "hello");

        // Warm: one provider and one chokepoint, one unmeasured request first.
        let built = rig(&cell, vec![prefix_reply(cell.wire)], Keys::None, HttpConfig::standard());
        first_delta(&built.provider, &request);
        let warm: Vec<Duration> = (0..20).map(|_| first_delta(&built.provider, &request)).collect();

        // Cold: a fresh chokepoint and a rebuilt provider for every request.
        let cold: Vec<Duration> = (0..10)
            .map(|_| {
                let fresh = rig(&cell, vec![prefix_reply(cell.wire)], Keys::None, HttpConfig::standard());
                first_delta(&fresh.provider, &request)
            })
            .collect();

        let warm_max = warm.iter().copied().max().expect("a series");
        let cold_max = cold.iter().copied().max().expect("a series");
        let (warm_median, cold_median) = (median(warm), median(cold));
        println!(
            "time to first delta {}: warm median {:.2} ms max {:.2} ms, cold median {:.2} ms max {:.2} ms",
            cell.wire.tag(),
            ms(warm_median),
            ms(warm_max),
            ms(cold_median),
            ms(cold_max)
        );
        if !cfg!(debug_assertions) {
            assert!(
                ms(warm_median) <= FIRST_DELTA_WARM_MS,
                "{}: warm median {:.2} ms, the ceiling is {FIRST_DELTA_WARM_MS}",
                cell.name,
                ms(warm_median)
            );
            assert!(
                ms(cold_median) <= FIRST_DELTA_COLD_MS,
                "{}: cold median {:.2} ms, the ceiling is {FIRST_DELTA_COLD_MS}",
                cell.name,
                ms(cold_median)
            );
        }
    }
    if cfg!(debug_assertions) {
        println!("budgets: not asserted in a debug build");
    }
}
