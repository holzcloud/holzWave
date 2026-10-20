//! The flood gate of the attention events (docs/design.md section 6.23): a pane lets one bell through per half second,
//! whatever its program writes. Synthetic instants only, nothing sleeps.

use std::time::{Duration, Instant};

use holzwave_core::attention::{GAP, Gate};

#[test]
fn the_gap_is_half_a_second() {
    assert_eq!(GAP, Duration::from_millis(500));
}

#[test]
fn the_first_call_passes() {
    let mut gate = Gate::new();
    assert!(gate.pass(Instant::now()));
}

#[test]
fn a_call_inside_the_gap_does_not_pass_and_one_at_the_gap_does() {
    let t0 = Instant::now();
    let mut gate = Gate::new();
    assert!(gate.pass(t0));
    assert!(!gate.pass(t0));
    assert!(!gate.pass(t0 + Duration::from_millis(499)));
    assert!(gate.pass(t0 + GAP));
}

#[test]
fn the_reference_is_the_last_pass_not_the_last_attempt() {
    let t0 = Instant::now();
    let mut gate = Gate::new();
    assert!(gate.pass(t0));
    // Attempts every 100 ms keep failing until 500 ms after the pass; they do not push the window out.
    for ms in [100, 200, 300, 400] {
        assert!(!gate.pass(t0 + Duration::from_millis(ms)), "{ms} ms");
    }
    assert!(gate.pass(t0 + Duration::from_millis(500)));
    assert!(!gate.pass(t0 + Duration::from_millis(600)));
    assert!(gate.pass(t0 + Duration::from_millis(1000)));
}

#[test]
fn a_flood_over_one_second_passes_at_most_two() {
    let t0 = Instant::now();
    let mut gate = Gate::new();
    let passed = (0..1000).filter(|i| gate.pass(t0 + Duration::from_micros(i * 1000))).count();
    assert_eq!(passed, 2, "the passes land at 0 ms and 500 ms");
}

#[test]
fn an_earlier_instant_neither_panics_nor_passes() {
    let t0 = Instant::now() + Duration::from_secs(10);
    let mut gate = Gate::new();
    assert!(gate.pass(t0));
    assert!(!gate.pass(t0 - Duration::from_secs(5)));
    assert!(!gate.pass(t0 - GAP));
}
