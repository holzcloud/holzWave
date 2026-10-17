//! The real `wsh` binary for the history key: `wsh hist open` against a real socket with a test handler, and what
//! `wsh init` prints with and without `--ctrl-r`.

#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use holzwave_ipc::{Cmd, Handler, Server, serve};

const TOKEN: &str = "test-token";

/// A socket folder below the temporary directory, with a short name (the path of a socket is limited). Each call gives a
/// new one: a server that was dropped removes its socket file from a thread of its own, which must not meet the next one.
#[cfg(unix)]
fn folder(tag: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("hw-wsh-r-{tag}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// A handler that records what it was asked and answers what the test configured.
struct Rec {
    seen: Mutex<Vec<(Option<u64>, Cmd)>>,
    answer: Result<String, String>,
}

impl Rec {
    fn new(answer: Result<String, String>) -> Arc<Rec> {
        Arc::new(Rec { seen: Mutex::default(), answer })
    }

    fn seen(&self) -> Vec<(Option<u64>, Cmd)> {
        self.seen.lock().unwrap().clone()
    }
}

impl Handler for Rec {
    fn handle(&self, pane: Option<u64>, cmd: Cmd) -> Result<String, String> {
        self.seen.lock().unwrap().push((pane, cmd));
        self.answer.clone()
    }
}

/// A running server and its handler.
#[cfg(unix)]
struct Rig {
    dir: PathBuf,
    rec: Arc<Rec>,
    _server: Server,
}

#[cfg(unix)]
fn rig(tag: &str, answer: Result<String, String>) -> Rig {
    let dir = folder(tag);
    let rec = Rec::new(answer);
    let server = serve(&dir, TOKEN.into(), rec.clone()).unwrap();
    Rig { dir, rec, _server: server }
}

/// What a run of the binary left.
struct Run {
    status: i32,
    out: String,
    err: String,
}

/// Run `wsh` with the pane variables of `rig` set (pane 7) and no input.
#[cfg(unix)]
fn run_in(rig: &Rig, args: &[&str]) -> Run {
    run_with(args, Some(&rig.dir))
}

fn run_with(args: &[&str], ipc: Option<&PathBuf>) -> Run {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_wsh"));
    cmd.args(args).stdin(Stdio::null()).env_remove("HOLZWAVE_IPC").env_remove("HOLZWAVE_TOKEN").env_remove("HOLZWAVE_PANE");
    if let Some(dir) = ipc {
        cmd.env("HOLZWAVE_IPC", dir).env("HOLZWAVE_TOKEN", TOKEN).env("HOLZWAVE_PANE", "7");
    }
    let out = cmd.output().unwrap();
    Run {
        status: out.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&out.stdout).into_owned(),
        err: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[cfg(unix)]
#[test]
fn wsh_hist_open_sends_the_command_with_the_pane() {
    let rig = rig("open", Ok(String::new()));
    let run = run_in(&rig, &["hist", "open"]);
    assert_eq!((run.status, run.out.as_str(), run.err.as_str()), (0, "", ""));
    assert_eq!(rig.rec.seen(), [(Some(7), Cmd::HistOpen)]);
}

#[cfg(unix)]
#[test]
fn wsh_hist_open_prints_the_refusal_of_the_app() {
    let rig = rig("refused", Err("this pane is not the one in focus".into()));
    let run = run_in(&rig, &["hist", "open"]);
    assert_eq!(run.status, 1);
    assert_eq!(run.out, "");
    assert_eq!(run.err.trim_end(), "wsh: this pane is not the one in focus");
    assert_eq!(rig.rec.seen(), [(Some(7), Cmd::HistOpen)]);
}

#[cfg(unix)]
#[test]
fn wsh_hist_open_with_an_argument_sends_nothing() {
    let rig = rig("args", Ok(String::new()));
    let run = run_in(&rig, &["hist", "open", "x"]);
    assert_eq!(run.status, 2);
    assert!(run.err.starts_with("usage: wsh"), "{}", run.err);
    assert!(rig.rec.seen().is_empty());
}

#[test]
fn wsh_init_adds_the_binding_only_with_the_flag() {
    for (shell, hook, binding) in [("bash", include_str!("../hooks/bash.sh"), include_str!("../hooks/bash-ctrl-r.sh"))] {
        let plain = run_with(&["init", shell], None);
        assert_eq!((plain.status, plain.err.as_str()), (0, ""), "{shell}");
        assert_eq!(plain.out, format!("{hook}\n"), "{shell}: the hook and the newline main prints after every message");
        let with_key = run_with(&["init", shell, "--ctrl-r"], None);
        assert_eq!((with_key.status, with_key.err.as_str()), (0, ""), "{shell}");
        assert!(with_key.out.len() > plain.out.len() && with_key.out.starts_with(hook), "{shell}");
        assert_eq!(with_key.out, format!("{hook}{binding}\n"), "{shell}");
        assert!(with_key.out.contains("wsh hist open"), "{shell}");
        let twice = run_with(&["init", shell, "--ctrl-r", "--ctrl-r"], None);
        assert_eq!(twice.status, 2, "{shell}");
        assert_eq!(twice.out, "", "{shell}");
    }
}
