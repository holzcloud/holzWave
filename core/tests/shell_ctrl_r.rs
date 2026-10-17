//! What real shells do when the Ctrl+R binding that `wsh init <shell> --ctrl-r` prints is loaded.
//!
//! Every test starts a real interactive shell in a PTY, puts a stand-in `wsh` first on its PATH (it only appends the
//! arguments it was called with to `calls.log`), loads one of the binding files under `wsh/hooks` the way an rc file does,
//! presses Ctrl+R (byte 0x12) and checks two things: the calls the stub received, and that the shell kept the typed line,
//! its prompt and the exit status of the last command. Outside holzWave (`HOLZWAVE_IPC` empty) the binding must bind
//! nothing, so the shell's own reverse search answers the key.
//!
//! A shell that is not installed (or a bash older than 4.0, where a test needs a newer one) is reported on stdout as
//! `shell_ctrl_r: skipped {shell}: {reason}` and the test returns; a shell that ran prints
//! `shell_ctrl_r: ran {shell} {version}`. The file needs no engine, so it builds with `--no-default-features`.
#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use holzwave_core::pty::{Pty, SpawnOptions};

/// The workspace directory that holds the binding files.
fn hooks_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../wsh/hooks")
}

/// Every distinct `name` on PATH or in the usual package directories, in search order.
fn find_programs(name: &str) -> Vec<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    for d in ["/opt/homebrew/bin", "/usr/local/bin", "/bin", "/usr/bin"] {
        dirs.push(PathBuf::from(d));
    }
    let mut found: Vec<PathBuf> = Vec::new();
    for p in dirs.into_iter().map(|d| d.join(name)).filter(|p| p.is_file()) {
        if !found.contains(&p) {
            found.push(p);
        }
    }
    found
}

/// `(major, minor)` of a bash program, or `None` when it is not a bash that answers.
fn bash_version(program: &Path) -> Option<(u32, u32)> {
    let out = Command::new(program).args(["-c", "echo ${BASH_VERSINFO[0]} ${BASH_VERSINFO[1]}"]).env_remove("BASH_ENV").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut it = text.split_whitespace().map(|n| n.parse::<u32>());
    match (it.next(), it.next()) {
        (Some(Ok(a)), Some(Ok(b))) => Some((a, b)),
        _ => None,
    }
}

/// The first bash that is 4.0 or newer (the first release whose binding the text accepts), with its version.
fn modern_bash() -> Option<(PathBuf, (u32, u32))> {
    find_programs("bash").into_iter().filter_map(|p| bash_version(&p).map(|v| (p, v))).find(|(_, v)| v.0 >= 4)
}

/// One shell the scenarios run in.
struct Shell {
    name: &'static str,
    program: PathBuf,
    version: String,
    args: &'static [&'static str],
    /// Lines that give a known plain prompt.
    setup: &'static [&'static str],
    /// The binding file under `wsh/hooks`.
    file: &'static str,
}

impl Shell {
    fn bash() -> Option<Shell> {
        let Some((program, (major, minor))) = modern_bash() else {
            println!("shell_ctrl_r: skipped bash: no bash 4.0 or newer was found");
            return None;
        };
        Some(Shell {
            name: "bash",
            program,
            version: format!("{major}.{minor}"),
            args: &["--norc", "--noprofile", "-i"],
            setup: &["PS1='hwr$ '"],
            file: "bash-ctrl-r.sh",
        })
    }
}

/// A real shell in a PTY, a stub `wsh` and everything the shell wrote.
struct Rig {
    pty: Option<Pty>,
    rx: Receiver<Vec<u8>>,
    bytes: Vec<u8>,
    /// The last bytes seen, so that a query split between two reads is still answered once.
    carry: Vec<u8>,
    dir: PathBuf,
}

impl Rig {
    /// Start `program` with `args` in an 80 by 24 PTY. `HOME` and the working directory are a fresh scratch directory;
    /// `PATH` has the stub directory first; `HOLZWAVE_IPC` is the scratch directory when `ipc` is true and empty when false
    /// (the parent may itself run in a holzWave pane and carry the variable).
    fn start(program: &Path, args: &[&str], ipc: bool) -> Rig {
        let dir = common::scratch_dir("ctrlr");
        let stub = dir.join("stub");
        std::fs::create_dir(&stub).expect("stub dir");
        let script = stub.join("wsh");
        let log = dir.join("calls.log");
        std::fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display())).expect("stub");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("mode");
        let path = format!("{}:{}", stub.display(), std::env::var("PATH").unwrap_or_default());
        let options = SpawnOptions::new(program.to_string_lossy().into_owned(), 80, 24)
            .args(args.iter().copied())
            .cwd(&dir)
            .env("HOME", dir.to_string_lossy().into_owned())
            .env("PATH", path)
            .env("TERM", "xterm")
            .env("INPUTRC", "/dev/null")
            .env("HOLZWAVE_IPC", if ipc { dir.to_string_lossy().into_owned() } else { String::new() })
            .env("PROMPT_COMMAND", "")
            .env("PS0", "")
            .env("HISTFILE", dir.join(".history").to_string_lossy().into_owned());
        let mut pty = Pty::spawn_with(&options).expect("spawn the shell");
        let mut reader = pty.take_reader().expect("reader");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    return;
                }
            }
        });
        Rig { pty: Some(pty), rx, bytes: Vec::new(), carry: Vec::new(), dir }
    }

    /// Write bytes to the shell.
    fn send(&mut self, bytes: &[u8]) {
        self.pty.as_mut().expect("pty").write(bytes).expect("write");
    }

    /// Write `text` and a carriage return to the shell.
    fn line(&mut self, text: &str) {
        self.send(text.as_bytes());
        self.send(b"\r");
    }

    /// Take what arrived, answering the terminal queries a shell sends (cursor position, device attributes).
    fn take(&mut self, chunk: Vec<u8>) {
        let mut seen = std::mem::take(&mut self.carry);
        let old = seen.len();
        seen.extend_from_slice(&chunk);
        let mut replies: Vec<&[u8]> = Vec::new();
        for (query, reply) in [
            (&b"\x1b[6n"[..], &b"\x1b[1;1R"[..]),
            (&b"\x1b[c"[..], &b"\x1b[?62;c"[..]),
            (&b"\x1b[0c"[..], &b"\x1b[?62;c"[..]),
        ] {
            for at in 0..seen.len().saturating_sub(query.len() - 1) {
                if &seen[at..at + query.len()] == query && at + query.len() > old {
                    replies.push(reply);
                }
            }
        }
        let keep = seen.len().saturating_sub(3);
        self.carry = seen[keep..].to_vec();
        self.bytes.extend_from_slice(&chunk);
        if let Some(pty) = self.pty.as_mut() {
            for reply in replies {
                let _ = pty.write(reply);
            }
        }
    }

    /// Read until 400 ms pass without data (the shell has printed everything it will), for at most 10 s.
    fn settle(&mut self) {
        let end = Instant::now() + Duration::from_secs(10);
        let mut quiet = Instant::now();
        while Instant::now() < end && quiet.elapsed() < Duration::from_millis(400) {
            if let Ok(chunk) = self.rx.recv_timeout(Duration::from_millis(20)) {
                self.take(chunk);
                quiet = Instant::now();
            }
        }
    }

    /// Read until the bytes seen contain `needle` `n` times; panics after `secs` with the escaped tail of the bytes.
    fn wait_for(&mut self, needle: &str, n: usize, secs: u64) {
        let end = Instant::now() + Duration::from_secs(secs);
        while count(&self.bytes, needle.as_bytes()) < n {
            let Some(left) = end.checked_duration_since(Instant::now()) else {
                let tail = &self.bytes[self.bytes.len().saturating_sub(600)..];
                panic!("timed out waiting for {needle:?}; the last bytes: {}", common::ascii(tail));
            };
            match self.rx.recv_timeout(left.min(Duration::from_millis(20))) {
                Ok(chunk) => self.take(chunk),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the shell ended; bytes: {}", common::ascii(&self.bytes)),
            }
        }
    }

    /// The lines the stub wrote so far.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("calls.log")).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    /// Wait until the stub has been called `n` times; panics after `secs`.
    fn wait_calls(&mut self, n: usize, secs: u64) {
        let end = Instant::now() + Duration::from_secs(secs);
        while self.calls().len() < n {
            assert!(Instant::now() < end, "the stub was called {} times, not {n}; the last bytes: {}", self.calls().len(), common::ascii(&self.bytes[self.bytes.len().saturating_sub(600)..]));
            if let Ok(chunk) = self.rx.recv_timeout(Duration::from_millis(20)) {
                self.take(chunk);
            }
        }
    }

    /// Set up the prompt of `shell`.
    fn prepare(&mut self, shell: &Shell) {
        self.settle();
        for line in shell.setup {
            self.line(line);
            self.settle();
        }
    }

    /// Load a file of the hooks directory the way an rc file does, and wait for the prompt.
    fn load(&mut self, file: &str) {
        let path = hooks_dir().join(file);
        self.line(&format!("source '{}'", path.display()));
        self.settle();
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(pty) = self.pty.take() {
            let _ = pty.close();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// How many times `needle` occurs in `bytes` (without overlap).
fn count(bytes: &[u8], needle: &[u8]) -> usize {
    let (mut n, mut i) = (0, 0);
    while let Some(at) = bytes[i..].windows(needle.len()).position(|w| w == needle) {
        n += 1;
        i += at + needle.len();
    }
    n
}

#[test]
fn bash_ctrl_r_asks_the_app_and_keeps_the_line() {
    let Some(sh) = Shell::bash() else { return };
    let mut rig = Rig::start(&sh.program, sh.args, true);
    rig.prepare(&sh);
    rig.load(sh.file);
    rig.send(b"echo kept");
    rig.send(&[0x12]);
    rig.wait_calls(1, 3);
    assert_eq!(rig.calls(), ["hist open"]);
    rig.send(b"\r");
    // The line is shown once as typed and once as the output of the command.
    rig.wait_for("kept\r\n", 2, 5);
    assert_eq!(rig.calls().len(), 1, "the key asked once and the line did not ask again");
    println!("shell_ctrl_r: ran {} {}", sh.name, sh.version);
}
