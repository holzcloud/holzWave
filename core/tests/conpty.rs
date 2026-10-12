//! Real Windows panes: cmd.exe and PowerShell through the real ConPTY, never a mock. This is the Windows counterpart of
//! the Unix-only `pty.rs`, `pane.rs` and `replies.rs`, and the part of plan 02.2-04 that only a Windows host can run.
//!
//! The owner runs it on Windows with
//!
//! ```text
//! cargo test -p holzwave-core --test conpty -- --nocapture
//! ```
//!
//! On macOS and Linux the file is empty (`running 0 tests`); `sh scripts/cross-check.sh core` compiles it for
//! `x86_64-pc-windows-gnu`, which is the only proof that can be had without Windows. Each case says what it was
//! waiting for when it fails, and maps to a row of the Windows test plan (`docs/test-plan.md`, written by plan 07):
//!
//! | Test                                              | Row | What it shows                                          |
//! |---------------------------------------------------|-----|--------------------------------------------------------|
//! | `cmd_prints_a_word_and_exits_with_code_0`         | W1  | output arrives, the exit code is read, nothing hangs   |
//! | `powershell_exits_with_its_own_code`              | W1  | the same for Windows PowerShell                        |
//! | `resize_works_on_a_running_cmd`                   | W5  | `ResizePseudoConsole` does not fail, input still works |
//! | `closing_a_running_cmd_returns_quickly_and_leaves_no_process` | W3 | close does not deadlock and leaves no shell |
//! | `a_cmd_pane_shows_typed_text_and_ends_after_exit` | W2  | the pane ends within seconds of `exit` (exit poll)     |
//! | `a_powershell_pane_announces_its_directory`       | W7  | the prompt wrapper runs and conhost passes OSC 7 on    |
//!
//! Every wait is bounded and every pane or PTY is closed at the end of its test.
#![cfg(windows)]
#![cfg(feature = "ghostty")]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use holzwave_core::engine::{DomKey, Mods};
use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec};
use holzwave_core::platform::{self, Os};
use holzwave_core::pty::Pty;
use holzwave_core::wire::RowMsg;

/// Starting a console host and a shell takes a moment on a cold machine; nothing here waits less than this for output.
const WAIT: Duration = Duration::from_secs(20);

/// Poll `f` every 10 ms until it is true or `timeout` has passed.
fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if f() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---- a raw PTY ---------------------------------------------------------------------------------------

/// What the reader thread of a raw PTY forwards.
enum Event {
    Data(Vec<u8>),
    End,
}

/// A `Pty` with its reader on a thread. The thread answers the cursor position request (`ESC [ 6 n`) that the
/// pseudo console sends at start (`PSEUDOCONSOLE_INHERIT_CURSOR`): without the answer conhost stops reading input,
/// which a real pane gets from its engine. It keeps reading until the end, as a pane's reader does, because before
/// Windows 11 24H2 closing a pseudo console waits for its output to be drained.
struct Raw {
    pty: Option<Pty>,
    rx: Receiver<Event>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    seen: Vec<u8>,
}

impl Raw {
    fn start(program: &str, args: &[&str]) -> Raw {
        let mut pty = Pty::spawn(program, args, 100, 30).unwrap_or_else(|e| panic!("{program} did not start: {e}"));
        let mut reader = pty.take_reader().expect("a reader");
        let writer = Arc::new(Mutex::new(pty.take_writer().expect("a writer")));
        let (tx, rx) = mpsc::channel();
        let answer = writer.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(Event::End);
                        return;
                    }
                    Ok(n) => {
                        if buf[..n].windows(4).any(|w| w == b"\x1b[6n") {
                            let mut out = answer.lock().unwrap();
                            let _ = out.write_all(b"\x1b[1;1R").and_then(|()| out.flush());
                        }
                        if tx.send(Event::Data(buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Raw { pty: Some(pty), rx, writer, seen: Vec::new() }
    }

    fn send(&self, text: &str) {
        let mut out = self.writer.lock().unwrap();
        out.write_all(text.as_bytes()).and_then(|()| out.flush()).expect("the PTY accepts input");
    }

    /// Everything read so far, as text.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.seen).into_owned()
    }

    /// Read until the output contains `needle`; panics after [`WAIT`] with what was seen.
    fn expect(&mut self, needle: &str) {
        let deadline = Instant::now() + WAIT;
        while !self.text().contains(needle) {
            match self.rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Event::Data(bytes)) => self.seen.extend(bytes),
                Ok(Event::End) => panic!("the output ended before {needle:?} appeared: {:?}", self.text()),
                Err(_) => panic!("no {needle:?} within {WAIT:?}: {:?}", self.text()),
            }
        }
    }

    /// The exit code, waiting up to [`WAIT`] for the child to end.
    fn exit_code(&mut self) -> u32 {
        let pty = self.pty.as_mut().expect("the PTY is open");
        let mut code = None;
        let ended = wait_until(WAIT, || {
            code = pty.try_wait().expect("try_wait works");
            code.is_some()
        });
        assert!(ended, "the child did not end within {WAIT:?}; output so far: {:?}", self.text());
        code.unwrap()
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        // A failed assertion must not leave a shell behind. Dropped here, after `close` (when it ran), in the order
        // the PTY needs: the child first.
        if let Some(pty) = self.pty.take() {
            let _ = pty.close();
        }
    }
}

/// Whether a process with this id is still running, asked of `tasklist` (the filter prints an INFO line when none).
fn process_exists(pid: u32) -> bool {
    let out = Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"]).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\""))).unwrap_or(false)
}

#[test]
fn cmd_prints_a_word_and_exits_with_code_0() {
    let mut raw = Raw::start("cmd.exe", &["/c", "echo holzwave"]);
    raw.expect("holzwave");
    assert_eq!(raw.exit_code(), 0);
}

#[test]
fn powershell_exits_with_its_own_code() {
    let mut raw = Raw::start("powershell.exe", &["-NoProfile", "-Command", "exit 7"]);
    assert_eq!(raw.exit_code(), 7);
}

#[test]
fn resize_works_on_a_running_cmd() {
    let mut raw = Raw::start("cmd.exe", &["/k"]);
    // `set /a` prints the sum, which the typed line does not contain.
    raw.send("set /a 40+2\r");
    raw.expect("42");

    let pty = raw.pty.as_ref().expect("the PTY is open");
    pty.resize_px(120, 40, 0, 0).expect("the first resize does not fail");
    pty.resize(60, 20).expect("the second resize does not fail");

    raw.send("set /a 50+7\r");
    raw.expect("57");
}

#[test]
fn closing_a_running_cmd_returns_quickly_and_leaves_no_process() {
    let mut raw = Raw::start("cmd.exe", &["/k"]);
    raw.send("set /a 40+2\r");
    raw.expect("42");
    let pid = raw.pty.as_ref().and_then(|pty| pty.process_id()).expect("the shell has a process id");
    assert!(process_exists(pid), "cmd.exe {pid} is not running before the close");

    let started = Instant::now();
    raw.pty.take().expect("the PTY is open").close().expect("the close reports the child's end");
    assert!(started.elapsed() < Duration::from_secs(2), "the close took {:?}", started.elapsed());
    let gone = wait_until(Duration::from_secs(2), || !process_exists(pid));
    assert!(gone, "cmd.exe {pid} is still running after the close");
}

// ---- a pane ------------------------------------------------------------------------------------------

type Log = Arc<Mutex<Vec<PaneEvent>>>;

fn sink(log: &Log) -> impl FnMut(PaneEvent) + Send + 'static {
    let log = log.clone();
    move |event| log.lock().unwrap().push(event)
}

fn row_text(row: &RowMsg) -> String {
    row.runs.iter().flat_map(|run| run.0.chars()).filter(|c| *c != '\0').collect()
}

fn has_text(log: &Log, needle: &str) -> bool {
    log.lock().unwrap().iter().any(|event| match event {
        PaneEvent::Frame(frame) => frame.changed.iter().any(|row| row_text(row).contains(needle)),
        _ => false,
    })
}

fn exits(log: &Log) -> Vec<Option<u32>> {
    log.lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            PaneEvent::Exited { code } => Some(*code),
            _ => None,
        })
        .collect()
}

fn cwds(log: &Log) -> Vec<String> {
    log.lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            PaneEvent::Cwd(dir) => Some(dir.clone()),
            _ => None,
        })
        .collect()
}

fn key(code: &str, key: &str) -> DomKey {
    DomKey { code: code.into(), key: key.into(), mods: Mods::NONE, ..DomKey::default() }
}

/// Type a line the way a person does: the text, then Enter.
fn type_line(pane: &PaneHandle, line: &str) {
    pane.text(line.to_string()).expect("the text is accepted");
    pane.key(key("Enter", "Enter")).expect("the key is accepted");
}

#[test]
fn a_cmd_pane_shows_typed_text_and_ends_after_exit() {
    let log = Log::default();
    let spec = PaneSpec::new("cmd.exe", 100, 30);
    // The default is the Windows rule of this plan: the pane polls, because the output may never end.
    assert_eq!(spec.exit_poll, platform::exit_poll(Os::Windows));
    let pane = PaneHandle::spawn_ghostty(1, spec, sink(&log)).expect("the pane starts");

    // The first prompt shows within seconds (the cursor position request of the pseudo console was answered).
    assert!(wait_until(WAIT, || has_text(&log, ">")), "no prompt: {:?}", log.lock().unwrap().len());
    type_line(&pane, "echo holzwave-typed");
    assert!(wait_until(WAIT, || has_text(&log, "holzwave-typed")), "the typed text never showed");

    type_line(&pane, "exit");
    let started = Instant::now();
    assert!(
        wait_until(Duration::from_secs(3), || !exits(&log).is_empty()),
        "no Exited within 3 seconds of `exit`: {} events",
        log.lock().unwrap().len()
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    // Exactly one, with the shell's code, and nothing after it.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(exits(&log), vec![Some(0)]);
    assert!(matches!(log.lock().unwrap().last(), Some(PaneEvent::Exited { .. })), "an event followed Exited");
    assert!(pane.join(Duration::from_secs(5)));
}

/// The path without a verbatim prefix and without case, so that `C:\Users\X~1\Temp` and the long name compare equal.
fn same_directory(a: &str, b: &Path) -> bool {
    let plain = |path: &Path| -> String {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
        let text = canonical.to_string_lossy().into_owned();
        text.strip_prefix(r"\\?\").unwrap_or(&text).trim_end_matches('\\').to_lowercase()
    };
    plain(Path::new(a)) == plain(b)
}

#[test]
fn a_powershell_pane_announces_its_directory() {
    let env = |key: &str| std::env::var(key).ok();
    let exists = |path: &str| Path::new(path).is_file();
    let launch = platform::shell_launch(Os::Windows, "", &env, &exists);
    let program = launch.program.to_lowercase();
    if !(program.contains("pwsh") || program.contains("powershell")) {
        println!("skipped: no PowerShell on this machine, the default shell is {}", launch.program);
        return;
    }

    let log = Log::default();
    let spec = PaneSpec::new(launch.program.clone(), 100, 30).args(launch.args.clone());
    let pane = PaneHandle::spawn_ghostty(1, spec, sink(&log)).expect("the pane starts");
    // The prompt, drawn by the wrapper around the user's own prompt, is `PS <path>>` unless a profile changed it.
    assert!(wait_until(WAIT, || has_text(&log, ">")), "no prompt from {}", launch.program);

    type_line(&pane, "cd $env:TEMP");
    let temp = std::env::temp_dir();
    let announced = wait_until(WAIT, || cwds(&log).iter().any(|dir| same_directory(dir, &temp)));
    assert!(announced, "no Cwd naming {} after `cd $env:TEMP`: {:?}", temp.display(), cwds(&log));

    pane.close();
    assert!(pane.join(Duration::from_secs(5)));
}
