//! The bytes real shells write when they run the hooks that `wsh init` prints, read through the real engine.
//!
//! Every test starts a real interactive shell in a PTY with `HOLZWAVE_IPC` set, loads one of the hook files under
//! `wsh/hooks` the way an rc file does, types commands, and checks two things: the OSC 133 marks the shell wrote
//! (A when a prompt is shown, B and C when a command starts, D with the exit status when it ended) and what
//! `GhosttyEngine::output_at` answers for the same bytes. The hooks must never write anything but those fixed marks
//! and must never touch the prompt, so a framework that rebuilds the prompt on every prompt cannot remove a mark.
//!
//! A shell that is not installed (or a bash older than 4.4 where a test needs one) is reported on stdout as
//! `shell_hooks: skipped {shell}: {reason}` and the test returns; a shell that ran prints
//! `shell_hooks: ran {shell} {version}`.
#![cfg(unix)]
#![cfg(feature = "ghostty")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use holzwave_core::pty::{Pty, SpawnOptions};
use holzwave_core::{GhosttyEngine, TerminalEngine};

/// OSC 133 introducer.
const OSC_133: &[u8] = b"\x1b]133;";
/// The mark that a prompt is shown, as the hooks write it.
const MARK_A: &[u8] = b"\x1b]133;A\x1b\\";

/// The workspace directory that holds the hook files.
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
    let out = Command::new(program)
        .args(["-c", "echo ${BASH_VERSINFO[0]} ${BASH_VERSINFO[1]}"])
        .env_remove("BASH_ENV")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut it = text.split_whitespace().map(|n| n.parse::<u32>());
    match (it.next(), it.next()) {
        (Some(Ok(a)), Some(Ok(b))) => Some((a, b)),
        _ => None,
    }
}

/// The first bash that is 4.4 or newer (the first release with `PS0`), with its version.
fn modern_bash() -> Option<(PathBuf, (u32, u32))> {
    find_programs("bash").into_iter().filter_map(|p| bash_version(&p).map(|v| (p, v))).find(|(_, v)| *v >= (4, 4))
}

/// One OSC 133 mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    A,
    B,
    C,
    D(u32),
}

/// Every OSC 133 sequence in `bytes`. Panics on anything but a string-terminated A, B, C or D with up to three
/// digits: this is the guard that the hooks write nothing but fixed marks and a number.
fn parse_marks(bytes: &[u8]) -> Vec<Mark> {
    let mut marks = Vec::new();
    let mut i = 0;
    while let Some(at) = bytes[i..].windows(OSC_133.len()).position(|w| w == OSC_133) {
        let start = i + at + OSC_133.len();
        let rest = &bytes[start..];
        let end = rest.iter().position(|&b| b == 0x1b || b == 0x07);
        let Some(end) = end else { panic!("unterminated OSC 133: {}", common::ascii(&bytes[start - OSC_133.len()..])) };
        if rest[end] == 0x07 || rest.get(end + 1) != Some(&b'\\') {
            panic!("OSC 133 without the string terminator: {}", common::ascii(&bytes[start - OSC_133.len()..start + end + 2]));
        }
        let payload = &rest[..end];
        let mark = match payload {
            b"A" => Mark::A,
            b"B" => Mark::B,
            b"C" => Mark::C,
            [b'D', b';', digits @ ..] if (1..=3).contains(&digits.len()) && digits.iter().all(u8::is_ascii_digit) => {
                Mark::D(std::str::from_utf8(digits).expect("digits").parse().expect("number"))
            }
            _ => panic!("OSC 133 payload that is no mark: {}", common::ascii(payload)),
        };
        marks.push(mark);
        i = start + end + 2;
    }
    marks
}

/// A real shell in a PTY and everything it wrote.
struct Pane {
    pty: Option<Pty>,
    rx: Receiver<Vec<u8>>,
    bytes: Vec<u8>,
    /// The last bytes seen, so that a query split between two reads is still answered once.
    carry: Vec<u8>,
    dir: PathBuf,
}

impl Pane {
    /// Start `program` with `args` in an 80 by 24 PTY. `HOME` and the working directory are a fresh scratch
    /// directory; `PATH` has the stub directory first; `HOLZWAVE_IPC` is the scratch directory when `ipc` is true
    /// and empty when false (the parent may itself run in a holzWave pane and carry the variable).
    fn start(program: &Path, args: &[&str], ipc: bool, stub_dir: Option<&Path>) -> Self {
        let dir = common::scratch_dir("hooks");
        let path = std::env::var("PATH").unwrap_or_default();
        let path = match stub_dir {
            Some(stub) => format!("{}:{path}", stub.display()),
            None => path,
        };
        let options = SpawnOptions::new(program.to_string_lossy().into_owned(), 80, 24)
            .args(args.iter().copied())
            .cwd(&dir)
            .env("HOME", dir.to_string_lossy().into_owned())
            .env("PATH", path)
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
        Self { pty: Some(pty), rx, bytes: Vec::new(), carry: Vec::new(), dir }
    }

    /// Write `text` and a carriage return to the shell.
    fn send(&mut self, text: &str) {
        let pty = self.pty.as_mut().expect("pty");
        pty.write(text.as_bytes()).expect("write");
        pty.write(b"\r").expect("write");
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

    /// Read for `span`.
    fn pump(&mut self, span: Duration) {
        let end = Instant::now() + span;
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            match self.rx.recv_timeout(left.min(Duration::from_millis(20))) {
                Ok(chunk) => self.take(chunk),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// Read until `done` holds for the bytes so far; panics after `bound` with the escaped tail.
    fn pump_until(&mut self, bound: Duration, what: &str, done: impl Fn(&[u8]) -> bool) {
        let end = Instant::now() + bound;
        while !done(&self.bytes) {
            let Some(left) = end.checked_duration_since(Instant::now()) else {
                let tail = &self.bytes[self.bytes.len().saturating_sub(600)..];
                panic!("timed out waiting for {what}; the last bytes: {}", common::ascii(tail));
            };
            match self.rx.recv_timeout(left.min(Duration::from_millis(20))) {
                Ok(chunk) => self.take(chunk),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the shell ended; bytes: {}", common::ascii(&self.bytes)),
            }
        }
    }

    /// Read until `n` prompts were marked, then a little longer for what follows the mark.
    fn wait_prompts(&mut self, n: usize, bound: Duration) {
        self.pump_until(bound, &format!("{n} prompt marks"), |b| count(b, MARK_A) >= n);
        self.pump(Duration::from_millis(150));
    }

    /// Read until 400 ms pass without data (the shell has printed everything it will).
    fn settle(&mut self) {
        let end = Instant::now() + Duration::from_secs(15);
        let mut quiet = Instant::now();
        while Instant::now() < end && quiet.elapsed() < Duration::from_millis(400) {
            if let Ok(chunk) = self.rx.recv_timeout(Duration::from_millis(20)) {
                self.take(chunk);
                quiet = Instant::now();
            }
        }
    }

    /// The marks written so far.
    fn marks(&self) -> Vec<Mark> {
        parse_marks(&self.bytes)
    }

    /// The marks written after `offset` bytes.
    fn marks_from(&self, offset: usize) -> Vec<Mark> {
        parse_marks(&self.bytes[offset..])
    }

    /// The engine with every byte from the first prompt mark on.
    ///
    /// Text that came before the first mark (the shell's own banner, the line that loaded the hook) is left out on
    /// purpose: the engine counts unmarked text that ends at the first prompt mark as the output of a command, and
    /// no hook can mark what was printed before it was loaded.
    fn engine(&self) -> GhosttyEngine {
        let first = self.bytes.windows(MARK_A.len()).position(|w| w == MARK_A).unwrap_or(0);
        let mut e = common::engine(80, 24);
        e.feed(&self.bytes[first..]);
        e
    }

    /// The output text of every command in the stream, in order, neighbouring duplicates merged (the rows of one
    /// output all answer with the same text).
    fn outputs(&self) -> Vec<String> {
        let e = self.engine();
        let mut all: Vec<String> = Vec::new();
        for row in 0..400u32 {
            if let Some(text) = e.output_at(row, 4096).expect("output_at") {
                if all.last() != Some(&text) {
                    all.push(text);
                }
            }
        }
        all
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        if let Some(pty) = self.pty.take() {
            let _ = pty.close();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// How often `needle` occurs in `hay`.
fn count(hay: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || hay.len() < needle.len() {
        return 0;
    }
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

/// The text of a hook file.
fn hook_path(file: &str) -> PathBuf {
    let p = hooks_dir().join(file);
    assert!(p.is_file(), "{} is missing", p.display());
    p
}

/// Print the skip line.
fn skipped(shell: &str, why: &str) {
    println!("shell_hooks: skipped {shell}: {why}");
}

#[test]
fn bash_hook_marks_one_command() {
    let Some((bash, (major, minor))) = modern_bash() else {
        return skipped("bash", "no bash 4.4 or newer");
    };
    let mut pane = Pane::start(&bash, &["--norc", "--noprofile", "-i"], true, None);
    pane.send("PS1='$ '");
    pane.settle();
    pane.send(&format!("source '{}'", hook_path("bash.sh").display()));
    pane.wait_prompts(1, Duration::from_secs(15));
    assert_eq!(pane.marks(), [Mark::A]);
    let before = pane.bytes.len();
    pane.send("printf 'alpha\\nbeta\\n'");
    pane.wait_prompts(2, Duration::from_secs(15));
    assert_eq!(pane.marks_from(0)[1..], [Mark::B, Mark::C, Mark::D(0), Mark::A], "{}", common::ascii(&pane.bytes[before..]));
    assert_eq!(pane.outputs(), ["alpha\nbeta"], "{}", common::ascii(&pane.bytes[before..]));
    assert!(!pane.bytes.iter().any(|&b| b == 1 || b == 2), "readline brackets leaked: {}", common::ascii(&pane.bytes));
    println!("shell_hooks: ran bash {major}.{minor}");
}
