//! Running a tool: an argument list and never a shell, a scrubbed environment, bounded output, a time limit,
//! cancellation, and a cap on how many children run at once.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

use super::args::EnvName;
use crate::exec::{exit_code, isolate, kill_tree, stop};
use crate::platform::Os;

/// The most bytes of standard output kept by default.
pub const DEFAULT_STDOUT: usize = 16 * 1024 * 1024;
/// The default time limit of a call.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
/// The least a call may ask for as a cap on standard output.
pub const MIN_STDOUT: usize = 1024;
/// The most a call may ask for as a cap on standard output.
pub const MAX_STDOUT: usize = 64 * 1024 * 1024;
/// The least time limit of a call.
pub const MIN_TIMEOUT: Duration = Duration::from_millis(10);
/// The most time a call may take.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(120);
/// How many children run at once.
pub const MAX_CHILDREN: usize = 4;
/// How long a call waits for a free slot before it answers [`RunError::Busy`].
pub const SLOT_WAIT: Duration = Duration::from_secs(1);
/// The bytes of standard error that are kept, from its start.
pub const STDERR_KEPT: usize = 64 * 1024;

/// The most variables a child gets, inherited and added together.
pub const MAX_ENV_PAIRS: usize = 1024;
/// The most variables the profile, the tool and the isolation can add together.
pub const MAX_EXTRA_PAIRS: usize = 256;
/// The most bytes of one variable value, a secret or not.
pub const MAX_ENV_VALUE: usize = 16 * 1024;

/// How often the run looks at the program, its time limit, its output cap and the switch.
const POLL: Duration = Duration::from_millis(10);
/// How long the readers get to reach the end of their pipes after the program ended by itself.
const DRAIN: Duration = Duration::from_millis(250);
/// How long the readers get after the process group was killed, before they are given up.
const REAP: Duration = Duration::from_millis(500);
/// The size of one read from a pipe.
const CHUNK: usize = 32 * 1024;

/// The names the app itself sets from its own state: a synced profile may not set them, the isolation of a pane does.
const APP_STATE_NAMES: [&str; 5] = ["KUBECONFIG", "TALOSCONFIG", "AZURE_CONFIG_DIR", "GAMCFGDIR", "GAMCFGSECTION"];
/// The prefixes of the names that never reach a child: the app's own, the loader's, the interpreters'.
const DENIED_PREFIXES: [&str; 6] = ["HOLZWAVE_", "LD_", "DYLD_", "PYTHON", "NODE_", "GIT_"];
/// The names that never reach a child, exact: shell start-up files and word splitting, and the `PATH` (which is rebuilt).
const DENIED_NAMES: [&str; 4] = ["BASH_ENV", "ENV", "IFS", "PATH"];

/// The environment of a child, built by [`EnvPolicy::build`]. The secret values are wiped when it is dropped, which
/// [`Runner::run_argv`] does right after the start. `Debug` prints counts.
pub struct Env {
    vars: Vec<(String, String)>,
    secrets: Vec<(String, Zeroizing<String>)>,
}

impl Env {
    /// The names, sorted, secret names included (a name is no secret).
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .vars
            .iter()
            .map(|(name, _)| name.as_str())
            .chain(self.secrets.iter().map(|(name, _)| name.as_str()))
            .collect();
        names.sort_unstable();
        names
    }

    /// The value of a plain variable. A secret is never read back this way.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.vars.iter().find(|(own, _)| own == name).map(|(_, value)| value.as_str())
    }

    /// How many variables, secrets included.
    pub fn len(&self) -> usize {
        self.vars.len() + self.secrets.len()
    }

    /// Whether there is no variable.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many of the variables are secrets.
    pub fn secret_count(&self) -> usize {
        self.secrets.len()
    }

    /// The plain pairs and the secret pairs, for a launch that hands them to a pane.
    pub(crate) fn into_parts(self) -> (Vec<(String, String)>, Vec<(String, Zeroizing<String>)>) {
        (self.vars, self.secrets)
    }
}

impl std::fmt::Debug for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Env").field("variables", &self.vars.len()).field("secrets", &self.secrets.len()).finish()
    }
}

/// What a call adds to the inherited environment. `Debug` prints counts.
#[derive(Default)]
pub struct EnvExtra {
    /// The plain variables of the profile; they may not take a name the app sets from its own state.
    pub plain: Vec<(String, String)>,
    /// The fixed variables of the tool's description (for Azure, the telemetry switch).
    pub fixed: &'static [(&'static str, &'static str)],
    /// The variable the selection asks for to isolate the tool's configuration.
    pub isolation: Vec<(String, String)>,
    /// The values read from the keychain for this call; wiped when the environment is dropped.
    pub secrets: Vec<(String, Zeroizing<String>)>,
}

impl std::fmt::Debug for EnvExtra {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvExtra")
            .field("plain", &self.plain.len())
            .field("fixed", &self.fixed.len())
            .field("isolation", &self.isolation.len())
            .field("secrets", &self.secrets.len())
            .finish()
    }
}

/// The rule for the environment of a child: the inverse of the command runner's. That runner lets a short list of names
/// through and drops everything that looks like a credential; a tool needs its credential variables and the loader and
/// the interpreters must not be hijacked, so here the inherited environment passes except what a hijacker could use.
pub struct EnvPolicy;

impl EnvPolicy {
    /// Whether the name never reaches a child: the prefixes `HOLZWAVE_`, `LD_`, `DYLD_`, `PYTHON`, `NODE_` and `GIT_`,
    /// and `BASH_ENV`, `ENV`, `IFS` and `PATH` exactly; without regard to case on Windows.
    pub fn is_denied(name: &str, os: Os) -> bool {
        let folded;
        let name = if os == Os::Windows {
            folded = name.to_ascii_uppercase();
            folded.as_str()
        } else {
            name
        };
        DENIED_PREFIXES.iter().any(|prefix| name.starts_with(prefix)) || DENIED_NAMES.contains(&name)
    }

    /// The environment of a child: the inherited variables minus the denied names (and minus entries that are not valid
    /// Unicode, have an empty name, a `=` or a NUL in a name or a NUL in a value), `PATH` replaced by `path`, then the
    /// plain variables of the profile, the fixed ones of the tool, the isolation variable and the secrets, each group
    /// winning over what came before. At most [`MAX_ENV_PAIRS`] variables.
    ///
    /// # Errors
    ///
    /// A fixed sentence when an added name is not a variable name, is denied, or is one the app sets from its own state
    /// (plain variables and secrets), when a value holds a NUL or is over [`MAX_ENV_VALUE`] bytes, or when more than
    /// [`MAX_EXTRA_PAIRS`] variables are added.
    pub fn build(
        inherited: impl IntoIterator<Item = (OsString, OsString)>,
        os: Os,
        path: String,
        extra: EnvExtra,
    ) -> Result<Env, RunError> {
        let EnvExtra { plain, fixed, isolation, secrets } = extra;
        if plain.len() + fixed.len() + isolation.len() + secrets.len() > MAX_EXTRA_PAIRS {
            return Err(RunError::TooManyVariables);
        }
        check_value(&path)?;
        for (name, value) in &plain {
            check_added(name, value, os, true)?;
        }
        for (name, value) in fixed {
            check_added(name, value, os, false)?;
        }
        for (name, value) in &isolation {
            check_added(name, value, os, false)?;
        }
        for (name, value) in &secrets {
            check_added(name, value, os, true)?;
        }

        // The key folds the case on Windows so that a later variable replaces an earlier one of the same name.
        let key = |name: &str| if os == Os::Windows { name.to_ascii_uppercase() } else { name.to_owned() };
        let mut vars: BTreeMap<String, (String, String)> = BTreeMap::new();
        let mut secret_vars: BTreeMap<String, (String, Zeroizing<String>)> = BTreeMap::new();
        let room = MAX_ENV_PAIRS - 1 - (plain.len() + fixed.len() + isolation.len() + secrets.len());
        let mut taken = 0;
        for (name, value) in inherited {
            if taken >= room {
                break;
            }
            let (Ok(name), Ok(value)) = (name.into_string(), value.into_string()) else { continue };
            if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') || Self::is_denied(&name, os) {
                continue;
            }
            vars.insert(key(&name), (name, value));
            taken += 1;
        }
        vars.insert(key("PATH"), ("PATH".to_owned(), path));
        let mut put = |name: String, value: String| {
            secret_vars.remove(&key(&name));
            vars.insert(key(&name), (name, value));
        };
        for (name, value) in plain {
            put(name, value);
        }
        for (name, value) in fixed {
            put((*name).to_owned(), (*value).to_owned());
        }
        for (name, value) in isolation {
            put(name, value);
        }
        for (name, value) in secrets {
            vars.remove(&key(&name));
            secret_vars.insert(key(&name), (name, value));
        }
        Ok(Env { vars: vars.into_values().collect(), secrets: secret_vars.into_values().collect() })
    }

    /// The names of `inherited` that a pane's environment must have removed: the denied ones, once each and sorted,
    /// except `PATH` (a launch sets its own). A name that is not valid Unicode, is empty or holds a `=` is not listed.
    pub fn removals(inherited: impl IntoIterator<Item = OsString>, os: Os) -> Vec<String> {
        let mut names: Vec<String> = inherited
            .into_iter()
            .filter_map(|name| name.into_string().ok())
            .filter(|name| !name.is_empty() && !name.contains(['=', '\0']))
            .filter(|name| !(if os == Os::Windows { name.eq_ignore_ascii_case("PATH") } else { name == "PATH" }))
            .filter(|name| Self::is_denied(name, os))
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// A value of a variable: no NUL and not over [`MAX_ENV_VALUE`] bytes.
fn check_value(value: &str) -> Result<(), RunError> {
    if value.contains('\0') || value.len() > MAX_ENV_VALUE {
        return Err(RunError::EnvValue);
    }
    Ok(())
}

/// A variable added by the profile, the tool, the isolation or the keychain: a valid name that is not denied, and (for
/// the profile and the keychain, whose names come from a synced file) not one the app sets from its own state.
fn check_added(name: &str, value: &str, os: Os, from_profile: bool) -> Result<(), RunError> {
    if EnvName::parse(name).is_err() || EnvPolicy::is_denied(name, os) {
        return Err(RunError::EnvName);
    }
    if from_profile && APP_STATE_NAMES.contains(&name) {
        return Err(RunError::EnvName);
    }
    check_value(value)
}

/// The characters a batch-file argument may have.
fn batch_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '@' | '=' | '+' | ',' | '-')
}

/// A batch file on Windows is run by the command interpreter, which reads `%`, `^`, `&`, `|`, `<`, `>`, quotes and line
/// feeds in an argument as its own syntax, and no quoting is safe. So a program that ends in `.cmd` or `.bat` (any
/// case) on Windows gets only arguments of `[A-Za-z0-9._:/@=+,-]`, at least one character each; any other program or
/// system gets any argument.
///
/// # Errors
///
/// [`RunError::BatchArgument`] for the first argument that is empty, not Unicode or has another character.
pub fn check_batch_args(program: &str, args: &[OsString], os: Os) -> Result<(), RunError> {
    let lower = program.to_ascii_lowercase();
    if os != Os::Windows || !(lower.ends_with(".cmd") || lower.ends_with(".bat")) {
        return Ok(());
    }
    for arg in args {
        match arg.to_str() {
            Some(text) if !text.is_empty() && text.chars().all(batch_char) => {}
            _ => return Err(RunError::BatchArgument),
        }
    }
    Ok(())
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

impl RunLimits {
    /// The limits brought into the allowed range: standard output from [`MIN_STDOUT`] to [`MAX_STDOUT`], the time from
    /// [`MIN_TIMEOUT`] to [`MAX_TIMEOUT`]. [`Runner::run_argv`] applies it to whatever the caller passes.
    pub fn clamped(self) -> Self {
        Self {
            max_stdout: self.max_stdout.clamp(MIN_STDOUT, MAX_STDOUT),
            timeout: self.timeout.clamp(MIN_TIMEOUT, MAX_TIMEOUT),
        }
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

impl RunResult {
    /// A result of a run that never started a process.
    fn without_output(end: RunEnd) -> Self {
        Self { stdout: Vec::new(), stderr: Vec::new(), status: None, end, stdout_dropped: false }
    }
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
    /// Four children already run and none ended within the wait.
    Busy,
    /// A variable name is not allowed: not a variable name, denied, or one the app sets from its own state.
    EnvName,
    /// A variable value holds a NUL or is too long.
    EnvValue,
    /// More variables than allowed were added.
    TooManyVariables,
    /// An argument of a batch file on Windows has a character outside the allowed set.
    BatchArgument,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotAbsolute => "the program is not given as an absolute path",
            Self::CannotStart => "the program could not be started",
            Self::Busy => "too many tools are running at once",
            Self::EnvName => "an environment variable name is not allowed",
            Self::EnvValue => "an environment variable value is not allowed",
            Self::TooManyVariables => "too many environment variables",
            Self::BatchArgument => "an argument is not allowed for a batch file",
        })
    }
}

impl std::error::Error for RunError {}

/// The slots: how many children run at once.
struct Slots {
    used: Mutex<usize>,
    freed: Condvar,
}

/// A held slot; dropping it frees the slot, on every path out of a call.
struct Slot(Arc<Slots>);

impl Slots {
    /// A slot, waiting at most `wait` for one to be freed; none when all are still held.
    fn acquire(self: &Arc<Self>, wait: Duration) -> Option<Slot> {
        let until = Instant::now() + wait;
        let mut used = self.used.lock().unwrap_or_else(PoisonError::into_inner);
        while *used >= MAX_CHILDREN {
            let left = until.checked_duration_since(Instant::now())?;
            let (guard, _) = self.freed.wait_timeout(used, left).unwrap_or_else(PoisonError::into_inner);
            used = guard;
        }
        *used += 1;
        Some(Slot(Arc::clone(self)))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut used = self.0.used.lock().unwrap_or_else(PoisonError::into_inner);
        *used = used.saturating_sub(1);
        self.0.freed.notify_one();
    }
}

/// The runner: the one place where an integration starts a child process. Clones share their slots.
#[derive(Clone)]
pub struct Runner {
    slots: Arc<Slots>,
}

impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let running = *self.slots.used.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("Runner").field("running", &running).finish()
    }
}

impl Default for Runner {
    fn default() -> Self {
        Self::new()
    }
}

impl Runner {
    /// A runner with all of its slots free.
    pub fn new() -> Self {
        Self { slots: Arc::new(Slots { used: Mutex::new(0), freed: Condvar::new() }) }
    }

    /// Runs `program` (an absolute path, taken as given) with `args` and nothing else: no shell, standard input closed,
    /// the child leading a process group of its own, the environment exactly `env`. Blocks until the program ended,
    /// the time limit passed, standard output passed its cap or `cancel` was raised; the last three kill the whole
    /// process group. `env` is dropped, and its secrets wiped, right after the start.
    ///
    /// # Errors
    ///
    /// A [`RunError`] with a fixed sentence: the program is not an absolute path, it cannot be started, or four
    /// children already run and none ended within [`SLOT_WAIT`] ([`RunError::Busy`]; the caller tries again later).
    pub fn run_argv(
        &self,
        program: &Path,
        args: &[OsString],
        env: Env,
        limits: &RunLimits,
        cancel: &RunCancel,
    ) -> Result<RunResult, RunError> {
        if !program.is_absolute() {
            return Err(RunError::NotAbsolute);
        }
        check_batch_args(&program.to_string_lossy(), args, Os::current())?;
        let limits = limits.clamped();
        if cancel.is_cancelled() {
            return Ok(RunResult::without_output(RunEnd::Cancelled));
        }
        let _slot = self.slots.acquire(SLOT_WAIT).ok_or(RunError::Busy)?;
        let mut command = Command::new(program);
        command.args(args).env_clear().stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (name, value) in &env.vars {
            command.env(name, value);
        }
        for (name, value) in &env.secrets {
            command.env(name, value.as_str());
        }
        isolate(&mut command);
        let spawned = command.spawn();
        drop(command);
        drop(env);
        let mut child = spawned.map_err(|_| RunError::CannotStart)?;
        let pid = child.id();
        let out = Arc::new(Mutex::new(Head::new(limits.max_stdout)));
        let err = Arc::new(Mutex::new(Head::new(STDERR_KEPT)));
        let made = (
            child.stdout.take().and_then(|pipe| reader("hw-run-out", pipe, Arc::clone(&out))),
            child.stderr.take().and_then(|pipe| reader("hw-run-err", pipe, Arc::clone(&err))),
        );
        let (Some(out_reader), Some(err_reader)) = made else {
            // A program whose output cannot be read is not run: it ends before it can write what nobody would see.
            stop(&mut child, pid);
            return Err(RunError::CannotStart);
        };
        let readers = [out_reader, err_reader];

        // The time limit is the program's own run time: it starts when the program does.
        let deadline = Instant::now() + limits.timeout;
        let (mut end, status) = loop {
            match child.try_wait() {
                Ok(Some(done)) => break (RunEnd::Finished, exit_code(&done)),
                Ok(None) => {}
                Err(_) => {
                    stop(&mut child, pid);
                    break (RunEnd::Lost, None);
                }
            }
            if out.lock().unwrap_or_else(PoisonError::into_inner).overflow {
                stop(&mut child, pid);
                break (RunEnd::Overflow, None);
            }
            if cancel.is_cancelled() {
                stop(&mut child, pid);
                break (RunEnd::Cancelled, None);
            }
            if Instant::now() >= deadline {
                stop(&mut child, pid);
                break (RunEnd::TimedOut, None);
            }
            thread::sleep(POLL);
        };

        // After a normal end the readers get a moment to reach the end of their pipes; a helper that still holds a pipe
        // keeps them blocked, and the group is killed then. After a stop the group is dead already.
        let mut given_up = false;
        if end == RunEnd::Finished {
            if !wait_for(&readers, DRAIN) {
                kill_tree(pid);
                given_up = !wait_for(&readers, REAP);
            }
        } else {
            given_up = !wait_for(&readers, REAP);
        }
        for handle in readers {
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
        let (stdout, stdout_dropped) = take(&out);
        let (stderr, _) = take(&err);
        if end == RunEnd::Finished {
            // The program ended and the cap was passed in the same moment, or its output could not be read to the end.
            if stdout_dropped {
                end = RunEnd::Overflow;
            } else if given_up {
                end = RunEnd::Lost;
            }
        }
        let status = if end == RunEnd::Finished { status } else { None };
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

/// Waits until every reader has finished or `limit` has passed; answers whether they all finished.
fn wait_for(readers: &[JoinHandle<()>], limit: Duration) -> bool {
    let until = Instant::now() + limit;
    loop {
        if readers.iter().all(JoinHandle::is_finished) {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        thread::sleep(POLL);
    }
}
