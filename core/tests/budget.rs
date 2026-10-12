//! Latency budgets, measured in release mode (D-16: keep events and measure).
//!
//! They are ignored by default because a debug build is meant to be slower than the budget. Run them with
//!
//! ```text
//! cargo test --release -p holzwave-core --test budget -- --ignored --nocapture
//! ```
//!
//! and quote the printed numbers in the verification notes and the spike baseline.
// These tests start `/bin/sh`, `/bin/bash` or `/bin/cat`, which a Windows host does not have. Plan 04's
// `core/tests/conpty.rs` is their Windows counterpart.
#![cfg(unix)]
#![cfg(feature = "ghostty")]

mod common;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::{engine, wait_until};
use holzwave_core::TerminalEngine;
use holzwave_core::engine::{DomKey, Mods};
use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec};
use holzwave_core::wire::FrameEncoder;

const COLS: u16 = 160;
const ROWS: u16 = 50;

/// A full screen of coloured text: every few characters a new truecolor foreground and a palette background.
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

#[test]
#[ignore = "run with --release: cargo test --release -p holzwave-core --test budget -- --ignored"]
fn engine_update_plus_wire_encode_of_a_forced_full_160x50_frame_is_at_most_2_ms() {
    let mut e = engine(COLS, ROWS);
    let mut enc = FrameEncoder::new(1);
    e.feed(&coloured_screen());
    // Warm up: the first update builds the library's render state.
    let first = enc.encode(e.update().expect("update")).expect("first frame");
    assert_eq!(first.changed.len(), usize::from(ROWS));

    const FRAMES: u32 = 200;
    let mut samples = Vec::with_capacity(FRAMES as usize);
    for _ in 0..FRAMES {
        e.request_full();
        enc.request_full();
        let started = Instant::now();
        let update = e.update().expect("update");
        let frame = enc.encode(update).expect("a requested frame is always emitted");
        samples.push(started.elapsed());
        assert!(frame.full);
        std::hint::black_box(frame);
    }
    samples.sort();
    let total: Duration = samples.iter().sum();
    let average = total / FRAMES;
    let p99 = samples[(samples.len() * 99 / 100).min(samples.len() - 1)];
    eprintln!(
        "BUDGET update+encode, {COLS}x{ROWS} coloured, {FRAMES} forced-full frames: average {average:?}, p99 {p99:?}, \
         max {:?} (budget: average <= 2 ms)",
        samples.last().unwrap()
    );
    assert!(average <= Duration::from_millis(2), "average {average:?} is over the 2 ms budget");
}

#[test]
#[ignore = "run with --release: cargo test --release -p holzwave-core --test budget -- --ignored"]
fn key_to_frame_p99_through_the_actor_with_cat_is_at_most_5_ms() {
    let (tx, rx) = mpsc::channel::<(Instant, Option<u16>)>();
    let pane = PaneHandle::spawn_ghostty(1, PaneSpec::new("/bin/cat", COLS, ROWS), move |event| {
        if let PaneEvent::Frame(frame) = event {
            let _ = tx.send((Instant::now(), frame.cursor.map(|c| c.x)));
        }
    })
    .expect("spawn");
    // The first frame (the empty screen) tells that the pane is up.
    assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok(), "no first frame");

    const SAMPLES: usize = 200;
    // A line is typed one character at a time; the cursor column after the n-th character is n. The sample ends
    // when a frame shows that column. After 60 characters Enter starts a new line (not sampled).
    let mut samples = Vec::with_capacity(SAMPLES);
    let mut column = 0u16;
    while samples.len() < SAMPLES {
        if column == 60 {
            pane.key(DomKey { code: "Enter".into(), key: "Enter".into(), mods: Mods::NONE, ..DomKey::default() })
                .expect("enter");
            // Let the echo and cat's copy of the line settle, and forget what arrived meanwhile.
            std::thread::sleep(Duration::from_millis(100));
            while rx.try_recv().is_ok() {}
            column = 0;
            continue;
        }
        let letter = char::from(b'a' + (samples.len() % 26) as u8).to_string();
        column += 1;
        // A pause longer than the frame interval: this measures the path of one key, not the pacing of a burst.
        std::thread::sleep(Duration::from_millis(20));
        while rx.try_recv().is_ok() {}
        let sent = Instant::now();
        pane.key(DomKey { code: "KeyA".into(), key: letter, mods: Mods::NONE, ..DomKey::default() }).expect("key");
        let deadline = sent + Duration::from_secs(2);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let (at, x) = rx.recv_timeout(left).expect("a frame after the key");
            if x == Some(column) {
                samples.push(at.duration_since(sent));
                break;
            }
        }
    }
    pane.close();
    assert!(wait_until(Duration::from_secs(3), || pane.join(Duration::from_millis(10))));

    samples.sort();
    let p50 = samples[samples.len() / 2];
    let p99 = samples[(samples.len() * 99 / 100).min(samples.len() - 1)];
    eprintln!(
        "BUDGET key to frame through the actor with cat, {SAMPLES} samples: p50 {p50:?}, p99 {p99:?}, max {:?} \
         (budget: p99 <= 5 ms, measured at the sink, without webview IPC)",
        samples.last().unwrap()
    );
    assert!(p99 <= Duration::from_millis(5), "p99 {p99:?} is over the 5 ms budget");
}
