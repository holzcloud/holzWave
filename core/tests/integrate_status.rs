//! The status of the tools: a cached value that the page reads cheaply and one background thread refreshes.
//!
//! The tools are fake programs of the rig, as in the service tests. The time of the cache is injected, so the rules of
//! 30 seconds, 60 seconds and 300 seconds are tested without waiting for them. The tests are Unix only.
#![cfg(unix)]

mod common;
#[path = "common/fake_cli.rs"]
mod fake_cli;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use holzwave_core::integrate::context::{ContextRow, RowState};
use holzwave_core::integrate::service::{ChangeEvent, ChangeKind, HostFacts, IntegrationService};
use holzwave_core::integrate::ToolId;
use holzwave_core::secrets::MemoryStore;

use common::scratch_dir;
use fake_cli::FakeCli;

/// A clock that only moves when a test moves it.
#[derive(Clone)]
struct TestClock {
    base: Instant,
    millis: Arc<AtomicU64>,
}

impl TestClock {
    fn new() -> TestClock {
        TestClock { base: Instant::now(), millis: Arc::new(AtomicU64::new(0)) }
    }

    fn advance(&self, by: Duration) {
        self.millis.fetch_add(u64::try_from(by.as_millis()).expect("a small step"), Ordering::SeqCst);
    }

    fn reader(&self) -> Arc<dyn Fn() -> Instant + Send + Sync> {
        let (base, millis) = (self.base, self.millis.clone());
        Arc::new(move || base + Duration::from_millis(millis.load(Ordering::SeqCst)))
    }
}

/// A scratch computer: a `bin` directory on the `PATH`, a home, a data directory, removed when the test ends.
struct Rig {
    root: PathBuf,
    bin: PathBuf,
    home: PathBuf,
    data: PathBuf,
    clock: TestClock,
}

impl Rig {
    fn new() -> Rig {
        let root = scratch_dir("status");
        let (bin, home, data) = (root.join("bin"), root.join("home"), root.join("data"));
        for dir in [&bin, &home, &data] {
            std::fs::create_dir(dir).expect("a directory of the rig");
        }
        Rig { root, bin, home, data, clock: TestClock::new() }
    }

    fn facts(&self) -> HostFacts {
        HostFacts::for_tests(&self.root, std::slice::from_ref(&self.bin), &self.home, &self.data)
    }

    /// The service as the shell builds it: shared, with a clock the test moves, and a callback that records.
    fn service(&self) -> (Arc<IntegrationService>, Events) {
        let events = Events::default();
        let service = IntegrationService::new(&self.data, Arc::new(MemoryStore::default()), self.facts(), &[])
            .with_clock(self.clock.reader())
            .shared();
        let sink = events.clone();
        service.set_on_change(Arc::new(move |event| sink.0.lock().expect("the log").push(event)));
        (service, events)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The change events a service sent.
#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<ChangeEvent>>>);

impl Events {
    fn of(&self, tool: ToolId) -> Vec<ChangeEvent> {
        self.0.lock().expect("the log").iter().filter(|event| event.tool == Some(tool)).cloned().collect()
    }
}

fn fixture(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integrations").join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {relative}: {e}"))
}

/// The fake logs `start` when it runs and `end` when it is done, after `secs` seconds.
fn logged(fake: FakeCli, secs: u32) -> FakeCli {
    use std::io::Write;
    let path = format!("{}.conf", fake.path().display());
    let mut conf = std::fs::OpenOptions::new().append(true).open(path).expect("the settings of the fake");
    writeln!(conf, "echo start >> \"$d/$n.log\"; sleep {secs}; echo end >> \"$d/$n.log\"").expect("one more line");
    fake
}

/// The fake logs into one file that all fakes share, so that the order of starts and ends can be read.
fn logged_together(fake: FakeCli, secs: u32) -> FakeCli {
    use std::io::Write;
    let path = format!("{}.conf", fake.path().display());
    let mut conf = std::fs::OpenOptions::new().append(true).open(path).expect("the settings of the fake");
    writeln!(conf, "echo \"start $n\" >> \"$d/all.log\"; sleep {secs}; echo \"end $n\" >> \"$d/all.log\"")
        .expect("one more line");
    fake
}

fn starts(fake: &FakeCli) -> usize {
    std::fs::read_to_string(format!("{}.log", fake.path().display()))
        .map(|text| text.lines().filter(|line| *line == "start").count())
        .unwrap_or_default()
}

/// Waits (bounded) until the background reads that were asked for are done.
fn settle(service: &IntegrationService) {
    assert!(service.wait_idle(Duration::from_secs(30)), "the background reads end");
}

fn row(rows: &[ContextRow], tool: ToolId) -> ContextRow {
    rows.iter().find(|row| row.tool == tool).expect("a row per tool").clone()
}

// ---- the tracer ----------------------------------------------------------------------------------------------------

#[test]
fn twenty_asks_cause_one_background_read() {
    let rig = Rig::new();
    let fake = logged(FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/config_view.json")), 0);
    let (service, events) = rig.service();

    let gate = Barrier::new(20);
    let answers: Vec<Vec<ContextRow>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..20)
            .map(|_| {
                scope.spawn(|| {
                    gate.wait();
                    service.context_rows(None, true)
                })
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().expect("a thread")).collect()
    });
    assert!(answers.iter().all(|rows| rows.len() == 4), "every ask is answered from the state at once");
    assert!(
        answers.iter().all(|rows| matches!(row(rows, ToolId::Kube).state, RowState::Unknown | RowState::Found)),
        "before the first read finishes the state is unknown, after it the tool is found"
    );

    settle(&service);
    assert_eq!(starts(&fake), 1, "twenty asks, one start of the tool");
    let kube = events.of(ToolId::Kube);
    assert_eq!(kube.len(), 1, "one change event for the tool: {kube:?}");
    assert_eq!(kube[0], ChangeEvent { kind: ChangeKind::Context, tool: Some(ToolId::Kube), pane: None, code: None });

    let rows = service.context_rows(None, false);
    let kube = row(&rows, ToolId::Kube);
    assert_eq!(kube.context.as_deref(), Some("dev"), "the current context of the recorded output");
    assert_eq!(kube.state, RowState::Found);
    assert_eq!(starts(&fake), 1, "a plain ask inside the time-to-live reads nothing");
}

#[test]
fn a_change_callback_fires_only_on_a_change() {
    let rig = Rig::new();
    let output = fixture("kubectl/config_view.json");
    let fake = logged(FakeCli::new(&rig.bin, "kubectl").prints(&output), 0);
    let (service, events) = rig.service();

    service.context_rows(None, true);
    settle(&service);
    assert_eq!((starts(&fake), events.of(ToolId::Kube).len()), (1, 1), "the first read is a change");

    // The same output again: the tool is read, and nothing is announced.
    rig.clock.advance(Duration::from_secs(3));
    service.context_rows(None, true);
    settle(&service);
    assert_eq!(starts(&fake), 2, "the forced refresh read the tool again");
    assert_eq!(events.of(ToolId::Kube).len(), 1, "an unchanged value is not announced");

    // The tool is switched outside holzWave: the next read is a change.
    let switched = output.replace("\"current-context\": \"dev\"", "\"current-context\": \"kind-test\"");
    assert_ne!(switched, output, "the fixture has the member that the test switches");
    let fake = logged(fake.prints(&switched), 0);
    rig.clock.advance(Duration::from_secs(3));
    service.context_rows(None, true);
    settle(&service);
    assert_eq!(starts(&fake), 3, "the switched tool was read once more");
    assert_eq!(events.of(ToolId::Kube).len(), 2, "the switch is announced once");
    let rows = service.context_rows(None, false);
    assert_eq!(row(&rows, ToolId::Kube).context.as_deref(), Some("kind-test"));
}

#[test]
fn a_missing_tool_is_not_read() {
    let rig = Rig::new();
    // Only a Kubernetes fake exists; the other three tools are not on the machine.
    let fake = logged_together(FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/config_view.json")), 0);
    let (service, events) = rig.service();

    let rows = service.context_rows(None, true);
    settle(&service);
    let log = std::fs::read_to_string(rig.bin.join("all.log")).unwrap_or_default();
    assert_eq!(log.lines().collect::<Vec<_>>(), ["start kubectl", "end kubectl"], "only the tool that exists started");
    assert!(fake.started());
    let rows_after = service.context_rows(None, false);
    for tool in [ToolId::Talos, ToolId::Azure, ToolId::Gam] {
        assert_eq!(row(&rows, tool).state, RowState::Unknown, "{tool:?} before");
        assert_eq!(row(&rows_after, tool).state, RowState::NotFound, "{tool:?} after: the detector found nothing");
        assert_eq!(events.of(tool).len(), 1, "{tool:?}: the change from unknown to not found is announced once");
    }
}

// ---- the rules: time-to-live, back-off, the forced refresh, no timer ---------------------------------------------

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// Moves the clock by `after`, asks, and waits until the reads that the ask started are done.
fn ask(rig: &Rig, service: &IntegrationService, after: Duration, forced: bool) -> Vec<ContextRow> {
    rig.clock.advance(after);
    let rows = service.context_rows(None, forced);
    settle(service);
    rows
}

#[test]
fn a_value_is_read_again_only_after_its_time_to_live() {
    let rig = Rig::new();
    let kube = logged(FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/config_view.json")), 0);
    let azure = logged(FakeCli::new(&rig.bin, "az").prints(&fixture("az/account_show.json")), 0);
    let (service, _events) = rig.service();

    ask(&rig, &service, secs(0), false);
    assert_eq!((starts(&kube), starts(&azure)), (1, 1), "the first ask reads what exists");

    ask(&rig, &service, secs(10), false);
    assert_eq!((starts(&kube), starts(&azure)), (1, 1), "10 s after: served from the cache");
    ask(&rig, &service, secs(19), false);
    assert_eq!((starts(&kube), starts(&azure)), (1, 1), "29 s after: the time-to-live of Kubernetes is not over");
    ask(&rig, &service, secs(2), false);
    assert_eq!((starts(&kube), starts(&azure)), (2, 1), "31 s after: Kubernetes is read again, Azure waits for 60 s");
    ask(&rig, &service, secs(28), false);
    assert_eq!((starts(&kube), starts(&azure)), (2, 1), "59 s after the first read");
    ask(&rig, &service, secs(2), false);
    assert_eq!((starts(&kube), starts(&azure)), (3, 2), "61 s after: Azure is read again");
}

#[test]
fn a_forced_refresh_respects_the_two_second_minimum() {
    let rig = Rig::new();
    let kube = logged(FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/config_view.json")), 0);
    let (service, _events) = rig.service();

    ask(&rig, &service, secs(0), true);
    assert_eq!(starts(&kube), 1);
    ask(&rig, &service, Duration::from_millis(1900), true);
    assert_eq!(starts(&kube), 1, "1.9 s after: too soon, however often it is forced");
    ask(&rig, &service, Duration::from_millis(100), true);
    assert_eq!(starts(&kube), 2, "2 s after: a forced refresh reads, long before the time-to-live");
}

#[test]
fn errors_back_off_and_a_success_resets() {
    let rig = Rig::new();
    let mut kube = logged(FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/config_view.json")), 0);
    let (service, events) = rig.service();
    let stale = |service: &IntegrationService| row(&service.context_rows(None, false), ToolId::Kube).stale;

    ask(&rig, &service, secs(0), false);
    assert_eq!((starts(&kube), stale(&service)), (1, false), "a good read");

    // The tool starts to fail. The waits are 60, 120, 240, 300 and 300 seconds.
    kube = logged(kube.prints_stderr("Error from server: boom").exits(1), 0);
    ask(&rig, &service, secs(31), false);
    assert_eq!((starts(&kube), stale(&service)), (2, true), "the first failure; the value is kept and marked old");
    let mut expected = 2;
    for wait in [60, 120, 240, 300, 300] {
        ask(&rig, &service, secs(wait - 1), false);
        assert_eq!(starts(&kube), expected, "{} s after the failure: still waiting", wait - 1);
        ask(&rig, &service, secs(2), false);
        expected += 1;
        assert_eq!(starts(&kube), expected, "{} s after the failure: read again", wait + 1);
    }
    let row_now = row(&service.context_rows(None, false), ToolId::Kube);
    assert_eq!(row_now.context.as_deref(), Some("dev"), "the old value is still shown while the tool fails");
    let announced = events.of(ToolId::Kube).len();
    assert_eq!(announced, 2, "the first value and the moment it became old, nothing more: {announced}");

    // A forced refresh ignores the back-off, not the two seconds.
    ask(&rig, &service, secs(1), true);
    assert_eq!(starts(&kube), expected, "1 s after: too soon even when forced");
    ask(&rig, &service, secs(2), true);
    expected += 1;
    assert_eq!(starts(&kube), expected, "3 s after: forced, in the middle of a wait of 300 s");
    let waiting = starts(&kube);
    ask(&rig, &service, secs(100), false);
    assert_eq!(starts(&kube), waiting, "the forced read failed too, so the wait is 300 s again");

    // The tool is well again: one success resets the wait to the time-to-live.
    kube = logged(kube.prints(&fixture("kubectl/config_view.json")).exits(0), 0);
    ask(&rig, &service, secs(201), false);
    assert_eq!((starts(&kube), stale(&service)), (waiting + 1, false), "a success clears the mark");
    ask(&rig, &service, secs(29), false);
    assert_eq!(starts(&kube), waiting + 1);
    ask(&rig, &service, secs(2), false);
    assert_eq!(starts(&kube), waiting + 2, "back to 30 s");
}

#[test]
fn signed_out_is_a_state_and_does_not_back_off() {
    let rig = Rig::new();
    let azure = logged(
        FakeCli::new(&rig.bin, "az").prints_stderr(&fixture("az/account_show_signed_out.stderr.txt")).exits(1),
        0,
    );
    let (service, events) = rig.service();

    let rows = ask(&rig, &service, secs(0), false);
    assert_eq!(row(&rows, ToolId::Azure).state, RowState::Unknown, "the answer of the first ask is the state before");
    let rows = service.context_rows(None, false);
    assert_eq!(row(&rows, ToolId::Azure).state, RowState::SignedOut, "signed out is a state of the row");
    assert_eq!(events.of(ToolId::Azure).len(), 1, "and it is announced once");
    assert_eq!(starts(&azure), 1);

    ask(&rig, &service, secs(59), false);
    assert_eq!(starts(&azure), 1);
    ask(&rig, &service, secs(2), false);
    assert_eq!(starts(&azure), 2, "read again after the normal 60 s");
    ask(&rig, &service, secs(61), false);
    assert_eq!(starts(&azure), 3, "and again after 60 s: no back-off, which would have asked for 120 s");
}

#[test]
fn nothing_runs_without_an_ask_and_dropping_ends_the_worker() {
    let rig = Rig::new();
    let kube = logged(FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/config_view.json")), 0);
    let (service, _events) = rig.service();
    let watch = service.worker_watch();

    // Building the service and waiting an hour starts no thread and no tool.
    rig.clock.advance(secs(3600));
    assert!(!watch.started(), "no thread before the first ask");
    assert_eq!(starts(&kube), 0);

    ask(&rig, &service, secs(0), false);
    assert!(watch.started() && !watch.ended());
    assert_eq!(starts(&kube), 1);

    // After the last ask the core has no timer: an hour passes and no tool is started.
    rig.clock.advance(secs(3600));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(starts(&kube), 1, "a bounded real wait; the injected hour started nothing");
    assert!(service.wait_idle(secs(1)));

    drop(service);
    assert!(common::wait_until(secs(10), || watch.ended()), "the thread ends when the service is dropped");
}

#[test]
fn one_background_read_per_tool_is_in_flight() {
    let rig = Rig::new();
    let kube = logged(FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/config_view.json")), 1);
    let (service, _events) = rig.service();

    for _ in 0..5 {
        service.context_rows(None, true);
        rig.clock.advance(secs(3));
    }
    settle(&service);
    assert_eq!(starts(&kube), 1, "five asks during the slow read, one start");
}

#[test]
fn background_reads_run_one_after_the_other_so_the_child_limit_holds() {
    let rig = Rig::new();
    let names = [("kubectl", "kubectl/config_view.json"), ("talosctl", "talosctl/config_contexts.txt")];
    let mut fakes = Vec::new();
    for (name, output) in names {
        fakes.push(logged_together(FakeCli::new(&rig.bin, name).prints(&fixture(output)), 1));
    }
    fakes.push(logged_together(FakeCli::new(&rig.bin, "az").prints(&fixture("az/account_show.json")), 1));
    fakes.push(logged_together(FakeCli::new(&rig.bin, "gam").prints(&fixture("gam/showsections.txt")), 1));
    let (service, _events) = rig.service();

    service.context_rows(None, true);
    settle(&service);
    let log = std::fs::read_to_string(rig.bin.join("all.log")).expect("the shared log");
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 8, "four tools, a start and an end each: {lines:?}");
    for pair in lines.chunks(2) {
        let (start, end) = (pair[0].strip_prefix("start "), pair[1].strip_prefix("end "));
        assert!(start.is_some() && start == end, "a read ends before the next one starts: {lines:?}");
    }
    assert!(fakes.iter().all(FakeCli::started));
}
