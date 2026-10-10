//! Integration tests for the PTY: closing a pane's shell, the spawn options, the writer hand-off and the lookup of
//! a running shell's working directory.
//!
//! Every test that starts a long-running child holds a `Reaper`, so a failing assertion never leaves a `sleep`
//! behind.

mod common;

use std::io::Read;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use common::wait_until;
use holzwave_core::pty::Pty;

/// A shell that ignores SIGHUP, announces that it does and then becomes a `sleep` that inherits the ignored signal.
/// Waiting for `ready` makes sure the trap is installed before the test signals the process.
const DEAF_SHELL: &str = "trap '' HUP; echo ready; exec sleep 1000";

/// Whether a process with this id still exists. A zombie counts as existing (`kill -0` succeeds on it), which is
/// exactly why this proves that the child was reaped and not only signalled.
fn process_exists(pid: u32) -> bool {
    Command::new("/bin/sh")
        .args(["-c", "kill -0 \"$1\" 2>/dev/null", "sh", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Kills the process it was made for when the test ends while the process is still there.
struct Reaper(u32);

impl Drop for Reaper {
    fn drop(&mut self) {
        if process_exists(self.0) {
            let _ = Command::new("/bin/sh")
                .args(["-c", "kill -9 \"$1\" 2>/dev/null", "sh", &self.0.to_string()])
                .status();
        }
    }
}

enum Event {
    Data(Vec<u8>),
    End,
}

/// Move a PTY reader to a thread that forwards what it reads and reports the end (EOF or error) once.
fn watch(mut reader: Box<dyn Read + Send>) -> Receiver<Event> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(Event::End);
                    return;
                }
                Ok(n) => {
                    if tx.send(Event::Data(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
    });
    rx
}

/// Read events until the collected output contains `needle`. Panics after five seconds.
fn read_until(rx: &Receiver<Event>, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut collected = Vec::new();
    while !String::from_utf8_lossy(&collected).contains(needle) {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Event::Data(d)) => collected.extend_from_slice(&d),
            Ok(Event::End) => panic!("output ended before {needle:?}: {collected:?}"),
            Err(_) => panic!("timed out waiting for {needle:?}: {collected:?}"),
        }
    }
}

/// A started PTY with a reader that already saw the shell's `ready`, so the SIGHUP trap is in place.
struct Deaf {
    pty: Pty,
    pid: u32,
    events: Receiver<Event>,
    _reaper: Reaper,
}

fn start_deaf_shell() -> Deaf {
    let mut pty = Pty::spawn("/bin/sh", &["-c", DEAF_SHELL], 80, 24).expect("spawn");
    let pid = pty.process_id().expect("process id");
    let reaper = Reaper(pid);
    let events = watch(pty.take_reader().expect("reader"));
    read_until(&events, "ready");
    Deaf { pty, pid, events, _reaper: reaper }
}

mod close {
    use super::*;

    #[test]
    fn ends_a_sighup_ignoring_shell_within_two_seconds() {
        let deaf = start_deaf_shell();
        let started = Instant::now();
        deaf.pty.close().expect("close must succeed");
        let took = started.elapsed();
        assert!(took < Duration::from_secs(2), "close took {took:?}");
    }

    #[test]
    fn reaps_the_child_so_its_pid_is_gone() {
        let deaf = start_deaf_shell();
        assert!(process_exists(deaf.pid), "the shell must be running before close");
        let pid = deaf.pid;
        deaf.pty.close().expect("close must succeed");
        assert!(!process_exists(pid), "pid {pid} still exists after close: not ended or not reaped");
    }

    #[test]
    fn a_reader_taken_before_close_sees_eof_within_two_seconds() {
        let deaf = start_deaf_shell();
        let events = deaf.events;
        deaf.pty.close().expect("close must succeed");
        let mut ended = false;
        let seen = wait_until(Duration::from_secs(2), || {
            while let Ok(e) = events.try_recv() {
                if matches!(e, Event::End) {
                    ended = true;
                }
            }
            ended
        });
        assert!(seen, "the reader did not see EOF or an error within two seconds of close");
    }

    #[test]
    fn a_child_that_already_exited_returns_its_exit_code() {
        let mut pty = Pty::spawn("/bin/sh", &["-c", "exit 7"], 80, 24).expect("spawn");
        let events = watch(pty.take_reader().expect("reader"));
        // The reader ends when the shell has exited: the last slave descriptor is closed with the process.
        let mut ended = false;
        let seen = wait_until(Duration::from_secs(5), || {
            while let Ok(e) = events.try_recv() {
                if matches!(e, Event::End) {
                    ended = true;
                }
            }
            ended
        });
        assert!(seen, "the short-lived shell never ended");
        let code = pty.close().expect("closing an exited child must not error");
        assert_eq!(code, 7);
    }
}
