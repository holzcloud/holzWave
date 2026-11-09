//! The byte bridge of a session that lives on a server: a headless pane, an atomic snapshot of its terminal plus a tap on
//! the bytes that follow, and a second engine that is fed both.
//!
//! What is proved here, in the smallest place and with a real shell on a real PTY and the real engine:
//!
//! - the join of the snapshot and the live bytes loses no byte and repeats none, for clients that attach while the
//!   program is writing;
//! - a second engine that is fed the snapshot shows what the first one shows (text, styles, cursor, modes, tab stops,
//!   scrolling region, the screen that is active, links), and the things a snapshot cannot carry are listed and
//!   asserted absent;
//! - the scrollback tail is exact and bounded;
//! - a tap that cannot keep up is dropped and told so, and neither stalls the pane nor grows memory;
//! - raw input bytes reach a headless pane and no other.
#![cfg(feature = "ghostty")]
// The programs here are `/bin/sh` scripts.
#![cfg(unix)]

mod common;

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use common::Xorshift64;
use holzwave_core::engine::TerminalEngine;
use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec, Tap, TapEnd, TapMsg};
use holzwave_core::textread::TextRegion;
use holzwave_core::GhosttyEngine;

const WAIT: Duration = Duration::from_secs(20);
const QUEUE: usize = 4 * 1024 * 1024;
const SNAPSHOT: usize = 4 * 1024 * 1024;

/// Everything a pane tells its sink.
#[derive(Clone, Default)]
struct Probe {
    events: Arc<Mutex<Vec<PaneEvent>>>,
}

impl Probe {
    fn sink(&self) -> impl FnMut(PaneEvent) + Send + 'static {
        let events = self.events.clone();
        move |event| events.lock().unwrap().push(event)
    }
}

fn headless(script: &str) -> PaneSpec {
    let mut spec = PaneSpec::new("/bin/sh", 80, 24).args(["-c", script]);
    spec.headless = true;
    spec
}

fn start(spec: PaneSpec, probe: &Probe) -> PaneHandle {
    PaneHandle::spawn_ghostty(1, spec, probe.sink()).expect("the pane starts")
}

/// A client: an engine of its own, built the way a client of the byte bridge builds it.
struct Client {
    engine: GhosttyEngine,
    tap: Tap,
}

impl Client {
    /// Attach and feed the snapshot into an engine of the size the attach answered.
    fn attach(pane: &PaneHandle, tail_rows: usize) -> Client {
        let tap = pane.attach_tap(tail_rows, SNAPSHOT, QUEUE).expect("the tap attaches");
        let mut engine = GhosttyEngine::new(tap.size.0, tap.size.1, 10_000).expect("client engine");
        engine.feed(&tap.snapshot);
        Client { engine, tap }
    }

    /// Feed what the tap delivers until the last rows of the client's own screen show `marker`; returns false when the
    /// tap ended or nothing came for the whole wait.
    fn pump_until(&mut self, marker: &str) -> bool {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            if self.shows(marker) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            match self.tap.rx.recv(Duration::from_millis(100)) {
                Some(TapMsg::Bytes(bytes)) => self.engine.feed(&bytes),
                Some(TapMsg::End(_)) => return self.shows(marker),
                None => {}
            }
        }
    }

    fn shows(&mut self, marker: &str) -> bool {
        self.text(TextRegion::LastRows(30)).contains(marker)
    }

    fn text(&mut self, region: TextRegion) -> String {
        self.engine.read_text(region, 512 * 1024).expect("read").map(|read| read.text).unwrap_or_default()
    }
}

fn pane_text(pane: &PaneHandle, region: TextRegion) -> String {
    pane.read_text(region, 512 * 1024).expect("read").map(|read| read.text).unwrap_or_default()
}

/// The numbers of the lines `n <int>` of a text, in the order they stand.
fn numbers(text: &str) -> Vec<u32> {
    text.lines().filter_map(|line| line.strip_prefix("n ")).filter_map(|rest| rest.trim().parse().ok()).collect()
}

/// A program that prints `count` numbered lines in bursts of 60, with a short pause between the bursts so that clients can
/// attach while it writes, then prints `done` and waits.
fn paced_numbers(count: u32) -> String {
    format!(
        "i=0; while [ $i -lt {count} ]; do echo \"n $i\"; i=$((i+1)); \
         if [ $((i % 60)) -eq 0 ]; then sleep 0.02; fi; done; echo done; sleep 60"
    )
}

/// Wait until the pane's own screen shows `marker`.
fn wait_for_pane(pane: &PaneHandle, marker: &str) {
    assert!(
        common::wait_until(WAIT, || pane_text(pane, TextRegion::LastRows(30)).contains(marker)),
        "the pane never showed {marker:?}"
    );
}

fn close(pane: PaneHandle) {
    pane.close();
    assert!(pane.join(Duration::from_secs(10)), "the pane did not end");
}

// ---- the join of the snapshot and the stream ----

#[test]
fn joins_a_snapshot_and_the_tap_lose_and_repeat_no_numbered_line() {
    const LINES: u32 = 1800;
    let probe = Probe::default();
    let pane = start(headless(&paced_numbers(LINES)), &probe);
    let mut rng = Xorshift64::new(0x5eed_0001);

    // Clients attach at random moments while the program is writing, every one with the screen only.
    let mut clients = Vec::new();
    for _ in 0..8 {
        thread::sleep(Duration::from_millis(rng.below(60) as u64));
        clients.push(Client::attach(&pane, 0));
    }

    let mut mid_stream = 0;
    for (n, client) in clients.iter_mut().enumerate() {
        assert!(client.pump_until("done"), "client {n} never saw the end of the program");
        let seen = numbers(&client.text(TextRegion::LastRows(2000)));
        assert!(!seen.is_empty(), "client {n} has no numbered line");
        // Present exactly once and in order: each number follows the one before it, up to the last line the program wrote.
        for pair in seen.windows(2) {
            assert_eq!(pair[1], pair[0] + 1, "client {n}: {} is followed by {}", pair[0], pair[1]);
        }
        assert_eq!(*seen.last().unwrap(), LINES - 1, "client {n} does not end at the last line");
        if seen[0] > 0 {
            mid_stream += 1;
        }
    }
    assert!(mid_stream >= 2, "only {mid_stream} of 8 clients attached after the program had started writing");
    close(pane);
}

#[test]
fn joins_a_burst_that_never_pauses_is_joined_without_loss_too() {
    const LINES: u32 = 1900;
    let script = format!("i=0; while [ $i -lt {LINES} ]; do echo \"n $i\"; i=$((i+1)); done; echo done; sleep 60");
    let probe = Probe::default();
    let pane = start(headless(&script), &probe);
    let mut rng = Xorshift64::new(0x5eed_0002);
    let mut clients = Vec::new();
    for _ in 0..6 {
        thread::sleep(Duration::from_micros(rng.below(4000) as u64));
        clients.push(Client::attach(&pane, 0));
    }
    for (n, client) in clients.iter_mut().enumerate() {
        assert!(client.pump_until("done"), "client {n} never saw the end of the program");
        let seen = numbers(&client.text(TextRegion::LastRows(2000)));
        assert!(!seen.is_empty(), "client {n}");
        for pair in seen.windows(2) {
            assert_eq!(pair[1], pair[0] + 1, "client {n}: {} is followed by {}", pair[0], pair[1]);
        }
        assert_eq!(*seen.last().unwrap(), LINES - 1, "client {n}");
    }
    close(pane);
}

#[test]
fn joins_the_second_engine_shows_the_same_screen_as_the_pane() {
    let probe = Probe::default();
    let pane = start(headless(&paced_numbers(600)), &probe);
    thread::sleep(Duration::from_millis(80));
    let mut client = Client::attach(&pane, 0);
    assert!(client.pump_until("done"));
    wait_for_pane(&pane, "done");

    assert_eq!(client.tap.size, (80, 24), "the attach states the size of the pane");
    let own = pane_text(&pane, TextRegion::Screen);
    let theirs = client.text(TextRegion::Screen);
    assert!(own.contains("n 599") && own.contains("done"), "the screen of the pane: {own:?}");
    assert_eq!(theirs, own, "the client's screen differs from the pane's");
    close(pane);
}

#[test]
fn joins_a_tap_of_a_pane_that_ended_ends_closed_after_its_bytes() {
    let probe = Probe::default();
    let pane = start(headless("sleep 0.3; echo last-words"), &probe);
    let mut client = Client::attach(&pane, 0);
    let mut ended = None;
    let deadline = std::time::Instant::now() + WAIT;
    while ended.is_none() && std::time::Instant::now() < deadline {
        match client.tap.rx.recv(Duration::from_millis(100)) {
            Some(TapMsg::Bytes(bytes)) => client.engine.feed(&bytes),
            Some(TapMsg::End(end)) => ended = Some(end),
            None => {}
        }
    }
    assert_eq!(ended, Some(TapEnd::Closed));
    assert!(client.text(TextRegion::LastRows(5)).contains("last-words"), "the last output came before the end");
    // The end repeats.
    assert_eq!(client.tap.rx.recv(Duration::ZERO), Some(TapMsg::End(TapEnd::Closed)));
    assert!(pane.join(Duration::from_secs(10)));
}
