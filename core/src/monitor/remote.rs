//! The system monitor of another computer: one fixed read-only command, run through the saved connection's exec session.
//!
//! What runs on the host is [`SCRIPT`] and nothing else. It is a constant: no value from the page, the pane, the block or
//! the connection is put into it, so nothing needs quoting and nothing can be injected. The host answers with numbers and
//! short names, which [`super::procfs`] reads without trusting them. The host key is the exec primitive's rule (an
//! unknown key is refused and never learned), and nothing connects before a sample is asked for by a shown monitor pane.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::Reading;
use super::procfs::{self, Parsed, Raw, derive};
use crate::connections::Connections;
use crate::ssh::{ExecCancel, ExecEnd, ExecLimits, MAX_EXEC_OUTPUT, SshExec, SshTarget};

/// The longest a sample may take on the host.
pub const SAMPLE_TIMEOUT: Duration = Duration::from_secs(10);

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
pub(super) fn fault_text(raw: &str) -> String {
    let text = raw.strip_prefix("unsupported: ").unwrap_or(raw);
    let text = text.strip_prefix("ssh exec: ").unwrap_or(text);
    let text: String = text.chars().filter(|c| !c.is_control()).collect();
    let text = text.trim();
    if text.is_empty() {
        return "unknown reason".into();
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
}

/// The monitor of one host.
pub struct RemoteMonitor {
    probe: Box<dyn Probe>,
    state: Mutex<State>,
}

impl RemoteMonitor {
    pub fn new(probe: Box<dyn Probe>) -> Self {
        Self { probe, state: Mutex::new(State { previous: None }) }
    }

    /// One sample now.
    pub fn sample(&self) -> Result<Reading, String> {
        self.sample_with(&Instant::now)
    }

    /// One sample, with the clock the caller gives. The state is locked for the whole call, so at most one command is in
    /// flight per host.
    pub fn sample_with(&self, clock: &dyn Fn() -> Instant) -> Result<Reading, String> {
        let _ = clock;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let bytes = self.probe.fetch()?;
        let text = String::from_utf8_lossy(&bytes);
        match procfs::parse(&text).map_err(str::to_owned)? {
            Parsed::Other(system) => {
                state.previous = None;
                Ok(Reading::Unsupported { system })
            }
            Parsed::Linux(raw) => {
                let snapshot = derive(state.previous.as_ref(), &raw);
                state.previous = Some(raw);
                Ok(Reading::Live(snapshot))
            }
        }
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
    /// monitor; another generation replaces it. The builder runs without the lock held.
    pub fn get_or_build(
        &self,
        id: &str,
        generation: u64,
        build: impl FnOnce() -> Result<RemoteMonitor, String>,
    ) -> Result<Arc<RemoteMonitor>, String> {
        {
            let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(entry) = entries.iter().find(|e| e.id == id && e.generation == generation) {
                return Ok(entry.monitor.clone());
            }
        }
        let monitor = Arc::new(build()?);
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|e| e.id != id);
        entries.push(Entry { id: id.to_owned(), generation, monitor: monitor.clone() });
        Ok(monitor)
    }

    /// One sample of the saved connection `id`: its session is made from the connection at the first call and again when
    /// the saved connections changed.
    pub fn sample(&self, connections: &Connections, id: &str) -> Result<Reading, String> {
        let generation = connections.generation();
        let monitor = self.get_or_build(id, generation, || {
            let target = connections.ssh_target(id).map_err(|e| fault_text(&e.to_string()))?;
            let label = connections.label_of(id).unwrap_or_else(|| id.to_owned());
            ssh_monitor(&label, target)
        })?;
        monitor.sample()
    }
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
}
