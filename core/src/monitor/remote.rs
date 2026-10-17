//! The system monitor of another computer: one fixed read-only command, run through the saved connection's exec session.
//!
//! What runs on the host is [`SCRIPT`] and nothing else. It is a constant: no value from the page, the pane, the block or
//! the connection is put into it, so nothing needs quoting and nothing can be injected. The host answers with numbers and
//! short names, which [`super::procfs`] reads without trusting them. The host key is the exec primitive's rule (an
//! unknown key is refused and never learned), and nothing connects before a sample is asked for by a shown monitor pane.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use super::{MIN_INTERVAL, Reading};
use super::procfs::{self, Parsed, Raw, derive};
use crate::connections::Connections;
use crate::ssh::{ExecCancel, ExecEnd, ExecLimits, MAX_EXEC_OUTPUT, SshExec, SshTarget};

/// The longest a sample may take on the host.
pub const SAMPLE_TIMEOUT: Duration = Duration::from_secs(10);
/// A host that is not Linux is not asked again for this long: it will not turn into one.
pub const UNSUPPORTED_KEPT: Duration = Duration::from_secs(30);
/// The most hosts kept at once; the oldest is dropped for a new one.
pub const MAX_HOSTS: usize = 8;
/// What a person is told when the host's key has not been seen yet. The monitor never learns a key.
pub const HOST_KEY_HINT: &str = "The host key is not known yet. Connect to this host once, then try again.";

/// The one command that runs on the host: 18 statements on one line, without a single quote or a history mark.
///
/// It is constant on purpose. Nothing from the page, the pane, the block or the connection may ever be put into it, and it
/// names no file that holds command lines or the variables a process runs with: only the per-process stat files (the pid,
/// the kernel's short name and numbers), the uptime, the cpu lines of the system stat file, the memory counters, the
/// interface counters and the page size. The process files come first because the exec primitive keeps the last bytes of
/// an oversized answer, so the counters must come last.
pub const SCRIPT: &str = "LC_ALL=C; export LC_ALL; echo @@begin; cat /proc/[0-9]*/stat 2>/dev/null; echo; echo @@uname; uname -s; echo @@uptime; cat /proc/uptime; echo @@stat; grep \"^cpu\" /proc/stat; echo @@mem; cat /proc/meminfo; echo @@net; cat /proc/net/dev; echo @@page; getconf PAGESIZE 2>/dev/null; echo @@end";

/// The text sent to the host. The wrapper makes a login shell that is fish or tcsh run the script as POSIX shell; nothing
/// is quoted because nothing is data.
pub fn command() -> String {
    format!("exec sh -c -- '{SCRIPT}'")
}

/// What a monitor needs from a host: the bytes its fixed command printed, or a plain reason it could not be run.
///
/// The seam between the reader and the network: the tests of the reader and of the pool put a double here.
pub trait Probe: Send + Sync {
    fn fetch(&self) -> Result<Vec<u8>, String>;
}

/// The probe of a saved connection: the fixed command through the exec primitive.
struct ExecProbe {
    exec: SshExec,
    limits: ExecLimits,
}

impl Probe for ExecProbe {
    fn fetch(&self) -> Result<Vec<u8>, String> {
        match self.exec.exec(&command(), &self.limits, &ExecCancel::new()) {
            // Whatever the exit status: a host without `sh` prints nothing and is judged by the reader.
            Ok(result) => match result.end {
                ExecEnd::Finished => Ok(result.stdout),
                ExecEnd::TimedOut => Err("the host did not answer in time".into()),
                ExecEnd::Cancelled => Err("the sample was cancelled".into()),
                ExecEnd::Lost => Err("the connection was lost".into()),
            },
            Err(e) => Err(fault_text(&e.to_string())),
        }
    }
}

/// The longest reason shown.
const FAULT_MAX: usize = 200;

/// A failure in plain words: without the prefixes of the core's errors, without control characters, trimmed and short.
/// A host key that is not known yet becomes the sentence that says what to do; a key that changed stays as the core says
/// it, because that warning must stay loud.
pub(super) fn fault_text(raw: &str) -> String {
    let text = raw.strip_prefix("unsupported: ").unwrap_or(raw);
    let text = text.strip_prefix("ssh exec: ").unwrap_or(text);
    let text: String = text.chars().filter(|c| !c.is_control()).collect();
    let text = text.trim();
    if text.is_empty() {
        return "unknown reason".into();
    }
    if text.contains("is not known") {
        return HOST_KEY_HINT.into();
    }
    if text.chars().count() > FAULT_MAX {
        let cut: String = text.chars().take(FAULT_MAX).collect();
        return format!("{cut}…");
    }
    text.to_owned()
}

struct State {
    /// The last good Linux reading, which the next one is compared with.
    previous: Option<Raw>,
    /// When the last good answer ended and what it was: given again while it is young.
    last: Option<(Instant, Reading)>,
}

/// The monitor of one host.
pub struct RemoteMonitor {
    probe: Box<dyn Probe>,
    state: Mutex<State>,
}

impl RemoteMonitor {
    pub fn new(probe: Box<dyn Probe>) -> Self {
        Self { probe, state: Mutex::new(State { previous: None, last: None }) }
    }

    /// One sample now.
    pub fn sample(&self) -> Result<Reading, String> {
        self.sample_with(&Instant::now)
    }

    /// One sample, with the clock the caller gives. The state is locked for the whole call, so at most one command is in
    /// flight per host. An answer younger than [`MIN_INTERVAL`] (younger than [`UNSUPPORTED_KEPT`] when the host is not
    /// Linux) is given again without asking the host. An error is never kept: the next call asks again.
    pub fn sample_with(&self, clock: &dyn Fn() -> Instant) -> Result<Reading, String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, reading)) = &state.last {
            let keep = if matches!(reading, Reading::Unsupported { .. }) { UNSUPPORTED_KEPT } else { MIN_INTERVAL };
            if clock().saturating_duration_since(*at) < keep {
                return Ok(reading.clone());
            }
        }
        let bytes = self.probe.fetch()?;
        let text = String::from_utf8_lossy(&bytes);
        let reading = match procfs::parse(&text).map_err(str::to_owned)? {
            Parsed::Other(system) => {
                state.previous = None;
                Reading::Unsupported { system }
            }
            Parsed::Linux(raw) => {
                let snapshot = derive(state.previous.as_ref(), &raw);
                state.previous = Some(raw);
                Reading::Live(snapshot)
            }
        };
        // The instant is taken after the host answered, so a slow answer is not counted as old.
        state.last = Some((clock(), reading.clone()));
        Ok(reading)
    }
}

/// A monitor for the host of `target`, with the default limits. Opens no connection.
pub fn ssh_monitor(label: &str, target: SshTarget) -> Result<RemoteMonitor, String> {
    ssh_monitor_with(label, target, ExecLimits { max_output: MAX_EXEC_OUTPUT, timeout: SAMPLE_TIMEOUT })
}

/// The same with the limits of the caller (a test makes the time short).
pub fn ssh_monitor_with(label: &str, target: SshTarget, limits: ExecLimits) -> Result<RemoteMonitor, String> {
    let exec = SshExec::new(label, target).map_err(|e| fault_text(&e.to_string()))?;
    Ok(RemoteMonitor::new(Box::new(ExecProbe { exec, limits })))
}

struct Entry {
    id: String,
    generation: u64,
    monitor: Arc<RemoteMonitor>,
}

/// The monitors of the hosts that monitor panes show.
pub struct RemoteMonitors {
    entries: Mutex<Vec<Entry>>,
}

impl Default for RemoteMonitors {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteMonitors {
    pub fn new() -> Self {
        Self { entries: Mutex::new(Vec::new()) }
    }

    /// The monitor of host `id` for this generation of the saved connections. The same id and generation gives the same
    /// monitor; another generation replaces it. The builder runs without the lock held. At most [`MAX_HOSTS`] monitors
    /// are kept: the oldest goes for a new one.
    pub fn get_or_build(
        &self,
        id: &str,
        generation: u64,
        build: impl FnOnce() -> Result<RemoteMonitor, String>,
    ) -> Result<Arc<RemoteMonitor>, String> {
        if let Some(found) = self.find(id, generation) {
            return Ok(found);
        }
        let monitor = Arc::new(build()?);
        let mut doomed = Vec::new();
        let kept = {
            let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
            // Two callers that built the same host at once end with one entry: the second finds the first's.
            if let Some(entry) = entries.iter().find(|e| e.id == id && e.generation == generation) {
                doomed.push(monitor);
                entry.monitor.clone()
            } else {
                let (old, rest): (Vec<Entry>, Vec<Entry>) = std::mem::take(&mut *entries).into_iter().partition(|e| e.id == id);
                doomed.extend(old.into_iter().map(|e| e.monitor));
                *entries = rest;
                entries.push(Entry { id: id.to_owned(), generation, monitor: monitor.clone() });
                while entries.len() > MAX_HOSTS {
                    doomed.push(entries.remove(0).monitor);
                }
                monitor
            }
        };
        drop_apart(doomed);
        Ok(kept)
    }

    fn find(&self, id: &str, generation: u64) -> Option<Arc<RemoteMonitor>> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.iter().find(|e| e.id == id && e.generation == generation).map(|e| e.monitor.clone())
    }

    /// Keep the monitors of `ids` and drop the rest: the hosts that no monitor pane shows any more.
    pub fn keep_only(&self, ids: &BTreeSet<String>) {
        let doomed: Vec<Arc<RemoteMonitor>> = {
            let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
            let (kept, gone): (Vec<Entry>, Vec<Entry>) = std::mem::take(&mut *entries).into_iter().partition(|e| ids.contains(&e.id));
            *entries = kept;
            gone.into_iter().map(|e| e.monitor).collect()
        };
        drop_apart(doomed);
    }

    /// How many hosts are kept.
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// One sample of the saved connection `id`: its session is made from the connection at the first call and again when
    /// the saved connections changed.
    pub fn sample(&self, connections: &Connections, id: &str) -> Result<Reading, String> {
        self.sample_with(connections, id, &Instant::now)
    }

    /// The same with the clock of the caller.
    pub fn sample_with(&self, connections: &Connections, id: &str, clock: &dyn Fn() -> Instant) -> Result<Reading, String> {
        let generation = connections.generation();
        let monitor = self.get_or_build(id, generation, || {
            let target = connections.ssh_target(id).map_err(|e| fault_text(&e.to_string()))?;
            let label = connections.label_of(id).unwrap_or_else(|| id.to_owned());
            ssh_monitor(&label, target)
        })?;
        monitor.sample_with(clock)
    }
}

/// Drop monitors on a thread of their own: a monitor holds a runtime, which must not be dropped by a caller that may be
/// inside an async context.
fn drop_apart(doomed: Vec<Arc<RemoteMonitor>>) {
    if doomed.is_empty() {
        return;
    }
    // If no thread can be made the closure, and with it the monitors, is dropped here: the same end, on this thread.
    let _ = thread::Builder::new().name("hw-monitor-drop".into()).spawn(move || drop(doomed));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_is_one_line_without_quotes_or_history_marks() {
        assert!(!SCRIPT.contains('\n') && !SCRIPT.contains('\''), "one line, no single quote");
        assert!(!SCRIPT.contains('!'), "no history mark");
        let statements: Vec<&str> = SCRIPT.split("; ").collect();
        assert_eq!(
            statements,
            [
                "LC_ALL=C",
                "export LC_ALL",
                "echo @@begin",
                "cat /proc/[0-9]*/stat 2>/dev/null",
                "echo",
                "echo @@uname",
                "uname -s",
                "echo @@uptime",
                "cat /proc/uptime",
                "echo @@stat",
                "grep \"^cpu\" /proc/stat",
                "echo @@mem",
                "cat /proc/meminfo",
                "echo @@net",
                "cat /proc/net/dev",
                "echo @@page",
                "getconf PAGESIZE 2>/dev/null",
                "echo @@end",
            ]
        );
        let command = command();
        assert!(command.starts_with("exec sh -c -- '") && command.ends_with('\''));
        assert_eq!(command.matches('\'').count(), 2, "one pair of quotes around the whole script");
    }

    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/monitor").join(name)).expect("the fixture exists")
    }

    /// A probe that counts its calls and answers from a queue (the last answer repeats).
    struct Counting {
        calls: AtomicUsize,
        answers: Mutex<VecDeque<Result<Vec<u8>, String>>>,
    }

    impl Counting {
        fn new(answers: Vec<Result<Vec<u8>, String>>) -> Arc<Self> {
            Arc::new(Self { calls: AtomicUsize::new(0), answers: Mutex::new(answers.into()) })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    struct Shared(Arc<Counting>);

    impl Probe for Shared {
        fn fetch(&self) -> Result<Vec<u8>, String> {
            self.0.calls.fetch_add(1, Ordering::SeqCst);
            let mut answers = self.0.answers.lock().unwrap();
            if answers.len() > 1 { answers.pop_front().unwrap() } else { answers.front().cloned().unwrap() }
        }
    }

    fn monitor_of(probe: &Arc<Counting>) -> RemoteMonitor {
        RemoteMonitor::new(Box::new(Shared(probe.clone())))
    }

    #[test]
    fn a_second_sample_within_half_a_second_is_the_first_one() {
        let probe = Counting::new(vec![Ok(fixture("linux_a.txt")), Ok(fixture("linux_b.txt"))]);
        let monitor = monitor_of(&probe);
        let t0 = Instant::now();
        let now = Cell::new(t0);
        let first = monitor.sample_with(&|| now.get()).unwrap();
        now.set(t0 + Duration::from_millis(100));
        assert_eq!(monitor.sample_with(&|| now.get()).unwrap(), first);
        assert_eq!(probe.calls(), 1, "100 ms apart: one command");
        now.set(t0 + Duration::from_millis(600));
        assert_ne!(monitor.sample_with(&|| now.get()).unwrap(), first);
        assert_eq!(probe.calls(), 2, "600 ms apart: two commands");
    }

    #[test]
    fn an_unsupported_reading_is_kept_for_half_a_minute() {
        let probe = Counting::new(vec![Ok(fixture("darwin.txt"))]);
        let monitor = monitor_of(&probe);
        let t0 = Instant::now();
        let now = Cell::new(t0);
        assert_eq!(monitor.sample_with(&|| now.get()).unwrap(), Reading::Unsupported { system: "Darwin".into() });
        now.set(t0 + Duration::from_secs(10));
        monitor.sample_with(&|| now.get()).unwrap();
        assert_eq!(probe.calls(), 1, "10 s later the host is not asked");
        now.set(t0 + Duration::from_secs(31));
        monitor.sample_with(&|| now.get()).unwrap();
        assert_eq!(probe.calls(), 2, "31 s later it is");
    }

    #[test]
    fn an_error_is_not_kept_and_the_next_call_asks_again() {
        let probe = Counting::new(vec![Ok(fixture("linux_a.txt")), Err("the connection was lost".into()), Ok(fixture("linux_b.txt"))]);
        let monitor = monitor_of(&probe);
        let t0 = Instant::now();
        let now = Cell::new(t0);
        monitor.sample_with(&|| now.get()).unwrap();
        now.set(t0 + Duration::from_secs(1));
        assert_eq!(monitor.sample_with(&|| now.get()), Err("the connection was lost".to_string()));
        now.set(t0 + Duration::from_secs(1));
        // The failed call left the earlier reading in place, so the next one is still a step from it.
        match monitor.sample_with(&|| now.get()).unwrap() {
            Reading::Live(snapshot) => assert_eq!(snapshot.cpu, 37.5),
            other => panic!("{other:?}"),
        }
        assert_eq!(probe.calls(), 3);
    }

    #[test]
    fn a_fault_text_is_short_clean_and_has_no_prefix() {
        assert_eq!(fault_text("unsupported: ssh exec: the connection timed out"), "the connection timed out");
        let long = fault_text(&"x".repeat(300));
        assert_eq!(long.chars().count(), 201, "200 characters and an ellipsis");
        assert!(long.ends_with('…'));
        assert_eq!(fault_text("a\u{1b}[31m\nb\u{7}"), "a[31mb");
        assert_eq!(fault_text(""), "unknown reason");
        assert_eq!(fault_text("  \n "), "unknown reason");
    }

    #[test]
    fn the_host_key_hint_replaces_the_unknown_key_reason_and_a_changed_key_stays_loud() {
        let unknown = fault_text("unsupported: ssh exec: the host key of 127.0.0.1 is not known");
        assert_eq!(unknown, "The host key is not known yet. Connect to this host once, then try again.");
        let changed = "the host key of 127.0.0.1 CHANGED (line 3 of the known hosts file). Someone may be intercepting the connection";
        assert_eq!(fault_text(&format!("unsupported: ssh exec: {changed}")), changed);
    }

    fn built(probe: &Arc<Counting>) -> Result<RemoteMonitor, String> {
        Ok(monitor_of(probe))
    }

    #[test]
    fn the_pool_reuses_rebuilds_and_stays_bounded() {
        let probe = Counting::new(vec![Ok(fixture("darwin.txt"))]);
        let pool = RemoteMonitors::new();
        let builds = Cell::new(0);
        let get = |id: &str, generation: u64| {
            pool.get_or_build(id, generation, || {
                builds.set(builds.get() + 1);
                built(&probe)
            })
            .unwrap()
        };
        let first = get("a", 1);
        assert!(Arc::ptr_eq(&first, &get("a", 1)), "the same id and generation gives the same monitor");
        assert_eq!(builds.get(), 1);
        get("a", 2);
        assert_eq!(builds.get(), 2, "another generation builds again");
        assert_eq!(pool.len(), 1, "and the old one is gone");
        get("a", 1);
        assert_eq!(builds.get(), 3, "the old generation is not kept");
        for i in 0..9 {
            get(&format!("h{i}"), 1);
        }
        assert_eq!(pool.len(), MAX_HOSTS, "nine hosts leave eight");
        let before = builds.get();
        get("h8", 1);
        assert_eq!(builds.get(), before, "the newest is kept");
        get("h0", 1);
        assert_eq!(builds.get(), before + 1, "the oldest is the one that went");
        let failing: Result<Arc<RemoteMonitor>, String> = pool.get_or_build("z", 1, || Err("no way".into()));
        assert_eq!(failing.err().as_deref(), Some("no way"));
        assert_eq!(pool.len(), MAX_HOSTS, "a failed builder stores nothing");
    }

    #[test]
    fn the_pool_builds_outside_its_lock() {
        let probe = Counting::new(vec![Ok(fixture("darwin.txt"))]);
        let pool = Arc::new(RemoteMonitors::new());
        let (tx, rx) = std::sync::mpsc::channel();
        let inner = pool.clone();
        std::thread::spawn(move || {
            let again = inner.clone();
            let outer = inner.get_or_build("a", 1, || {
                // A builder that asks the same pool for another host would wait for ever if the lock were held.
                again.get_or_build("b", 1, || built(&probe)).map(|_| monitor_of(&probe))
            });
            let _ = tx.send(outer.is_ok());
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(true));
        assert_eq!(pool.len(), 2);
    }

    /// A probe that says on which thread it was dropped.
    struct DropSpy(Arc<Mutex<Vec<std::thread::ThreadId>>>);

    impl Probe for DropSpy {
        fn fetch(&self) -> Result<Vec<u8>, String> {
            Err("unused".into())
        }
    }

    impl Drop for DropSpy {
        fn drop(&mut self) {
            self.0.lock().unwrap().push(std::thread::current().id());
        }
    }

    #[test]
    fn keep_only_drops_the_hosts_nobody_shows() {
        let dropped = Arc::new(Mutex::new(Vec::new()));
        let pool = RemoteMonitors::new();
        for id in ["a", "b", "c"] {
            pool.get_or_build(id, 1, || Ok(RemoteMonitor::new(Box::new(DropSpy(dropped.clone()))))).unwrap();
        }
        pool.keep_only(&BTreeSet::from(["b".to_string()]));
        assert_eq!(pool.len(), 1);
        let deadline = Instant::now() + Duration::from_secs(5);
        while dropped.lock().unwrap().len() < 2 {
            assert!(Instant::now() < deadline, "the dropped monitors were never dropped");
            std::thread::sleep(Duration::from_millis(10));
        }
        let me = std::thread::current().id();
        assert!(dropped.lock().unwrap().iter().all(|t| *t != me), "dropped off the calling thread");
    }
}
