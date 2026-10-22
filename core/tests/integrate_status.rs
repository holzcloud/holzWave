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
