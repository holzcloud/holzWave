//! Signalling the processes of a person without ever signalling the wrong one.
//!
//! A process id can be reused the moment its process has been collected, so the helper never signals by number. It
//! opens a process descriptor for the process first (which keeps pointing at that very process), then reads the real user
//! of the process, and signals through the descriptor only when the user is the one asked for. If the process has gone and
//! the number has been given to another process in between, the descriptor answers "no such process" and nothing happens.

/// What the helper reads from `/proc/<pid>/status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStatus {
    /// The real user id.
    pub real_uid: u32,
    /// The process has ended and only waits to be collected; signalling it does nothing.
    pub gone: bool,
}

/// Reads the real user and the state out of the text of a `status` file. `None` when there is no readable `Uid:` line.
pub fn parse_status(text: &str) -> Option<ProcStatus> {
    let mut real_uid = None;
    let mut gone = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            real_uid = rest.split_whitespace().next().and_then(|word| word.parse::<u32>().ok());
        } else if let Some(rest) = line.strip_prefix("State:") {
            gone = rest.trim_start().starts_with(['Z', 'X']);
        }
    }
    Some(ProcStatus { real_uid: real_uid?, gone })
}

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "linux")]
mod linux {
    use super::parse_status;
    use crate::protocol::Signal;
    use crate::validate::FLOOR;
    use rustix::fd::OwnedFd;
    use rustix::io::Errno;
    use rustix::process::{Pid, PidfdFlags, pidfd_open, pidfd_send_signal};
    use std::collections::BTreeSet;
    use std::io;
    use std::time::Duration;

    fn wire(signal: Signal) -> rustix::process::Signal {
        match signal {
            Signal::Term => rustix::process::Signal::TERM,
            Signal::Kill => rustix::process::Signal::KILL,
        }
    }

    /// Sends a signal to the process a descriptor stands for.
    pub fn signal_process(pidfd: &OwnedFd, signal: Signal) -> Result<(), Errno> {
        pidfd_send_signal(pidfd, wire(signal))
    }

    /// One walk through `/proc`: the pids signalled in this pass (each once).
    fn pass(uid: u32, own_pid: i32, signal: Signal, done: &mut BTreeSet<i32>) -> io::Result<u32> {
        let mut signalled = 0;
        let numbers: Vec<i32> = std::fs::read_dir("/proc")?
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<i32>().ok())
            .collect();
        for number in numbers {
            if number == own_pid {
                continue;
            }
            let Some(pid) = Pid::from_raw(number) else { continue };
            // The descriptor first, the user second: the descriptor pins the process the user is read for.
            let Ok(pidfd) = pidfd_open(pid, PidfdFlags::empty()) else { continue };
            let Ok(text) = std::fs::read_to_string(format!("/proc/{number}/status")) else { continue };
            let Some(status) = parse_status(&text) else { continue };
            if status.real_uid != uid || status.gone {
                continue;
            }
            if signal_process(&pidfd, signal).is_ok() {
                signalled += 1;
                done.insert(number);
            }
        }
        Ok(signalled)
    }

    /// Signals every process whose real user is `uid`, through process descriptors. The caller has raised `CAP_KILL`.
    /// `SIGKILL` walks `/proc` again until a pass finds nobody (a process may fork while it is being killed), at most
    /// twenty times. The answer is the number of distinct processes signalled.
    pub fn kill_uid(uid: u32, signal: Signal) -> io::Result<u32> {
        if uid < FLOOR || uid == u32::MAX {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let own_pid = std::process::id() as i32;
        let mut done = BTreeSet::new();
        let passes = if signal == Signal::Kill { 20 } else { 1 };
        for _ in 0..passes {
            if pass(uid, own_pid, signal, &mut done)? == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(done.len() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Name:\tfake-worker.sh\nUmask:\t0022\nState:\tS (sleeping)\nTgid:\t812\nUid:\t20001\t20001\t20001\t20001\nGid:\t20001\t20001\t20001\t20001\n";

    #[test]
    fn reads_the_real_user_and_the_state() {
        assert_eq!(parse_status(SAMPLE), Some(ProcStatus { real_uid: 20001, gone: false }));
    }

    #[test]
    fn a_zombie_is_gone_and_the_real_user_is_the_first_number() {
        let zombie = "State:\tZ (zombie)\nUid:\t20002\t0\t0\t0\n";
        assert_eq!(parse_status(zombie), Some(ProcStatus { real_uid: 20002, gone: true }));
        assert_eq!(parse_status("State:\tX (dead)\nUid:\t5\t5\t5\t5\n"), Some(ProcStatus { real_uid: 5, gone: true }));
    }

    #[test]
    fn no_uid_line_or_a_broken_one_is_no_answer() {
        assert_eq!(parse_status(""), None);
        assert_eq!(parse_status("State:\tS (sleeping)\n"), None);
        assert_eq!(parse_status("Uid:\tabc\t1\t1\t1\n"), None);
        assert_eq!(parse_status("Uid:\t-1\t1\t1\t1\n"), None);
        assert_eq!(parse_status("Uid:\n"), None);
    }
}
