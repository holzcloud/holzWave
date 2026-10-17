//! The local runner of the command tool: one command in a separate process, with a scrubbed environment, no input,
//! bounded output, a timeout, a stop that can always be raised and a kill of everything the command started.
//!
//! Nothing here names a pane, a terminal or the network. The caller passes the command string and the folder exactly as
//! the person approved them; this module never interprets, rewrites or classifies the string.
//!
//! - **A fixed interpreter.** `/bin/sh -c -- <command>` on macOS and Linux, Windows PowerShell with `-NoProfile` and
//!   `-EncodedCommand` on Windows. Never the user's login shell, never an interactive shell, never a profile or rc file,
//!   so a file that exports a key is not read.
//! - **Nothing of the session is inherited.** The environment is built from an allow-list of names plus `TERM=dumb`
//!   and `PWD`; a name that carries a secret word never passes, even when it is listed. Standard input is closed, and
//!   standard output and error are pipes read by two threads into bounded tails.
//! - **A bounded, stoppable run.** The run polls for the end of the command, its time limit and the stop switch every
//!   [`POLL`]. A time out or a stop kills the whole process group (on Windows the process tree) before the leader is
//!   reaped; after a normal end the readers get [`DRAIN`] to reach the end of their pipes, then the group is killed, then
//!   they get [`REAP`], and a reader that is still blocked is released and the result is marked truncated.
//!
//! The limits, stated honestly: a process that leaves its group (a daemon that calls `setsid`) escapes the kill, and
//! nothing here restricts what a command can do with the rights of the account that runs it. The group and the tree are
//! killed through the system's own tools in [`kill_tree`], one function for each family of systems, so no signal call and
//! no job object is needed.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::ssh::{ExecCancel, ExecEnd, ExecLimits, ExecResult, MAX_EXEC_COMMAND};
use crate::{Error, Result};

/// The names that reach a command on Unix: the places it looks for programs and its identity, its language and its time
/// zone. The one place to extend the list.
#[cfg(not(windows))]
pub const ALLOWED_ENV: &[&str] = &["PATH", "HOME", "USER", "LOGNAME", "LANG", "LANGUAGE", "TZ", "TMPDIR"];

/// The names that reach a command on Windows (compared without case): the variables the system itself needs.
#[cfg(windows)]
pub const ALLOWED_ENV: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
    "TEMP",
    "TMP",
    "USERNAME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "COMPUTERNAME",
    "OS",
    "PROCESSOR_ARCHITECTURE",
    "NUMBER_OF_PROCESSORS",
];

/// The prefixes of names that reach a command: the locale variables of Unix.
#[cfg(not(windows))]
pub const ALLOWED_ENV_PREFIXES: &[&str] = &["LC_"];

/// Windows has no prefix of names that is passed on.
#[cfg(windows)]
pub const ALLOWED_ENV_PREFIXES: &[&str] = &[];

/// A name that contains one of these words (compared without case) never reaches a command, even when it is listed or
/// prefixed.
pub const SECRET_WORDS: &[&str] = &["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL", "AUTH"];

/// The most variables a runner keeps.
pub const MAX_ENV_PAIRS: usize = 64;

/// The one line that sets UTF-8 output before the command, in the script PowerShell is given on Windows.
pub const WINDOWS_PRELUDE: &str = "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8";

/// How often the run looks at the command, its time limit and the stop switch.
const POLL: Duration = Duration::from_millis(10);
/// How long the readers get to reach the end of their pipes after the command ended.
const DRAIN: Duration = Duration::from_millis(250);
/// How long the readers get after the group was killed, before they are released.
const REAP: Duration = Duration::from_millis(500);
/// The size of one read from a pipe.
const CHUNK: usize = 16 * 1024;

const EMPTY: &str = "exec: the command is empty";
const NUL: &str = "exec: the command holds a NUL byte";
const TOO_LONG: &str = "exec: the command is longer than the limit";
const NOT_A_FOLDER: &str = "exec: the working directory is not a folder";
const CANNOT_START: &str = "exec: cannot start the command";

/// Whether a command may inherit the variable `name`: it is listed or has a listed prefix, and it carries no secret word.
/// On Unix a listed name must match exactly in case; on Windows names are compared without case.
pub fn env_allowed(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    if SECRET_WORDS.iter().any(|word| upper.contains(word)) {
        return false;
    }
    let same = |listed: &str| if cfg!(windows) { listed.eq_ignore_ascii_case(name) } else { listed == name };
    if ALLOWED_ENV.iter().any(|listed| same(listed)) {
        return true;
    }
    ALLOWED_ENV_PREFIXES.iter().any(|prefix| {
        if cfg!(windows) {
            upper.starts_with(&prefix.to_ascii_uppercase())
        } else {
            name.starts_with(prefix)
        }
    })
}

/// The runner. It holds the variables a command may see and nothing else. `Debug` prints the number of pairs only.
#[derive(Clone)]
pub struct LocalExec {
    env: Vec<(String, String)>,
}

impl std::fmt::Debug for LocalExec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalExec").field("pairs", &self.env.len()).finish()
    }
}

impl LocalExec {
    /// A runner that keeps the pairs of `base` that [`env_allowed`] lets through, at most [`MAX_ENV_PAIRS`]. A pair whose
    /// name or value is not valid Unicode is dropped.
    pub fn new(base: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        let mut env = Vec::new();
        for (name, value) in base {
            if env.len() >= MAX_ENV_PAIRS {
                break;
            }
            let (Ok(name), Ok(value)) = (name.into_string(), value.into_string()) else { continue };
            if env_allowed(&name) && !name.is_empty() && !name.contains('=') && !name.contains('\0') && !value.contains('\0') {
                env.push((name, value));
            }
        }
        Self { env }
    }

    /// A runner over the environment of this process: the one place that reads it.
    pub fn from_process() -> Self {
        Self::new(std::env::vars_os())
    }

    /// The names of the kept variables, sorted. For tests.
    pub fn env_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.env.iter().map(|(name, _)| name.clone()).collect();
        names.sort();
        names
    }

    /// Runs `command` in `cwd` and waits for it.
    ///
    /// Blocks until the command ended, `limits.timeout` passed or `cancel` was raised; call it from a thread of its own.
    /// A time out and a stop end the command and everything it started. The output is raw bytes, the last
    /// `limits.max_output` of each stream; the caller cleans it before it shows or sends it.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] with a fixed text that never holds the command or the folder: the command is empty, holds a
    /// NUL byte or is longer than [`MAX_EXEC_COMMAND`], the working directory is not a folder, or the process could not be
    /// started.
    pub fn exec(&self, command: &str, cwd: &Path, limits: &ExecLimits, cancel: &ExecCancel) -> Result<ExecResult> {
        if command.trim().is_empty() {
            return Err(Error::Unsupported(EMPTY.to_owned()));
        }
        if command.contains('\0') {
            return Err(Error::Unsupported(NUL.to_owned()));
        }
        if command.len() > MAX_EXEC_COMMAND {
            return Err(Error::Unsupported(TOO_LONG.to_owned()));
        }
        if !cwd.is_dir() {
            return Err(Error::Unsupported(NOT_A_FOLDER.to_owned()));
        }
        if cancel.is_cancelled() {
            return Ok(ExecResult {
                stdout: Vec::new(),
                stderr: Vec::new(),
                status: None,
                end: ExecEnd::Cancelled,
                truncated: false,
                dropped: 0,
            });
        }
        let limits = limits.clamped();
        let mut child = self
            .command(command, cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| Error::Unsupported(CANNOT_START.to_owned()))?;
        let pid = child.id();
        let out_tail = Arc::new(Mutex::new(Tail::new(limits.max_output)));
        let err_tail = Arc::new(Mutex::new(Tail::new(limits.max_output)));
        let made = (
            child.stdout.take().and_then(|pipe| reader("hw-exec-out", pipe, Arc::clone(&out_tail))),
            child.stderr.take().and_then(|pipe| reader("hw-exec-err", pipe, Arc::clone(&err_tail))),
        );
        let (Some(out_reader), Some(err_reader)) = made else {
            // A command that cannot be read is not run: it is ended before it can write anything that nobody would see.
            stop(&mut child, pid);
            return Err(Error::Unsupported(CANNOT_START.to_owned()));
        };
        let readers = vec![out_reader, err_reader];

        let deadline = Instant::now() + limits.timeout;
        let (end, status) = loop {
            match child.try_wait() {
                Ok(Some(done)) => break (ExecEnd::Finished, exit_code(&done)),
                Ok(None) => {}
                Err(_) => {
                    kill_tree(pid);
                    let _ = child.kill();
                    let _ = child.wait();
                    break (ExecEnd::Lost, None);
                }
            }
            if cancel.is_cancelled() {
                stop(&mut child, pid);
                break (ExecEnd::Cancelled, None);
            }
            if Instant::now() >= deadline {
                stop(&mut child, pid);
                break (ExecEnd::TimedOut, None);
            }
            thread::sleep(POLL);
        };

        // After a normal end the readers get a moment to reach the end of their pipes; a helper that still holds a pipe
        // keeps them blocked, and the group is killed then. After a stop the group is dead already.
        let mut released = false;
        if end == ExecEnd::Finished {
            if !wait_for(&readers, DRAIN) {
                kill_tree(pid);
                released = !wait_for(&readers, REAP);
            }
        } else {
            released = !wait_for(&readers, REAP);
        }
        // A reader that finished is joined; one that is still blocked is released: its handle is dropped, and it ends when
        // the pipe closes. What it read so far is already in the tail.
        for handle in readers {
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
        let (stdout, out_dropped) = take(&out_tail);
        let (stderr, err_dropped) = take(&err_tail);
        let dropped = out_dropped.saturating_add(err_dropped);
        Ok(ExecResult { stdout, stderr, status, end, truncated: dropped > 0 || released, dropped })
    }

    /// The command to start: the fixed interpreter, the scrubbed environment and the folder.
    fn command(&self, command: &str, cwd: &Path) -> Command {
        let mut child = interpreter(command, &self.env);
        child.env_clear();
        for (name, value) in &self.env {
            child.env(name, value);
        }
        child.env("TERM", "dumb");
        child.env("PWD", cwd.as_os_str());
        child.current_dir(cwd);
        isolate(&mut child);
        child
    }
}

/// The interpreter with its arguments: `/bin/sh -c -- <command>`.
#[cfg(not(windows))]
fn interpreter(command: &str, _env: &[(String, String)]) -> Command {
    let mut child = Command::new("/bin/sh");
    child.arg("-c").arg("--").arg(command);
    child
}

/// The interpreter with its arguments: Windows PowerShell from the system folder, with no profile and no prompt, running
/// the encoded script.
#[cfg(windows)]
fn interpreter(command: &str, env: &[(String, String)]) -> Command {
    let mut child = Command::new(format!("{}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", system_root(env)));
    child.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand"]);
    child.arg(crate::winshell::encode_command(&format!("{WINDOWS_PRELUDE}\n{command}")));
    child
}

/// The Windows folder: the kept `SystemRoot`, or `C:\Windows`.
#[cfg(windows)]
fn system_root(env: &[(String, String)]) -> String {
    env.iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("SystemRoot"))
        .map_or_else(|| "C:\\Windows".to_owned(), |(_, value)| value.clone())
}

/// The child leads a process group of its own, so the group can be killed as a whole.
#[cfg(not(windows))]
fn isolate(child: &mut Command) {
    use std::os::unix::process::CommandExt;
    child.process_group(0);
}

/// No console window appears for the child.
#[cfg(windows)]
fn isolate(child: &mut Command) {
    use std::os::windows::process::CommandExt;
    child.creation_flags(0x0800_0000);
}

/// Kills the group of the process `pid` through the system's `kill`, while the leader is still unreaped, so the group id
/// cannot have been reused. The process id is a number this code made itself, never text of the model. Answers whether
/// the helper could be run.
#[cfg(not(windows))]
fn kill_tree(pid: u32) -> bool {
    Command::new("/bin/sh")
        .args(["-c", "kill -s KILL -- \"-$1\"", "sh", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Kills the process `pid` and its children through `taskkill` from the system folder. Answers whether the helper could be
/// run.
#[cfg(windows)]
fn kill_tree(pid: u32) -> bool {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_owned());
    let mut helper = Command::new(format!("{root}\\System32\\taskkill.exe"));
    helper.args(["/PID", &pid.to_string(), "/T", "/F"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    isolate(&mut helper);
    helper.status().is_ok()
}

/// Ends the command and what it started, and reaps the leader.
fn stop(child: &mut std::process::Child, pid: u32) {
    kill_tree(pid);
    let _ = child.kill();
    let _ = child.wait();
}

/// The exit code of a command: the code, or 128 plus the signal that ended it.
fn exit_code(status: &std::process::ExitStatus) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return u32::try_from(signal).ok().map(|n| 128 + n);
        }
    }
    status.code().map(|code| u32::from_ne_bytes(code.to_ne_bytes()))
}

/// A thread that copies a pipe into a bounded tail until the pipe ends; none when the system refuses the thread.
fn reader(name: &str, mut pipe: impl Read + Send + 'static, tail: Arc<Mutex<Tail>>) -> Option<JoinHandle<()>> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let mut buffer = vec![0u8; CHUNK];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => tail.lock().unwrap_or_else(PoisonError::into_inner).push(&buffer[..n]),
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

/// The bytes a tail holds and the count of the bytes it dropped.
fn take(tail: &Mutex<Tail>) -> (Vec<u8>, u64) {
    let mut guard = tail.lock().unwrap_or_else(PoisonError::into_inner);
    let dropped = guard.dropped;
    (Vec::from(std::mem::take(&mut guard.bytes)), dropped)
}

/// The last `cap` bytes of everything pushed, and the count of the bytes that fell off the front.
struct Tail {
    cap: usize,
    bytes: VecDeque<u8>,
    dropped: u64,
}

impl Tail {
    fn new(cap: usize) -> Self {
        Self { cap, bytes: VecDeque::new(), dropped: 0 }
    }

    fn push(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        if chunk.len() >= self.cap {
            let lost = self.bytes.len() + (chunk.len() - self.cap);
            self.dropped = self.dropped.saturating_add(lost as u64);
            self.bytes.clear();
            self.bytes.extend(&chunk[chunk.len() - self.cap..]);
            return;
        }
        let excess = (self.bytes.len() + chunk.len()).saturating_sub(self.cap);
        if excess > 0 {
            self.bytes.drain(..excess);
            self.dropped = self.dropped.saturating_add(excess as u64);
        }
        self.bytes.extend(chunk);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tail_keeps_the_last_bytes_and_counts_the_rest() {
        let mut tail = Tail::new(4);
        tail.push(b"ab");
        tail.push(b"cdef");
        tail.push(b"g");
        assert_eq!(Vec::from(tail.bytes.clone()), b"defg");
        assert_eq!(tail.dropped, 3);
    }

    #[test]
    fn the_secret_words_win_over_the_list() {
        assert!(env_allowed("PATH"));
        assert!(!env_allowed("PATH_KEY"));
        assert!(!env_allowed("HOLZWAVE_IPC"));
    }
}
