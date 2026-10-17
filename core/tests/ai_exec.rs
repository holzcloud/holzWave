//! The local runner of the command tool, with no terminal, no page and no network.
//!
//! Every test that starts a process runs a real `/bin/sh` and is marked for Unix; the tests marked for Windows run only on a
//! Windows machine. Nothing here imports the shell: the runner is `holzwave_core::exec::LocalExec` and its output is
//! `holzwave_core::ssh::ExecResult`, the same value the exec primitive of a saved connection gives.

#![cfg(any(unix, windows))]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use holzwave_core::exec::{ALLOWED_ENV, ALLOWED_ENV_PREFIXES, LocalExec, env_allowed};
use holzwave_core::ssh::{ExecCancel, ExecEnd, ExecLimits, ExecResult};

mod common;

/// Text a command prints, and values in the environment of the app; they appear in test code only.
const OUT_CANARY: &str = "zq-cmd-out-6a1f40";
const ENV_CANARY: &str = "zq-env-secret-d41c";

/// A runner over `pairs` as the environment of the app.
fn local(pairs: &[(&str, &str)]) -> LocalExec {
    LocalExec::new(pairs.iter().map(|(name, value)| (OsString::from(name), OsString::from(value))))
}

/// A runner with only the `PATH` of this process, so the commands find their programs.
fn plain() -> LocalExec {
    let path = std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/usr/bin:/bin"));
    LocalExec::new([(OsString::from("PATH"), path)])
}

fn limits(millis: u64) -> ExecLimits {
    ExecLimits { max_output: 65536, timeout: Duration::from_millis(millis) }
}

fn run(runner: &LocalExec, command: &str, dir: &Path, millis: u64) -> ExecResult {
    runner.exec(command, dir, &limits(millis), &ExecCancel::new()).expect("the command was started")
}

/// A scratch folder that is removed at the end of the test.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        Scratch(common::scratch_dir(tag))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Whether the process is alive: a zombie counts as dead.
#[cfg(unix)]
fn alive(pid: u32) -> bool {
    let out = std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().expect("ps runs");
    let state = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    !state.is_empty() && !state.starts_with('Z')
}

/// The number a command wrote to `path`, waited for.
#[cfg(unix)]
fn read_pid(path: &Path) -> u32 {
    let mut found = None;
    assert!(
        common::wait_until(Duration::from_secs(3), || {
            found = std::fs::read_to_string(path).ok().and_then(|text| text.trim().parse::<u32>().ok());
            found.is_some()
        }),
        "the command did not write {}",
        path.display()
    );
    found.expect("a pid")
}

#[cfg(unix)]
#[test]
fn a_command_runs_in_a_separate_process_and_its_output_comes_back() {
    let dir = Scratch::new("exec-run");
    let result = run(&plain(), &format!("printf '{OUT_CANARY}\\n'"), &dir.0, 5000);
    assert_eq!(text(&result.stdout), format!("{OUT_CANARY}\n"));
    assert!(result.stderr.is_empty(), "nothing on standard error");
    assert_eq!((result.status, result.end, result.truncated, result.dropped), (Some(0), ExecEnd::Finished, false, 0));

    let again = run(&plain(), "echo $$", &dir.0, 5000);
    let pid: u32 = text(&again.stdout).trim().parse().expect("a process id");
    assert_ne!(pid, std::process::id(), "the command ran in a process of its own");
}

#[cfg(unix)]
#[test]
fn stdout_stderr_and_the_exit_status_are_kept_apart() {
    let dir = Scratch::new("exec-status");
    let split = run(&plain(), "printf out; printf err >&2; exit 7", &dir.0, 5000);
    assert_eq!((text(&split.stdout), text(&split.stderr), split.status), ("out".to_owned(), "err".to_owned(), Some(7)));
    assert_eq!(split.end, ExecEnd::Finished);

    let signalled = run(&plain(), "kill -s TERM $$", &dir.0, 5000);
    assert_eq!(signalled.status, Some(143), "128 plus the signal");
    assert_eq!(signalled.end, ExecEnd::Finished);

    let missing = run(&plain(), "zq-no-such-program-91c4", &dir.0, 5000);
    assert_eq!(missing.status, Some(127));
}

#[cfg(unix)]
#[test]
fn the_child_gets_a_scrubbed_environment_and_a_closed_stdin() {
    let dir = Scratch::new("exec-env");
    let secret = ENV_CANARY;
    let base = [
        ("PATH", &std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_owned())[..]),
        ("HOME", "/home/holz"),
        ("LANG", "C"),
        ("LC_ALL", "C"),
        ("TZ", "UTC"),
        ("TMPDIR", "/tmp"),
        ("USER", "holz"),
        ("HOLZWAVE_IPC", secret),
        ("HOLZWAVE_TOKEN", secret),
        ("HOLZWAVE_PANE", secret),
        ("ANTHROPIC_API_KEY", secret),
        ("OPENAI_API_KEY", secret),
        ("SSH_AUTH_SOCK", secret),
        ("GITHUB_TOKEN", secret),
        ("AWS_SECRET_ACCESS_KEY", secret),
        ("DATABASE_URL", secret),
        ("EDITOR", secret),
    ];
    let runner = local(&base);
    let shown = run(&runner, "env", &dir.0, 5000);
    let names: Vec<String> = text(&shown.stdout)
        .lines()
        .filter(|line| line.contains('='))
        .map(|line| line.split('=').next().unwrap_or("").to_owned())
        .collect();
    let extra = ["PWD", "TERM", "SHLVL", "_", "OLDPWD"];
    for name in &names {
        let allowed = env_allowed(name) || extra.contains(&name.as_str());
        assert!(allowed, "the child sees {name}");
        for secret_name in ["HOLZWAVE", "API_KEY", "SSH_AUTH_SOCK", "TOKEN", "SECRET", "DATABASE_URL", "EDITOR"] {
            assert!(!name.contains(secret_name), "{name} reached the child");
        }
    }
    assert!(!text(&shown.stdout).contains(ENV_CANARY), "a value of a secret name reached the child");
    assert!(runner.env_names().contains(&"PATH".to_owned()) && !runner.env_names().contains(&"EDITOR".to_owned()));

    let facts = run(&runner, "printf '%s' \"$TERM\"", &dir.0, 5000);
    assert_eq!(text(&facts.stdout), "dumb");
    let at = run(&runner, "printf '%s' \"$PWD\"", &dir.0, 5000);
    let canonical = dir.0.canonicalize().expect("canonical");
    let pwd = PathBuf::from(text(&at.stdout));
    assert!(pwd == dir.0 || pwd == canonical, "PWD is the folder of the call: {}", pwd.display());

    // Standard input is closed: a reader of it ends at once.
    let cat = run(&plain(), "cat", &dir.0, 5000);
    assert_eq!((cat.stdout.len(), cat.status, cat.end), (0, Some(0), ExecEnd::Finished));
    let line = run(&plain(), "read line; echo \"[$line]\"", &dir.0, 5000);
    assert_eq!(text(&line.stdout), "[]\n");
    assert!(ALLOWED_ENV.contains(&"PATH") && ALLOWED_ENV_PREFIXES.contains(&"LC_"));
}

#[cfg(unix)]
#[test]
fn a_timeout_ends_the_command_and_everything_it_started() {
    let dir = Scratch::new("exec-timeout");
    let runner = plain();
    let started = Instant::now();
    let result = run(&runner, "sleep 30 & echo $! > bg.pid; echo $$ > sh.pid; wait", &dir.0, 400);
    assert!(started.elapsed() < Duration::from_secs(3), "the run took {:?}", started.elapsed());
    assert_eq!((result.end, result.status), (ExecEnd::TimedOut, None));
    let (bg, sh) = (read_pid(&dir.0.join("bg.pid")), read_pid(&dir.0.join("sh.pid")));
    assert!(
        common::wait_until(Duration::from_secs(2), || !alive(bg) && !alive(sh)),
        "a process of the command is still alive (helper {bg}: {}, shell {sh}: {})",
        alive(bg),
        alive(sh)
    );

    let again = run(&runner, "printf again", &dir.0, 5000);
    assert_eq!(text(&again.stdout), "again", "the runner is usable after a time out");
}
