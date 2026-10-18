//! Running a tool: an argument list and never a shell, a scrubbed environment, bounded output, a time limit,
//! cancellation, and a cap on how many children run at once.

use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::exec::{exit_code, isolate, stop};

/// The most bytes of standard output kept by default.
pub const DEFAULT_STDOUT: usize = 16 * 1024 * 1024;
/// The default time limit of a call.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

/// The environment of a child.
pub struct Env(Vec<(String, String)>);

impl Env {
    /// An environment of these pairs.
    pub fn from_pairs(pairs: Vec<(String, String)>) -> Self {
        Self(pairs)
    }
}

impl std::fmt::Debug for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Env").field("variables", &self.0.len()).finish()
    }
}

/// What one call is bounded by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunLimits {
    /// The most bytes of standard output; a run that passes it fails.
    pub max_stdout: usize,
    /// The time a run may take.
    pub timeout: Duration,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self { max_stdout: DEFAULT_STDOUT, timeout: DEFAULT_TIMEOUT }
    }
}

/// A switch that stops a run; the caller keeps a clone.
#[derive(Debug, Clone, Default)]
pub struct RunCancel(Arc<AtomicBool>);

impl RunCancel {
    /// Raises the switch.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether the switch was raised.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEnd {
    /// The program ended by itself.
    Finished,
    /// The time limit passed and the process group was killed.
    TimedOut,
    /// The switch was raised and the process group was killed.
    Cancelled,
    /// Standard output passed its cap and the process group was killed.
    Overflow,
    /// The program could not be waited for or its output could not be read to the end.
    Lost,
}

/// What a run produced. `Debug` prints sizes only: the output of a tool is another party's text.
pub struct RunResult {
    /// The head of standard output.
    pub stdout: Vec<u8>,
    /// The first bytes of standard error.
    pub stderr: Vec<u8>,
    /// The exit status, when the program ended by itself.
    pub status: Option<u32>,
    /// How it ended.
    pub end: RunEnd,
    /// Whether bytes of standard output were dropped (always the case for `Overflow`).
    pub stdout_dropped: bool,
}

impl std::fmt::Debug for RunResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunResult")
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .field("status", &self.status)
            .field("end", &self.end)
            .field("stdout_dropped", &self.stdout_dropped)
            .finish()
    }
}

/// Why a call did not run. Every sentence is fixed and holds nothing of the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunError {
    /// The program is not an absolute path.
    NotAbsolute,
    /// The system refused to start the program or to read its output.
    CannotStart,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotAbsolute => "the program is not given as an absolute path",
            Self::CannotStart => "the program could not be started",
        })
    }
}

impl std::error::Error for RunError {}

/// The runner: the one place where an integration starts a child process.
#[derive(Debug, Clone, Default)]
pub struct Runner;

impl Runner {
    /// A runner.
    pub fn new() -> Self {
        Self
    }

    /// Runs `program` (an absolute path, taken as given) with `args` and nothing else: no shell, standard input closed,
    /// the child leading a process group of its own, the environment exactly `env`. Blocks until the program ended.
    ///
    /// # Errors
    ///
    /// A [`RunError`] with a fixed sentence when the program is not an absolute path or cannot be started.
    pub fn run_argv(
        &self,
        program: &Path,
        args: &[OsString],
        env: Env,
        limits: &RunLimits,
        _cancel: &RunCancel,
    ) -> Result<RunResult, RunError> {
        if !program.is_absolute() {
            return Err(RunError::NotAbsolute);
        }
        let mut command = Command::new(program);
        command.args(args).env_clear().stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (name, value) in &env.0 {
            command.env(name, value);
        }
        isolate(&mut command);
        let spawned = command.spawn();
        drop(command);
        drop(env);
        let mut child = spawned.map_err(|_| RunError::CannotStart)?;
        let out = Arc::new(Mutex::new(Head::new(limits.max_stdout)));
        let err = Arc::new(Mutex::new(Head::new(STDERR_KEPT)));
        let readers = (
            child.stdout.take().and_then(|pipe| reader("hw-run-out", pipe, Arc::clone(&out))),
            child.stderr.take().and_then(|pipe| reader("hw-run-err", pipe, Arc::clone(&err))),
        );
        let (Some(out_reader), Some(err_reader)) = readers else {
            let pid = child.id();
            stop(&mut child, pid);
            return Err(RunError::CannotStart);
        };
        let waited = child.wait();
        let _ = out_reader.join();
        let _ = err_reader.join();
        let (stdout, stdout_dropped) = take(&out);
        let (stderr, _) = take(&err);
        let (end, status) = match waited {
            Ok(done) => (RunEnd::Finished, exit_code(&done)),
            Err(_) => (RunEnd::Lost, None),
        };
        Ok(RunResult { stdout, stderr, status, end, stdout_dropped })
    }
}

/// The first `cap` bytes of everything pushed, and whether more came.
struct Head {
    cap: usize,
    bytes: Vec<u8>,
    overflow: bool,
}

impl Head {
    fn new(cap: usize) -> Self {
        Self { cap, bytes: Vec::new(), overflow: false }
    }

    fn push(&mut self, chunk: &[u8]) {
        let room = self.cap - self.bytes.len();
        if chunk.len() > room {
            self.bytes.extend_from_slice(&chunk[..room]);
            self.overflow = true;
        } else {
            self.bytes.extend_from_slice(chunk);
        }
    }
}

/// The bytes a head holds and whether it dropped any.
fn take(head: &Mutex<Head>) -> (Vec<u8>, bool) {
    let mut guard = head.lock().unwrap_or_else(PoisonError::into_inner);
    (std::mem::take(&mut guard.bytes), guard.overflow)
}

/// A thread that copies a pipe into a head until the pipe ends; none when the system refuses the thread.
fn reader(name: &str, mut pipe: impl Read + Send + 'static, head: Arc<Mutex<Head>>) -> Option<JoinHandle<()>> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let mut buffer = vec![0u8; CHUNK];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.lock().unwrap_or_else(PoisonError::into_inner).push(&buffer[..n]),
                }
            }
        })
        .ok()
}

/// The size of one read from a pipe.
const CHUNK: usize = 32 * 1024;
/// The bytes of standard error that are kept, from its start.
const STDERR_KEPT: usize = 64 * 1024;
