//! The performance measurements of the server (POL-02, D-14, D-16): a key typed on a real socket to the frame that shows it,
//! the time to reattach to a pane that kept running, and the memory per pane with many panes.
//!
//! Every test here is `#[ignore]` and a release measurement: it runs on request only (`sh scripts/perf-check.sh server`),
//! never in `cargo test`, and it prints one `PERF {json}` line per metric (see `core/tests/perf_report`) and writes no file.
//! They need the `in-process-workers` feature and the built page (`npm --prefix app run build`), like the other tests of the
//! real server.
//!
//! What is measured is the whole path a browser's socket takes: TLS and the WebSocket on a loopback port, the supervisor, the
//! worker (in this process), the pane actor with a real pseudo terminal, the engine, the frame encoder and the way back. The
//! browser itself is not in it; `app/tests/web/perf.mjs` measures that part.
#![cfg(feature = "in-process-workers")]

#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/parity_env.rs"]
mod parity_env;
#[path = "../../core/tests/perf_report/mod.rs"]
mod perf_report;
#[path = "support/ws_env.rs"]
mod ws_env;

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use parity_env::{Line, Probed, Screen, fresh, start_focused_pane};
use perf_report::{percentile, report};
use serde_json::{Value, json};
use ws_env::{Got, Incoming, Person, WsOptions};

/// One measurement at a time: the tests share the machine, and a second one running beside the first would be in its numbers.
static ONE: Mutex<()> = Mutex::new(());

fn exclusive() -> MutexGuard<'static, ()> {
    ONE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The runs of the key loop. The p99 of 200 keys is its two slowest and moves with the scheduler (between three recordings the
/// median of 5 runs moved by a third), so the median is taken over more runs than the other metrics have.
const KEY_RUNS: usize = 9;

/// The longest any wait of a measurement takes before it is a failure and not a slow number.
const BOUND: Duration = Duration::from_secs(60);

// ---- reading frames -----------------------------------------------------------------------------------------------------

/// A `frame` event of `pane` read from the socket: when it was read (before any parsing) and its payload.
struct Frame {
    at: Instant,
    payload: Value,
}

/// Reads the next `frame` event of `pane` within `within`; the events of other panes, answers and pings are skipped.
/// `None` when none came.
async fn next_frame(line: &mut Line, pane: u64, within: Duration) -> Option<Frame> {
    let deadline = Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        match line.client.read(left).await {
            Got::Message(Incoming::Text(text)) => {
                let at = Instant::now();
                // A cheap test first: only an event of the name `frame` is parsed.
                if !text.starts_with("{\"event\":\"frame\"") {
                    continue;
                }
                let value: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("a frame that is not JSON ({e})"));
                if value["payload"]["pane"].as_u64() == Some(pane) {
                    return Some(Frame { at, payload: value["payload"].clone() });
                }
            }
            Got::Message(Incoming::Close(code, _)) => panic!("the server closed the socket during a measurement: {code:?}"),
            Got::Message(_) => {}
            Got::Ended => panic!("the socket ended during a measurement"),
            Got::Quiet => return None,
        }
    }
}

/// Reads and drops everything until the socket has been silent for `quiet` (at most `BOUND`).
async fn settle(line: &mut Line, quiet: Duration) {
    let deadline = Instant::now() + BOUND;
    while Instant::now() < deadline {
        match line.client.read(quiet).await {
            Got::Quiet => return,
            Got::Message(Incoming::Close(code, _)) => panic!("the server closed the socket while it settled: {code:?}"),
            Got::Ended => panic!("the socket ended while it settled"),
            Got::Message(_) => {}
        }
    }
    panic!("the socket never went quiet");
}

// ---- panes --------------------------------------------------------------------------------------------------------------

/// Sends one key the way the page sends it: the `key` command with the code, the key, the modifier bits and the flags. The
/// answer is not waited for (the page does not wait for it either).
async fn send_key(line: &mut Line, pane: u64, code: &str, key: &str) {
    line.send("key", &json!({"pane": pane, "code": code, "key": key, "mods": 0, "repeat": false, "release": false, "composing": false})).await;
}

async fn send_text(line: &mut Line, pane: u64, text: &str) {
    line.send("text", &json!({"pane": pane, "text": text})).await;
}

/// Puts `cat` in place of the shell of a pane (`exec cat`), and waits until it is proven to run: a probe line is on the screen
/// twice, as the terminal's echo and as cat's copy of it (the shell would have said `not found` instead of copying it). The
/// screen is rebuilt from the frames as the page does, because the echo and the copy come in different frames.
async fn become_cat(line: &mut Line, pane: u64) {
    send_text(line, pane, "exec cat\r").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    send_text(line, pane, "hw-probe\r").await;
    let mut screen = Screen::default();
    let deadline = Instant::now() + BOUND;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let frame = next_frame(line, pane, left.max(Duration::from_millis(1))).await.expect("cat never answered the probe line");
        screen.apply(&frame.payload);
        if screen.rows.iter().filter(|row| row.as_str() == "hw-probe").count() >= 2 {
            break;
        }
    }
    settle(line, Duration::from_millis(100)).await;
}

/// One person and a focused pane that runs `cat`.
async fn cat_pane(probed: &Probed, person: &Person) -> (Line, u64) {
    let mut line = fresh(probed, person).await;
    let pane = start_focused_pane(&mut line).await;
    become_cat(&mut line, pane).await;
    (line, pane)
}

// ---- the key to the frame -----------------------------------------------------------------------------------------------

/// One run: `samples` keys, one at a time, 20 ms apart, each timed from the call that sends it to the first frame of the pane
/// that read from the socket with the cursor on the column the character moves it to. A line is typed up to 60 characters,
/// then Enter starts the next (not sampled). Returns the p50 and the p99 in ms.
///
/// The frame that "shows" the character is recognised by the cursor: cat's echo of a letter moves the cursor one column to the
/// right, and the frame header carries the cursor in every frame, so the first frame with `cursor.x == column` is the one that
/// the key caused. A frame that only changed another header member has the old column and is not counted.
async fn key_run(line: &mut Line, pane: u64, samples: usize) -> (f64, f64) {
    let mut taken: Vec<f64> = Vec::with_capacity(samples);
    let mut column = 0u64;
    let mut typed = 0usize;
    // A fresh line, so that the cursor column counts from 0.
    send_key(line, pane, "Enter", "Enter").await;
    settle(line, Duration::from_millis(100)).await;
    while taken.len() < samples {
        if column == 60 {
            send_key(line, pane, "Enter", "Enter").await;
            settle(line, Duration::from_millis(100)).await;
            column = 0;
            continue;
        }
        let letter = char::from(b'a' + (typed % 26) as u8).to_string();
        typed += 1;
        column += 1;
        // A pause longer than the frame interval of 8 ms: the path of one key, not the pacing of a burst. What arrived
        // meanwhile (the answer of the last key) is dropped.
        settle(line, Duration::from_millis(20)).await;
        let sent = Instant::now();
        send_key(line, pane, "KeyA", &letter).await;
        let deadline = sent + Duration::from_secs(2);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let frame = next_frame(line, pane, left).await.expect("a frame after the key");
            if frame.payload["cursor"]["x"].as_u64() == Some(column) {
                taken.push(frame.at.duration_since(sent).as_secs_f64() * 1000.0);
                break;
            }
        }
    }
    (percentile(&taken, 50.0), percentile(&taken, 99.0))
}

/// `server.key_to_frame_p50_ms` and `server.key_to_frame_p99_ms`: 200 keys per run, `KEY_RUNS` runs, after 20 warm-up keys.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement: sh scripts/perf-check.sh server"]
async fn perf_server_key_to_frame() {
    let _one = exclusive();
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let (mut line, pane) = cat_pane(&probed, &alice).await;

    key_run(&mut line, pane, 20).await;
    let runs: Vec<(f64, f64)> = {
        let mut runs = Vec::new();
        for _ in 0..KEY_RUNS {
            runs.push(key_run(&mut line, pane, 200).await);
        }
        runs
    };
    let p50: Vec<f64> = runs.iter().map(|r| r.0).collect();
    let p99: Vec<f64> = runs.iter().map(|r| r.1).collect();
    report("server.key_to_frame_p50_ms", "ms", &p50);
    report("server.key_to_frame_p99_ms", "ms", &p99);
}
