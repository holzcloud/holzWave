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

use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use parity_env::{Line, Probed, Screen, fresh, split_and_start, start_focused_pane};
use perf_report::{percentile, report, rss_kib};
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

// ---- reattach -----------------------------------------------------------------------------------------------------------

const REATTACH_RUNS: usize = 10;

/// `server.reattach_ms`: one pane keeps running (`cat`, with a screenful of text above it) while the sockets of its person come
/// and go. The first socket is dropped without a close frame, as `reattach.rs` does. Each run is the time from the call that
/// attaches a new socket of the same person (the TLS connection and the WebSocket upgrade) to the first full frame of that pane
/// on it. What a page does after a reconnect is sent at once, as the page does it: the layout, then `request_full` for the pane.
/// The sign-in of the person (a cookie made by the test application) is made before the clock starts.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement: sh scripts/perf-check.sh server"]
async fn perf_server_reattach() {
    let _one = exclusive();
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut line = fresh(&probed, &alice).await;
    let pane = start_focused_pane(&mut line).await;
    // Something on the screen, so that a full frame has rows to carry.
    send_text(&mut line, pane, "seq 1 150\r").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    become_cat(&mut line, pane).await;

    let mut times = Vec::with_capacity(REATTACH_RUNS);
    for run in 0..=REATTACH_RUNS {
        drop(line);
        let key = alice.key;
        probed.env.eventually("the socket to detach", |env| env.supervisor.account_stats(key).sockets == 0).await;
        let signed = probed.env.sign_in_again(&alice);
        let started = Instant::now();
        let mut next = Line::new(probed.env.connect_as(&signed).await);
        next.send("get_layout", &json!({})).await;
        next.send("request_full", &json!({"pane": pane})).await;
        let at = loop {
            let frame = next_frame(&mut next, pane, BOUND).await.expect("no full frame of the pane after the new socket attached");
            if frame.payload["full"].as_bool() == Some(true) {
                break frame.at;
            }
        };
        // The first attach warms the paths of a new socket; it is not one of the runs.
        if run > 0 {
            times.push(at.duration_since(started).as_secs_f64() * 1000.0);
        }
        line = next;
        settle(&mut line, Duration::from_millis(100)).await;
    }
    report("server.reattach_ms", "ms", &times);
}

// ---- memory per pane ----------------------------------------------------------------------------------------------------

/// The panes of a memory run (the worker's default limit is 64).
const PANES: usize = 50;
const MEMORY_RUNS: usize = 5;
/// Set in the environment of the child process that takes one memory figure.
const MEMORY_CHILD: &str = "HOLZWAVE_PERF_SERVER_MEMORY_CHILD";
/// The marker line a pane prints when its flood is done; the typed command holds it split by quotes, so only the output is
/// a line of exactly these characters.
const MARKER: &str = "hw-mark";

/// The text of a row of a frame: its runs joined, the NULs of wide characters and the blanks at the end cut.
fn row_text(row: &Value) -> String {
    let mut text = String::new();
    for run in row["runs"].as_array().map(Vec::as_slice).unwrap_or_default() {
        text.push_str(run[0].as_str().unwrap_or_default());
    }
    text.replace('\0', "").trim_end().to_owned()
}

/// Reads the next `frame` event of any pane that holds `needle` in its text (`""` takes every frame): the payload, or `None`
/// when nothing came within `within`. The cheap test on the text comes before the parse, so a flood costs the reader little.
async fn next_frame_with(line: &mut Line, needle: &str, within: Duration) -> Option<Value> {
    let deadline = Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        match line.client.read(left).await {
            Got::Message(Incoming::Text(text)) => {
                if !text.starts_with("{\"event\":\"frame\"") || !text.contains(needle) {
                    continue;
                }
                let mut value: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("a frame that is not JSON ({e})"));
                return Some(value["payload"].take());
            }
            Got::Message(Incoming::Close(code, _)) => panic!("the server closed the socket during the memory run: {code:?}"),
            Got::Message(_) => {}
            Got::Ended => panic!("the socket ended during the memory run"),
            Got::Quiet => return None,
        }
    }
}

/// Reads frames until every pane in `wanted` has a row that is exactly `needle` (a pane is struck from the list when it has).
async fn until_every_pane_shows(line: &mut Line, wanted: &mut Vec<u64>, needle: &str, within: Duration) {
    let deadline = Instant::now() + within;
    while !wanted.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        let frame = next_frame_with(line, needle, left).await;
        let Some(payload) = frame else { panic!("{} panes never showed {needle:?}", wanted.len()) };
        let shown = payload["changed"].as_array().map(Vec::as_slice).unwrap_or_default().iter().any(|row| row_text(row) == needle);
        if let (true, Some(pane)) = (shown, payload["pane"].as_u64()) {
            wanted.retain(|p| *p != pane);
        }
    }
}

/// Whether a frame payload has drawn a row that is not blank (the prompt of a shell).
fn drew_something(payload: &Value) -> bool {
    payload["changed"].as_array().map(Vec::as_slice).unwrap_or_default().iter().any(|row| !row_text(row).is_empty())
}

/// Strikes the panes from `wanted` that a frame among `events` shows drawn, and drops the events: the commands of the split and
/// of the start keep the frames that came meanwhile, and a prompt that came then must count.
fn take_drawn(line: &mut Line, wanted: &mut Vec<u64>) {
    for event in std::mem::take(&mut line.events) {
        if event["event"] == "frame" && drew_something(&event["payload"]) {
            if let Some(pane) = event["payload"]["pane"].as_u64() {
                wanted.retain(|p| *p != pane);
            }
        }
    }
    line.strays.clear();
}

/// Reads frames until every pane in `wanted` has drawn a row that is not blank (the prompt of its shell).
async fn until_every_pane_has_drawn(line: &mut Line, wanted: &mut Vec<u64>, within: Duration) {
    let deadline = Instant::now() + within;
    take_drawn(line, wanted);
    while !wanted.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        let frame = next_frame_with(line, "", left).await;
        let Some(payload) = frame else { panic!("{} panes never drew a prompt", wanted.len()) };
        if let (true, Some(pane)) = (drew_something(&payload), payload["pane"].as_u64()) {
            wanted.retain(|p| *p != pane);
        }
    }
}

/// The figure of one run, taken in a process of its own so that nothing an earlier test left resident is in the delta (a process
/// does not give freed memory back at once). It prints `RAWMEM <empty> <scrollback>`: KiB per pane, the delta of the resident
/// set of the whole process divided by the panes.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "run by perf_server_memory, in a process of its own"]
async fn perf_server_memory_child() {
    if std::env::var_os(MEMORY_CHILD).is_none() {
        return;
    }
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut line = fresh(&probed, &alice).await;
    line.events.clear();

    let before_empty = rss_kib().expect("resident memory is not readable on this OS");
    // The panes are made with the calls a browser makes: the first pane of the layout, then splits of the focused one.
    let mut panes = vec![start_focused_pane(&mut line).await];
    let mut drawing: Vec<u64> = panes.clone();
    for _ in 1..PANES {
        let pane = split_and_start(&mut line).await;
        panes.push(pane);
        drawing.push(pane);
        // The answers keep the frames that came meanwhile; a prompt among them counts, then they go (they are not measured).
        take_drawn(&mut line, &mut drawing);
    }
    until_every_pane_has_drawn(&mut line, &mut drawing, BOUND).await;
    settle(&mut line, Duration::from_millis(500)).await;
    let before_feed = rss_kib().expect("resident memory");

    // 30 000 lines of 150 characters on every pane: the default scrollback holds 10 000 lines, 20 000 are to spare. The marker
    // after the flood tells that the pane has printed it all (a printed line of its own, split by quotes in the command).
    let alphabet: String = (0..150).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
    let flood = format!("yes {alphabet} | head -n 30000; echo hw-m\"\"ark\r");
    for pane in &panes {
        send_text(&mut line, *pane, &flood).await;
    }
    let mut flooding = panes.clone();
    until_every_pane_shows(&mut line, &mut flooding, MARKER, Duration::from_secs(300)).await;
    settle(&mut line, Duration::from_millis(500)).await;
    let after_feed = rss_kib().expect("resident memory");
    let per = |from: u64, to: u64| to.saturating_sub(from) / PANES as u64;
    println!("\nRAWMEM {} {}", per(before_empty, before_feed), per(before_feed, after_feed));
}

/// `server.memory_pane_empty_mib` and `server.memory_pane_scrollback_mib`: 50 panes in one server, the resident set of the whole
/// process (the server, its in-process workers and the test's own socket) before any pane, after the 50 have drawn, and after
/// each has printed 30 000 lines; the deltas divided by 50. Five runs, each in a fresh process (`perf_server_memory_child`).
#[test]
#[ignore = "a measurement: sh scripts/perf-check.sh server"]
fn perf_server_memory() {
    let _one = exclusive();
    let exe = std::env::current_exe().expect("the path of the test binary");
    let mut empty = Vec::new();
    let mut full = Vec::new();
    for _ in 0..MEMORY_RUNS {
        let out = Command::new(&exe)
            .args(["perf_server_memory_child", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
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
            .map(|n| n.parse::<f64>().expect("a number") / 1024.0)
            .collect();
        assert_eq!(figures.len(), 2, "RAWMEM has two figures");
        empty.push(figures[0]);
        full.push(figures[1]);
    }
    report("server.memory_pane_empty_mib", "mib", &empty);
    report("server.memory_pane_scrollback_mib", "mib", &full);
}
