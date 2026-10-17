//! The real `wsh` binary reaches a real `holzwave_ipc` server through `HOLZWAVE_SOCKET` alone, as it does on a host reached over
//! SSH, and says its version without any app.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use holzwave_ipc::{Cmd, Gone, Handler, Server, Sink, serve};

const TOKEN: &str = "test-token";

/// A socket folder below the temporary directory with a short name (the path of a socket is limited), new on each call.
fn folder(tag: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("hw-sock-{tag}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// A handler that answers `ping` and streams one scripted answer for `ai`.
#[derive(Default)]
struct Script {
    seen: Mutex<Vec<(Option<u64>, String)>>,
}

impl Handler for Script {
    fn handle(&self, pane: Option<u64>, cmd: Cmd) -> Result<String, String> {
        self.seen.lock().unwrap().push((pane, format!("{cmd:?}")));
        match cmd {
            Cmd::Ping => Ok("holzWave".into()),
            _ => Err("not here".into()),
        }
    }

    fn handle_stream(&self, pane: Option<u64>, cmd: Cmd, _input: String, _gone: &Gone, out: &mut dyn Sink) -> Result<(), String> {
        self.seen.lock().unwrap().push((pane, format!("{cmd:?}")));
        out.text("streamed ").map_err(|e| e.to_string())?;
        out.text("answer").map_err(|e| e.to_string())
    }
}

struct Rig {
    dir: PathBuf,
    script: Arc<Script>,
    _server: Server,
}

impl Rig {
    fn new(tag: &str) -> Rig {
        let dir = folder(tag);
        let script = Arc::new(Script::default());
        let server = serve(&dir, TOKEN.into(), script.clone()).unwrap();
        Rig { dir, script, _server: server }
    }

    /// The socket of the server, as a forward would name it.
    fn socket(&self) -> PathBuf {
        self.dir.join("ipc.sock")
    }
}

struct Run {
    status: i32,
    stdout: String,
    stderr: String,
}

/// The binary with exactly the variables given (every variable of a holzWave terminal is removed first), at most 20 s.
fn wsh(vars: &[(&str, &Path)], token: Option<&str>, args: &[&str]) -> Run {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wsh"));
    command.args(args).env_remove("HOLZWAVE_IPC").env_remove("HOLZWAVE_SOCKET").env_remove("HOLZWAVE_TOKEN").env_remove("HOLZWAVE_PANE");
    for (name, value) in vars {
        command.env(name, value);
    }
    if let Some(token) = token {
        command.env("HOLZWAVE_TOKEN", token);
    }
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    drop(child.stdin.take());
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let stdout = thread::spawn(move || {
        let mut v = String::new();
        let _ = out.read_to_string(&mut v);
        v
    });
    let stderr = thread::spawn(move || {
        let mut v = String::new();
        let _ = err.read_to_string(&mut v);
        v
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code().unwrap_or(-1);
        }
        assert!(started.elapsed() < Duration::from_secs(20), "wsh did not finish");
        thread::sleep(Duration::from_millis(10));
    };
    Run { status, stdout: stdout.join().unwrap(), stderr: stderr.join().unwrap() }
}

#[test]
fn socket_alone_reaches_the_app() {
    let rig = Rig::new("alone");
    let socket = rig.socket();
    let r = wsh(&[("HOLZWAVE_SOCKET", &socket)], Some(TOKEN), &["ping"]);
    assert_eq!((r.status, r.stdout.as_str()), (0, "holzWave\n"), "{}", r.stderr);
    assert_eq!(rig.script.seen.lock().unwrap().len(), 1);
}

#[test]
fn socket_wins_over_the_folder() {
    let right = Rig::new("right");
    let wrong = Rig::new("wrong");
    let (right_socket, wrong_dir) = (right.socket(), wrong.dir.clone());
    let r = wsh(&[("HOLZWAVE_SOCKET", &right_socket), ("HOLZWAVE_IPC", &wrong_dir)], Some(TOKEN), &["ping"]);
    assert_eq!((r.status, r.stdout.as_str()), (0, "holzWave\n"), "{}", r.stderr);
    assert_eq!(right.script.seen.lock().unwrap().len(), 1);
    assert!(wrong.script.seen.lock().unwrap().is_empty(), "the folder must not be asked when the socket is set");

    // The reverse: a wrong socket is not rescued by a right folder.
    let (right_dir, wrong_socket) = (right.dir.clone(), wrong.dir.join("nothing.sock"));
    let r = wsh(&[("HOLZWAVE_SOCKET", &wrong_socket), ("HOLZWAVE_IPC", &right_dir)], Some(TOKEN), &["ping"]);
    assert_eq!(r.status, 1);
    assert!(r.stderr.contains("holzWave does not answer"), "{}", r.stderr);
    assert_eq!(right.script.seen.lock().unwrap().len(), 1, "no second request reached the right server");
}

#[test]
fn an_empty_socket_variable_is_not_a_socket() {
    let rig = Rig::new("empty");
    let dir = rig.dir.clone();
    let r = wsh(&[("HOLZWAVE_SOCKET", Path::new("")), ("HOLZWAVE_IPC", &dir)], Some(TOKEN), &["ping"]);
    assert_eq!((r.status, r.stdout.as_str()), (0, "holzWave\n"), "{}", r.stderr);
}

#[test]
fn a_wrong_token_over_the_socket_is_refused_in_words() {
    let rig = Rig::new("token");
    let socket = rig.socket();
    let r = wsh(&[("HOLZWAVE_SOCKET", &socket)], Some("guess"), &["ping"]);
    assert_eq!(r.status, 1);
    assert!(r.stderr.contains("refused"), "{}", r.stderr);
    assert!(r.stdout.is_empty());
    assert!(rig.script.seen.lock().unwrap().is_empty());
}

#[test]
fn wsh_ai_streams_through_the_socket() {
    let rig = Rig::new("ai");
    let socket = rig.socket();
    let r = wsh(&[("HOLZWAVE_SOCKET", &socket)], Some(TOKEN), &["ai", "hello"]);
    assert_eq!(r.status, 0, "{}", r.stderr);
    assert_eq!(r.stdout, "streamed answer\n");
    assert_eq!(rig.script.seen.lock().unwrap().len(), 1);
}

#[test]
fn version_needs_no_app() {
    let r = wsh(&[], None, &["version"]);
    assert_eq!(r.status, 0, "{}", r.stderr);
    assert_eq!(r.stdout, format!("wsh {}\n", env!("CARGO_PKG_VERSION")));
    assert!(r.stderr.is_empty());
    // It is in the usage text, and it takes no argument.
    let usage = wsh(&[], None, &["help"]);
    assert_eq!(usage.status, 2);
    assert!(usage.stderr.contains("version"), "{}", usage.stderr);
    let extra = wsh(&[], None, &["version", "now"]);
    assert_eq!(extra.status, 2);
}

#[test]
fn neither_variable_says_so() {
    let r = wsh(&[], Some(TOKEN), &["ping"]);
    assert_eq!(r.status, 1);
    assert!(r.stderr.contains("this is not a holzWave terminal"), "{}", r.stderr);
    assert!(r.stderr.contains("HOLZWAVE_SOCKET"), "{}", r.stderr);
    // The token alone is not enough either, and a socket without a token says the same.
    let dir = folder("neither");
    let r = wsh(&[("HOLZWAVE_SOCKET", &dir.join("ipc.sock"))], None, &["ping"]);
    assert_eq!(r.status, 1);
    assert!(r.stderr.contains("this is not a holzWave terminal"), "{}", r.stderr);
}

#[test]
fn a_socket_path_that_is_too_long_is_said() {
    let long = PathBuf::from(format!("/tmp/{}", "a".repeat(140)));
    let r = wsh(&[("HOLZWAVE_SOCKET", &long)], Some(TOKEN), &["ping"]);
    assert_eq!(r.status, 1);
    assert!(r.stderr.contains("too long"), "{}", r.stderr);
}

#[test]
fn a_remote_process_over_the_socket_is_kept_in_its_scope_by_the_server_not_the_client() {
    // `wsh` sends whatever the person typed; the refusal of `hist` and `ai` over a forward is the server's (serve_remote). Here
    // only the local server answers, to show that the client puts nothing of its own between the person and the server.
    let rig = Rig::new("hist");
    let socket = rig.socket();
    let r = wsh(&[("HOLZWAVE_SOCKET", &socket)], Some(TOKEN), &["hist", "add", "0", "/tmp", "x"]);
    assert_eq!(r.status, 1);
    assert!(r.stderr.contains("not here"), "{}", r.stderr);
}
