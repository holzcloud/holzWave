//! The real `wsh` binary against a real `holzwave_ipc` server with a test handler.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use holzwave_ipc::{Cmd, Gone, Handler, Server, Sink, serve};

const TOKEN: &str = "test-token";

/// A socket folder below the temporary directory, with a short name (the path of a socket is limited).
fn folder(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hw-wsh-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// What a test handler was asked: the pane, the prompt and the input.
type Seen = (Option<u64>, String, String);

/// A handler for the tests: it records what it was asked and streams what it was told to.
#[derive(Default)]
struct Script {
    seen: Mutex<Vec<Seen>>,
    /// The text events, in order.
    chunks: Vec<String>,
    /// A note written before the chunks.
    note: Option<String>,
    /// `Err` of the stream after the chunks.
    fail: Option<String>,
    /// The pause before every chunk.
    delay: Duration,
    /// The answer of the commands that do not stream.
    old: Option<Result<String, String>>,
    /// Write a piece every 20 ms until the sink fails or the client is gone, and report which.
    keep_writing: Option<Mutex<Sender<String>>>,
}

impl Script {
    fn chunks(chunks: &[&str]) -> Script {
        Script { chunks: chunks.iter().map(|c| (*c).to_owned()).collect(), ..Script::default() }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl Handler for Script {
    fn handle(&self, _pane: Option<u64>, _cmd: Cmd) -> Result<String, String> {
        self.old.clone().unwrap_or_else(|| Err("not a stream".into()))
    }

    fn handle_stream(&self, pane: Option<u64>, cmd: Cmd, input: String, gone: &Gone, out: &mut dyn Sink) -> Result<(), String> {
        let Cmd::Ai { prompt, .. } = cmd else { return Err("not an ai request".into()) };
        self.seen.lock().unwrap().push((pane, prompt, input));
        if let Some(note) = &self.note {
            out.note(note).map_err(|e| e.to_string())?;
        }
        for chunk in &self.chunks {
            thread::sleep(self.delay);
            out.text(chunk).map_err(|e| e.to_string())?;
        }
        if let Some(report) = &self.keep_writing {
            let report = report.lock().unwrap().clone();
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(10) {
                if gone.is_gone() {
                    let _ = report.send("gone".into());
                    return Ok(());
                }
                if let Err(e) = out.text("x") {
                    let _ = report.send(format!("failed {:?}", e.kind()));
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(20));
            }
            let _ = report.send("still writing".into());
        }
        match &self.fail {
            Some(message) => Err(message.clone()),
            None => Ok(()),
        }
    }
}

/// A running server and its handler.
struct Rig {
    dir: PathBuf,
    script: Arc<Script>,
    _server: Server,
}

fn rig(tag: &str, script: Script) -> Rig {
    let dir = folder(tag);
    let script = Arc::new(script);
    let server = serve(&dir, TOKEN.into(), script.clone()).unwrap();
    Rig { dir, script, _server: server }
}

/// What a run of the binary left.
struct Run {
    status: i32,
    stdout: Vec<u8>,
    stderr: String,
}

impl Run {
    fn out(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// The binary with the variables of a holzWave terminal (each one that is `None` is removed).
fn wsh(dir: &Path, token: Option<&str>, pane: Option<&str>, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wsh"));
    command.args(args).env_remove("HOLZWAVE_IPC").env_remove("HOLZWAVE_TOKEN").env_remove("HOLZWAVE_PANE");
    command.env("HOLZWAVE_IPC", dir);
    if let Some(t) = token {
        command.env("HOLZWAVE_TOKEN", t);
    }
    if let Some(p) = pane {
        command.env("HOLZWAVE_PANE", p);
    }
    command
}

/// Run the command with `stdin` piped in (the pipe is closed after the bytes) and wait at most 20 seconds.
fn run(mut command: Command, stdin: Option<Vec<u8>>) -> Run {
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let mut pipe = child.stdin.take().unwrap();
    let writer = thread::spawn(move || {
        if let Some(bytes) = stdin {
            let _ = pipe.write_all(&bytes);
        }
        drop(pipe);
    });
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let stdout = thread::spawn(move || {
        let mut v = Vec::new();
        let _ = out.read_to_end(&mut v);
        v
    });
    let stderr = thread::spawn(move || {
        let mut v = Vec::new();
        let _ = err.read_to_end(&mut v);
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
    let _ = writer.join();
    Run { status, stdout: stdout.join().unwrap(), stderr: String::from_utf8_lossy(&stderr.join().unwrap()).into_owned() }
}

#[test]
fn wsh_ai_sends_the_prompt_and_the_piped_text_and_prints_the_streamed_answer() {
    let rig = rig("tr", Script::chunks(&["alpha ", "beta ", "gamma"]));
    let input = "first line\nzweite Zeile \u{e4}\nthird\n";
    let r = run(wsh(&rig.dir, Some(TOKEN), Some("7"), &["ai", "why", "is", "it", "slow"]), Some(input.as_bytes().to_vec()));
    assert_eq!(r.out(), "alpha beta gamma\n");
    assert_eq!(r.status, 0, "{}", r.stderr);
    assert_eq!(r.stderr, "");
    assert_eq!(rig.script.seen(), [(Some(7), "why is it slow".to_owned(), input.to_owned())]);
}
