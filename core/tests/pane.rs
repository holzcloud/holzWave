//! The pane actor, headless: a real shell on a real PTY, a real engine and a sink that records what the shell would
//! have received. No Tauri, no window (D-19).
//!
//! Every wait is bounded and every pane is closed at the end of its test (and by the handle's `Drop` when an
//! assertion fails first), so no child outlives the test binary.
#![cfg(feature = "ghostty")]

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use common::wait_until;
use holzwave_core::engine::{DomKey, KeyEvent, Mods, PasteOutcome, Screen, TerminalEngine, Update};
use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec};
use holzwave_core::queue::OUTPUT_LIMIT;
use holzwave_core::wire::{FrameMsg, RowMsg};

const WAIT: Duration = Duration::from_secs(5);

/// One event with the moment it reached the sink and the thread that ran the sink.
type Logged = (Instant, ThreadId, PaneEvent);

/// Collects everything a pane sends to its sink.
#[derive(Clone, Default)]
struct Probe {
    log: Arc<Mutex<Vec<Logged>>>,
}

impl Probe {
    fn sink(&self) -> impl FnMut(PaneEvent) + Send + 'static {
        let log = self.log.clone();
        move |event| log.lock().unwrap().push((Instant::now(), thread::current().id(), event))
    }

    fn logged(&self) -> Vec<Logged> {
        self.log.lock().unwrap().clone()
    }

    fn events(&self) -> Vec<PaneEvent> {
        self.logged().into_iter().map(|(_, _, e)| e).collect()
    }

    fn frames(&self) -> Vec<FrameMsg> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                PaneEvent::Frame(f) => Some(f),
                _ => None,
            })
            .collect()
    }

    /// Whether any frame carried a changed row with this text.
    fn has_text(&self, needle: &str) -> bool {
        self.frames().iter().any(|f| f.changed.iter().any(|r| row_text(r).contains(needle)))
    }

    fn exits(&self) -> Vec<Option<u32>> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                PaneEvent::Exited { code } => Some(code),
                _ => None,
            })
            .collect()
    }

    fn failures(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                PaneEvent::Failed(m) => Some(m),
                _ => None,
            })
            .collect()
    }
}

/// The text of a wire row: one code point per cell, the spacer marker left out.
fn row_text(row: &RowMsg) -> String {
    row.runs.iter().flat_map(|run| run.0.chars()).filter(|c| *c != '\0').collect()
}

/// The facts of a plain key press.
fn key(code: &str, key: &str) -> DomKey {
    DomKey { code: code.into(), key: key.into(), mods: Mods::NONE, ..DomKey::default() }
}

fn spec_sh(script: &str) -> PaneSpec {
    PaneSpec::new("/bin/sh", 80, 24).args(["-c", script])
}

fn spawn(spec: PaneSpec, probe: &Probe) -> PaneHandle {
    PaneHandle::spawn_ghostty(1, spec, probe.sink()).expect("the pane starts")
}

/// Whether a process with this id still exists. A zombie counts, which is why this proves the child was reaped.
fn process_exists(pid: u32) -> bool {
    Command::new("/bin/sh")
        .args(["-c", "kill -0 \"$1\" 2>/dev/null", "sh", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn the_output_of_a_real_shell_becomes_a_frame_event_with_its_text() {
    let probe = Probe::default();
    let pane = spawn(spec_sh("printf hello; sleep 3"), &probe);
    assert!(pane.pid().is_some(), "spawn reports the pid of the shell");

    assert!(
        wait_until(WAIT, || probe.has_text("hello")),
        "no frame with the text arrived: {:?}",
        probe.events()
    );
    pane.close();
    assert!(pane.join(Duration::from_secs(3)));
}

#[test]
fn a_key_becomes_echoed_output() {
    let probe = Probe::default();
    let pane = spawn(PaneSpec::new("/bin/cat", 80, 24), &probe);

    pane.key(key("KeyX", "x")).expect("the key is accepted");

    assert!(wait_until(WAIT, || probe.has_text("x")), "the tty echo never arrived: {:?}", probe.events());
    pane.close();
    assert!(pane.join(Duration::from_secs(3)));
}

#[test]
fn close_ends_a_sighup_ignoring_shell_with_exactly_one_exited_event() {
    let probe = Probe::default();
    let pane = spawn(spec_sh("trap '' HUP; echo ready; exec sleep 1000"), &probe);
    let pid = pane.pid().expect("pid");
    assert!(wait_until(WAIT, || probe.has_text("ready")), "the trap is not installed yet");
    assert!(process_exists(pid));

    pane.close();

    assert!(
        wait_until(Duration::from_secs(3), || !probe.exits().is_empty()),
        "no Exited within 3 seconds: {:?}",
        probe.events()
    );
    assert!(!process_exists(pid), "pid {pid} still exists after Exited: not ended or not reaped");
    // Nothing follows: not a second Exited, not another frame.
    let count = probe.events().len();
    thread::sleep(Duration::from_millis(400));
    assert_eq!(probe.exits().len(), 1, "exactly one Exited: {:?}", probe.events());
    assert_eq!(probe.events().len(), count, "an event arrived after Exited");
    assert!(pane.join(Duration::from_secs(3)));
}

#[test]
fn a_shell_that_exits_by_itself_reports_its_code_and_then_goes_silent() {
    let probe = Probe::default();
    let pane = spawn(spec_sh("exit 3"), &probe);

    assert!(wait_until(WAIT, || !probe.exits().is_empty()), "no Exited: {:?}", probe.events());
    assert_eq!(probe.exits(), vec![Some(3)]);
    let count = probe.events().len();
    thread::sleep(Duration::from_millis(400));
    assert_eq!(probe.events().len(), count, "an event arrived after Exited");
    assert!(pane.join(Duration::from_secs(3)));
}

#[test]
fn output_written_right_before_the_shell_exits_is_in_a_frame_that_precedes_exited() {
    // Running it a few times: the order is what is under test, and a race would show up as a missing frame.
    for round in 0..5 {
        let probe = Probe::default();
        let pane = spawn(spec_sh("printf done"), &probe);

        assert!(wait_until(WAIT, || !probe.exits().is_empty()), "round {round}: no Exited: {:?}", probe.events());
        let events = probe.events();
        let exited = events.iter().position(|e| matches!(e, PaneEvent::Exited { .. })).unwrap();
        let framed = events.iter().position(|e| match e {
            PaneEvent::Frame(f) => f.changed.iter().any(|r| row_text(r).contains("done")),
            _ => false,
        });
        match framed {
            Some(i) => assert!(i < exited, "round {round}: the frame came after Exited: {events:?}"),
            None => panic!("round {round}: the last output never reached a frame: {events:?}"),
        }
        assert!(pane.join(Duration::from_secs(3)));
    }
}

#[test]
fn a_failing_spawn_returns_an_error_and_reports_failed_exactly_once() {
    let probe = Probe::default();
    let result = PaneHandle::spawn_ghostty(
        1,
        PaneSpec::new("/nonexistent/holzwave-no-such-program", 80, 24),
        probe.sink(),
    );

    assert!(result.is_err(), "spawn must report the failure to its caller");
    // The sink has already seen it when spawn returns: the failure is reported before the error is.
    let failures = probe.failures();
    assert_eq!(failures.len(), 1, "exactly one Failed: {:?}", probe.events());
    assert!(!failures[0].is_empty());
    thread::sleep(Duration::from_millis(200));
    assert_eq!(probe.failures().len(), 1);
    assert!(probe.exits().is_empty(), "a pane that never started did not exit");
}

#[test]
fn the_sink_runs_on_the_engine_thread_and_the_handle_can_be_used_from_any_thread() {
    fn assert_clone_send_sync<T: Clone + Send + Sync + 'static>() {}
    assert_clone_send_sync::<PaneHandle>();

    let probe = Probe::default();
    let pane = spawn(PaneSpec::new("/bin/cat", 80, 24), &probe);
    let from_elsewhere = pane.clone();
    thread::spawn(move || from_elsewhere.key(key("KeyQ", "q")).expect("key from another thread"))
        .join()
        .unwrap();

    assert!(wait_until(WAIT, || probe.has_text("q")), "{:?}", probe.events());
    let threads: std::collections::HashSet<ThreadId> = probe.logged().iter().map(|(_, t, _)| *t).collect();
    assert_eq!(threads.len(), 1, "every event comes from one thread");
    assert!(!threads.contains(&thread::current().id()), "the sink must not run on the caller's thread");
    pane.close();
    assert!(pane.join(Duration::from_secs(3)));
}

#[test]
fn a_title_the_program_sets_arrives_as_a_title_event() {
    let probe = Probe::default();
    // OSC 2 terminated by BEL; the engine sanitises and queues it, the actor forwards it.
    let pane = spawn(spec_sh("printf '\\033]2;build done\\007'; sleep 3"), &probe);

    let titles = || {
        probe
            .events()
            .into_iter()
            .filter_map(|e| match e {
                PaneEvent::Title(t) => Some(t),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert!(wait_until(WAIT, || !titles().is_empty()), "no Title event: {:?}", probe.events());
    assert_eq!(titles(), vec!["build done".to_string()]);
    pane.close();
    assert!(pane.join(Duration::from_secs(3)));
}

#[test]
fn frames_are_paced_to_the_interval_and_none_are_sent_while_nothing_changes() {
    let probe = Probe::default();
    let interval = Duration::from_millis(60);
    let spec = PaneSpec {
        frame_interval: interval,
        ..spec_sh("i=0; while [ $i -lt 60 ]; do echo line $i; i=$((i+1)); sleep 0.01; done; sleep 5")
    };
    let pane = spawn(spec, &probe);
    assert!(wait_until(WAIT, || probe.has_text("line 59")), "the output never arrived: {:?}", probe.events());

    let times: Vec<Instant> = probe
        .logged()
        .into_iter()
        .filter(|(_, _, e)| matches!(e, PaneEvent::Frame(_)))
        .map(|(t, _, _)| t)
        .collect();
    assert!(times.len() >= 3, "too few frames to say anything about pacing: {}", times.len());
    for pair in times.windows(2) {
        let gap = pair[1].duration_since(pair[0]);
        assert!(gap >= interval, "two frames {gap:?} apart, the interval is {interval:?}");
    }

    // The shell is now asleep: nothing changes and nothing is sent.
    thread::sleep(Duration::from_millis(300));
    let settled = probe.frames().len();
    thread::sleep(Duration::from_millis(700));
    assert_eq!(probe.frames().len(), settled, "frames were sent while nothing changed");
    pane.close();
    assert!(pane.join(Duration::from_secs(3)));
}

// ---- replies, paste, backpressure and the no-deadlock property (task 2) ---------------------------------------

/// The size of one read of the reader thread: the queue never holds more than its limit plus one chunk.
const CHUNK: usize = 64 * 1024;

/// The time a slow consumer needs for `CHUNK` bytes: 16 MB/s.
const SLOW_CONSUMER: Duration = Duration::from_millis(4);

impl Probe {
    /// The sequence number of the last frame, 0 when there was none.
    fn last_seq(&self) -> u64 {
        self.frames().last().map_or(0, |f| f.seq)
    }

    fn exited(&self) -> bool {
        !self.exits().is_empty()
    }
}

fn at_bottom(frame: &FrameMsg) -> bool {
    let (offset, total, len) = frame.scrollbar;
    offset + len >= total
}

fn shift(code: &str, key: &str) -> DomKey {
    DomKey { code: code.into(), key: key.into(), mods: Mods::SHIFT, ..DomKey::default() }
}

/// What a [`CountingEngine`] saw, shared with the test that built it.
#[derive(Default)]
struct Spy {
    fed: AtomicUsize,
    /// Bit `n` is set once the capital letter `A + n` was fed.
    upper: AtomicU32,
    resizes: Mutex<Vec<(u16, u16)>>,
}

impl Spy {
    /// Whether this capital letter was fed at some point.
    fn saw(&self, letter: u8) -> bool {
        assert!(letter.is_ascii_uppercase());
        self.upper.load(Ordering::Relaxed) & (1 << (letter - b'A')) != 0
    }
}

/// A terminal engine that does nothing but count what it is fed.
///
/// The volume tests need it: in a debug build the libghostty library itself is built without optimisation and parses
/// about 100 KB/s, so a 100 MiB flood would take a quarter of an hour. What these tests prove is the actor's
/// behaviour (the queue bound, input priority, no deadlock) against a real child on a real PTY; which engine sits
/// behind the trait does not matter for that, and the actor is meant to be testable with any `TerminalEngine`.
struct CountingEngine {
    spy: Arc<Spy>,
    size: (u16, u16),
    /// How long feeding `CHUNK` bytes takes: a consumer with a fixed throughput that the producer outruns.
    feed_delay: Duration,
    replies: Vec<u8>,
    reset: bool,
}

impl CountingEngine {
    fn new(spy: Arc<Spy>, feed_delay: Duration) -> Self {
        Self { spy, size: (80, 24), feed_delay, replies: Vec::new(), reset: true }
    }
}

impl TerminalEngine for CountingEngine {
    fn feed(&mut self, bytes: &[u8]) {
        self.spy.fed.fetch_add(bytes.len(), Ordering::Relaxed);
        let mut seen = 0u32;
        for b in bytes.iter().filter(|b| b.is_ascii_uppercase()) {
            seen |= 1 << (b - b'A');
        }
        self.spy.upper.fetch_or(seen, Ordering::Relaxed);
        if !self.feed_delay.is_zero() {
            thread::sleep(self.feed_delay.mul_f64(bytes.len() as f64 / CHUNK as f64));
        }
    }

    fn resize(&mut self, cols: u16, rows: u16, _cell_px: (u32, u32)) -> holzwave_core::Result<()> {
        self.spy.resizes.lock().unwrap().push((cols, rows));
        self.size = (cols, rows);
        self.reset = true;
        Ok(())
    }

    fn size(&self) -> holzwave_core::Result<(u16, u16)> {
        Ok(self.size)
    }

    fn scroll(&mut self, _rows: isize) {}

    fn scroll_to_bottom(&mut self) {}

    fn screen(&mut self) -> holzwave_core::Result<Screen> {
        Ok(Screen::default())
    }

    fn set_option_as_alt(&mut self, _on: bool) {}

    fn encode_key(&mut self, event: &KeyEvent) -> holzwave_core::Result<Vec<u8>> {
        if event.code == "Enter" {
            return Ok(b"\r".to_vec());
        }
        Ok(event.text.clone().unwrap_or_default().into_bytes())
    }

    fn encode_text(&mut self, text: &str) -> holzwave_core::Result<Vec<u8>> {
        Ok(text.as_bytes().to_vec())
    }

    fn paste(&mut self, text: &str, _allow_unsafe: bool) -> holzwave_core::Result<PasteOutcome> {
        // Like the real engine: the pasted bytes come out through `take_replies`.
        self.replies.extend_from_slice(text.replace('\n', "\r").as_bytes());
        Ok(PasteOutcome::Written)
    }

    fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    fn update(&mut self) -> holzwave_core::Result<Update> {
        let reset = std::mem::take(&mut self.reset);
        Ok(Update { full: reset, reset, cols: self.size.0, rows: self.size.1, ..Update::default() })
    }

    fn request_full(&mut self) {
        self.reset = true;
    }
}

fn spawn_counting(spec: PaneSpec, probe: &Probe, feed_delay: Duration) -> (PaneHandle, Arc<Spy>) {
    let spy = Arc::new(Spy::default());
    let engine_spy = spy.clone();
    let pane = PaneHandle::spawn(1, spec, move |_| Ok(CountingEngine::new(engine_spy, feed_delay)), probe.sink())
        .expect("the pane starts");
    (pane, spy)
}

#[test]
fn query_replies_travel_from_the_engine_to_the_child_without_the_test_touching_them() {
    let probe = Probe::default();
    // Echo is off, so the only thing `cat -v` can show is what the engine wrote back: the answer to the cursor
    // position query, with ESC made visible as ^[. "ready" comes after the query in the output, so once it is on the
    // screen the reply is already queued ahead of anything typed afterwards.
    let pane = spawn(spec_sh("stty -echo; printf '\\033[6n'; printf ready; exec cat -v"), &probe);
    assert!(wait_until(WAIT, || probe.has_text("ready")), "the child never got going");

    // The answer waits in the tty until the line ends.
    pane.key(key("Enter", "Enter")).expect("enter");

    assert!(wait_until(WAIT, || probe.has_text("^[[1;1R")), "the reply never reached the child");
    pane.close();
    assert!(pane.join(Duration::from_secs(3)));
}

#[test]
fn a_paste_with_a_newline_is_rejected_without_bracketed_mode_and_writes_nothing() {
    let probe = Probe::default();
    let pane = spawn(PaneSpec::new("/bin/cat", 80, 24), &probe);

    let outcome = pane.paste("first\nsecond".into(), false).expect("the paste is answered");
    assert_eq!(outcome, PasteOutcome::Rejected);
    thread::sleep(Duration::from_millis(400));
    assert!(!probe.has_text("first"), "a rejected paste wrote bytes: {:?}", probe.events().len());

    let outcome = pane.paste("first\nsecond".into(), true).expect("the paste is answered");
    assert_eq!(outcome, PasteOutcome::Written);
    assert!(wait_until(WAIT, || probe.has_text("first") && probe.has_text("second")), "cat did not echo both lines");
    pane.close();
    assert!(pane.join(Duration::from_secs(3)));
}

#[test]
fn pasted_text_is_not_replayed_into_the_replies_of_a_later_query() {
    let probe = Probe::default();
    // Echo off. The child reads one line, then asks for the cursor position, then shows what arrives. If the paste
    // bytes stayed in the engine's reply buffer, the answer to the query would carry them: `cat -v` would print
    // "go" again.
    let pane = spawn(
        spec_sh("stty -echo; printf ready; read -r go; printf '\\033[6n'; printf next; exec cat -v"),
        &probe,
    );
    assert!(wait_until(WAIT, || probe.has_text("ready")), "the child is not ready");

    assert_eq!(pane.paste("go\n".into(), true).expect("paste"), PasteOutcome::Written);
    // "next" follows the query in the child's output, so once it is shown the answer is queued ahead of the Enter.
    assert!(wait_until(WAIT, || probe.has_text("next")), "the child did not read the paste");
    pane.key(key("Enter", "Enter")).expect("enter");

    // The cursor was after "ready", column 6 of row 1, when the query came.
    assert!(wait_until(WAIT, || probe.has_text("^[[1;6R")), "the answer never arrived");
    assert!(!probe.has_text("go"), "the paste came back as a reply");
    pane.close();
    assert!(pane.join(Duration::from_secs(3)));
}

#[test]
fn a_paste_over_sixteen_mebibytes_is_refused_at_the_handle() {
    let probe = Probe::default();
    let pane = spawn(PaneSpec::new("/bin/cat", 80, 24), &probe);

    let result = pane.paste("a".repeat((16 << 20) + 1), true);

    let error = result.expect_err("too large a paste must be an error and not reach the engine");
    assert!(error.to_string().contains("too large"), "{error}");
    pane.close();
    assert!(pane.join(Duration::from_secs(3)));
}

#[test]
fn a_100_mib_flood_keeps_the_queue_within_its_bound_and_ends_the_pane_cleanly() {
    let probe = Probe::default();
    // The consumer manages 16 MB/s, the child produces much faster, so the queue has to push back.
    let (pane, spy) = spawn_counting(
        spec_sh("head -c 104857600 /dev/zero | tr '\\0' a"),
        &probe,
        SLOW_CONSUMER,
    );

    assert!(wait_until(Duration::from_secs(120), || probe.exited()), "the flood did not finish");

    let high = pane.queue_high_water();
    assert!(high <= OUTPUT_LIMIT + CHUNK, "{high} queued bytes at once, the bound is {}", OUTPUT_LIMIT + CHUNK);
    assert!(high >= OUTPUT_LIMIT / 2, "the queue never filled ({high}): the backpressure was not exercised");
    assert_eq!(spy.fed.load(Ordering::Relaxed), 104_857_600, "bytes were lost on the way");
    assert_eq!(probe.exits(), vec![Some(0)]);
    assert!(pane.join(Duration::from_secs(3)));
}

#[test]
fn input_resize_and_close_get_through_a_flood() {
    let probe = Probe::default();
    // Endless output from a background job, so the flood is certainly still running whenever the pane is asked for
    // something, while the shell itself waits for one line of input and then reports it.
    let (pane, spy) = spawn_counting(
        spec_sh("stty -echo; yes aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa & read -r line; printf 'GOT-%s' \"$line\"; sleep 30"),
        &probe,
        SLOW_CONSUMER,
    );
    assert!(wait_until(WAIT, || spy.fed.load(Ordering::Relaxed) > 4 * OUTPUT_LIMIT), "the flood never got going");

    pane.resize(100, 30, (0, 0)).expect("resize");
    assert!(
        wait_until(Duration::from_secs(2), || probe.frames().iter().any(|f| (f.cols, f.rows) == (100, 30))),
        "the resize starved behind the flood"
    );

    let before = probe.last_seq();
    pane.request_full().expect("request_full");
    assert!(
        wait_until(Duration::from_secs(2), || probe.frames().iter().any(|f| f.seq > before && f.full)),
        "the full-frame request starved behind the flood"
    );

    // The line reaches the shell through the flood: it comes back as "GOT-Z" in the output.
    pane.key(shift("KeyZ", "Z")).expect("key");
    pane.key(key("Enter", "Enter")).expect("enter");
    assert!(wait_until(Duration::from_secs(2), || spy.saw(b'Z')), "a key starved behind the flood");
    let high = pane.queue_high_water();
    assert!(high <= OUTPUT_LIMIT + CHUNK, "{high} queued bytes at once");
    assert!(high >= OUTPUT_LIMIT / 2, "the queue never filled ({high})");

    let started = Instant::now();
    pane.close();
    assert!(wait_until(Duration::from_secs(3), || probe.exited()), "close starved behind the flood");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(probe.exits().len(), 1);
    assert!(pane.join(Duration::from_secs(3)));
}

#[test]
fn a_4_mib_paste_into_cat_does_not_deadlock() {
    let probe = Probe::default();
    // Echo is off: the tty would otherwise echo every pasted line on top of what cat copies, and a terminal drops
    // echo when its output queue is full, which would make the end of this test depend on that.
    let (pane, spy) = spawn_counting(spec_sh("stty -echo; exec cat"), &probe, SLOW_CONSUMER);

    // 65 536 lines of 63 characters and a newline: short lines, because a terminal in canonical mode cuts long ones.
    // Lower case and digits only, so the capital Q typed afterwards can only come from the key.
    let line = format!("{}\n", "abcdefghijklmnopqrstuvwxyz0123456789".repeat(2).get(..63).unwrap());
    let text = line.repeat(65_536);
    assert_eq!(text.len(), 4 << 20);
    assert_eq!(pane.paste(text, true).expect("paste"), PasteOutcome::Written);

    // The key and its Enter queue up behind the paste; cat copies the line once the paste is through.
    pane.key(shift("KeyQ", "Q")).expect("key");
    pane.key(key("Enter", "Enter")).expect("enter");
    assert!(
        wait_until(Duration::from_secs(20), || spy.saw(b'Q')),
        "a key after the paste never echoed: the pane is stuck ({} bytes fed)",
        spy.fed.load(Ordering::Relaxed)
    );
    let high = pane.queue_high_water();
    assert!(high <= OUTPUT_LIMIT + CHUNK, "{high} queued bytes at once");
    pane.close();
    assert!(pane.join(Duration::from_secs(5)));
}

#[test]
fn typing_while_scrolled_back_returns_the_viewport_to_the_bottom() {
    let probe = Probe::default();
    let pane = spawn(
        spec_sh("i=0; while [ $i -lt 100 ]; do echo line $i; i=$((i+1)); done; exec cat"),
        &probe,
    );
    assert!(wait_until(WAIT, || probe.has_text("line 99")), "the output never arrived");
    assert!(wait_until(WAIT, || probe.frames().last().is_some_and(at_bottom)));

    pane.scroll(-30).expect("scroll");
    assert!(
        wait_until(WAIT, || probe.frames().last().is_some_and(|f| !at_bottom(f))),
        "scrolling back did not move the viewport"
    );

    let before = probe.last_seq();
    pane.key(key("KeyA", "a")).expect("key");
    assert!(
        wait_until(WAIT, || probe.frames().iter().any(|f| f.seq > before && at_bottom(f))),
        "typing did not return to the live screen"
    );
    pane.close();
    assert!(pane.join(Duration::from_secs(3)));
}

#[test]
fn a_nonsense_size_is_clamped_before_it_reaches_the_engine() {
    let probe = Probe::default();
    let (pane, spy) = spawn_counting(PaneSpec::new("/bin/cat", 80, 24), &probe, Duration::ZERO);

    pane.resize(0, 0, (0, 0)).expect("resize");
    pane.resize(u16::MAX, u16::MAX, (u32::MAX, u32::MAX)).expect("resize");

    assert!(wait_until(WAIT, || spy.resizes.lock().unwrap().len() == 2), "the resizes were not applied");
    assert_eq!(*spy.resizes.lock().unwrap(), vec![(2, 1), (1000, 1000)]);
    pane.close();
    assert!(pane.join(Duration::from_secs(3)));
}
