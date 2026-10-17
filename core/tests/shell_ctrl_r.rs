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

/// The line that loads the copy of a hook file, as an rc file would.
const LOAD: &str = "source ./h";

/// One shell the scenarios run in.
struct Shell {
    name: &'static str,
    program: PathBuf,
    version: String,
    args: &'static [&'static str],
    /// Bytes that show the shell is ready for its first line (fish probes the terminal first and is slow to start).
    ready: Option<&'static str>,
    /// Lines that give a known plain prompt.
    setup: &'static [&'static str],
    /// The prompt those lines give.
    prompt: &'static str,
    /// The binding file under `wsh/hooks`.
    file: &'static str,
    /// The history hook under `wsh/hooks`, which the binding sits beside.
    base: &'static str,
    /// What the shell's own reverse search prints, when it prints something recognisable.
    search: Option<&'static str>,
    /// Lines that turn on the vi key bindings.
    vi: &'static [&'static str],
    /// The line that makes an unset variable an error, for a shell that has one.
    nounset: Option<&'static str>,
    /// The name of the exit status in an `echo`.
    status: &'static str,
}

impl Shell {
    fn bash() -> Option<Shell> {
        let Some((program, (major, minor))) = modern_bash() else {
            println!("shell_ctrl_r: skipped bash: no bash 4.0 or newer was found");
            return None;
        };
        Some(Shell::of_bash(program, major, minor))
    }

    /// A bash older than 4.0, which gets no binding.
    fn old_bash() -> Option<Shell> {
        let old = find_programs("bash").into_iter().filter_map(|p| bash_version(&p).map(|v| (p, v))).find(|(_, v)| v.0 < 4);
        let Some((program, (major, minor))) = old else {
            println!("shell_ctrl_r: skipped bash: no bash older than 4.0 was found");
            return None;
        };
        Some(Shell::of_bash(program, major, minor))
    }

    fn of_bash(program: PathBuf, major: u32, minor: u32) -> Shell {
        Shell {
            name: "bash",
            program,
            version: format!("{major}.{minor}"),
            args: &["--norc", "--noprofile", "-i"],
            ready: None,
            setup: &["PS1='hwr$ '"],
            prompt: "hwr$ ",
            file: "bash-ctrl-r.sh",
            base: "bash.sh",
            search: Some("reverse-i-search"),
            vi: &["set -o vi"],
            nounset: Some("set -u"),
            status: "?",
        }
    }

    fn zsh() -> Option<Shell> {
        let Some(program) = find_programs("zsh").into_iter().next() else {
            println!("shell_ctrl_r: skipped zsh: not installed");
            return None;
        };
        let version = version_of(&program, "--version");
        Some(Shell {
            name: "zsh",
            program,
            version,
            args: &["-f", "-i"],
            ready: None,
            setup: &["PROMPT='hwr> '", "unsetopt prompt_sp"],
            prompt: "hwr> ",
            file: "zsh-ctrl-r.zsh",
            base: "zsh.zsh",
            search: Some("bck-i-search"),
            vi: &["bindkey -v"],
            nounset: Some("setopt nounset"),
            status: "?",
        })
    }

    fn fish() -> Option<Shell> {
        let Some(program) = find_programs("fish").into_iter().next() else {
            println!("shell_ctrl_r: skipped fish: not installed");
            return None;
        };
        let version = version_of(&program, "--version");
        Some(Shell {
            name: "fish",
            program,
            version,
            args: &["--no-config", "-i"],
            ready: Some("\u{1b}[?2004h"),
            setup: &["function fish_prompt; echo -n 'hwr> '; end"],
            prompt: "hwr> ",
            file: "fish-ctrl-r.fish",
            base: "fish.fish",
            search: None,
            vi: &["fish_vi_key_bindings"],
            nounset: None,
            status: "status",
        })
    }

    /// The seconds to wait for the first prompt.
    fn first_prompt(&self) -> u64 {
        if self.name == "fish" { 20 } else { 10 }
    }
}

/// The first line of what `program flag` prints, for the report.
fn version_of(program: &Path, flag: &str) -> String {
    let out = Command::new(program).arg(flag).output().ok();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    text.lines().next().unwrap_or("unknown").replace("zsh ", "").replace("fish, version ", "").trim().to_owned()
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
        if let Some(ready) = shell.ready {
            self.wait_for(ready, 1, shell.first_prompt());
        }
        self.settle();
        for line in shell.setup {
            self.line(line);
            self.settle();
        }
    }

    /// Load a file of the hooks directory the way an rc file does, and wait for the prompt. The file is copied to a short
    /// name in the scratch directory first, so that a line editor that redraws a line wider than the terminal stays out
    /// of the way.
    fn load(&mut self, file: &str) {
        std::fs::copy(hooks_dir().join(file), self.dir.join("h")).expect("copy the hook");
        self.line(LOAD);
        self.settle();
    }

    /// Read until the text `text` was printed alone on a line (as the output of a command, not as the echo of the typed line).
    fn wait_output(&mut self, text: &str, secs: u64) {
        // After a carriage return, a line feed or the end of a mark (the hooks write marks right before the output).
        let starts = ["\r", "\n", "\x1b\\"].map(|lead| format!("{lead}{text}\r\n"));
        let end = Instant::now() + Duration::from_secs(secs);
        while starts.iter().all(|needle| count(&self.bytes, needle.as_bytes()) == 0) {
            let Some(left) = end.checked_duration_since(Instant::now()) else {
                let tail = &self.bytes[self.bytes.len().saturating_sub(600)..];
                panic!("timed out waiting for the output {text:?}; the last bytes: {}", common::ascii(tail));
            };
            match self.rx.recv_timeout(left.min(Duration::from_millis(20))) {
                Ok(chunk) => self.take(chunk),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the shell ended; bytes: {}", common::ascii(&self.bytes)),
            }
        }
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

/// The text of `bytes` without the escape sequences a shell writes around its prompt.
fn plain(bytes: &[u8]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            i += 2;
            while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                i += 1;
            }
            i += 1;
        } else if bytes[i] == 0x08 {
            out.pop();
            i += 1;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Start `sh` in a holzWave pane (`ipc`) or outside one, with its plain prompt.
fn start(sh: &Shell, ipc: bool) -> Rig {
    let mut rig = Rig::start(&sh.program, sh.args, ipc);
    rig.prepare(sh);
    rig
}

fn report(sh: &Shell) {
    println!("shell_ctrl_r: ran {} {}", sh.name, sh.version);
}

fn asks_the_app_and_keeps_the_line(sh: &Shell) {
    let mut rig = start(sh, true);
    rig.load(sh.file);
    rig.send(b"echo kept");
    rig.send(&[0x12]);
    rig.wait_calls(1, 3);
    assert_eq!(rig.calls(), ["hist open"]);
    rig.send(b"\r");
    rig.wait_output("kept", 5);
    assert_eq!(rig.calls().len(), 1, "the key asked once and the line did not ask again");
    report(sh);
}

fn keeps_the_status_of_the_last_command(sh: &Shell) {
    let mut rig = start(sh, true);
    rig.load(sh.file);
    rig.line("false");
    rig.settle();
    rig.send(&[0x12]);
    rig.wait_calls(1, 3);
    rig.settle();
    rig.line(&format!("echo status=${}", sh.status));
    rig.wait_output("status=1", 5);
    assert_eq!(rig.calls(), ["hist open"]);
    report(sh);
}

fn stays_with_the_shell_outside_holzwave(sh: &Shell) {
    let mut rig = start(sh, false);
    // What the shell writes to load a file that does nothing (fish writes marks and a title of its own).
    std::fs::write(rig.dir.join("h"), "# nothing\n").expect("control file");
    let before = rig.bytes.len();
    rig.line(LOAD);
    rig.settle();
    let control = plain(&rig.bytes[before..]);
    let before = rig.bytes.len();
    rig.load(sh.file);
    let loaded = plain(&rig.bytes[before..]);
    assert_eq!(loaded, control, "loading the binding wrote something that loading an empty file does not");
    if sh.name != "fish" {
        let rest = loaded.replace(LOAD, "").replace(sh.prompt, "").replace(['\r', '\n'], "");
        assert_eq!(rest, "", "loading wrote something beyond the echo of the line and the prompt");
    }
    rig.line("echo after-hook");
    rig.wait_output("after-hook", 5);
    rig.settle();
    rig.send(&[0x12]);
    if let Some(marker) = sh.search {
        rig.wait_for(marker, 1, 5);
    }
    rig.settle();
    assert!(rig.calls().is_empty(), "nothing was asked outside holzWave: {:?}", rig.calls());
    rig.send(&[0x03]);
    rig.settle();
    report(sh);
}

fn is_nounset_safe(sh: &Shell) {
    let mut rig = start(sh, true);
    rig.line(sh.nounset.expect("a shell with an unset-variable error"));
    rig.settle();
    let before = rig.bytes.len();
    rig.load(sh.file);
    rig.line("echo ok");
    rig.wait_output("ok", 5);
    let seen = plain(&rig.bytes[before..]);
    for bad in ["unbound variable", "parameter not set"] {
        assert!(!seen.contains(bad), "{bad}: {seen}");
    }
    report(sh);
}

fn sourced_twice_asks_once(sh: &Shell) {
    let mut rig = start(sh, true);
    rig.load(sh.file);
    rig.load(sh.file);
    rig.send(&[0x12]);
    rig.wait_calls(1, 3);
    rig.settle();
    assert_eq!(rig.calls(), ["hist open"]);
    report(sh);
}

fn binds_vi_insert_mode_too(sh: &Shell) {
    let mut rig = start(sh, true);
    rig.load(sh.file);
    for line in sh.vi {
        rig.line(line);
        rig.settle();
    }
    rig.send(&[0x12]);
    rig.wait_calls(1, 3);
    rig.settle();
    assert_eq!(rig.calls(), ["hist open"]);
    report(sh);
}

fn works_beside_the_history_and_mark_hooks(sh: &Shell) {
    if !hooks_dir().join(sh.base).is_file() {
        println!("shell_ctrl_r: skipped {}: the history hook {} does not exist", sh.name, sh.base);
        return;
    }
    let mut rig = start(sh, true);
    rig.load(sh.base);
    rig.load(sh.file);
    rig.send(&[0x12]);
    rig.wait_calls(rig.calls().len() + 1, 3);
    rig.settle();
    let at = rig.calls().iter().position(|c| c == "hist open").expect("the key asked");
    rig.line("echo beside-the-hook");
    rig.wait_output("beside-the-hook", 5);
    rig.settle();
    let after = rig.calls();
    assert_eq!(after.iter().filter(|c| *c == "hist open").count(), 1, "{after:?}");
    let report_at = after.iter().position(|c| c.starts_with("hist add 0 ") && c.ends_with("echo beside-the-hook")).unwrap_or_else(|| panic!("no history entry for the command: {after:?}"));
    assert!(at < report_at, "the key asked first: {after:?}");
    report(sh);
}

fn of_an_old_bash_binds_nothing(sh: &Shell) {
    let mut rig = start(sh, true);
    rig.load(sh.file);
    rig.send(&[0x12]);
    rig.wait_for(sh.search.expect("a search marker"), 1, 5);
    rig.settle();
    assert!(rig.calls().is_empty(), "no binding in a bash older than 4.0: {:?}", rig.calls());
    rig.send(&[0x03]);
    rig.settle();
    report(sh);
}

/// A one-line test of one scenario in one shell; a missing shell reports itself and passes.
macro_rules! scenario {
    ($name:ident, $shell:ident, $run:ident) => {
        #[test]
        fn $name() {
            if let Some(sh) = Shell::$shell() {
                $run(&sh);
            }
        }
    };
}

scenario!(bash_ctrl_r_asks_the_app_and_keeps_the_line, bash, asks_the_app_and_keeps_the_line);
scenario!(bash_ctrl_r_keeps_the_status_of_the_last_command, bash, keeps_the_status_of_the_last_command);
scenario!(bash_ctrl_r_stays_with_the_shell_outside_holzwave, bash, stays_with_the_shell_outside_holzwave);
scenario!(bash_ctrl_r_is_nounset_safe, bash, is_nounset_safe);
scenario!(bash_ctrl_r_sourced_twice_asks_once, bash, sourced_twice_asks_once);
scenario!(bash_ctrl_r_binds_vi_insert_mode_too, bash, binds_vi_insert_mode_too);
scenario!(bash_ctrl_r_works_beside_the_history_and_mark_hooks, bash, works_beside_the_history_and_mark_hooks);
scenario!(bash_ctrl_r_of_an_old_bash_binds_nothing, old_bash, of_an_old_bash_binds_nothing);

scenario!(zsh_ctrl_r_asks_the_app_and_keeps_the_line, zsh, asks_the_app_and_keeps_the_line);
scenario!(zsh_ctrl_r_keeps_the_status_of_the_last_command, zsh, keeps_the_status_of_the_last_command);
scenario!(zsh_ctrl_r_stays_with_the_shell_outside_holzwave, zsh, stays_with_the_shell_outside_holzwave);
scenario!(zsh_ctrl_r_is_nounset_safe, zsh, is_nounset_safe);
scenario!(zsh_ctrl_r_sourced_twice_asks_once, zsh, sourced_twice_asks_once);
scenario!(zsh_ctrl_r_binds_vi_insert_mode_too, zsh, binds_vi_insert_mode_too);
scenario!(zsh_ctrl_r_works_beside_the_history_and_mark_hooks, zsh, works_beside_the_history_and_mark_hooks);

scenario!(fish_ctrl_r_asks_the_app_and_keeps_the_line, fish, asks_the_app_and_keeps_the_line);
scenario!(fish_ctrl_r_keeps_the_status_of_the_last_command, fish, keeps_the_status_of_the_last_command);
scenario!(fish_ctrl_r_stays_with_the_shell_outside_holzwave, fish, stays_with_the_shell_outside_holzwave);
scenario!(fish_ctrl_r_sourced_twice_asks_once, fish, sourced_twice_asks_once);
scenario!(fish_ctrl_r_binds_vi_insert_mode_too, fish, binds_vi_insert_mode_too);
scenario!(fish_ctrl_r_works_beside_the_history_and_mark_hooks, fish, works_beside_the_history_and_mark_hooks);

#[test]
fn ctrl_r_texts_parse_in_their_own_shell() {
    let mut ran = Vec::new();
    for (shell, file, flag) in [("bash", "bash-ctrl-r.sh", "-n"), ("zsh", "zsh-ctrl-r.zsh", "-n"), ("fish", "fish-ctrl-r.fish", "--no-execute")] {
        let programs = find_programs(shell);
        if programs.is_empty() {
            println!("shell_ctrl_r: skipped {shell}: not installed");
            continue;
        }
        for program in programs {
            let out = Command::new(&program).arg(flag).arg(hooks_dir().join(file)).output().expect("run the shell");
            assert!(out.status.success(), "{} {flag} {file}: {}", program.display(), String::from_utf8_lossy(&out.stderr));
            ran.push(format!("{shell} {}", program.display()));
        }
    }
    println!("shell_ctrl_r: parsed with {}", ran.join(", "));
}
