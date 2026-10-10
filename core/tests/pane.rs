//! The pane actor, headless: a real shell on a real PTY, a real engine and a sink that records what the shell would
//! have received. No Tauri, no window (D-19).
//!
//! Every wait is bounded and every pane is closed at the end of its test (and by the handle's `Drop` when an
//! assertion fails first), so no child outlives the test binary.
#![cfg(feature = "ghostty")]

mod common;

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use common::wait_until;
use holzwave_core::engine::{DomKey, Mods};
use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec};
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
