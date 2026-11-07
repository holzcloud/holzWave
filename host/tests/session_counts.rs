//! The live sessions of the app, counted and stopped: how many shells and how many connections are alive, read without
//! touching anything, and `stop_all`, which ends them all and leaves the layout, the windows and the app alone.
//!
//! The shells are real programs of `/bin/sh` in a real pseudo terminal. A connection that is up needs an SSH server, and
//! the one in-process server of the tests lives in the core's own test support, which is built on the core's `russh`; this
//! crate has no such dependency and gets none for a test. What is proved about connections here is therefore every state of
//! the rule itself (`count_sessions`, a table) and that a pane of a saved SSH connection that cannot be reached is no shell
//! and no connection. A connection that is `Connected` counted through a real login is proved by the table and by nothing
//! else, and is said so.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use holzwave_core::conn::ConnState;
use holzwave_core::connections::SecretChange;
use holzwave_core::profiles::{Auth, Profile};
use holzwave_core::vault::VaultState;
use holzwave_host::commands::vault::{vault_lock, vault_unlock};
use holzwave_host::seams::{Host, NeutralHost};
use holzwave_host::state::{AppCore, Out, ShellSpec, Sink, Target, count_sessions};
use holzwave_host::windows::{CursorSpot, Place};
use support::Fixture;

/// A sink that keeps what the tests ask about: the panes that were told `pane-exit`, whether the app was told to exit, and
/// the layouts that were sent.
#[derive(Default)]
struct Recorder {
    exits: Mutex<Vec<u64>>,
    app_exit: AtomicUsize,
    layouts: AtomicUsize,
}

impl Sink for Recorder {
    fn send(&self, _target: Target, out: Out) {
        match out {
            Out::PaneExit { pane, .. } => self.exits.lock().expect("exits").push(pane),
            Out::Exit => {
                self.app_exit.fetch_add(1, Ordering::SeqCst);
            }
            Out::Layout(_) => {
                self.layouts.fetch_add(1, Ordering::SeqCst);
            }
            _ => {}
        }
    }

    fn close_window(&self, _label: &str) {}

    fn open_window(&self, _label: &str, _place: &Place) -> Result<(), String> {
        Ok(())
    }

    fn cursor_spot(&self, _source: &str) -> CursorSpot {
        CursorSpot::Unknown
    }
}

/// A scratch folder that is removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("holzwave-counts-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data")).expect("scratch folder");
        Scratch(root.canonicalize().expect("canonical scratch folder"))
    }

    fn data(&self) -> PathBuf {
        self.0.join("data")
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let begun = Instant::now();
    while !done() {
        assert!(begun.elapsed() < Duration::from_secs(20), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn core_with(scratch: &Scratch, shell: ShellSpec) -> (Arc<AppCore>, Arc<Recorder>) {
    let recorder = Arc::new(Recorder::default());
    let host: Arc<dyn Host> = Arc::new(NeutralHost::new());
    let core = AppCore::new_with_host(scratch.data(), recorder.clone(), shell, None, None, host);
    (core, recorder)
}

/// A shell that records each start in a file and then waits.
fn waiting_shell(scratch: &Scratch) -> ShellSpec {
    let started = scratch.file("started");
    let script = format!("echo started >> '{}'; sleep 60", started.display());
    ShellSpec::new("/bin/sh", &["-c", &script])
}

/// A shell that records its start and then ignores the hangup and the termination request, for as long as it lives.
fn stubborn_shell(scratch: &Scratch) -> ShellSpec {
    let started = scratch.file("started");
    let script = format!("trap '' HUP TERM INT; echo started >> '{}'; while :; do sleep 1; done", started.display());
    ShellSpec::new("/bin/sh", &["-c", &script])
}

fn starts(scratch: &Scratch) -> usize {
    std::fs::read_to_string(scratch.file("started")).map_or(0, |text| text.lines().count())
}

fn focus(core: &AppCore) -> u64 {
    core.view_in("main").expect("the window main exists").focus
}

/// The layout is written a quarter of a second after the last change; wait until what is going to be written was.
fn settle() {
    std::thread::sleep(Duration::from_millis(700));
}

fn file_bytes(dir: &Path, name: &str) -> Option<Vec<u8>> {
    std::fs::read(dir.join(name)).ok()
}

// ---- the rule ---------------------------------------------------------------------------------------------------------

#[test]
fn every_state_of_a_pane_has_one_answer() {
    let up = ConnState::Connected;
    let again = ConnState::Reconnecting { attempt: 1, of: 3, in_secs: 2 };
    assert_eq!(count_sessions([]), (0, 0));
    assert_eq!(count_sessions([ConnState::Local]), (1, 0), "a shell on this computer");
    assert_eq!(count_sessions([up.clone()]), (0, 1), "a connection that is up is a connection and no shell");
    assert_eq!(count_sessions([again]), (0, 1), "a session that is being made again is still the person's");
    for dead in [ConnState::Connecting, ConnState::Failed, ConnState::Closed] {
        assert_eq!(count_sessions([dead.clone()]), (0, 0), "{dead:?} lives nothing");
    }
    assert_eq!(
        count_sessions([ConnState::Local, up.clone(), ConnState::Local, ConnState::Failed, up, ConnState::Closed]),
        (2, 2)
    );
}

// ---- counting ---------------------------------------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn a_new_core_has_nothing_a_started_shell_is_one_and_a_shell_that_ended_is_none_again() {
    let scratch = Scratch::new("count");
    let go = scratch.file("go");
    // The shell waits for a file, so the test sees it alive before it ends.
    let script = format!("while [ ! -e '{}' ]; do sleep 0.05; done", go.display());
    let (core, _) = core_with(&scratch, ShellSpec::new("/bin/sh", &["-c", &script]));
    assert_eq!(core.session_counts(), (0, 0), "a new core");

    core.start_pane(focus(&core), 80, 24, 0, 0).expect("the shell starts");
    assert_eq!(core.session_counts(), (1, 0), "one shell");

    std::fs::write(&go, "").expect("the signal");
    wait_for("the shell to end and be forgotten", || core.session_counts() == (0, 0));
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn counting_is_a_read_a_thousand_calls_start_stop_and_change_nothing() {
    let scratch = Scratch::new("read");
    let (core, recorder) = core_with(&scratch, waiting_shell(&scratch));
    let pane = focus(&core);
    core.start_pane(pane, 80, 24, 0, 0).expect("the shell starts");
    wait_for("the shell to report its start", || starts(&scratch) == 1);

    let view = core.view_in("main").expect("the view");
    let layouts = recorder.layouts.load(Ordering::SeqCst);
    let begun = Instant::now();
    for _ in 0..1000 {
        assert_eq!(core.session_counts(), (1, 0));
    }
    let spent = begun.elapsed();
    assert!(spent < Duration::from_secs(1), "a thousand counts took {spent:?}");

    // Nothing was started again (the fake counted its starts), nothing was told to a window, the layout is the layout it
    // was, and the pane is still alive.
    assert_eq!(starts(&scratch), 1, "no process was started");
    assert_eq!(recorder.layouts.load(Ordering::SeqCst), layouts, "no layout was sent");
    assert!(recorder.exits.lock().expect("exits").is_empty(), "no pane was ended");
    let after = core.view_in("main").expect("the view");
    // Asking for a view counts one rev itself, so the counts built none of theirs.
    assert_eq!((after.rev, after.focus, after.panes.len()), (view.rev + 1, view.focus, view.panes.len()), "the layout is untouched");
    assert_eq!(core.session_counts(), (1, 0));
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn a_pane_whose_start_failed_is_not_counted() {
    let scratch = Scratch::new("failed");
    let (core, _) = core_with(&scratch, ShellSpec::new("/nonexistent/holzwave-no-such-shell", &[]));
    assert!(core.start_pane(focus(&core), 80, 24, 0, 0).is_err(), "the shell cannot start");
    assert_eq!(core.session_counts(), (0, 0));
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn a_shell_in_a_tab_that_is_not_shown_is_counted_and_a_saved_connection_that_cannot_be_reached_is_neither() {
    let scratch = Scratch::new("tabs");
    let (core, _) = core_with(&scratch, waiting_shell(&scratch));
    core.start_pane(focus(&core), 80, 24, 0, 0).expect("the first shell starts");

    // A second tab takes the view; the first pane runs on in the one that is not shown.
    let second = core.new_tab_in("main").expect("a second tab");
    core.start_pane(second.focus, 80, 24, 0, 0).expect("the second shell starts");
    assert_eq!(core.session_counts(), (2, 0), "the shell of the hidden tab counts");

    // A saved SSH connection to a port nobody listens on: its pane runs, tries, and is no shell and no connection.
    let profile = Profile {
        port: 1,
        user: "holz".into(),
        auth: Auth::Key,
        key_path: "/nonexistent/holzwave-test-key".into(),
        ..Profile::new("down", "Down", "127.0.0.1")
    };
    core.connection_save(profile, SecretChange::Keep).expect("a valid connection");
    let third = core.connect_in("main", "down").expect("a tab for the connection");
    let _ = core.start_pane(third.focus, 80, 24, 0, 0);
    wait_for("the connection pane to say what it is", || !core.pane_connections().is_empty());
    assert_eq!(core.session_counts(), (2, 0), "an SSH pane that is not connected is not a shell, and not a connection");
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn a_locked_vault_changes_neither_the_counts_nor_stop_all() {
    let fixture = Fixture::new("counts-locked");
    // The fixture's shell is `cat`, which waits for input in its pseudo terminal.
    vault_unlock(&fixture.core).expect("the vault is made");
    fixture.core.start_pane(focus(&fixture.core), 80, 24, 0, 0).expect("the shell starts");
    assert_eq!(fixture.core.session_counts(), (1, 0), "unlocked");

    vault_lock(&fixture.core).expect("locked");
    assert_eq!(fixture.vault.status().state, VaultState::Locked);
    assert_eq!(fixture.core.session_counts(), (1, 0), "locked: the session still lives");

    let begun = Instant::now();
    fixture.core.stop_all();
    assert!(begun.elapsed() < Duration::from_secs(2), "locked: stop_all is not held up");
    assert_eq!(fixture.core.session_counts(), (0, 0), "locked: everything ended");
    fixture.core.shutdown();
    let _ = std::fs::remove_dir_all(&fixture.dir);
}

// ---- stopping ---------------------------------------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn stop_all_ends_every_shell_tells_each_pane_and_leaves_the_rest_alone() {
    let scratch = Scratch::new("stop");
    let (core, recorder) = core_with(&scratch, waiting_shell(&scratch));
    let first = focus(&core);
    core.start_pane(first, 80, 24, 0, 0).expect("the first shell starts");
    let second = core.new_tab_in("main").expect("a second tab").focus;
    core.start_pane(second, 80, 24, 0, 0).expect("the second shell starts");
    let third = core.split_in("main", true).expect("a split").focus;
    core.start_pane(third, 80, 24, 0, 0).expect("the third shell starts");
    wait_for("three shells", || starts(&scratch) == 3);
    assert_eq!(core.session_counts(), (3, 0));

    // What must not change: the files of the app and the layout.
    settle();
    let layout_file = file_bytes(&scratch.data(), "layout.json");
    let settings_file = file_bytes(&scratch.data(), "settings.json");
    let view = core.view_in("main").expect("the view");

    let begun = Instant::now();
    core.stop_all();
    let spent = begun.elapsed();
    assert!(spent < Duration::from_secs(2), "stop_all took {spent:?}");
    assert_eq!(core.session_counts(), (0, 0), "no shell lives");

    // Each pane was told what it is told when a shell ends by itself.
    let mut told = recorder.exits.lock().expect("exits").clone();
    told.sort_unstable();
    let mut wanted = vec![first, second, third];
    wanted.sort_unstable();
    assert_eq!(told, wanted, "pane-exit for every pane, once");
    assert_eq!(recorder.app_exit.load(Ordering::SeqCst), 0, "the app was not told to exit");

    // The layout, the windows and the files are what they were.
    let after = core.view_in("main").expect("the window is still there");
    assert_eq!(after.panes.len(), view.panes.len(), "the panes of the shown tab are still in the layout");
    assert_eq!(after.tabs.len(), view.tabs.len(), "the tabs are still there");
    assert_eq!(after.focus, view.focus);
    settle();
    assert_eq!(file_bytes(&scratch.data(), "layout.json"), layout_file, "layout.json is untouched");
    assert_eq!(file_bytes(&scratch.data(), "settings.json"), settings_file, "settings.json is untouched");

    // A second call is harmless and quick.
    let begun = Instant::now();
    core.stop_all();
    assert!(begun.elapsed() < Duration::from_millis(500), "a second stop_all has nothing to wait for");
    assert_eq!(core.session_counts(), (0, 0));
    assert_eq!(recorder.exits.lock().expect("exits").len(), 3, "no pane was told twice");

    // The core is not left ending: a pane can be started again.
    core.start_pane(first, 80, 24, 0, 0).expect("a shell starts after a stop");
    wait_for("the new shell to be counted", || core.session_counts() == (1, 0));
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn stop_all_returns_within_two_seconds_when_a_shell_ignores_the_hangup() {
    let scratch = Scratch::new("stubborn");
    let (core, _) = core_with(&scratch, stubborn_shell(&scratch));
    core.start_pane(focus(&core), 80, 24, 0, 0).expect("the shell starts");
    wait_for("the shell to report its start", || starts(&scratch) == 1);
    assert_eq!(core.session_counts(), (1, 0));

    let begun = Instant::now();
    core.stop_all();
    let spent = begun.elapsed();
    assert!(spent < Duration::from_secs(2), "stop_all took {spent:?}");
    assert_eq!(core.session_counts(), (0, 0), "the shell that ignored the hangup is gone");
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn stop_all_with_nothing_running_does_nothing() {
    let scratch = Scratch::new("none");
    let (core, recorder) = core_with(&scratch, waiting_shell(&scratch));
    let begun = Instant::now();
    core.stop_all();
    assert!(begun.elapsed() < Duration::from_millis(500));
    assert_eq!(core.session_counts(), (0, 0));
    assert!(recorder.exits.lock().expect("exits").is_empty());
    assert_eq!(recorder.app_exit.load(Ordering::SeqCst), 0);
    core.shutdown();
}
