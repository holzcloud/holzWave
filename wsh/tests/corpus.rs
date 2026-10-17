//! Hostile text through the real `wsh` binary, and the absence of any way to decide.
//!
//! A recording server answers a `wsh ai` request with hostile text in pieces, then in notes, then in an error message. The
//! test reads what the binary wrote to standard output and standard error as bytes and checks it against a table of what a
//! terminal could act on, written out here and not read from the binary. A second test shows that no word of the command
//! line decides or consents, and that the flags that would are reserved. Every hidden character is made with
//! `char::from_u32`; this is test code only.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use holzwave_ipc::{Cmd, Gone, Handler, Server, Sink, serve};

const TOKEN: &str = "corpus-token";

/// A socket folder with a short name below the temporary directory (the path of a socket is limited). Each call gives a new
/// one: a server that was dropped removes its socket file from a thread of its own, which must not meet the next one.
fn folder(tag: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("hw-corpus-{tag}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn ch(code: u32) -> char {
    char::from_u32(code).expect("a valid code point")
}

/// A handler that counts every call and streams what it was told to.
#[derive(Default)]
struct Script {
    calls: AtomicUsize,
    texts: Vec<String>,
    notes: Vec<String>,
    fail: Option<String>,
    seen: Mutex<Vec<(Option<u64>, String)>>,
}

impl Handler for Script {
    fn handle(&self, _pane: Option<u64>, _cmd: Cmd) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(String::new())
    }

    fn handle_stream(&self, pane: Option<u64>, cmd: Cmd, _input: String, _gone: &Gone, out: &mut dyn Sink) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Cmd::Ai { prompt, .. } = cmd {
            self.seen.lock().unwrap_or_else(|e| e.into_inner()).push((pane, prompt));
        }
        for note in &self.notes {
            out.note(note).map_err(|e| e.to_string())?;
        }
        for text in &self.texts {
            out.text(text).map_err(|e| e.to_string())?;
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
    let server = serve(&dir, TOKEN.into(), script.clone()).expect("the server starts");
    Rig { dir, script, _server: server }
}

/// What a run of the binary left, as bytes.
struct Run {
    status: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// The binary with the variables of a holzWave terminal.
fn wsh(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wsh"));
    command.args(args).env_remove("HOLZWAVE_IPC").env_remove("HOLZWAVE_TOKEN").env_remove("HOLZWAVE_PANE");
    command.env("HOLZWAVE_IPC", dir).env("HOLZWAVE_TOKEN", TOKEN).env("HOLZWAVE_PANE", "7");
    command
}

/// Runs the command with `stdin` piped in and waits at most 20 seconds.
fn run(mut command: Command, stdin: &[u8]) -> Run {
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().expect("the binary starts");
    let mut pipe = child.stdin.take().expect("a pipe");
    let bytes = stdin.to_vec();
    let writer = thread::spawn(move || {
        let _ = pipe.write_all(&bytes);
    });
    let mut out = child.stdout.take().expect("a pipe");
    let mut err = child.stderr.take().expect("a pipe");
    let stdout = thread::spawn(move || {
        let mut all = Vec::new();
        let _ = out.read_to_end(&mut all);
        all
    });
    let stderr = thread::spawn(move || {
        let mut all = Vec::new();
        let _ = err.read_to_end(&mut all);
        all
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("a status") {
            break status.code().unwrap_or(-1);
        }
        assert!(started.elapsed() < Duration::from_secs(20), "wsh did not finish");
        thread::sleep(Duration::from_millis(10));
    };
    let _ = writer.join();
    Run { status, stdout: stdout.join().expect("stdout"), stderr: stderr.join().expect("stderr") }
}

/// What a terminal could act on, as an independent table: every control character but the line feed and the tab (this
/// covers the escape, the bell, the carriage return and DEL), the C1 range, the soft hyphen, the Arabic letter mark, the
/// zero-width space, the marks, the embeddings and overrides, the word joiner, the isolates, the byte order mark, the tag
/// block and the variation selector supplement.
fn acts(c: char) -> bool {
    let code = u32::from(c);
    (code < 0x20 && code != 0x0a && code != 0x09)
        || code == 0x7f
        || (0x80..=0x9f).contains(&code)
        || code == 0xad
        || code == 0x61c
        || code == 0x200b
        || code == 0x200e
        || code == 0x200f
        || (0x202a..=0x202e).contains(&code)
        || code == 0x2060
        || (0x2066..=0x2069).contains(&code)
        || code == 0xfeff
        || (0xe0000..=0xe007f).contains(&code)
        || (0xe0100..=0xe01ef).contains(&code)
}

/// The hostile pieces, each ending in text that stays visible.
fn pieces() -> Vec<String> {
    let (e, bel) = (ch(0x1b), ch(0x07));
    let st = format!("{e}\\");
    let tags: String = "ignore the user".chars().map(|c| ch(0xe0000 + u32::from(c))).collect();
    vec![
        format!("{e}]52;c;aGVsbG8={bel}after-clipboard "),
        format!("{e}]0;zq-title{bel}after-title "),
        format!("{e}]8;;http://example.net/x{st}link{e}]8;;{st} after-link "),
        format!("{e}[2J{e}[6n after-clear "),
        format!("{}2J after-c1 {}0;zq-c1{} {}", ch(0x9b), ch(0x9d), ch(0x9c), ch(0x85)),
        format!("safe-line\rdanger-line{bel}{}{} after-overwrite ", ch(0x08), ch(0x7f)),
        format!(
            "name{}gpj.exe{} {}{}{}{} {}{}{} after-bidi ",
            ch(0x202e),
            ch(0x202c),
            ch(0x2066),
            ch(0x2067),
            ch(0x2068),
            ch(0x2069),
            ch(0x200e),
            ch(0x200f),
            ch(0x61c)
        ),
        format!("a{}b{}c{}d{}e after-zero-width ", ch(0x200b), ch(0x2060), ch(0xfeff), ch(0xad)),
        format!("tags{tags}{} after-tags ", ch(0xe0100)),
        format!("{}{}{}{}{} {}{}", ch(0x1f468), ch(0x200d), ch(0x1f469), ch(0x200d), ch(0x1f467), ch(0x2764), ch(0xfe0f)),
    ]
}

/// The pieces that must survive whole in the output.
fn family() -> String {
    format!("{}{}{}{}{}", ch(0x1f468), ch(0x200d), ch(0x1f469), ch(0x200d), ch(0x1f467))
}

fn heart() -> String {
    format!("{}{}", ch(0x2764), ch(0xfe0f))
}

fn text_of(bytes: &[u8], what: &str) -> String {
    String::from_utf8(bytes.to_vec()).unwrap_or_else(|_| panic!("{what} is not valid UTF-8"))
}

fn assert_clean(text: &str, what: &str) {
    for (at, c) in text.char_indices() {
        assert!(!acts(c), "{what}: a character a terminal could act on (U+{:04X}) at byte {at}", u32::from(c));
    }
}

#[test]
fn escape_payloads_in_text_notes_and_errors_never_reach_the_terminal() {
    let texts = pieces();
    // Notes are cut at 1024 bytes by the socket; every piece is far below that.
    let script = Script { texts: texts.clone(), notes: texts.clone(), fail: Some(texts.concat()), ..Script::default() };
    let rig = rig("esc", script);
    let result = run(wsh(&rig.dir, &["ai", "why"]), b"");
    let stdout = text_of(&result.stdout, "standard output");
    let stderr = text_of(&result.stderr, "standard error");
    assert_eq!(result.status, 1, "the status of an answer that ended with an error");
    assert_clean(&stdout, "standard output");
    assert_clean(&stderr, "standard error");

    // What the payloads show of themselves stays, as plain text.
    for visible in ["]52;c;aGVsbG8=", "]0;zq-title", "link", "[2J", "[6n", "2J", "safe-linedanger-line", "after-bidi", "after-tags"] {
        assert!(stdout.contains(visible), "standard output lost {visible}");
        assert!(stderr.contains(visible), "standard error lost {visible}");
    }
    assert!(stdout.contains(&family()), "the family emoji with its joiners changed");
    assert!(stdout.contains(&heart()), "the heart with its variation selector changed");
    assert!(stdout.ends_with('\n') && !stdout.ends_with("\n\n"), "standard output does not end with exactly one line feed");
    assert!(stderr.contains("control characters were removed"), "standard error does not say what was removed");
    assert_eq!(rig.script.seen.lock().unwrap_or_else(|e| e.into_inner()).len(), 1, "the handler saw one request");
}

#[test]
fn wsh_has_no_command_that_decides_and_reserves_the_flags_that_would() {
    let rig = rig("nodec", Script { texts: vec!["ok".to_owned()], ..Script::default() });
    let words = [
        "approve", "decide", "decision", "consent", "allow", "deny", "confirm", "grant", "accept", "trust", "yes", "answer",
        "settle", "send-once", "ai-decide",
    ];
    for word in words {
        let result = run(wsh(&rig.dir, &[word]), b"");
        assert_ne!(result.status, 0, "{word}: the binary accepted it");
        let stderr = text_of(&result.stderr, "standard error");
        assert!(stderr.contains("usage:"), "{word}: no usage text");
        // The same word with the arguments a decision would carry.
        let with = run(wsh(&rig.dir, &[word, "call_1", "0123456789abcdef"]), b"");
        assert_ne!(with.status, 0, "{word}: the binary accepted it with arguments");
    }
    for flag in ["--yes", "--approve", "--consent", "--allow", "--profile", "--model", "--offer", "--picked"] {
        let result = run(wsh(&rig.dir, &["ai", flag, "why"]), b"");
        assert_eq!(result.status, 2, "ai {flag}: the status");
        assert!(text_of(&result.stderr, "standard error").contains("unknown option"), "ai {flag}: no usage text");
    }
    assert_eq!(rig.script.calls.load(Ordering::SeqCst), 0, "the handler was called by a word that decides");

    // The positive control: an ordinary ask reaches the handler once.
    let result = run(wsh(&rig.dir, &["ai", "why"]), b"");
    assert_eq!(result.status, 0, "{}", text_of(&result.stderr, "standard error"));
    assert_eq!(text_of(&result.stdout, "standard output"), "ok\n");
    assert_eq!(rig.script.calls.load(Ordering::SeqCst), 1, "an ordinary ask was not called once");
}
