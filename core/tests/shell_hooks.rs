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
/// The start of the mark that a prompt is shown (a shell's own mark may add options and end with BEL).
const MARK_A: &[u8] = b"\x1b]133;A";

/// The workspace directory that holds the hook files.
fn hooks_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../wsh/hooks")
}

/// The first executable `name` on PATH or in the usual package directories.
fn find_program(name: &str) -> Option<PathBuf> {
    find_programs(name).into_iter().next()
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

/// Every OSC 133 sequence in `bytes`.
///
/// Strict (what the hooks write): panics on anything but a string-terminated A, B, C or D with up to three digits.
/// That is the guard that the hooks write nothing but fixed marks and a number. Not strict (a shell that writes its own
/// marks, such as fish 4): either terminator and `;key=value` options after the letter are accepted.
fn parse_marks(bytes: &[u8], strict: bool) -> Vec<Mark> {
    let mut marks = Vec::new();
    let mut i = 0;
    while let Some(at) = bytes[i..].windows(OSC_133.len()).position(|w| w == OSC_133) {
        let start = i + at + OSC_133.len();
        let rest = &bytes[start..];
        let end = rest.iter().position(|&b| b == 0x1b || b == 0x07);
        let Some(end) = end else { panic!("unterminated OSC 133: {}", common::ascii(&bytes[start - OSC_133.len()..])) };
        let st = rest[end] == 0x1b && rest.get(end + 1) == Some(&b'\\');
        if strict && !st {
            panic!("OSC 133 without the string terminator: {}", common::ascii(&bytes[start - OSC_133.len()..start + end + 2]));
        }
        let payload = &rest[..end];
        let fields: Vec<&[u8]> = payload.split(|&b| b == b';').collect();
        let mark = match fields[0] {
            b"A" if !strict || fields.len() == 1 => Mark::A,
            b"B" if !strict || fields.len() == 1 => Mark::B,
            b"C" if !strict || fields.len() == 1 => Mark::C,
            b"D" if fields.len() >= 2 && (!strict || fields.len() == 2) => {
                let digits = fields[1];
                if !((1..=3).contains(&digits.len()) && digits.iter().all(u8::is_ascii_digit)) {
                    panic!("OSC 133 D without a status: {}", common::ascii(payload));
                }
                Mark::D(std::str::from_utf8(digits).expect("digits").parse().expect("number"))
            }
            _ => panic!("OSC 133 payload that is no mark: {}", common::ascii(payload)),
        };
        marks.push(mark);
        i = start + end + if st { 2 } else { 1 };
    }
    marks
}

/// How many OSC 133 sequences end with the string terminator, which is how the hooks write them (a shell's own
/// marks end with BEL).
fn string_terminated(bytes: &[u8]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while let Some(at) = bytes[i..].windows(OSC_133.len()).position(|w| w == OSC_133) {
        let start = i + at + OSC_133.len();
        let rest = &bytes[start..];
        match rest.iter().position(|&b| b == 0x1b || b == 0x07) {
            Some(end) => {
                if rest[end] == 0x1b && rest.get(end + 1) == Some(&b'\\') {
                    n += 1;
                }
                i = start + end;
            }
            None => break,
        }
    }
    n
}

/// A real shell in a PTY and everything it wrote.
struct Pane {
    pty: Option<Pty>,
    rx: Receiver<Vec<u8>>,
    bytes: Vec<u8>,
    /// The last bytes seen, so that a query split between two reads is still answered once.
    carry: Vec<u8>,
    dir: PathBuf,
    /// A stand-in `wsh`, when the test gave none.
    _stub: Option<Stub>,
    /// The shell writes OSC 133 marks itself (fish 4): the hook then adds none, and its marks are read loosely.
    native: bool,
    /// Where the hook was loaded, for a shell that marks natively.
    loaded_at: usize,
}

impl Pane {
    /// Start `program` with `args` in an 80 by 24 PTY. `HOME` and the working directory are a fresh scratch
    /// directory; `PATH` has the stub directory first; `HOLZWAVE_IPC` is the scratch directory when `ipc` is true
    /// and empty when false (the parent may itself run in a holzWave pane and carry the variable).
    fn start(program: &Path, args: &[&str], ipc: bool, stub_dir: Option<&Path>, extra_env: &[(&str, &str)]) -> Self {
        let dir = common::scratch_dir("hooks");
        let path = std::env::var("PATH").unwrap_or_default();
        let own = if stub_dir.is_none() { Some(Stub::new()) } else { None };
        let stub = stub_dir.map(Path::to_path_buf).or_else(|| own.as_ref().map(|s| s.dir.clone()));
        let path = match stub {
            Some(stub) => format!("{}:{path}", stub.display()),
            None => path,
        };
        let mut options = SpawnOptions::new(program.to_string_lossy().into_owned(), 80, 24)
            .args(args.iter().copied())
            .cwd(&dir)
            .env("HOME", dir.to_string_lossy().into_owned())
            .env("PATH", path)
            .env("HOLZWAVE_IPC", if ipc { dir.to_string_lossy().into_owned() } else { String::new() })
            .env("PROMPT_COMMAND", "")
            .env("PS0", "")
            .env("HISTFILE", dir.join(".history").to_string_lossy().into_owned());
        for (key, value) in extra_env {
            options = options.env(*key, *value);
        }
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
        Self { pty: Some(pty), rx, bytes: Vec::new(), carry: Vec::new(), dir, _stub: own, native: false, loaded_at: 0 }
    }

    /// The scratch directory (home and working directory of the shell).
    fn dir(&self) -> &Path {
        &self.dir
    }

    /// How many prompts were marked so far.
    fn prompts(&self) -> usize {
        count(&self.bytes, MARK_A)
    }

    /// Load the hook for the first time and wait for the prompt after it.
    fn load(&mut self, sh: &Shell) {
        self.loaded_at = self.bytes.len();
        self.run(&sh.load_line());
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

    /// Send `line` and read until the next prompt was marked.
    fn run(&mut self, line: &str) {
        let n = self.prompts() + 1;
        self.send(line);
        self.wait_prompts(n, Duration::from_secs(15));
    }

    /// Read until `needle` occurred `n` times, then a little longer.
    fn wait_count(&mut self, needle: &str, n: usize) {
        let needle = needle.as_bytes().to_vec();
        self.pump_until(Duration::from_secs(15), &format!("{n} times {:?}", needle.escape_ascii().to_string()), |b| {
            count(b, &needle) >= n
        });
        self.pump(Duration::from_millis(150));
    }

    /// Send a `printf` that writes `{tag}<value>{tag}` and return the value. `args` is what follows the format.
    fn capture(&mut self, tag: &str, args: &str) -> String {
        let end = format!(">{tag}\r\n");
        let n = count(&self.bytes, end.as_bytes()) + 1;
        self.send(&format!("printf '{tag}<%s>{tag}\\n' {args}"));
        self.wait_count(&end, n);
        let text = String::from_utf8_lossy(&self.bytes).into_owned();
        let open = format!("{tag}<");
        let close = format!(">{tag}\r\n");
        let to = text.rfind(&close).expect("the answer");
        let from = text[..to].rfind(&open).expect("the start of the answer") + open.len();
        text[from..to].to_owned()
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

    /// The marks written since the hook was loaded. A shell that marks natively wrote the marks of the load command
    /// itself (C and D); they are dropped, so that every shell reads as `[A]` right after the load.
    fn marks(&self) -> Vec<Mark> {
        let mut marks = parse_marks(&self.bytes[self.loaded_at..], !self.native);
        if self.native {
            assert_eq!(marks[..2], [Mark::C, Mark::D(0)], "{}", common::ascii(&self.bytes[self.loaded_at..]));
            marks.drain(..2);
        }
        marks
    }

    /// The marks of one command that ended with `status`, as this shell writes them.
    fn command(&self, status: u32) -> Vec<Mark> {
        if self.native { vec![Mark::C, Mark::D(status), Mark::A] } else { vec![Mark::B, Mark::C, Mark::D(status), Mark::A] }
    }

    /// The marks of `n` commands that each ended with status 0.
    fn commands(&self, n: usize) -> Vec<Mark> {
        (0..n).flat_map(|_| self.command(0)).collect()
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


/// The path of a hook file.
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
    let mut pane = Pane::start(&bash, &["--norc", "--noprofile", "-i"], true, None, &[]);
    pane.send("PS1='$ '");
    pane.settle();
    pane.loaded_at = pane.bytes.len();
    pane.run(&format!("source '{}'", hook_path("bash.sh").display()));
    assert_eq!(pane.marks(), [Mark::A]);
    let before = pane.bytes.len();
    pane.run("printf 'alpha\\nbeta\\n'");
    assert_eq!(pane.marks()[1..], [Mark::B, Mark::C, Mark::D(0), Mark::A], "{}", common::ascii(&pane.bytes[before..]));
    assert_eq!(pane.outputs(), ["alpha\nbeta"], "{}", common::ascii(&pane.bytes[before..]));
    assert!(!pane.bytes.iter().any(|&b| b == 1 || b == 2), "readline brackets leaked: {}", common::ascii(&pane.bytes));
    println!("shell_hooks: ran bash {major}.{minor}");
}

/// The shells the hooks support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bash,
    Zsh,
    Fish,
}

/// An installed shell.
struct Shell {
    kind: Kind,
    program: PathBuf,
    version: String,
    /// `fish_features` for fish: `no-mark-prompt` makes a fish 4 behave like one that does not mark by itself.
    features: Option<&'static str>,
}

/// The first line of the output of `program` with `arg`, reduced to its version number.
fn version_of(program: &Path, arg: &str) -> String {
    let out = Command::new(program).arg(arg).output().expect("run the shell for its version");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = text.lines().next().unwrap_or("");
    line.split(|c: char| c.is_whitespace() || c == ',')
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or(line)
        .to_owned()
}

/// The shell of `kind`, or `None` after the skip line.
fn shell(kind: Kind) -> Option<Shell> {
    match kind {
        Kind::Bash => {
            let Some((program, (major, minor))) = modern_bash() else {
                skipped("bash", "no bash 4.4 or newer");
                return None;
            };
            Some(Shell { kind, program, version: format!("{major}.{minor}"), features: None })
        }
        Kind::Zsh => {
            let Some(program) = find_program("zsh") else {
                skipped("zsh", "not installed");
                return None;
            };
            let version = version_of(&program, "--version");
            Some(Shell { kind, program, version, features: None })
        }
        Kind::Fish => {
            let Some(program) = find_program("fish") else {
                skipped("fish", "not installed");
                return None;
            };
            let version = version_of(&program, "--version");
            Some(Shell { kind, program, version, features: None })
        }
    }
}

impl Shell {
    fn name(&self) -> &'static str {
        match self.kind {
            Kind::Bash => "bash",
            Kind::Zsh => "zsh",
            Kind::Fish => "fish",
        }
    }

    fn args(&self) -> &'static [&'static str] {
        match self.kind {
            Kind::Bash => &["--norc", "--noprofile", "-i"],
            Kind::Zsh => &["-f", "-i"],
            Kind::Fish => &["--no-config", "-i"],
        }
    }

    /// Lines that put the shell in a known state before the hook is loaded.
    fn setup(&self) -> &'static [&'static str] {
        match self.kind {
            Kind::Bash => &["PS1='$ '"],
            Kind::Zsh => &["unsetopt prompt_sp", "PROMPT='%# '"],
            Kind::Fish => &[],
        }
    }

    fn hook_file(&self) -> PathBuf {
        hook_path(match self.kind {
            Kind::Bash => "bash.sh",
            Kind::Zsh => "zsh.zsh",
            Kind::Fish => "fish.fish",
        })
    }

    /// The line that loads the hook the way a user does after editing the rc file.
    fn load_line(&self) -> String {
        format!("source '{}'", self.hook_file().display())
    }

    /// The line an rc file holds: what the text of `wsh init` is fed to.
    fn eval_line(&self) -> String {
        match self.kind {
            Kind::Fish => format!("cat '{}' | source", self.hook_file().display()),
            _ => format!("eval \"$(cat '{}')\"", self.hook_file().display()),
        }
    }

    /// A command that prints the text `ok-2` (and nothing before it that contains it).
    fn say_ok(&self) -> &'static str {
        match self.kind {
            Kind::Fish => "echo ok-(math 1+1)",
            _ => "echo ok-$((1+1))",
        }
    }

    /// The command that prints the status of the last command as `status=N`.
    fn say_status(&self) -> &'static str {
        match self.kind {
            Kind::Fish => "echo status=$status",
            _ => "echo status=$?",
        }
    }

    /// `printf` arguments that print the definition of the prompt.
    fn prompt_args(&self) -> &'static str {
        match self.kind {
            Kind::Bash => "\"$PS1\"",
            Kind::Zsh => "\"$PROMPT\"",
            Kind::Fish => "(functions fish_prompt | string collect)",
        }
    }

    /// Lines of a prompt framework that rebuilds the prompt on every prompt, the first time and again after the hook.
    fn framework(&self, again: bool) -> &'static str {
        match (self.kind, again) {
            (Kind::Bash, false) => "__fw() { PS1='fw$ '; }; PROMPT_COMMAND=\"${PROMPT_COMMAND:+$PROMPT_COMMAND;}__fw\"",
            (Kind::Bash, true) => "PROMPT_COMMAND=\"${PROMPT_COMMAND:+$PROMPT_COMMAND;}__fw\"",
            (Kind::Zsh, false) => "__fw() { PROMPT='fw%# ' }; precmd_functions+=(__fw)",
            (Kind::Zsh, true) => "precmd_functions=(__fw $precmd_functions)",
            (Kind::Fish, false) => "function fish_prompt; echo -n 'fw> '; end",
            (Kind::Fish, true) => "function fish_prompt; echo -n 'fw2> '; end",
        }
    }

    /// `printf` arguments that print what the hook leaves behind (all of it empty or separators when the hook did
    /// nothing).
    fn residue_args(&self) -> &'static str {
        match self.kind {
            Kind::Bash => "\"$PROMPT_COMMAND|$PS0|${__holzwave_hook-}|${__holzwave_ran-}\"",
            Kind::Zsh => "\"${precmd_functions[*]}|${preexec_functions[*]}|${__holzwave_hook-}\"",
            Kind::Fish => "(string join '|' \"$__holzwave_hook\" (functions -n | string match '__holzwave*'))",
        }
    }

    fn env(&self) -> Vec<(&'static str, &'static str)> {
        self.features.map(|f| vec![("fish_features", f)]).unwrap_or_default()
    }

    fn ran(&self) {
        let how = match self.features {
            Some(f) => format!(" ({f})"),
            None => String::new(),
        };
        println!("shell_hooks: ran {} {}{how}", self.name(), self.version);
    }
}

/// Whether a hookless shell writes OSC 133 marks itself: only fish 4 and newer does, and only with the feature on.
fn marks_natively(sh: &Shell) -> bool {
    if sh.kind != Kind::Fish {
        return false;
    }
    let mut p = Pane::start(&sh.program, sh.args(), false, None, &sh.env());
    p.pump_until(Duration::from_secs(20), "the first output", |b| !b.is_empty());
    p.settle();
    count(&p.bytes, MARK_A) > 0
}

/// A directory with a `wsh` stand-in that logs its arguments, one call per line.
struct Stub {
    dir: PathBuf,
}

impl Stub {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = common::scratch_dir("hooks-stub");
        let script = dir.join("wsh");
        let log = dir.join("wsh.log");
        std::fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display())).expect("stub");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        Self { dir }
    }

    fn log(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("wsh.log")).unwrap_or_default().lines().map(str::to_owned).collect()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A shell that has started and is in its known state.
fn boot(sh: &Shell, ipc: bool, stub: Option<&Stub>) -> Pane {
    let mut pane = Pane::start(&sh.program, sh.args(), ipc, stub.map(|s| s.dir.as_path()), &sh.env());
    pane.native = marks_natively_for(sh);
    let first = if sh.kind == Kind::Fish { 20 } else { 15 };
    pane.pump_until(Duration::from_secs(first), "the first output", |b| !b.is_empty());
    pane.settle();
    for line in sh.setup() {
        pane.send(line);
        pane.settle();
    }
    pane
}

/// `marks_natively`, asked once per shell setup.
fn marks_natively_for(sh: &Shell) -> bool {
    use std::sync::OnceLock;
    static PLAIN: OnceLock<bool> = OnceLock::new();
    static NO_MARKS: OnceLock<bool> = OnceLock::new();
    match (sh.kind, sh.features) {
        (Kind::Fish, None) => *PLAIN.get_or_init(|| marks_natively(sh)),
        (Kind::Fish, Some(_)) => *NO_MARKS.get_or_init(|| marks_natively(sh)),
        _ => false,
    }
}

fn marks_each_command_once(sh: &Shell) {
    let mut p = boot(sh, true, None);
    p.load(sh);
    assert_eq!(p.marks(), [Mark::A]);
    p.run("printf 'one\\n'");
    p.run("false");
    p.run("printf 'two\\n'");
    let want: Vec<Mark> = [Mark::A].into_iter().chain(p.command(0)).chain(p.command(1)).chain(p.command(0)).collect();
    assert_eq!(p.marks(), want, "{}", common::ascii(&p.bytes));
    assert_eq!(p.outputs(), ["one", "two"], "{}", common::ascii(&p.bytes));
    sh.ran();
}

fn writes_no_command_marks_for_an_empty_line(sh: &Shell) {
    let mut p = boot(sh, true, None);
    p.load(sh);
    p.run("");
    assert_eq!(p.marks(), [Mark::A, Mark::A], "{}", common::ascii(&p.bytes));
    sh.ran();
}

fn keeps_the_status_for_the_next_prompt(sh: &Shell) {
    let mut p = boot(sh, true, None);
    p.load(sh);
    p.run("false");
    p.run(sh.say_status());
    assert_eq!(p.outputs(), ["status=1"], "{}", common::ascii(&p.bytes));
    let want: Vec<Mark> = [Mark::A].into_iter().chain(p.command(1)).chain(p.command(0)).collect();
    assert_eq!(p.marks(), want);
    sh.ran();
}

fn never_edits_the_prompt_and_survives_a_framework(sh: &Shell) {
    let mut p = boot(sh, true, None);
    let before = p.capture("P0", sh.prompt_args());
    p.load(sh);
    let after = p.capture("P0", sh.prompt_args());
    assert_eq!(before, after, "loading the hook changed the prompt definition");
    drop(p);

    let mut p = boot(sh, true, None);
    p.send(sh.framework(false));
    p.settle();
    p.load(sh);
    p.run("printf 'one\\n'");
    p.run("printf 'two\\n'");
    p.run(sh.framework(true));
    p.run("printf 'three\\n'");
    let want: Vec<Mark> = [Mark::A].into_iter().chain(p.commands(4)).collect();
    assert_eq!(p.marks(), want, "{}", common::ascii(&p.bytes));
    assert_eq!(p.outputs(), ["one", "two", "three"], "{}", common::ascii(&p.bytes));
    sh.ran();
}

fn sourced_twice_marks_once(sh: &Shell) {
    let mut p = boot(sh, true, None);
    p.load(sh);
    p.run(&sh.load_line());
    p.run("printf 'x\\n'");
    let want: Vec<Mark> = [Mark::A].into_iter().chain(p.commands(2)).collect();
    assert_eq!(p.marks(), want, "{}", common::ascii(&p.bytes));
    assert_eq!(p.outputs(), ["x"]);
    sh.ran();
}

fn still_reports_history(sh: &Shell) {
    let stub = Stub::new();
    let mut p = boot(sh, true, Some(&stub));
    p.load(sh);
    p.run("false");
    p.run("echo hist-probe");
    let name = p.dir().file_name().expect("name").to_string_lossy().into_owned();
    let has = |status: &str, command: &str| {
        let log = stub.log();
        let hits = log
            .iter()
            .filter(|l| l.starts_with(&format!("hist add {status} ")) && l.contains(&format!("/{name} ")) && l.ends_with(command))
            .count();
        hits == 1
    };
    assert!(
        common::wait_until(Duration::from_secs(3), || has("1", " false") && has("0", " echo hist-probe")),
        "the history calls were: {:?}",
        stub.log()
    );
    sh.ran();
}

fn does_nothing_outside_holzwave(sh: &Shell) {
    let mut p = boot(sh, false, None);
    let rc = p.dir().join("rc");
    std::fs::write(&rc, format!("{}\necho after-rc-done\n", sh.eval_line())).expect("rc file");
    p.send(&format!("source '{}'", rc.display()));
    p.wait_count("after-rc-done", 1);
    p.send(sh.say_ok());
    p.wait_count("ok-2", 1);
    // A shell may write marks of its own (fish 4, with BEL); the hook writes none, so none ends with the string terminator.
    assert_eq!(string_terminated(&p.bytes), 0, "marks outside holzWave: {}", common::ascii(&p.bytes));
    let left = p.capture("V0", sh.residue_args());
    assert!(left.chars().all(|c| c == '|'), "the hook left {left:?} behind");
    sh.ran();
}

fn is_nounset_safe(sh: &Shell) {
    let mut p = boot(sh, true, None);
    p.send("set -u");
    p.settle();
    p.load(sh);
    p.run(sh.say_ok());
    let text = String::from_utf8_lossy(&p.bytes).into_owned();
    assert!(!text.contains("unbound variable") && !text.contains("parameter not set"), "{text:?}");
    let want: Vec<Mark> = [Mark::A].into_iter().chain(p.commands(1)).collect();
    assert_eq!(p.marks(), want, "{}", common::ascii(&p.bytes));
    assert_eq!(p.outputs(), ["ok-2"]);
    sh.ran();
}

macro_rules! per_shell {
    ($scenario:ident => $bash:ident, $zsh:ident, $fish:ident) => {
        #[test]
        fn $bash() {
            if let Some(sh) = shell(Kind::Bash) {
                $scenario(&sh);
            }
        }
        #[test]
        fn $zsh() {
            if let Some(sh) = shell(Kind::Zsh) {
                $scenario(&sh);
            }
        }
        #[test]
        fn $fish() {
            if let Some(sh) = shell(Kind::Fish) {
                $scenario(&sh);
            }
        }
    };
}

per_shell!(marks_each_command_once => bash_hook_marks_each_command_once, zsh_hook_marks_each_command_once, fish_hook_marks_each_command_once);
per_shell!(writes_no_command_marks_for_an_empty_line => bash_hook_writes_no_command_marks_for_an_empty_line, zsh_hook_writes_no_command_marks_for_an_empty_line, fish_hook_writes_no_command_marks_for_an_empty_line);
per_shell!(keeps_the_status_for_the_next_prompt => bash_hook_keeps_the_status_for_the_next_prompt, zsh_hook_keeps_the_status_for_the_next_prompt, fish_hook_keeps_the_status_for_the_next_prompt);
per_shell!(never_edits_the_prompt_and_survives_a_framework => bash_hook_never_edits_the_prompt_and_survives_a_framework, zsh_hook_never_edits_the_prompt_and_survives_a_framework, fish_hook_never_edits_the_prompt_and_survives_a_framework);
per_shell!(sourced_twice_marks_once => bash_hook_sourced_twice_marks_once, zsh_hook_sourced_twice_marks_once, fish_hook_sourced_twice_marks_once);
per_shell!(still_reports_history => bash_hook_still_reports_history, zsh_hook_still_reports_history, fish_hook_still_reports_history);
per_shell!(does_nothing_outside_holzwave => bash_hook_does_nothing_outside_holzwave, zsh_hook_does_nothing_outside_holzwave, fish_hook_does_nothing_outside_holzwave);

#[test]
fn bash_hook_is_nounset_safe() {
    if let Some(sh) = shell(Kind::Bash) {
        is_nounset_safe(&sh);
    }
}

#[test]
fn zsh_hook_is_nounset_safe() {
    if let Some(sh) = shell(Kind::Zsh) {
        is_nounset_safe(&sh);
    }
}

#[test]
fn bash_hook_of_an_old_bash_reports_history_only() {
    let old = find_programs("bash").into_iter().filter_map(|p| bash_version(&p).map(|v| (p, v))).find(|(_, v)| *v < (4, 4));
    let Some((program, (major, minor))) = old else {
        return skipped("bash", "no bash older than 4.4");
    };
    let sh = Shell { kind: Kind::Bash, program, version: format!("{major}.{minor}"), features: None };
    let stub = Stub::new();
    let mut p = boot(&sh, true, Some(&stub));
    p.send(&sh.load_line());
    p.settle();
    p.send("echo hist-old-$((1+1))");
    p.wait_count("hist-old-2", 1);
    p.settle();
    assert_eq!(count(&p.bytes, OSC_133), 0, "an old bash wrote marks: {}", common::ascii(&p.bytes));
    let name = p.dir().file_name().expect("name").to_string_lossy().into_owned();
    assert!(
        common::wait_until(Duration::from_secs(3), || stub
            .log()
            .iter()
            .any(|l| l.starts_with("hist add 0 ") && l.contains(&format!("/{name} ")) && l.ends_with(" echo hist-old-$((1+1))"))),
        "the history calls were: {:?}",
        stub.log()
    );
    sh.ran();
}

/// A fish that marks by itself (fish 4) gets no second set from the hook, and one that does not (an older one, or
/// `fish_features=no-mark-prompt`) gets the hook's marks: with or without native marks each prompt has exactly one A.
#[test]
fn fish_hook_adds_no_second_set_of_marks_to_a_fish_that_marks_itself() {
    let Some(mut sh) = shell(Kind::Fish) else { return };
    for features in [None, Some("no-mark-prompt")] {
        sh.features = features;
        let native = marks_natively_for(&sh);
        let mut p = boot(&sh, true, None);
        p.native = native;
        let hookless_first = p.prompts();
        assert_eq!(hookless_first, usize::from(native), "a hookless fish: {}", common::ascii(&p.bytes));
        p.load(&sh);
        let at = p.prompts();
        p.run("true");
        p.run("true");
        assert_eq!(p.prompts() - at, 2, "each prompt has exactly one A: {}", common::ascii(&p.bytes[p.loaded_at..]));
        assert_eq!(string_terminated(&p.bytes[p.loaded_at..]) > 0, !native, "the hook writes marks only when the shell does not");
        sh.ran();
    }
}

/// The same scenarios on a fish that has to rely on the hook for its marks.
#[test]
fn fish_hook_without_native_marks_marks_each_command_once() {
    let Some(mut sh) = shell(Kind::Fish) else { return };
    sh.features = Some("no-mark-prompt");
    marks_each_command_once(&sh);
}

#[test]
fn fish_hook_without_native_marks_never_edits_the_prompt_and_survives_a_framework() {
    let Some(mut sh) = shell(Kind::Fish) else { return };
    sh.features = Some("no-mark-prompt");
    never_edits_the_prompt_and_survives_a_framework(&sh);
}

#[test]
fn fish_hook_without_native_marks_sourced_twice_marks_once() {
    let Some(mut sh) = shell(Kind::Fish) else { return };
    sh.features = Some("no-mark-prompt");
    sourced_twice_marks_once(&sh);
}

#[test]
fn hook_texts_parse_in_their_own_shell() {
    for (kind, args, file) in [(Kind::Bash, vec!["-n"], "bash.sh"), (Kind::Zsh, vec!["-n"], "zsh.zsh"), (Kind::Fish, vec!["--no-execute"], "fish.fish")] {
        let Some(sh) = shell(kind) else { continue };
        let out = Command::new(&sh.program).args(&args).arg(hook_path(file)).output().expect("run the parser");
        assert!(out.status.success(), "{} does not parse {file}: {}", sh.name(), String::from_utf8_lossy(&out.stderr));
        sh.ran();
    }
}
