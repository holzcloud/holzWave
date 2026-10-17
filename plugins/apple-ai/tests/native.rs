//! The system backend against whatever state the model of this Mac is in. These tests pass on a Mac without Apple
//! Intelligence too: they check that the answers agree with the availability, not that the model answers.
#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use holzwave_apple_ai::{AppleError, Availability, Backend, GenerateRequest, SystemBackend, native, probe};
use holzwave_core::ai::provider::{CancelToken, Flow};

/// The (major, minor) of the macOS this test runs on, from `sw_vers`.
fn macos_version() -> (u32, u32) {
    let output = std::process::Command::new("sw_vers").arg("-productVersion").output().expect("sw_vers runs");
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let mut parts = text.split('.').map(|part| part.parse::<u32>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

#[test]
fn the_system_backend_reports_the_probe_state() {
    let backend = SystemBackend::new();
    assert_eq!(backend.availability(), probe());
    println!("availability {}", backend.availability().as_str());
}

#[test]
fn context_size_and_token_count_agree_with_availability() {
    let backend = SystemBackend::new();
    let (major, minor) = macos_version();

    let started = Instant::now();
    let window = backend.context_size();
    let count = backend.count_tokens("hello world");
    let took = started.elapsed();
    println!("macOS {major}.{minor} availability {} window {window:?} count {count:?} in {took:?}", backend.availability().as_str());

    assert!(took < Duration::from_secs(15), "the calls took {took:?}");
    if backend.availability() == Availability::Available {
        let window = window.expect("an available model reports its window");
        assert!((1024..=1_000_000).contains(&window), "window {window}");
        if (major, minor) >= (26, 4) {
            assert!(count.is_some_and(|count| count > 0), "count {count:?}");
        } else {
            assert_eq!(count, None, "counting needs macOS 26.4");
        }
    } else {
        assert_eq!(window, None);
        assert_eq!(count, None);
    }
}

#[test]
fn a_call_without_the_model_ends_with_one_typed_error_or_the_model_has_a_window() {
    let backend = SystemBackend::new();
    if backend.availability() == Availability::Available {
        let window = backend.context_size().expect("an available model reports its window");
        assert!(window > 1024, "window {window}");
        return;
    }
    let late_before = native::late_events();
    let request = GenerateRequest { instructions: String::new(), prompt: "hello".to_owned(), max_response_tokens: 64 };
    let mut delivered = 0;

    let result = backend.generate(
        &request,
        &mut |_| {
            delivered += 1;
            Flow::Continue
        },
        &CancelToken::new(),
    );

    assert_eq!(result, Err(AppleError::Unavailable));
    assert_eq!(delivered, 0);
    assert_eq!(native::late_events(), late_before, "an event arrived after the terminal one");

    // Text over the bound is refused before any foreign call, whatever the state of the model.
    let huge = GenerateRequest { prompt: "x".repeat(holzwave_apple_ai::MAX_INPUT_BYTES + 1), ..request };
    assert_eq!(backend.generate(&huge, &mut |_| Flow::Continue, &CancelToken::new()), Err(AppleError::ContextExceeded));
}

#[test]
fn cancelling_an_unknown_handle_is_harmless() {
    for handle in [0, -1, 1, 42, i64::MAX, i64::MIN] {
        native::cancel_handle(handle);
    }
    // And twice, as a finished call would see it.
    native::cancel_handle(7);
    native::cancel_handle(7);
}

#[test]
fn token_counting_never_blocks_for_long() {
    let backend = SystemBackend::new();
    let started = Instant::now();
    let _ = backend.count_tokens("hello");
    let _ = backend.count_tokens("");
    assert!(started.elapsed() < Duration::from_secs(15), "counting took {:?}", started.elapsed());

    // A text over the bound is `None` without a foreign call.
    let started = Instant::now();
    assert_eq!(backend.count_tokens(&"x".repeat(holzwave_apple_ai::MAX_INPUT_BYTES + 1)), None);
    assert!(started.elapsed() < Duration::from_millis(500));
}
