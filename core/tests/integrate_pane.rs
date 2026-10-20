//! A tool in a pane: the pane stays on screen with its last words when the tool fails, the child's environment loses
//! the names a launch removes, and a launch describes a pane without a socket token.
//!
//! The tools are fake programs from the rig of the runner tests (POSIX shell scripts in a scratch directory), run in a
//! real PTY with the real engine. Every wait is bounded and every pane is closed at the end of its test.
#![cfg(feature = "ghostty")]
#![cfg(unix)]

mod common;
#[path = "common/fake_cli.rs"]
mod fake_cli;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use common::{scratch_dir, wait_until};
use fake_cli::FakeCli;
use holzwave_core::engine::{DomKey, Mods};
use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec};
use holzwave_core::wire::{FrameMsg, RowMsg};

const WAIT: Duration = Duration::from_secs(10);

/// Collects everything a pane sends to its sink.
#[derive(Clone, Default)]
struct Probe {
    log: Arc<Mutex<Vec<PaneEvent>>>,
}

impl Probe {
    fn sink(&self) -> impl FnMut(PaneEvent) + Send + 'static {
        let log = self.log.clone();
        move |event| log.lock().unwrap().push(event)
    }

    fn events(&self) -> Vec<PaneEvent> {
        self.log.lock().unwrap().clone()
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

    fn helds(&self) -> Vec<Option<u32>> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                PaneEvent::Held { code } => Some(code),
                _ => None,
            })
            .collect()
    }
}

/// The text of a wire row: one code point per cell, the spacer marker left out.
fn row_text(row: &RowMsg) -> String {
    row.runs.iter().flat_map(|run| run.0.chars()).filter(|c| *c != '\0').collect()
}

/// A scratch directory that is removed when the test ends, even when it fails.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        Scratch(scratch_dir("integrate_pane"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dom(code: &str, key: &str, mods: Mods, release: bool) -> DomKey {
    DomKey { code: code.into(), key: key.into(), mods, release, ..DomKey::default() }
}

fn enter() -> DomKey {
    dom("Enter", "Enter", Mods::NONE, false)
}

fn spawn(spec: PaneSpec, probe: &Probe) -> PaneHandle {
    PaneHandle::spawn_ghostty(1, spec, probe.sink()).expect("the pane starts")
}

fn tool_spec(fake: &FakeCli, linger: Option<Duration>) -> PaneSpec {
    PaneSpec { linger, ..PaneSpec::new(fake.path().to_string_lossy().into_owned(), 100, 24) }
}

const NOTFOUND: &str = "Error from server (NotFound): pods \"x\" not found\n";

#[test]
fn a_failing_tool_stays_on_screen_until_enter() {
    let dir = Scratch::new();
    let fake = FakeCli::new(&dir.0, "kubectl").prints(NOTFOUND).exits(1);
    let probe = Probe::default();
    let pane = spawn(tool_spec(&fake, Some(Duration::from_millis(300))), &probe);

    assert!(wait_until(WAIT, || !probe.helds().is_empty()), "no Held event: {:?}", probe.events());
    assert!(probe.has_text("Error from server (NotFound): pods \"x\" not found"), "the tool's line is on screen");
    assert!(
        wait_until(WAIT, || probe.has_text("[holzWave] exited with status 1. Press Enter to close.")),
        "the notice line is on screen"
    );
    assert_eq!(probe.helds(), [Some(1)], "one Held with the status");
    assert!(probe.exits().is_empty(), "no Exited yet");

    // Nothing is closed by waiting, by a letter, by a modified Enter or by the release of Enter.
    thread::sleep(Duration::from_secs(1));
    for key in [
        dom("KeyA", "a", Mods::NONE, false),
        dom("Enter", "Enter", Mods::SHIFT, false),
        dom("Enter", "Enter", Mods::CTRL, false),
        dom("Enter", "Enter", Mods::NONE, true),
        dom("Escape", "Escape", Mods::NONE, false),
    ] {
        let _ = pane.key(key);
    }
    thread::sleep(Duration::from_millis(500));
    assert!(probe.exits().is_empty(), "the pane ended without Enter: {:?}", probe.events());
    assert_eq!(probe.helds(), [Some(1)], "still one Held");

    pane.key(enter()).expect("the held pane takes a key");
    assert!(wait_until(WAIT, || !probe.exits().is_empty()), "Enter did not end the pane: {:?}", probe.events());
    assert!(pane.join(Duration::from_secs(5)));
    thread::sleep(Duration::from_millis(300));
    assert_eq!(probe.exits(), [Some(1)], "exactly one Exited, with the tool's status");
    assert!(matches!(probe.events().last(), Some(PaneEvent::Exited { code: Some(1) })), "nothing after Exited");
}

#[test]
fn a_quick_success_is_held_and_a_slow_success_closes_at_once() {
    let dir = Scratch::new();

    // Within the linger time even a clean exit is held: a tool that did nothing and left would vanish unread.
    let quick = FakeCli::new(&dir.0, "quick").prints("done\n").exits(0);
    let probe = Probe::default();
    let pane = spawn(tool_spec(&quick, Some(Duration::from_secs(30))), &probe);
    assert!(wait_until(WAIT, || !probe.helds().is_empty()), "a quick success is held: {:?}", probe.events());
    assert_eq!(probe.helds(), [Some(0)]);
    assert!(wait_until(WAIT, || probe.has_text("[holzWave] exited with status 0. Press Enter to close.")));
    thread::sleep(Duration::from_millis(300));
    assert!(probe.exits().is_empty());
    pane.key(enter()).unwrap();
    assert!(wait_until(WAIT, || !probe.exits().is_empty()));
    assert_eq!(probe.exits(), [Some(0)]);
    assert!(pane.join(Duration::from_secs(5)));

    // A tool that ran longer than the linger time and exits 0 closes at once, as a shell does.
    let slow = FakeCli::new(&dir.0, "slow").prints("done\n").sleeps(1).exits(0);
    let probe = Probe::default();
    let pane = spawn(tool_spec(&slow, Some(Duration::from_millis(300))), &probe);
    assert!(wait_until(WAIT, || !probe.exits().is_empty()), "a slow success closes at once: {:?}", probe.events());
    assert_eq!(probe.exits(), [Some(0)]);
    assert!(probe.helds().is_empty(), "no Held for a slow success: {:?}", probe.events());
    assert!(pane.join(Duration::from_secs(5)));
}

#[test]
fn closing_a_held_pane_from_outside_ends_it_once() {
    let dir = Scratch::new();
    let fake = FakeCli::new(&dir.0, "talosctl").prints("rpc error: code = Unavailable\n").exits(2);
    let probe = Probe::default();
    let pane = spawn(tool_spec(&fake, Some(Duration::from_secs(30))), &probe);

    assert!(wait_until(WAIT, || !probe.helds().is_empty()), "{:?}", probe.events());
    assert!(probe.exits().is_empty());
    pane.close();
    pane.close();
    assert!(wait_until(WAIT, || !probe.exits().is_empty()), "closing did not end the held pane: {:?}", probe.events());
    assert!(pane.join(Duration::from_secs(5)));
    thread::sleep(Duration::from_millis(300));
    assert_eq!(probe.exits(), [Some(2)], "one Exited with the status the tool had");
    assert!(matches!(probe.events().last(), Some(PaneEvent::Exited { .. })));
}

#[test]
fn without_linger_a_pane_ends_as_before() {
    let dir = Scratch::new();
    let fake = FakeCli::new(&dir.0, "az").prints("ERROR: Please run 'az login'\n").exits(1);
    let probe = Probe::default();
    let started = Instant::now();
    let pane = spawn(tool_spec(&fake, None), &probe);

    assert!(wait_until(WAIT, || !probe.exits().is_empty()), "{:?}", probe.events());
    assert_eq!(probe.exits(), [Some(1)]);
    assert!(probe.helds().is_empty(), "no Held without linger: {:?}", probe.events());
    assert!(!probe.has_text("Press Enter"), "no notice without linger");
    assert!(pane.join(Duration::from_secs(5)));
    assert!(started.elapsed() < WAIT);
}

#[test]
fn a_held_wait_after_the_end_of_the_output_waits_for_a_control_message_and_does_not_spin() {
    use holzwave_core::queue::{PaneQueue, Popped};

    let queue = PaneQueue::<u32>::new();
    assert!(queue.push_output(b"last words".to_vec()));
    queue.finish();
    // The ordinary pop reports the end again and again; the held one delivers what is queued, then waits.
    assert_eq!(queue.pop_held(Duration::from_millis(50)), Popped::Output(b"last words".to_vec()));
    assert_eq!(queue.pop(Duration::from_millis(50)), Popped::Finished);
    let started = Instant::now();
    assert_eq!(queue.pop_held(Duration::from_millis(120)), Popped::Timeout);
    assert!(started.elapsed() >= Duration::from_millis(100), "returned at once instead of waiting");

    let queue = Arc::new(queue);
    let waiter = {
        let queue = queue.clone();
        thread::spawn(move || queue.pop_held(Duration::from_secs(10)))
    };
    thread::sleep(Duration::from_millis(80));
    assert!(queue.push_control(7), "control is still accepted after the end of the output");
    assert_eq!(waiter.join().unwrap(), Popped::Control(7));
    queue.close();
    assert_eq!(queue.pop_held(Duration::from_secs(1)), Popped::Closed);
}

/// Run `fake` in a pane with this environment handling and return the environment the fake recorded.
fn run_and_record(
    fake: &FakeCli,
    env: &[(&str, &str)],
    env_remove: &[&str],
) -> std::collections::BTreeMap<String, String> {
    let probe = Probe::default();
    let spec = PaneSpec {
        env: env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
        env_remove: env_remove.iter().map(|name| (*name).to_owned()).collect(),
        ..tool_spec(fake, None)
    };
    let pane = spawn(spec, &probe);
    assert!(wait_until(WAIT, || !probe.exits().is_empty()), "the tool did not finish: {:?}", probe.events());
    assert!(pane.join(Duration::from_secs(5)));
    fake.env()
}

#[test]
fn removed_names_do_not_reach_the_child() {
    assert!(std::env::var_os("HOME").is_some(), "the test needs a variable that always exists");
    let dir = Scratch::new();
    let fake = FakeCli::new(&dir.0, "kubectl");

    let without = run_and_record(&fake, &[("HW_PROBE", "1")], &["HOME"]);
    assert!(!without.contains_key("HOME"), "HOME was removed: {:?}", without.keys().collect::<Vec<_>>());
    assert_eq!(without.get("HW_PROBE").map(String::as_str), Some("1"), "the set variable arrives");
    assert_eq!(without.get("TERM").map(String::as_str), Some("xterm-256color"), "the pane's own variables stay");

    let with = run_and_record(&fake, &[("HW_PROBE", "1")], &[]);
    assert!(with.contains_key("HOME"), "without a removal the child inherits HOME");
}

#[test]
fn a_set_variable_wins_over_a_removal() {
    let dir = Scratch::new();
    let fake = FakeCli::new(&dir.0, "kubectl");
    let seen = run_and_record(&fake, &[("HOME", "/nonexistent-home")], &["HOME"]);
    assert_eq!(seen.get("HOME").map(String::as_str), Some("/nonexistent-home"));
}

#[test]
fn an_unusable_removal_is_ignored() {
    let dir = Scratch::new();
    let fake = FakeCli::new(&dir.0, "kubectl");
    // An empty name, a name with `=` and a name with NUL are no variable names: they are skipped, the valid name next to
    // them is applied, and a name that is not present is not an error.
    let seen = run_and_record(&fake, &[], &["", "A=B", "HOME=/x", "WITH\0NUL", "HW_NOT_THERE_8F2A", "HOME"]);
    assert!(!seen.contains_key("HOME"), "the valid removal next to the unusable ones still applies");
    assert!(seen.contains_key("PATH") || seen.contains_key("TERM"), "the child ran with the rest of its environment");
}
