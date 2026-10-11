//! A pseudo-terminal running a child process, on top of `portable-pty`.
//!
//! [`Pty`] owns the master side and the child. A pane starts one with [`SpawnOptions`], moves the reader (and
//! usually the writer) to its own threads and, when the pane closes, calls [`Pty::close`], which really ends the
//! child. [`process_cwd`] reads where a running shell currently is, for the session file.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::{Error, Result};

fn pty<E: std::fmt::Display>(e: E) -> Error {
    Error::Pty(e.to_string())
}

/// The locale a child gets when the parent has none: a UTF-8 `LC_CTYPE`. A Finder-launched app has no `LANG`,
/// and a shell without a UTF-8 locale mangles every non-ASCII character.
const UTF8_LOCALE: &str = if cfg!(target_os = "macos") { "en_US.UTF-8" } else { "C.UTF-8" };

/// How a pane's child process is started.
///
/// Built with [`SpawnOptions::new`] and the setters, then passed to [`Pty::spawn_with`]:
///
/// ```ignore
/// let options = SpawnOptions::new("/bin/zsh", 80, 24).args(["-l"]).cwd("/Users/me/project").cell_px(9, 18);
/// ```
#[derive(Debug, Clone)]
pub struct SpawnOptions {
    program: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    cols: u16,
    rows: u16,
    cell_px: (u32, u32),
    env: Vec<(String, String)>,
}

impl SpawnOptions {
    /// Start `program` in a PTY of `cols` x `rows` cells, without arguments, in the parent's default directory.
    pub fn new(program: impl Into<String>, cols: u16, rows: u16) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            cols,
            rows,
            cell_px: (0, 0),
            env: Vec::new(),
        }
    }

    /// The program's arguments (not including the program itself). Replaces earlier arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// The directory the child starts in. A directory that does not exist is not an error: the child then starts
    /// in the user's home directory (portable-pty's rule).
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// The size of one cell in pixels. It is reported to the child as the PTY's pixel size (`cols * width` by
    /// `rows * height`), which programs that draw images read. Zero means unknown.
    pub fn cell_px(mut self, width: u32, height: u32) -> Self {
        self.cell_px = (width, height);
        self
    }

    /// An extra environment variable for the child. It is applied after the defaults of
    /// [`child_env`](Self::child_env), so it overrides them, and a later pair overrides an earlier one.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// The variables set for the child on top of the parent's environment, in the order they are applied.
    ///
    /// `parent` looks up a variable of the parent process. The list is:
    ///
    /// 1. `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=holzWave` and `TERM_PROGRAM_VERSION` with
    ///    this crate's version;
    /// 2. a UTF-8 `LC_CTYPE`, only when the parent has none of `LANG`, `LC_ALL` and `LC_CTYPE` (unset or empty);
    ///    a parent that has one is left alone;
    /// 3. the caller's pairs from [`env`](Self::env), which therefore win.
    pub fn child_env(&self, parent: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = vec![
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
            ("TERM_PROGRAM".into(), "holzWave".into()),
            ("TERM_PROGRAM_VERSION".into(), env!("CARGO_PKG_VERSION").into()),
        ];
        let has_locale = ["LANG", "LC_ALL", "LC_CTYPE"]
            .iter()
            .any(|key| parent(key).is_some_and(|value| !value.is_empty()));
        if !has_locale {
            env.push(("LC_CTYPE".into(), UTF8_LOCALE.into()));
        }
        env.extend(self.env.iter().cloned());
        env
    }

    /// The PTY size: the grid in cells, and the pixel size derived from the cell size.
    fn pty_size(&self) -> PtySize {
        pty_size(
            self.cols,
            self.rows,
            self.cell_px.0.saturating_mul(u32::from(self.cols)),
            self.cell_px.1.saturating_mul(u32::from(self.rows)),
        )
    }
}

fn pty_size(cols: u16, rows: u16, px_w: u32, px_h: u32) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: u16::try_from(px_w).unwrap_or(u16::MAX),
        pixel_height: u16::try_from(px_h).unwrap_or(u16::MAX),
    }
}

/// The current working directory of the process with this id, when it can be read.
///
/// Linux reads the `/proc/<pid>/cwd` link. macOS runs `/usr/sbin/lsof -a -p <pid> -d cwd -Fn` by absolute path,
/// as an argument vector without a shell, and takes the `n` line; the result is only ever used as a [`PathBuf`].
/// A process that does not exist, one that belongs to someone else, or any other platform gives `None`.
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    // A pid_t is an i32; 0 would mean "the calling process group" to some tools. Neither is a real child.
    if pid == 0 || i32::try_from(pid).is_err() {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }
    #[cfg(target_os = "macos")]
    {
        // `-b` keeps lsof from calling stat and readlink on the files it reports: those block for good on a stale
        // network mount, which is where a shell's directory tends to be when something is wrong. The exit status
        // is not read: `-b` makes lsof warn about what it skipped, and a missing process gives no `n` line.
        let mut command = std::process::Command::new("/usr/sbin/lsof");
        command.args(["-b", "-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"]).env_clear();
        parse_lsof_cwd(&output_within(command, LOOKUP_DEADLINE)?)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// The longest a helper process of [`process_cwd`] may run before it is given up.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const LOOKUP_DEADLINE: Duration = Duration::from_millis(750);

/// The most output of a helper process that is read. It is far above what `lsof -Fn` of one directory prints.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const LOOKUP_OUTPUT_LIMIT: u64 = 64 * 1024;

/// Run `command` and return what it wrote to stdout, or `None` when it could not be started or did not end within
/// `deadline`. Stdin and stderr are closed.
///
/// A child that is over time is killed and reaped on a short-lived thread, so the caller never waits for it: a
/// process in an uninterruptible kernel call does not die on a signal, and waiting for it would be the hang this
/// function exists to avoid.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn output_within(mut command: Command, deadline: Duration) -> Option<Vec<u8>> {
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    let reader = thread::Builder::new().name("holzwave-lookup".into()).spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.take(LOOKUP_OUTPUT_LIMIT).read_to_end(&mut out);
        let _ = tx.send(out);
    });
    let output = match reader {
        Ok(_) => rx.recv_timeout(deadline).ok(),
        Err(_) => None,
    };
    // After the end of its output a child is gone or about to be; one that is not is killed like an overdue one.
    if output.is_none() || !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.kill();
        // Reaped on its own thread. Without a thread the child stays a zombie until the app ends, which is the
        // lesser harm.
        let _ = thread::Builder::new().name("holzwave-reap".into()).spawn(move || {
            let _ = child.wait();
        });
    }
    output
}

/// The path from the first `n` line of `lsof -Fn` output: lines are tagged by their first byte (`p` pid, `f`
/// descriptor, `n` name). Only an absolute path counts.
#[cfg(target_os = "macos")]
fn parse_lsof_cwd(output: &[u8]) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    output
        .split(|b| *b == b'\n')
        .find(|line| line.starts_with(b"n/"))
        .map(|line| PathBuf::from(OsString::from_vec(line[1..].to_vec())))
}

pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    /// `None` once the pane handed the writer to its writer thread with [`Pty::take_writer`].
    writer: Option<Box<dyn Write + Send>>,
    reader: Option<Box<dyn Read + Send>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Pty {
    /// Start `program` with `args` in a new PTY of the given size. See [`SpawnOptions`] for the other settings.
    pub fn spawn(program: &str, args: &[&str], cols: u16, rows: u16) -> Result<Self> {
        Self::spawn_with(&SpawnOptions::new(program, cols, rows).args(args.iter().copied()))
    }

    /// Start a child as described by `options`: its working directory, the pixel size and the environment of
    /// [`SpawnOptions::child_env`] applied to the parent's environment.
    pub fn spawn_with(options: &SpawnOptions) -> Result<Self> {
        let pair = native_pty_system().openpty(options.pty_size()).map_err(pty)?;
        let mut cmd = CommandBuilder::new(&options.program);
        cmd.args(&options.args);
        if let Some(dir) = &options.cwd {
            cmd.cwd(dir);
        }
        let parent = |key: &str| std::env::var_os(key).map(|v| v.to_string_lossy().into_owned());
        for (key, value) in options.child_env(parent) {
            cmd.env(key, value);
        }
        let child = pair.slave.spawn_command(cmd).map_err(pty)?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().map_err(pty)?;
        let writer = pair.master.take_writer().map_err(pty)?;
        Ok(Self { master: pair.master, writer: Some(writer), reader: Some(reader), child })
    }

    /// The read end. It can be taken once, to move it to a reader thread.
    pub fn take_reader(&mut self) -> Option<Box<dyn Read + Send>> {
        self.reader.take()
    }

    /// The write end. It can be taken once, to move it to a writer thread that may block on a full PTY; after
    /// that [`Pty::write`] returns an error.
    ///
    /// Drop the writer only after [`Pty::close`] has returned: its `Drop` writes a newline and EOF into the PTY,
    /// which answers a live program's prompt.
    pub fn take_writer(&mut self) -> Option<Box<dyn Write + Send>> {
        self.writer.take()
    }

    /// Write to the child's input. Fails with [`Error::Pty`] after [`Pty::take_writer`].
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| Error::Pty("the writer was handed to a writer thread".into()))?;
        writer.write_all(bytes)?;
        writer.flush()?;
        Ok(())
    }

    /// The operating system's id of the child, while it is known.
    pub fn process_id(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// Resize the PTY to `cols` x `rows` cells without a pixel size.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.resize_px(cols, rows, 0, 0)
    }

    /// Resize the PTY and report its size in pixels (the whole grid, `px_w` x `px_h`) to the child. Values above
    /// 65535 are clamped, which is the most the kernel's `winsize` can hold.
    pub fn resize_px(&self, cols: u16, rows: u16, px_w: u32, px_h: u32) -> Result<()> {
        self.master.resize(pty_size(cols, rows, px_w, px_h)).map_err(pty)
    }

    /// The exit code if the child has exited, `None` while it runs. Does not block.
    pub fn try_wait(&mut self) -> Result<Option<u32>> {
        Ok(self.child.try_wait()?.map(|status| status.exit_code()))
    }

    /// Wait for the child to exit and return its exit code.
    pub fn wait(&mut self) -> Result<u32> {
        Ok(self.child.wait()?.exit_code())
    }

    /// End the child, reap it and release the PTY. Returns the child's exit code.
    ///
    /// A child that has not exited gets SIGHUP, a short grace period and then SIGKILL (portable-pty's
    /// `Child::kill`); `close` always waits afterwards, so no zombie remains, even for a shell that ignores
    /// SIGHUP. The writer and the master are dropped only after the child is gone, in that order: the writer's
    /// `Drop` writes a newline and EOF into the PTY, which would answer a live program's prompt. A writer that was
    /// handed out with [`Pty::take_writer`] must be dropped by its owner after this call.
    ///
    /// This blocks for up to about 200 ms while the child is given time to leave. Never call it on the Tauri main
    /// thread.
    pub fn close(mut self) -> Result<u32> {
        // A child that was already reaped must not be signalled: its pid may belong to someone else by now.
        let exited = self.child.try_wait().ok().flatten();
        let outcome = match exited {
            Some(status) => Ok(status.exit_code()),
            None => {
                let _ = self.child.kill();
                self.child.wait().map(|status| status.exit_code())
            }
        };
        // The child is gone (or could not be waited for): only now release the PTY.
        drop(self.writer);
        drop(self.master);
        drop(self.reader);
        Ok(outcome?)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::parse_lsof_cwd;
    use std::path::PathBuf;

    #[test]
    fn the_name_line_of_lsof_output_is_the_working_directory() {
        let out = b"p1234\nfcwd\nn/Users/me/with space\n";
        assert_eq!(parse_lsof_cwd(out), Some(PathBuf::from("/Users/me/with space")));
    }

    #[test]
    fn output_without_an_absolute_name_line_is_none() {
        assert_eq!(parse_lsof_cwd(b""), None);
        assert_eq!(parse_lsof_cwd(b"p1234\nfcwd\n"), None);
        assert_eq!(parse_lsof_cwd(b"p1234\nfcwd\nnrelative\n"), None);
    }
}

#[cfg(all(test, unix))]
mod lookup_tests {
    use super::output_within;
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[test]
    fn the_output_of_a_helper_that_ends_in_time_is_returned() {
        let mut command = Command::new("/bin/echo");
        command.arg("hello");
        assert_eq!(output_within(command, Duration::from_secs(5)), Some(b"hello\n".to_vec()));
    }

    #[test]
    fn a_helper_that_does_not_end_in_time_is_given_up_without_waiting_for_it() {
        // `sleep` stands for an lsof that hangs on a stale mount (WR-04).
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        let start = Instant::now();
        assert_eq!(output_within(command, Duration::from_millis(200)), None);
        assert!(start.elapsed() < Duration::from_secs(5), "waited {:?} for a helper that hangs", start.elapsed());
    }

    #[test]
    fn a_helper_that_cannot_be_started_gives_none() {
        let command = Command::new("/nonexistent/holzwave-helper");
        assert_eq!(output_within(command, Duration::from_secs(1)), None);
    }
}
