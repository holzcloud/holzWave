//! The real `wsh` binary against a real `holzwave_ipc` server with a test handler.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use holzwave_ipc::{Cmd, Gone, Handler, Server, Sink, serve};

const TOKEN: &str = "test-token";

/// A socket folder below the temporary directory, with a short name (the path of a socket is limited). Each call gives a
/// new one: a server that was dropped removes its socket file from a thread of its own, which must not meet the next one.
fn folder(tag: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("hw-wsh-{tag}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
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

/// Whether `text` holds a character that a terminal could act on in the ways the cleaning is meant to stop.
fn has_escape_material(text: &str) -> bool {
    text.chars().any(|c| c == '\u{1b}' || c == '\u{7}' || ('\u{80}'..='\u{9f}').contains(&c))
}

fn ask(rig: &Rig, words: &[&str], stdin: &[u8]) -> Run {
    let mut args = vec!["ai"];
    args.extend_from_slice(words);
    run(wsh(&rig.dir, Some(TOKEN), Some("7"), &args), Some(stdin.to_vec()))
}

#[test]
fn terminal_escapes_in_the_answer_never_reach_standard_output() {
    let rig = rig(
        "esc",
        Script::chunks(&[
            "\u{1b}]52;c;aGVsbG8=\u{7}",
            "safe \u{1b}]0;pwned\u{7}",
            "c1 \u{9d}52;c;eA==\u{9c} \u{9b}2J",
            "done",
        ]),
    );
    let r = ask(&rig, &["q"], b"");
    assert_eq!(r.status, 0, "{}", r.stderr);
    assert!(!has_escape_material(&r.out()), "{:?}", r.out());
    assert_eq!(r.out(), "]52;c;aGVsbG8=safe ]0;pwnedc1 52;c;eA== 2Jdone\n");
    assert!(r.stderr.contains("wsh: 7 control characters were removed from the answer"), "{}", r.stderr);
    assert!(!has_escape_material(&r.stderr));
}

#[test]
fn a_note_goes_to_standard_error_and_the_answer_to_standard_output() {
    let rig = rig("note", Script { note: Some("waiting for you\u{1b}".into()), ..Script::chunks(&["answer\n"]) });
    let r = ask(&rig, &["q"], b"");
    assert_eq!(r.status, 0);
    assert_eq!(r.out(), "answer\n");
    assert_eq!(r.stderr, "wsh: waiting for you\nwsh: 1 control character was removed from the answer\n");
}

#[test]
fn a_refusal_is_printed_on_standard_error_with_status_1() {
    let rig = rig("refuse", Script { fail: Some("no chat is open".into()), ..Script::chunks(&["partial"]) });
    let r = ask(&rig, &["q"], b"");
    assert_eq!(r.status, 1);
    assert_eq!(r.stderr, "wsh: no chat is open\n");
    assert_eq!(r.out(), "partial\n", "what was printed before stays");
}

#[test]
fn without_the_token_nothing_is_read_and_nothing_is_sent() {
    let rig = rig("notoken", Script::chunks(&["never"]));
    let mut command = wsh(&rig.dir, None, None, &["ai", "q"]);
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    // The pipe stays open and silent: a binary that read it would wait here.
    let _stdin = child.stdin.take().unwrap();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code().unwrap_or(-1);
        }
        if started.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            panic!("wsh ai waited for its input without the token");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let mut stderr = String::new();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    assert_eq!(status, 1);
    assert!(stderr.contains("not a holzWave terminal"), "{stderr}");
    assert!(rig.script.seen().is_empty());
}

#[test]
fn a_wrong_token_is_refused() {
    let rig = rig("guess", Script::chunks(&["never"]));
    let r = run(wsh(&rig.dir, Some("guess"), Some("7"), &["ai", "q"]), Some(Vec::new()));
    assert_eq!(r.status, 1);
    assert!(r.stderr.contains("refused"), "{}", r.stderr);
    assert_eq!(r.out(), "");
    assert!(rig.script.seen().is_empty());
}

#[test]
fn an_input_over_the_limit_never_reaches_the_app() {
    let rig = rig("over", Script::chunks(&["never"]));
    let r = ask(&rig, &["q"], &vec![b'a'; 1024 * 1024 + 1]);
    assert_eq!(r.status, 2);
    assert!(r.stderr.contains("longer than 1048576 bytes"), "{}", r.stderr);
    assert!(rig.script.seen().is_empty());
    let r = ask(&rig, &["q"], &vec![b'a'; 1024 * 1024]);
    assert_eq!(r.status, 0, "{}", r.stderr);
    assert_eq!(rig.script.seen().len(), 1, "the limit itself goes through");
}

#[test]
fn input_that_is_not_text_never_reaches_the_app() {
    let rig = rig("bin", Script::chunks(&["never"]));
    for bytes in [&[0xffu8, 0xfe, 0x00][..], b"a\0b", b"\xc3("] {
        let r = ask(&rig, &["q"], bytes);
        assert_eq!(r.status, 2, "{bytes:?}");
        assert!(r.stderr.contains("not text"), "{}", r.stderr);
    }
    assert!(rig.script.seen().is_empty());
}

#[test]
fn a_prompt_that_starts_with_a_dash_needs_the_double_dash() {
    let rig = rig("dash", Script::chunks(&["ok"]));
    let r = ask(&rig, &["--model", "x"], b"");
    assert_eq!(r.status, 2);
    assert!(r.stderr.contains("unknown option `--model`"), "{}", r.stderr);
    assert!(rig.script.seen().is_empty(), "nothing was sent");
    assert_eq!(ask(&rig, &["--", "--model", "x"], b"").status, 0);
    assert_eq!(ask(&rig, &["explain", "-v"], b"").status, 0);
    let prompts: Vec<String> = rig.script.seen().into_iter().map(|(_, prompt, _)| prompt).collect();
    assert_eq!(prompts, ["--model x", "explain -v"]);
    let r = ask(&rig, &[], b"");
    assert_eq!((r.status, r.stderr.as_str()), (2, "wsh: nothing to ask: give a prompt or pipe text in\n"));
    let r = ask(&rig, &[], b"only the input\n");
    assert_eq!(r.status, 0, "the question may be in the input");
}

#[test]
fn closing_standard_output_ends_the_request() {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let rig = rig("close", Script { keep_writing: Some(Mutex::new(tx)), ..Script::default() });
    let mut command = wsh(&rig.dir, Some(TOKEN), Some("7"), &["ai", "q"]);
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut first = [0u8; 1];
    stdout.read_exact(&mut first).unwrap();
    assert_eq!(&first, b"x");
    let closed = Instant::now();
    drop(stdout);
    let report = rx.recv_timeout(Duration::from_secs(3)).expect("the handler was not told");
    eprintln!("the handler learned in {:?}: {report}", closed.elapsed());
    assert!(report == "gone" || report == "failed BrokenPipe", "{report}");
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code().unwrap_or(-1);
        }
        assert!(started.elapsed() < Duration::from_secs(5), "wsh did not end");
        thread::sleep(Duration::from_millis(10));
    };
    let mut stderr = String::new();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    assert_eq!(status, 1);
    assert_eq!(stderr, "", "a closed standard output is not announced");
}

#[test]
fn messages_of_the_old_commands_are_cleaned_too() {
    let first = rig("old1", Script { old: Some(Err("bad \u{1b}]0;x\u{7} file".into())), ..Script::default() });
    let r = run(wsh(&first.dir, Some(TOKEN), Some("7"), &["ping"]), None);
    assert_eq!(r.status, 1);
    assert_eq!(r.stderr, "wsh: bad ]0;x file\n");
    let rig = rig("old2", Script { old: Some(Ok("fine\u{1b}[2J".into())), ..Script::default() });
    let r = run(wsh(&rig.dir, Some(TOKEN), Some("7"), &["ping"]), None);
    assert_eq!((r.status, r.out().as_str()), (0, "fine[2J\n"));
}

#[test]
fn an_answer_without_a_final_newline_gets_one() {
    for (chunks, expect) in [(&["a", "b"][..], "ab\n"), (&["ab\n"], "ab\n"), (&[], "")] {
        let rig = rig("nl", Script::chunks(chunks));
        let r = ask(&rig, &["q"], b"");
        assert_eq!(r.status, 0, "{}", r.stderr);
        assert_eq!(r.out(), expect, "{chunks:?}");
    }
}
