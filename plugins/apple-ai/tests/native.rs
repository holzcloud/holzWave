//! The system backend against whatever state the model of this Mac is in. These tests pass on a Mac without Apple
//! Intelligence too: they check that the answers agree with the availability, not that the model answers.
#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use holzwave_apple_ai::{Availability, Backend, SystemBackend, probe};

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

    assert!(took < Duration::from_secs(3), "the calls took {took:?}");
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
