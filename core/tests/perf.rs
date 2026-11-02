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
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

use common::{Xorshift64, engine, wait_until};
use holzwave_core::engine::{DomKey, Mods};
use holzwave_core::pane::{DEFAULT_SCROLLBACK, PaneEvent, PaneHandle, PaneSpec};
use holzwave_core::wire::FrameEncoder;
use holzwave_core::{GhosttyEngine, TerminalEngine};
use perf_report::{percentile, report, rss_kib};

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

/// A fixed corpus of `bytes` or a little more, built from a seed so that every run and every machine feeds the same bytes:
/// lines of 20 to 150 printable characters, an SGR change every 12 characters on average, a newline after every line.
fn corpus(bytes: usize) -> Vec<u8> {
    let mut rng = Xorshift64::new(0x7065_7266_636f_7265);
    let mut out = Vec::with_capacity(bytes + 1024);
    while out.len() < bytes {
        let len = 20 + rng.below(131);
        for _ in 0..len {
            let r = rng.next_u64();
            if r.is_multiple_of(12) {
                let c = (r >> 8) as u32;
                let sgr = match (r >> 4) % 5 {
                    0 => format!("\x1b[3{}m", c % 8),
                    1 => "\x1b[1m".to_string(),
                    2 => format!("\x1b[38;2;{};{};{}m", c % 256, (c >> 8) % 256, (c >> 16) % 256),
                    3 => format!("\x1b[48;5;{}m", c % 256),
                    _ => "\x1b[0m".to_string(),
                };
                out.extend_from_slice(sgr.as_bytes());
            }
            out.push(b' ' + ((r >> 32) % 95) as u8);
        }
        out.extend_from_slice(b"\r\n");
    }
    out
}

const MIB: f64 = 1024.0 * 1024.0;

/// One run of `core.parse_mib_s`: the engine fed the corpus in slices of 64 KiB with an update after each, MiB per second.
fn parse_run(corpus: &[u8]) -> f64 {
    let mut e = GhosttyEngine::new(COLS, ROWS, 1000).expect("engine");
    let started = Instant::now();
    for slice in corpus.chunks(64 * 1024) {
        e.feed(slice);
        black_box(e.update().expect("update"));
    }
    corpus.len() as f64 / MIB / started.elapsed().as_secs_f64()
}

const OUTPUT_BYTES: u64 = 128 * 1024 * 1024;

/// One run of `core.pane_output_mib_s`: a shell prints 128 MiB through the pane actor to a sink that counts frames and
/// drops them; MiB per second from the spawn to the end of the pane.
fn output_run() -> f64 {
    let line = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ-_";
    assert_eq!(line.len(), 64);
    let script = format!("yes \"{line}\" | head -c {OUTPUT_BYTES}");
    let frames = Arc::new(AtomicU64::new(0));
    let (tx, rx) = mpsc::channel::<Result<Instant, String>>();
    let counted = frames.clone();
    let started = Instant::now();
    let pane = PaneHandle::spawn_ghostty(1, PaneSpec::new("/bin/sh", COLS, ROWS).args(["-c", script.as_str()]), move |event| {
        match event {
            PaneEvent::Frame(frame) => {
                counted.fetch_add(1, Ordering::Relaxed);
                black_box(frame);
            }
            PaneEvent::Exited { .. } => {
                let _ = tx.send(Ok(Instant::now()));
            }
            PaneEvent::Failed(why) => {
                let _ = tx.send(Err(why));
            }
            _ => {}
        }
    })
    .expect("spawn");
    let ended = rx
        .recv_timeout(Duration::from_secs(120))
        .expect("the shell did not end within 120 s")
        .expect("the pane did not start");
    pane.close();
    assert!(wait_until(Duration::from_secs(3), || pane.join(Duration::from_millis(10))));
    assert!(frames.load(Ordering::Relaxed) > 0, "no frame reached the sink");
    OUTPUT_BYTES as f64 / MIB / ended.duration_since(started).as_secs_f64()
}

/// `core.parse_mib_s` (the engine alone, no pty) and `core.pane_output_mib_s` (the whole pane).
#[test]
#[ignore = "a measurement: sh scripts/perf-check.sh"]
fn perf_parse_and_output() {
    let _one = exclusive();
    let data = corpus(64 * 1024 * 1024);
    // Warm up: pages of the library and of the allocator, on a shorter feed.
    black_box(parse_run(&data[..4 * 1024 * 1024]));
    let parse: Vec<f64> = (0..5).map(|_| parse_run(&data)).collect();
    report("core.parse_mib_s", "mib_s", &parse);
    drop(data);

    black_box(output_run());
    let output: Vec<f64> = (0..5).map(|_| output_run()).collect();
    report("core.pane_output_mib_s", "mib_s", &output);
}

fn enter() -> DomKey {
    DomKey { code: "Enter".into(), key: "Enter".into(), mods: Mods::NONE, ..DomKey::default() }
}

/// One run of the key loop of `budget.rs`: `samples` keys one at a time through the actor with `cat`, 20 ms between them.
/// Returns the p50 and the p99 of the time from the key to the frame that shows it, in ms.
fn key_run(samples: usize) -> (f64, f64) {
    let (tx, rx) = mpsc::channel::<(Instant, Option<u16>)>();
    let pane = PaneHandle::spawn_ghostty(1, PaneSpec::new("/bin/cat", COLS, ROWS), move |event| {
        if let PaneEvent::Frame(frame) = event {
            let _ = tx.send((Instant::now(), frame.cursor.map(|c| c.x)));
        }
    })
    .expect("spawn");
    // The first frame (the empty screen) tells that the pane is up.
    assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok(), "no first frame");

    // A line is typed one character at a time; the cursor column after the n-th character is n. The sample ends when a
    // frame shows that column. After 60 characters Enter starts a new line (not sampled).
    let mut taken: Vec<f64> = Vec::with_capacity(samples);
    let mut column = 0u16;
    while taken.len() < samples {
        if column == 60 {
            pane.key(enter()).expect("enter");
            // Let the echo and cat's copy of the line settle, and forget what arrived meanwhile.
            std::thread::sleep(Duration::from_millis(100));
            while rx.try_recv().is_ok() {}
            column = 0;
            continue;
        }
        let letter = char::from(b'a' + (taken.len() % 26) as u8).to_string();
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
                taken.push(at.duration_since(sent).as_secs_f64() * 1000.0);
                break;
            }
        }
    }
    pane.close();
    assert!(wait_until(Duration::from_secs(3), || pane.join(Duration::from_millis(10))));
    (percentile(&taken, 50.0), percentile(&taken, 99.0))
}

/// One run of `core.pane_start_ms`: from the call that starts a pane running `cat` to its first frame at the sink, in ms.
fn start_run() -> f64 {
    let (tx, rx) = mpsc::channel::<Instant>();
    let started = Instant::now();
    let pane = PaneHandle::spawn_ghostty(1, PaneSpec::new("/bin/cat", COLS, ROWS), move |event| {
        if let PaneEvent::Frame(_) = event {
            let _ = tx.send(Instant::now());
        }
    })
    .expect("spawn");
    let first = rx.recv_timeout(Duration::from_secs(5)).expect("no first frame");
    pane.close();
    assert!(wait_until(Duration::from_secs(3), || pane.join(Duration::from_millis(10))));
    first.duration_since(started).as_secs_f64() * 1000.0
}

/// `core.key_to_frame_p50_ms`, `core.key_to_frame_p99_ms` (200 keys per run, 5 runs) and `core.pane_start_ms` (10 runs).
#[test]
#[ignore = "a measurement: sh scripts/perf-check.sh"]
fn perf_key_to_frame_and_start() {
    let _one = exclusive();
    black_box(key_run(40));
    let runs: Vec<(f64, f64)> = (0..5).map(|_| key_run(200)).collect();
    let p50: Vec<f64> = runs.iter().map(|r| r.0).collect();
    let p99: Vec<f64> = runs.iter().map(|r| r.1).collect();
    report("core.key_to_frame_p50_ms", "ms", &p50);
    report("core.key_to_frame_p99_ms", "ms", &p99);

    black_box(start_run());
    let starts: Vec<f64> = (0..10).map(|_| start_run()).collect();
    report("core.pane_start_ms", "ms", &starts);
}

const ENGINES: usize = 8;
const MEMORY_RUNS: usize = 5;
/// Set in the environment of the child process that takes one memory figure.
const MEMORY_CHILD: &str = "HOLZWAVE_PERF_MEMORY_CHILD";

/// The figure of one run, taken in a process of its own so that no earlier engine, corpus or pane has left resident pages
/// behind (a process does not give freed memory back at once, so a delta taken late in a long process says too little).
/// Prints one `RAWMEM <empty> <scrollback>` line: KiB per engine, the delta of the resident set of the whole process
/// divided by the engine count.
#[test]
#[ignore = "run by perf_memory, in a process of its own"]
fn perf_memory_child() {
    if std::env::var_os(MEMORY_CHILD).is_none() {
        return;
    }
    let before_empty = rss_kib().expect("resident memory is not readable on this OS");
    let mut engines: Vec<GhosttyEngine> = (0..ENGINES)
        .map(|_| GhosttyEngine::new(COLS, ROWS, DEFAULT_SCROLLBACK).expect("engine"))
        .collect();
    // A pane has drawn its first frame: the render state of the library exists.
    for e in &mut engines {
        black_box(e.update().expect("update"));
    }
    let before_feed = rss_kib().expect("resident memory");

    // 30 000 lines of 150 characters fill the default scrollback of 10 000 lines with 20 000 lines to spare.
    let mut lines = Vec::with_capacity(32 * 1024 * 1024 / 4);
    for n in 0..30_000usize {
        lines.extend((0..150).map(|c| b'a' + ((n + c) % 26) as u8));
        lines.extend_from_slice(b"\r\n");
    }
    for e in &mut engines {
        for slice in lines.chunks(64 * 1024) {
            e.feed(slice);
        }
        black_box(e.update().expect("update"));
    }
    drop(lines);
    let after_feed = rss_kib().expect("resident memory");
    black_box(&engines);
    let per = |from: u64, to: u64| to.saturating_sub(from) / ENGINES as u64;
    println!("\nRAWMEM {} {}", per(before_empty, before_feed), per(before_feed, after_feed));
}

/// `core.memory_pane_empty_mib` and `core.memory_pane_scrollback_mib`: eight engines held at once, the resident set of
/// the whole process before and after divided by eight; five runs, each in a fresh process (`perf_memory_child`).
#[test]
#[ignore = "a measurement: sh scripts/perf-check.sh"]
fn perf_memory() {
    let _one = exclusive();
    let exe = std::env::current_exe().expect("the path of the test binary");
    let mut empty = Vec::new();
    let mut full = Vec::new();
    for _ in 0..MEMORY_RUNS {
        let out = Command::new(&exe)
            .args(["perf_memory_child", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
            .env(MEMORY_CHILD, "1")
            .output()
            .expect("the child process");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "the memory child failed: {}{}", text, String::from_utf8_lossy(&out.stderr));
        let figures: Vec<f64> = text
            .lines()
            .find_map(|l| l.strip_prefix("RAWMEM "))
            .expect("the child printed no RAWMEM line")
            .split_whitespace()
            .map(|v| v.parse::<f64>().expect("a number") / 1024.0)
            .collect();
        assert_eq!(figures.len(), 2);
        empty.push(figures[0]);
        full.push(figures[1]);
    }
    report("core.memory_pane_empty_mib", "mib", &empty);
    report("core.memory_pane_scrollback_mib", "mib", &full);
}
