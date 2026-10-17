//! The local socket has no way to decide, to consent or to choose, however the request is written.
//!
//! A real socket server with a handler that records every call, and raw request lines written by hand: the words of a
//! decision are not commands, a stray member of an `ai` request is refused by name before any handler, a stray member of any
//! other command carries nothing to the handler, a wrong token gets one answer for every command, and a refused request is
//! answered before its announced body is read. Every wait is bounded. Test code only.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use holzwave_ipc::{
    Cmd, Gone, Handler, Limits, MAX_LINE, Request, Server, Sink, connect, new_token, serve_with, stream_request,
};
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{RecvHalf, SendHalf};
use serde_json::{Value, json};

/// A socket folder with a short name below the temporary directory, removed first. Each call gives a new one: a server that
/// was dropped removes its socket file from a thread of its own, which must not meet the next one.
fn folder(tag: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("hw-nodec-{tag}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A handler that records what it was called with and answers `ok`.
#[derive(Default)]
struct Rec {
    calls: Mutex<Vec<(Option<u64>, Cmd)>>,
    streams: Mutex<Vec<(Option<u64>, Cmd, String)>>,
}

impl Rec {
    /// How many times the handler was called, of either kind.
    fn count(&self) -> usize {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).len() + self.streams.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    fn calls(&self) -> Vec<(Option<u64>, Cmd)> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Handler for Rec {
    fn handle(&self, pane: Option<u64>, cmd: Cmd) -> Result<String, String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).push((pane, cmd));
        Ok("ok".into())
    }

    fn handle_stream(&self, pane: Option<u64>, cmd: Cmd, input: String, _gone: &Gone, out: &mut dyn Sink) -> Result<(), String> {
        self.streams.lock().unwrap_or_else(|e| e.into_inner()).push((pane, cmd, input));
        out.text("ok").map_err(|e| e.to_string())
    }
}

/// A running server, its token and its handler.
struct Rig {
    dir: PathBuf,
    token: String,
    rec: Arc<Rec>,
    _server: Server,
}

fn rig(tag: &str) -> Rig {
    let dir = folder(tag);
    let token = new_token().expect("a token");
    let rec = Arc::new(Rec::default());
    let limits = Limits { clients: 8, streams: 2, read_timeout: Duration::from_secs(2) };
    let server = serve_with(&dir, token.clone(), rec.clone(), limits).expect("the server starts");
    Rig { dir, token, rec, _server: server }
}

/// A connection written and read by hand.
struct Raw {
    reader: BufReader<RecvHalf>,
    send: SendHalf,
}

impl Raw {
    fn open(dir: &Path) -> Raw {
        let stream = connect(dir).expect("a connection");
        // Bounded reads (Unix; a named pipe has none and its tests rely on the server answering).
        let _ = stream.set_recv_timeout(Some(Duration::from_secs(2)));
        let (recv, send) = stream.split();
        Raw { reader: BufReader::new(recv), send }
    }

    /// The next line, or an empty text when the connection ended.
    fn line(&mut self) -> String {
        let mut line = String::new();
        let _ = self.reader.read_line(&mut line);
        line.trim_end().to_owned()
    }
}

/// Writes `bytes` and a line feed on a fresh connection and reads one line of the answer.
fn raw(dir: &Path, bytes: &[u8]) -> String {
    let mut connection = Raw::open(dir);
    let _ = connection.send.write_all(bytes);
    let _ = connection.send.write_all(b"\n");
    connection.line()
}

fn answer(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| panic!("the answer is not JSON ({} bytes)", text.len()))
}

/// The request line of `value`.
fn line_of(value: &Value) -> Vec<u8> {
    value.to_string().into_bytes()
}

#[test]
fn no_deciding_word_is_a_command_of_the_socket() {
    let rig = rig("words");
    let words = [
        "decide", "decision", "approve", "approval", "allow", "deny", "consent", "confirm", "grant", "accept", "trust", "permit",
        "authorize", "authorise", "answer", "settle", "send", "refuse", "yes", "ai_decide", "ai_ask_answer", "ai_send",
        "ai_context_share", "ai_context_preview", "ai_undo", "ai_key_set", "ai_profile_save", "set_setting", "ask_answer",
        "ask-answer",
    ];
    assert_eq!(words.len(), 30);
    for word in words {
        for extra in [false, true] {
            let mut request = json!({ "token": rig.token, "pane": 1, "cmd": word });
            if extra {
                request["call_id"] = json!("call_1");
                request["decision"] = json!("allow");
                request["hash"] = json!("0".repeat(64));
            }
            let reply = answer(&raw(&rig.dir, &line_of(&request)));
            assert_eq!(reply["ok"], false, "{word}: the word was answered ok");
            assert!(reply["message"].as_str().unwrap_or("").starts_with("unknown command"), "{word}: not refused as an unknown command");
        }
    }
    assert_eq!(rig.rec.count(), 0, "a deciding word reached the handler");

    // The control: a command that exists is answered and called once.
    let reply = answer(&raw(&rig.dir, &line_of(&json!({ "token": rig.token, "pane": 1, "cmd": "ping" }))));
    assert_eq!(reply["ok"], true);
    assert_eq!(rig.rec.calls(), [(Some(1), Cmd::Ping)]);
}

#[test]
fn stray_members_of_an_ai_request_are_refused_before_a_handler_runs() {
    let rig = rig("ai-stray");
    let mut names: Vec<String> = [
        "consent", "approve", "approved", "allow", "decision", "decide", "hash", "call_id", "offer", "picked", "seal", "ask",
        "verdict", "profile", "model", "send", "yes", "trust", "__proto__",
    ]
    .iter()
    .map(|name| (*name).to_owned())
    .collect();
    names.push("x".repeat(200));
    assert_eq!(names.len(), 20);
    for name in &names {
        let mut request = json!({ "token": rig.token, "pane": 1, "cmd": "ai", "prompt": "hi", "stdin_bytes": 0 });
        request[name.as_str()] = json!(true);
        let reply = answer(&raw(&rig.dir, &line_of(&request)));
        assert_eq!(reply["ok"], false, "a stray member was accepted");
        let message = reply["message"].as_str().unwrap_or("");
        assert!(message.starts_with("unknown field"), "the refusal does not name the member: {}", message.len());
        let shown = message.split('`').nth(1).unwrap_or("");
        assert!(shown.chars().count() <= 60, "a long name is shown in {} characters", shown.chars().count());
    }
    assert_eq!(rig.rec.count(), 0, "a stray member reached the handler");

    // The control: the clean request reaches the handler once.
    let request = Request { token: rig.token.clone(), pane: Some(1), cmd: Cmd::Ai { prompt: "hi".to_owned(), stdin_bytes: 0 } };
    let outcome = stream_request(&rig.dir, &request, b"", &mut |_| Ok(())).expect("the clean request is answered");
    assert!(outcome.ok, "the clean request failed: {}", outcome.message.len());
    assert_eq!(rig.rec.count(), 1);
}

#[test]
fn stray_members_of_other_commands_carry_nothing_to_the_handler() {
    let rig = rig("other-stray");
    let clean: Vec<Value> = vec![
        json!({ "cmd": "ping" }),
        json!({ "cmd": "view", "path": "/tmp/a.txt" }),
        json!({ "cmd": "edit", "path": "/tmp/a.txt" }),
        json!({ "cmd": "setmeta", "key": "title", "value": "named" }),
        json!({ "cmd": "hist", "line": "ls -la", "cwd": "/tmp", "exit": 0 }),
        json!({ "cmd": "hist_open" }),
    ];
    let strays = ["consent", "approve", "decision", "hash", "call_id", "offer", "picked"];
    for base in &clean {
        let name = base["cmd"].as_str().unwrap_or("").to_owned();
        let with = |extra: Option<&str>| -> Value {
            let mut request = base.clone();
            request["token"] = json!(rig.token);
            request["pane"] = json!(3);
            if let Some(extra) = extra {
                request[extra] = json!("zq-stray");
            }
            request
        };
        let before = rig.rec.calls().len();
        let reply = answer(&raw(&rig.dir, &line_of(&with(None))));
        assert_eq!(reply["ok"], true, "{name}: the clean request was refused");
        let calls = rig.rec.calls();
        assert_eq!(calls.len(), before + 1, "{name}: the clean request did not reach the handler");
        let expected = calls[before].clone();
        for stray in strays {
            let reply = answer(&raw(&rig.dir, &line_of(&with(Some(stray)))));
            // The request is answered or refused; when the handler is called it gets exactly the clean command.
            let _ = reply;
        }
        let calls = rig.rec.calls();
        for (pane, cmd) in &calls[before..] {
            assert_eq!((pane, cmd), (&expected.0, &expected.1), "{name}: a stray member changed what the handler received");
        }
    }
    let calls = rig.rec.calls();
    assert!(calls.iter().any(|(_, cmd)| *cmd == Cmd::HistOpen), "hist_open did not reach the handler");
    assert!(calls.iter().all(|(pane, _)| *pane == Some(3)));
}

#[test]
fn a_wrong_token_gets_the_same_answer_for_every_command() {
    let rig = rig("token");
    let right = rig.token.clone();
    let tokens: Vec<(&str, Option<Value>)> = vec![
        ("a wrong token", Some(json!("not-the-token"))),
        ("a missing token", None),
        ("a token of another type", Some(json!(12345))),
        ("a prefix of the right token", Some(json!(right[..right.len() - 1]))),
        ("the right token with a trailing blank", Some(json!(format!("{right} ")))),
        ("an empty token", Some(json!(""))),
    ];
    let mut commands: Vec<Value> = vec![
        json!({ "cmd": "ping" }),
        json!({ "cmd": "view", "path": "/tmp/a.txt" }),
        json!({ "cmd": "ai", "prompt": "hi", "stdin_bytes": 0 }),
    ];
    for word in ["decide", "approve", "consent", "allow", "settle", "ai_decide"] {
        commands.push(json!({ "cmd": word, "call_id": "call_1", "decision": "allow", "hash": "0".repeat(64) }));
    }
    let mut answers = std::collections::BTreeSet::new();
    for (label, token) in &tokens {
        for command in &commands {
            let mut request = command.clone();
            request["pane"] = json!(1);
            if let Some(token) = token {
                request["token"] = token.clone();
            }
            let reply = raw(&rig.dir, &line_of(&request));
            assert_eq!(answer(&reply)["ok"], false, "{label}: the request was accepted");
            answers.insert(reply);
        }
    }
    assert_eq!(answers.len(), 1, "{} different answers for a wrong token", answers.len());
    assert_eq!(rig.rec.count(), 0, "a request with a wrong token reached the handler");
}

#[test]
fn the_token_is_checked_before_the_body_is_announced_or_read() {
    let rig = rig("body");
    let started = Instant::now();

    // A wrong token with an announced body of 1 MiB, of which the client sends none: refused at once, with no `ready`.
    let wrong = json!({ "token": "not-the-token", "pane": 1, "cmd": "ai", "prompt": "hi", "stdin_bytes": 1_048_576 });
    let reply = answer(&raw(&rig.dir, &line_of(&wrong)));
    assert_eq!(reply["ok"], false);
    assert!(reply.get("event").is_none(), "the server said ready to a wrong token");

    // The right token with a body one byte over the limit: refused before `ready`.
    let over = json!({ "token": rig.token, "pane": 1, "cmd": "ai", "prompt": "hi", "stdin_bytes": 1_048_577 });
    let reply = answer(&raw(&rig.dir, &line_of(&over)));
    assert_eq!(reply["ok"], false);
    assert!(reply.get("event").is_none(), "the server said ready to a body over the limit");

    // A line over the limit with no line feed: refused or closed.
    let mut connection = Raw::open(&rig.dir);
    let long = vec![b'a'; MAX_LINE + 4096];
    let _ = connection.send.write_all(&long);
    let reply = connection.line();
    assert!(reply.is_empty() || answer(&reply)["ok"] == false, "a line over the limit was accepted");
    assert!(started.elapsed() < Duration::from_secs(10), "the refusals took {:?}", started.elapsed());
    assert_eq!(rig.rec.count(), 0, "a refused request reached the handler");
    // Give a late handler call a moment to show itself.
    thread::sleep(Duration::from_millis(200));
    assert_eq!(rig.rec.count(), 0);
}
