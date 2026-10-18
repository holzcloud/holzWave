//! The remote scope of the protocol: what a request that arrives through an SSH forward may ask, through the public
//! function and in-memory buffers. The pane comes from the caller and never from the request, the token is checked first,
//! only `ping`, `view`, `edit` and `setmeta` reach the handler, and a request budget bounds a pane. Test code only.

use std::io::{self, BufRead, Cursor, Read};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use holzwave_ipc::{Cmd, Handler, MAX_LINE, REMOTE_REFUSED, REMOTE_REQUESTS, REMOTE_WINDOW, RateLimit, serve_remote};
use serde_json::{Value, json};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// A handler that records every call and answers with the command it saw.
#[derive(Default)]
struct Rec {
    calls: Mutex<Vec<(Option<u64>, Cmd)>>,
}

impl Rec {
    fn calls(&self) -> Vec<(Option<u64>, Cmd)> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Handler for Rec {
    fn handle(&self, pane: Option<u64>, cmd: Cmd) -> Result<String, String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).push((pane, cmd.clone()));
        match cmd {
            Cmd::View { path } if path == "/boom" => Err("no such file".into()),
            other => Ok(format!("handled {other:?}")),
        }
    }
}

fn limit() -> RateLimit {
    RateLimit::new(REMOTE_REQUESTS, REMOTE_WINDOW)
}

/// Run one request line (`bytes` is everything the peer sent) and return the answer as JSON and what is left unread.
fn remote(bytes: &[u8], token: &str, pane: u64, handler: &dyn Handler, limit: &RateLimit) -> (Value, Vec<u8>) {
    let mut reader = Cursor::new(bytes.to_vec());
    let mut out = Vec::new();
    serve_remote(&mut reader, &mut out, token, pane, handler, limit);
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text.matches('\n').count(), 1, "exactly one answer line: {text:?}");
    (serde_json::from_str(text.trim_end()).expect("the answer is JSON"), rest)
}

fn line(value: Value) -> Vec<u8> {
    let mut text = value.to_string();
    text.push('\n');
    text.into_bytes()
}

fn refused() -> Value {
    json!({"ok": false, "message": REMOTE_REFUSED})
}

#[test]
fn ping_view_edit_and_setmeta_reach_the_handler_with_the_bound_pane() {
    let rec = Rec::default();
    let limit = limit();
    let requests = [
        (json!({"token": TOKEN, "cmd": "ping"}), Cmd::Ping),
        (json!({"token": TOKEN, "cmd": "view", "path": "/srv/a.md"}), Cmd::View { path: "/srv/a.md".into() }),
        (json!({"token": TOKEN, "cmd": "edit", "path": "/srv/a.md"}), Cmd::Edit { path: "/srv/a.md".into() }),
        (json!({"token": TOKEN, "cmd": "setmeta", "key": "title", "value": "prod"}), Cmd::Setmeta { key: "title".into(), value: "prod".into() }),
    ];
    for (request, cmd) in &requests {
        let (answer, _) = remote(&line(request.clone()), TOKEN, 7, &rec, &limit);
        assert_eq!(answer, json!({"ok": true, "message": format!("handled {cmd:?}")}));
    }
    let seen = rec.calls();
    assert_eq!(seen.len(), 4);
    for ((_, expected), (pane, got)) in requests.iter().zip(&seen) {
        assert_eq!(*pane, Some(7), "the pane is the caller's");
        assert_eq!(got, expected);
    }
    // The handler's own failure comes back as the answer.
    let (answer, _) = remote(&line(json!({"token": TOKEN, "cmd": "view", "path": "/boom"})), TOKEN, 7, &rec, &limit);
    assert_eq!(answer, json!({"ok": false, "message": "no such file"}));
}

#[test]
fn the_pane_of_the_request_is_ignored() {
    let rec = Rec::default();
    let (answer, _) = remote(&line(json!({"token": TOKEN, "pane": 99, "cmd": "ping"})), TOKEN, 4, &rec, &limit());
    assert_eq!(answer["ok"], true);
    assert_eq!(rec.calls(), [(Some(4), Cmd::Ping)], "pane 99 of the request never reaches the handler");
    // A request with no pane at all gets the bound one too.
    remote(&line(json!({"token": TOKEN, "cmd": "ping"})), TOKEN, 5, &rec, &limit());
    assert_eq!(rec.calls()[1].0, Some(5));
}

#[test]
fn ai_hist_and_hist_open_are_refused_before_the_handler() {
    let rec = Rec::default();
    let limit = limit();
    let body = b"the body of an ai request, which must stay unread".to_vec();
    for request in [
        json!({"token": TOKEN, "cmd": "ai", "prompt": "why", "stdin_bytes": body.len()}),
        json!({"token": TOKEN, "cmd": "hist", "line": "ls", "cwd": "/", "exit": 0}),
        json!({"token": TOKEN, "cmd": "hist_open"}),
    ] {
        let is_ai = request["cmd"] == "ai";
        let mut bytes = line(request);
        let line_len = bytes.len();
        bytes.extend_from_slice(&body);
        let (answer, rest) = remote(&bytes, TOKEN, 3, &rec, &limit);
        assert_eq!(answer, refused());
        if is_ai {
            assert_eq!(rest, body, "no body was read for ai");
        }
        assert!(bytes.len() > line_len);
    }
    // Also when the refused command is malformed or too big for its own checks: the answer is the scope's.
    let (answer, _) = remote(&line(json!({"token": TOKEN, "cmd": "ai", "prompt": "x".repeat(40_000), "stdin_bytes": 0})), TOKEN, 3, &rec, &limit);
    assert_eq!(answer, refused());
    assert!(rec.calls().is_empty(), "the handler was never called");
}

#[test]
fn a_wrong_or_missing_token_is_refused_first() {
    let rec = Rec::default();
    let limit = limit();
    let wrong = line(json!({"token": "guess", "cmd": "ping"}));
    let missing = line(json!({"cmd": "ping"}));
    let not_a_string = line(json!({"token": 7, "cmd": "ping"}));
    // A refused command with a wrong token gets the token's answer, not the scope's: the token is looked at first.
    let wrong_ai = line(json!({"token": "guess", "cmd": "ai", "prompt": "q", "stdin_bytes": 0}));
    for bytes in [wrong, missing, not_a_string, wrong_ai] {
        let (answer, _) = remote(&bytes, TOKEN, 1, &rec, &limit);
        assert_eq!(answer["ok"], false);
        assert!(answer["message"].as_str().unwrap().starts_with("refused:"), "{answer}");
    }
    let (answer, _) = remote(b"not json\n", TOKEN, 1, &rec, &limit);
    assert_eq!(answer, json!({"ok": false, "message": "not a valid request"}));
    let (answer, _) = remote(b"", TOKEN, 1, &rec, &limit);
    assert_eq!(answer["message"], "not a valid request", "an empty request");
    assert!(rec.calls().is_empty());
}

/// A reader that never has anything: every read times out, as a silent socket does.
struct Silent;

impl Read for Silent {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::TimedOut, "silent"))
    }
}

impl BufRead for Silent {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        Err(io::Error::new(io::ErrorKind::TimedOut, "silent"))
    }

    fn consume(&mut self, _: usize) {}
}

#[test]
fn the_request_line_is_bounded() {
    let rec = Rec::default();
    let limit = limit();
    // MAX_LINE + 1 bytes without a newline, then a valid request that must never be reached.
    let mut bytes = vec![b'x'; MAX_LINE + 1];
    bytes.push(b'\n');
    bytes.extend(line(json!({"token": TOKEN, "cmd": "ping"})));
    let (answer, _) = remote(&bytes, TOKEN, 1, &rec, &limit);
    assert_eq!(answer, json!({"ok": false, "message": "the request is too long"}));
    assert!(rec.calls().is_empty());

    let mut out = Vec::new();
    serve_remote(&mut Silent, &mut out, TOKEN, 1, &rec, &limit);
    let answer: Value = serde_json::from_str(String::from_utf8(out).unwrap().trim_end()).unwrap();
    assert_eq!(answer, json!({"ok": false, "message": "no request arrived in time"}));
    assert!(rec.calls().is_empty());
}

#[test]
fn too_many_requests_are_busy() {
    let rec = Rec::default();
    let limit = limit();
    let t0 = Instant::now();
    for n in 0..REMOTE_REQUESTS {
        assert!(limit.allow(t0 + Duration::from_millis(n as u64)), "request {n} is inside the budget");
    }
    assert!(!limit.allow(t0 + Duration::from_secs(1)), "the next one is over it");
    assert!(!limit.allow(t0 + REMOTE_WINDOW - Duration::from_millis(1)), "still inside the window");
    assert!(limit.allow(t0 + REMOTE_WINDOW + Duration::from_millis(REMOTE_REQUESTS as u64)), "after the window they pass again");

    // Through the function: the 21st request of a pane is `busy` and never reaches the handler.
    let limit = RateLimit::new(REMOTE_REQUESTS, Duration::from_secs(3600));
    let ping = line(json!({"token": TOKEN, "cmd": "ping"}));
    for _ in 0..REMOTE_REQUESTS {
        assert_eq!(remote(&ping, TOKEN, 1, &rec, &limit).0["ok"], true);
    }
    assert_eq!(remote(&ping, TOKEN, 1, &rec, &limit).0, json!({"ok": false, "message": "busy"}));
    assert_eq!(rec.calls().len(), REMOTE_REQUESTS);

    // A refused request (wrong token, scope) does not use up the budget.
    let limit = RateLimit::new(2, Duration::from_secs(3600));
    for _ in 0..10 {
        remote(&line(json!({"token": "guess", "cmd": "ping"})), TOKEN, 1, &rec, &limit);
        remote(&line(json!({"token": TOKEN, "cmd": "hist_open"})), TOKEN, 1, &rec, &limit);
    }
    assert_eq!(remote(&ping, TOKEN, 1, &rec, &limit).0["ok"], true);
    assert_eq!(remote(&ping, TOKEN, 1, &rec, &limit).0["ok"], true);
    assert_eq!(remote(&ping, TOKEN, 1, &rec, &limit).0["message"], "busy");
}
