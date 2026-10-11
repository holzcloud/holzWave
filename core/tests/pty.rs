//! Integration tests for the PTY: closing a pane's shell, the spawn options, the writer hand-off and the lookup of
//! a running shell's working directory.
//!
//! Every test that starts a long-running child holds a `Reaper`, so a failing assertion never leaves a `sleep`
//! behind.

mod common;

use std::io::{Read, Write};
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

mod spawn_options {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    use holzwave_core::Error;
    use holzwave_core::pty::{SpawnOptions, process_cwd};

    /// Run the options to the end of the child's output and return that output.
    fn output_of(options: &SpawnOptions) -> String {
        let mut pty = Pty::spawn_with(options).expect("spawn_with");
        let events = watch(pty.take_reader().expect("reader"));
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut collected = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match events.recv_timeout(left) {
                Ok(Event::Data(d)) => collected.extend_from_slice(&d),
                Ok(Event::End) => break,
                Err(_) => panic!("the child did not finish: {collected:?}"),
            }
        }
        let _ = pty.close();
        String::from_utf8_lossy(&collected).into_owned()
    }

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |key| map.get(key).cloned()
    }

    fn last_value(env: &[(String, String)], key: &str) -> Option<String> {
        env.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    const UTF8_FALLBACK: &str = if cfg!(target_os = "macos") { "en_US.UTF-8" } else { "C.UTF-8" };

    #[test]
    fn a_child_starts_in_the_given_working_directory() {
        let dir = common::scratch_dir("pty-cwd");
        let options = SpawnOptions::new("/bin/sh", 80, 24).args(["-c", "pwd"]).cwd(&dir);
        let out = output_of(&options);
        let line = out.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
        let reported = Path::new(line).canonicalize().expect("pwd printed a real path");
        assert_eq!(reported, dir.canonicalize().unwrap());
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn a_missing_working_directory_does_not_fail_the_spawn() {
        let dir = common::scratch_dir("pty-gone");
        let missing = dir.join("not-there");
        let options = SpawnOptions::new("/bin/sh", 80, 24).args(["-c", "pwd"]).cwd(&missing);
        let out = output_of(&options);
        assert!(!out.contains("not-there"), "the child must not claim the missing directory: {out:?}");
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn the_child_sees_term_colorterm_term_program_and_version() {
        let options = SpawnOptions::new("/bin/sh", 80, 24).args([
            "-c",
            "printf '%s|%s|%s|%s\\n' \"$TERM\" \"$COLORTERM\" \"$TERM_PROGRAM\" \"$TERM_PROGRAM_VERSION\"",
        ]);
        let out = output_of(&options);
        let expected = format!("xterm-256color|truecolor|holzWave|{}", env!("CARGO_PKG_VERSION"));
        assert!(out.lines().any(|l| l.trim() == expected), "expected {expected:?} in {out:?}");
    }

    #[test]
    fn caller_environment_pairs_win_over_the_defaults() {
        let options = SpawnOptions::new("/bin/sh", 80, 24)
            .args(["-c", "printf '%s|%s\\n' \"$COLORTERM\" \"$HOLZ_PTY_TEST\""])
            .env("COLORTERM", "24bit")
            .env("HOLZ_PTY_TEST", "yes");
        let out = output_of(&options);
        assert!(out.lines().any(|l| l.trim() == "24bit|yes"), "got {out:?}");
    }

    #[test]
    fn child_env_lists_the_defaults_in_order_and_then_the_callers_pairs() {
        let options = SpawnOptions::new("/bin/sh", 80, 24).env("A", "1").env("B", "2");
        let env = options.child_env(lookup(&[("LANG", "en_US.UTF-8")]));
        let names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["TERM", "COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "A", "B"]);
        assert_eq!(last_value(&env, "TERM_PROGRAM_VERSION").as_deref(), Some(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn a_utf8_ctype_is_added_only_when_the_parent_has_no_locale() {
        let options = SpawnOptions::new("/bin/sh", 80, 24);
        let none = options.child_env(lookup(&[]));
        assert_eq!(last_value(&none, "LC_CTYPE").as_deref(), Some(UTF8_FALLBACK));

        let empty = options.child_env(lookup(&[("LANG", ""), ("LC_ALL", ""), ("LC_CTYPE", "")]));
        assert_eq!(last_value(&empty, "LC_CTYPE").as_deref(), Some(UTF8_FALLBACK), "empty counts as unset");

        for key in ["LANG", "LC_ALL", "LC_CTYPE"] {
            let env = options.child_env(lookup(&[(key, "de_CH.UTF-8")]));
            assert_eq!(last_value(&env, "LC_CTYPE"), None, "{key} present: the locale is left alone");
        }
    }

    #[test]
    fn a_callers_lc_ctype_wins_over_the_fallback() {
        let options = SpawnOptions::new("/bin/sh", 80, 24).env("LC_CTYPE", "de_CH.UTF-8");
        let env = options.child_env(lookup(&[]));
        assert_eq!(last_value(&env, "LC_CTYPE").as_deref(), Some("de_CH.UTF-8"));
    }

    #[test]
    fn a_spawned_child_follows_the_same_locale_rule_as_child_env() {
        let options = SpawnOptions::new("/bin/sh", 80, 24)
            .args(["-c", "printf 'ctype=%s\\n' \"$LC_CTYPE\""]);
        let out = output_of(&options);
        let parent_has_locale = ["LANG", "LC_ALL", "LC_CTYPE"]
            .iter()
            .any(|k| std::env::var(k).is_ok_and(|v| !v.is_empty()));
        let expected = if parent_has_locale {
            std::env::var("LC_CTYPE").unwrap_or_default()
        } else {
            UTF8_FALLBACK.to_string()
        };
        assert!(out.lines().any(|l| l.trim() == format!("ctype={expected}")), "expected ctype={expected:?} in {out:?}");
    }

    #[test]
    fn a_pty_spawned_the_old_way_still_works() {
        let mut pty = Pty::spawn("/bin/sh", &["-c", "echo old-way"], 80, 24).expect("spawn");
        let events = watch(pty.take_reader().expect("reader"));
        read_until(&events, "old-way");
        let _ = pty.close();
    }

    #[test]
    fn take_writer_returns_the_writer_once_and_write_then_errors_cleanly() {
        let mut pty = Pty::spawn("/bin/sh", &["-c", "read line; echo got:$line"], 80, 24).expect("spawn");
        let events = watch(pty.take_reader().expect("reader"));
        let mut writer = pty.take_writer().expect("the first take_writer returns the writer");
        assert!(pty.take_writer().is_none(), "the writer can be taken only once");
        match pty.write(b"x") {
            Err(Error::Pty(_)) => {}
            other => panic!("write after take_writer must be Error::Pty, got {other:?}"),
        }
        writer.write_all(b"hello\n").unwrap();
        writer.flush().unwrap();
        read_until(&events, "got:hello");
        let _ = pty.close();
        drop(writer);
    }

    #[test]
    fn write_works_while_the_pty_still_holds_the_writer() {
        let mut pty = Pty::spawn("/bin/sh", &["-c", "read line; echo got:$line"], 80, 24).expect("spawn");
        let events = watch(pty.take_reader().expect("reader"));
        pty.write(b"hi\n").expect("write");
        read_until(&events, "got:hi");
        let _ = pty.close();
    }

    #[test]
    fn resize_px_with_pixels_reaches_the_child() {
        let options = SpawnOptions::new("/bin/sh", 80, 24)
            .cell_px(8, 16)
            .args(["-c", "read line; stty size"]);
        let mut pty = Pty::spawn_with(&options).expect("spawn_with");
        let events = watch(pty.take_reader().expect("reader"));
        pty.resize_px(100, 30, 800, 480).expect("resize with pixels must not error");
        pty.write(b"\n").expect("write");
        read_until(&events, "30 100");
        let _ = pty.close();
    }

    /// `stty size` only shows rows and columns; the pixel fields (ws_xpixel, ws_ypixel) are read back here with
    /// TIOCGWINSZ through python3 (standard library only). Skipped with a message when python3 is absent.
    #[test]
    fn resize_px_pixel_fields_reach_the_child_winsize() {
        let has_python = Command::new("python3")
            .args(["-c", "import fcntl, termios"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !has_python {
            eprintln!("SKIPPED: python3 with fcntl/termios is not available; ws_xpixel/ws_ypixel not read back");
            return;
        }
        let script = "import sys,fcntl,termios,struct\n\
                      sys.stdin.readline()\n\
                      r,c,x,y = struct.unpack('HHHH', fcntl.ioctl(0, termios.TIOCGWINSZ, b'\\0'*8))\n\
                      print('WS %d %d %d %d' % (r,c,x,y), flush=True)\n";
        let options = SpawnOptions::new("python3", 80, 24).cell_px(8, 16).args(["-c", script]);
        let mut pty = Pty::spawn_with(&options).expect("spawn_with");
        let events = watch(pty.take_reader().expect("reader"));
        // Distinct values that differ from the spawn size (80x24, 640x384) in every field.
        pty.resize_px(100, 30, 1234, 567).expect("resize with pixels must not error");
        pty.write(b"\n").expect("write");
        read_until(&events, "WS 30 100 1234 567");
        let _ = pty.close();
    }

    #[test]
    fn try_wait_reports_none_while_running_and_the_code_after_exit() {
        let mut running = Pty::spawn("/bin/sleep", &["1000"], 80, 24).expect("spawn");
        let _reaper = Reaper(running.process_id().expect("process id"));
        assert_eq!(running.try_wait().expect("try_wait"), None);
        running.close().expect("close");

        let mut quick = Pty::spawn("/bin/sh", &["-c", "exit 3"], 80, 24).expect("spawn");
        let code = std::cell::Cell::new(None);
        let seen = wait_until(Duration::from_secs(5), || {
            code.set(quick.try_wait().expect("try_wait"));
            code.get().is_some()
        });
        assert!(seen, "the short-lived child never reported an exit");
        assert_eq!(code.get(), Some(3));
    }

    #[test]
    fn process_cwd_reads_the_working_directory_of_a_running_child() {
        let dir = common::scratch_dir("pty-proc-cwd");
        let options = SpawnOptions::new("/bin/sleep", 80, 24).args(["1000"]).cwd(&dir);
        let pty = Pty::spawn_with(&options).expect("spawn_with");
        let pid = pty.process_id().expect("process id");
        let _reaper = Reaper(pid);
        let cwd = process_cwd(pid).expect("a running process has a working directory");
        assert_eq!(cwd.canonicalize().unwrap(), dir.canonicalize().unwrap());
        pty.close().expect("close");
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn process_cwd_of_this_process_is_the_current_directory() {
        let cwd = process_cwd(std::process::id()).expect("own working directory");
        assert_eq!(cwd.canonicalize().unwrap(), std::env::current_dir().unwrap().canonicalize().unwrap());
    }

    #[test]
    fn process_cwd_of_a_pid_that_does_not_exist_is_none() {
        assert_eq!(process_cwd(u32::MAX), None);
        let mut gone = Pty::spawn("/bin/sh", &["-c", "exit 0"], 80, 24).expect("spawn");
        let pid = gone.process_id().expect("process id");
        gone.wait().expect("wait");
        assert_eq!(process_cwd(pid), None, "a reaped pid has no working directory");
    }
}
