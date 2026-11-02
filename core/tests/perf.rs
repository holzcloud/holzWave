//! Performance measurements of the core, release mode (POL-02, D-14, D-15).
//!
//! They are ignored by default: a debug build is meant to be slower, and a measurement has no place in `cargo test`. They
//! print one `PERF` line per metric (see `perf_report`) and write no file. Run them through the guard, which collects the
//! lines and judges them against the baseline of this machine:
//!
//! ```text
//! sh scripts/perf-check.sh      # measure and compare with perf/baseline-<os>-<arch>.json
//! sh scripts/perf-record.sh     # measure and record the baseline of this machine
//! ```
//!
//! or by hand: `cargo test --release -p holzwave-core --test perf -- --ignored --nocapture`.
// These tests start `/bin/sh` and `/bin/cat`, which a Windows host does not have.
#![cfg(unix)]
#![cfg(feature = "ghostty")]

mod common;
mod perf_report;

use std::hint::black_box;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use common::engine;
use holzwave_core::TerminalEngine;
use holzwave_core::wire::FrameEncoder;
use perf_report::report;

const COLS: u16 = 160;
const ROWS: u16 = 50;

/// One measurement at a time: two of them in parallel threads would disturb each other, whatever the test runner's
/// `--test-threads` is.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn exclusive() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A full screen of coloured text: every few characters a new truecolor foreground and a palette background. (A copy of
/// the one in `budget.rs`, which stays as it is.)
fn coloured_screen() -> Vec<u8> {
    let mut out = Vec::new();
    for row in 0..ROWS {
        for word in 0..(COLS / 8) {
            let n = u32::from(row) * 31 + u32::from(word) * 17;
            let (r, g, b) = ((n * 7) % 256, (n * 13) % 256, (n * 29) % 256);
            out.extend_from_slice(format!("\x1b[38;2;{r};{g};{b}m\x1b[48;5;{}m{:<8}", 16 + n % 216, "word").as_bytes());
        }
        out.extend_from_slice(b"\x1b[0m\r\n");
    }
    out
}

/// `core.update_encode_ms`: the engine update plus the wire encode of a forced full 160 by 50 frame, the mean of 200 frames
/// per run after a warm-up frame, 5 runs.
#[test]
#[ignore = "a measurement: sh scripts/perf-check.sh"]
fn perf_update_encode() {
    let _one = exclusive();
    const FRAMES: u32 = 200;
    const RUNS: usize = 5;
    let mut e = engine(COLS, ROWS);
    let mut enc = FrameEncoder::new(1);
    e.feed(&coloured_screen());
    // Warm up: the first update builds the library's render state.
    let first = enc.encode(e.update().expect("update")).expect("first frame");
    assert_eq!(first.changed.len(), usize::from(ROWS));

    let mut means_ms = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let mut total = 0.0f64;
        for _ in 0..FRAMES {
            e.request_full();
            enc.request_full();
            let started = Instant::now();
            let update = e.update().expect("update");
            let frame = enc.encode(update).expect("a requested frame is always emitted");
            total += started.elapsed().as_secs_f64();
            assert!(frame.full);
            black_box(frame);
        }
        means_ms.push(total / f64::from(FRAMES) * 1000.0);
    }
    report("core.update_encode_ms", "ms", &means_ms);
}
