//! The protocol of the streaming commands over real sockets: framing, limits, slots, timeouts and cancelling.
//!
//! Every wait in these tests is bounded (a receive timeout or a loop of a few seconds), so a defect fails a test instead of
//! hanging it.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use holzwave_ipc::{
    Cmd, Event, Gone, Handler, Limits, MAX_CHUNK, MAX_OUTPUT, MAX_STDIN, Outcome, Request, Server, Sink, connect, request, serve_with,
    stream_request,
};
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{RecvHalf, SendHalf};

const TOKEN: &str = "stream-token";

fn folder(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hw-ipcs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// What a test handler saw: the pane, the prompt and the input.
type Seen = (Option<u64>, String, String);
type Act = Box<dyn Fn(&str, &Gone, &mut dyn Sink) -> Result<(), String> + Send + Sync>;

/// A handler that records what it was asked and then does what the test says.
struct Probe {
    seen: Mutex<Vec<Seen>>,
    act: Act,
}

impl Probe {
    fn new(act: impl Fn(&str, &Gone, &mut dyn Sink) -> Result<(), String> + Send + Sync + 'static) -> Probe {
        Probe { seen: Mutex::default(), act: Box::new(act) }
    }

    /// Answers `ok` and nothing else.
    fn quiet() -> Probe {
        Probe::new(|_, _, out| out.text("ok").map_err(|e| e.to_string()))
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl Handler for Probe {
    fn handle(&self, _pane: Option<u64>, _cmd: Cmd) -> Result<String, String> {
        Ok(String::new())
    }

    fn handle_stream(&self, pane: Option<u64>, cmd: Cmd, input: String, gone: &Gone, out: &mut dyn Sink) -> Result<(), String> {
        let Cmd::Ai { prompt, .. } = cmd else { return Err("not an ai request".into()) };
        self.seen.lock().unwrap().push((pane, prompt.clone(), input));
        (self.act)(&prompt, gone, out)
    }
}

struct Rig {
    dir: PathBuf,
    probe: Arc<Probe>,
    _server: Server,
}

fn rig(tag: &str, limits: Limits, probe: Probe) -> Rig {
    let dir = folder(tag);
    let probe = Arc::new(probe);
    let server = serve_with(&dir, TOKEN.into(), probe.clone(), limits).unwrap();
    Rig { dir, probe, _server: server }
}

fn limits(clients: usize, streams: usize, read_timeout_ms: u64) -> Limits {
    Limits { clients, streams, read_timeout: Duration::from_millis(read_timeout_ms) }
}

fn ai(prompt: &str, n: usize) -> Request {
    Request { token: TOKEN.into(), pane: Some(1), cmd: Cmd::Ai { prompt: prompt.into(), stdin_bytes: n as u32 } }
}

/// The bytes of a request line for `ai`.
fn raw_header(token: &str, prompt: &str, n: usize) -> Vec<u8> {
    let value = serde_json::json!({"token": token, "pane": 1, "cmd": "ai", "prompt": prompt, "stdin_bytes": n});
    format!("{value}\n").into_bytes()
}

/// A connection written and read by hand.
struct Raw {
    reader: BufReader<RecvHalf>,
    send: SendHalf,
}

impl Raw {
    fn open(dir: &Path) -> Raw {
        let stream = connect(dir).unwrap();
        // Bounded reads (Unix; a named pipe has none and its tests rely on the server answering).
        let _ = stream.set_recv_timeout(Some(Duration::from_secs(5)));
        let (recv, send) = stream.split();
        Raw { reader: BufReader::new(recv), send }
    }

    fn write(&mut self, bytes: &[u8]) {
        self.send.write_all(bytes).unwrap();
    }

    /// The next line as JSON, or `None` at the end of the connection.
    fn value(&mut self) -> Option<serde_json::Value> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).expect("no line arrived in time");
        (n > 0).then(|| serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}")))
    }

    /// Every line up to and including the `end` event.
    fn until_end(&mut self) -> Vec<serde_json::Value> {
        let mut all = Vec::new();
        while let Some(v) = self.value() {
            let end = v["event"] == "end";
            all.push(v);
            if end {
                break;
            }
        }
        all
    }
}

/// The `end` line among `lines`.
fn end_of(lines: &[serde_json::Value]) -> &serde_json::Value {
    let last = lines.last().expect("no line");
    assert_eq!(last["event"], "end", "{lines:?}");
    last
}

fn collect(dir: &Path, request: &Request, input: &[u8]) -> (Vec<Event>, Outcome) {
    let mut events = Vec::new();
    let outcome = stream_request(dir, request, input, &mut |e| {
        events.push(e.clone());
        Ok(())
    })
    .unwrap();
    (events, outcome)
}

/// Try `f` for up to 3 seconds until it gives a value.
fn eventually<T>(mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(started.elapsed() < Duration::from_secs(3), "the state did not come in time");
        thread::sleep(Duration::from_millis(20));
    }
}

fn ping_answered(dir: &Path) -> Option<()> {
    let ping = Request { token: TOKEN.into(), pane: None, cmd: Cmd::Ping };
    request(dir, &ping).ok().filter(|r| r.ok).map(|_| ())
}

/// A counted gate: `wait` returns when a permit is there (or after 10 seconds, with `false`).
#[derive(Default)]
struct Gate {
    permits: Mutex<usize>,
    changed: Condvar,
}

impl Gate {
    fn release(&self, n: usize) {
        *self.permits.lock().unwrap() += n;
        self.changed.notify_all();
    }

    fn wait(&self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut permits = self.permits.lock().unwrap();
        while *permits == 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            permits = self.changed.wait_timeout(permits, left).unwrap().0;
        }
        *permits -= 1;
        true
    }
}

#[test]
fn header_and_input_written_together_and_in_pieces_give_the_same_input() {
    let rig = rig("pieces", Limits::default(), Probe::quiet());
    let input = "\u{e4}".repeat(35_000);
    assert_eq!(input.len(), 70_000);

    let mut together = Raw::open(&rig.dir);
    let mut all = raw_header(TOKEN, "q", input.len());
    all.extend_from_slice(input.as_bytes());
    together.write(&all);
    let lines = together.until_end();
    assert_eq!(end_of(&lines)["ok"], true, "{lines:?}");
    assert!(lines.iter().any(|l| l["event"] == "text" && l["text"] == "ok"));

    let mut pieces = Raw::open(&rig.dir);
    pieces.write(&raw_header(TOKEN, "q", input.len()));
    assert_eq!(pieces.value().unwrap()["event"], "ready");
    let bytes = input.as_bytes();
    // 1001 and 40001 are odd offsets: they fall inside a two-byte character.
    for part in [&bytes[..1001], &bytes[1001..40_001], &bytes[40_001..]] {
        pieces.write(part);
        thread::sleep(Duration::from_millis(30));
    }
    let lines = pieces.until_end();
    assert_eq!(end_of(&lines)["ok"], true, "{lines:?}");
    assert_eq!(rig.probe.seen(), [(Some(1), "q".to_owned(), input.clone()), (Some(1), "q".to_owned(), input)]);
}

#[test]
fn ready_is_written_before_the_input_is_read() {
    let rig = rig("ready", Limits::default(), Probe::quiet());
    let mut raw = Raw::open(&rig.dir);
    raw.write(&raw_header(TOKEN, "q", 5));
    assert_eq!(raw.value().unwrap(), serde_json::json!({"event": "ready"}));
    assert!(rig.probe.seen().is_empty(), "the handler waits for the input");
}

#[test]
fn a_wrong_token_never_reaches_ready_or_the_handler() {
    let rig = rig("token", Limits::default(), Probe::quiet());
    let mut raw = Raw::open(&rig.dir);
    raw.write(&raw_header("guess", "q", 5));
    let refusal = raw.value().unwrap();
    assert!(refusal.get("event").is_none() && refusal["ok"] == false, "{refusal}");
    assert!(refusal["message"].as_str().unwrap().contains("refused"), "{refusal}");
    assert!(raw.value().is_none(), "nothing else is sent by the server");
    assert!(rig.probe.seen().is_empty());
}

#[test]
fn an_input_over_the_limit_is_refused_before_ready() {
    let rig = rig("big", Limits::default(), Probe::quiet());
    let mut raw = Raw::open(&rig.dir);
    raw.write(&raw_header(TOKEN, "q", MAX_STDIN + 1));
    let refusal = raw.value().unwrap();
    assert!(refusal.get("event").is_none() && refusal["ok"] == false, "{refusal}");
    assert!(refusal["message"].as_str().unwrap().contains("the input is longer than 1048576 bytes"), "{refusal}");
    assert!(raw.value().is_none(), "no ready follows");
    assert!(rig.probe.seen().is_empty());
}

#[test]
fn members_that_could_carry_a_consent_are_refused_by_name() {
    let rig = rig("member", Limits::default(), Probe::quiet());
    for member in ["consent", "approve", "allow"] {
        let value = serde_json::json!({"token": TOKEN, "pane": 1, "cmd": "ai", "prompt": "q", "stdin_bytes": 0, member: true});
        let mut raw = Raw::open(&rig.dir);
        raw.write(format!("{value}\n").as_bytes());
        let refusal = raw.value().unwrap();
        let message = refusal["message"].as_str().unwrap();
        assert!(refusal["ok"] == false && message.starts_with("unknown field") && message.contains(member), "{refusal}");
        assert!(raw.value().is_none());
    }
    assert!(rig.probe.seen().is_empty(), "no handler ran");
    let mut raw = Raw::open(&rig.dir);
    raw.write(&raw_header(TOKEN, "q", 0));
    let lines = raw.until_end();
    assert_eq!(end_of(&lines)["ok"], true, "{lines:?}");
    assert_eq!(rig.probe.seen().len(), 1, "the same request without the extra member runs");
}

#[test]
fn bytes_after_the_announced_input_refuse_the_request() {
    let rig = rig("extra", Limits::default(), Probe::quiet());
    let mut raw = Raw::open(&rig.dir);
    raw.write(&raw_header(TOKEN, "q", 5));
    assert_eq!(raw.value().unwrap()["event"], "ready");
    raw.write(b"helloextra");
    let lines = raw.until_end();
    let end = end_of(&lines);
    assert!(end["ok"] == false && end["message"] == "unexpected data after the input", "{end}");
    assert!(rig.probe.seen().is_empty(), "the handler never ran");
}

#[cfg(unix)]
#[test]
fn a_short_input_ends_the_stream_with_an_error_after_the_timeout() {
    let rig = rig("short", limits(1, 1, 300), Probe::quiet());
    let mut raw = Raw::open(&rig.dir);
    raw.write(&raw_header(TOKEN, "q", 10));
    assert_eq!(raw.value().unwrap()["event"], "ready");
    raw.write(b"four");
    let started = Instant::now();
    let lines = raw.until_end();
    let end = end_of(&lines);
    assert!(end["ok"] == false && end["message"] == "the input did not arrive in time", "{end}");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(rig.probe.seen().is_empty());
    eventually(|| ping_answered(&rig.dir));
}

#[cfg(unix)]
#[test]
fn a_silent_connection_frees_its_slot_after_the_timeout() {
    let rig = rig("silent", limits(1, 1, 300), Probe::quiet());
    let mut idle = Raw::open(&rig.dir);
    thread::sleep(Duration::from_millis(700));
    eventually(|| ping_answered(&rig.dir));
    let answer = idle.value().expect("the silent connection was told");
    assert_eq!(answer["message"], "no request arrived in time");
}

#[test]
fn streams_have_their_own_slots_and_history_keeps_its_own() {
    let gate = Arc::new(Gate::default());
    let (entered_tx, entered_rx) = channel::<()>();
    let g = gate.clone();
    let probe = Probe::new(move |prompt, _, _| {
        if prompt == "block" {
            let _ = entered_tx.send(());
            assert!(g.wait(), "the test never released the handler");
        }
        Ok(())
    });
    let rig = rig("slots", limits(8, 2, 10_000), probe);
    let blocked: Vec<_> = (0..2)
        .map(|_| {
            let dir = rig.dir.clone();
            thread::spawn(move || stream_request(&dir, &ai("block", 0), b"", &mut |_| Ok(())).unwrap())
        })
        .collect();
    for _ in 0..2 {
        entered_rx.recv_timeout(Duration::from_secs(5)).expect("a stream handler did not start");
    }

    let third = stream_request(&rig.dir, &ai("block", 0), b"", &mut |_| Ok(())).unwrap();
    assert!(!third.ok && third.message.starts_with("busy:"), "{third:?}");

    let hist = Request { token: TOKEN.into(), pane: Some(1), cmd: Cmd::Hist { line: "ls".into(), cwd: "/".into(), exit: Some(0) } };
    assert!(request(&rig.dir, &hist).unwrap().ok, "history still has its slots while two streams run");

    gate.release(1);
    eventually(|| stream_request(&rig.dir, &ai("quick", 0), b"", &mut |_| Ok(())).ok().filter(|o| o.ok));
    gate.release(1);
    for handle in blocked {
        assert!(handle.join().unwrap().ok);
    }
}

#[test]
fn a_panicking_handler_ends_the_stream_with_an_error_and_frees_its_slot() {
    let act = |prompt: &str, _: &Gone, _: &mut dyn Sink| {
        if prompt == "boom" {
            panic!("a handler that fails");
        }
        Ok(())
    };
    let rig = rig("boom", limits(8, 1, 10_000), Probe::new(act));
    let (_, outcome) = collect(&rig.dir, &ai("boom", 0), b"");
    assert!(!outcome.ok && outcome.message == "holzWave failed while answering", "{outcome:?}");
    eventually(|| stream_request(&rig.dir, &ai("fine", 0), b"", &mut |_| Ok(())).ok().filter(|o| o.ok));

    let rig = rig_with_one_client("boom1", act);
    let (_, outcome) = collect(&rig.dir, &ai("boom", 0), b"");
    assert_eq!(outcome.message, "holzWave failed while answering");
    eventually(|| ping_answered(&rig.dir));
}

fn rig_with_one_client(tag: &str, act: impl Fn(&str, &Gone, &mut dyn Sink) -> Result<(), String> + Send + Sync + 'static) -> Rig {
    rig(tag, limits(1, 1, 10_000), Probe::new(act))
}

#[test]
fn a_client_that_leaves_tells_the_handler_and_the_next_write_fails() {
    let (entered_tx, entered_rx) = channel::<()>();
    let (report_tx, report_rx) = channel::<String>();
    let report = Mutex::new(report_tx);
    let probe = Probe::new(move |prompt, gone, out| {
        let report = report.lock().unwrap().clone();
        let _ = entered_tx.send(());
        match prompt {
            "hook" => {
                let hook_report: Sender<String> = report.clone();
                gone.on_gone(move || {
                    let _ = hook_report.send("hook".into());
                });
                let started = Instant::now();
                while !gone.is_gone() && started.elapsed() < Duration::from_secs(5) {
                    thread::sleep(Duration::from_millis(10));
                }
            }
            _ => {
                let started = Instant::now();
                while started.elapsed() < Duration::from_secs(5) {
                    if let Err(e) = out.text("x") {
                        let _ = report.send(format!("{:?}", e.kind()));
                        return Ok(());
                    }
                    thread::sleep(Duration::from_millis(20));
                }
            }
        }
        Ok(())
    });
    let rig = rig("leave", Limits::default(), probe);

    let mut raw = Raw::open(&rig.dir);
    raw.write(&raw_header(TOKEN, "hook", 2));
    assert_eq!(raw.value().unwrap()["event"], "ready");
    raw.write(b"hi");
    entered_rx.recv_timeout(Duration::from_secs(5)).expect("the handler did not start");
    let left = Instant::now();
    drop(raw);
    assert_eq!(report_rx.recv_timeout(Duration::from_secs(3)).unwrap(), "hook");
    eprintln!("cancel latency: the hook ran {:?} after the client closed", left.elapsed());
    assert!(left.elapsed() < Duration::from_secs(1), "the hook is run within a second");

    let mut raw = Raw::open(&rig.dir);
    raw.write(&raw_header(TOKEN, "spin", 0));
    assert_eq!(raw.value().unwrap()["event"], "ready");
    entered_rx.recv_timeout(Duration::from_secs(5)).expect("the handler did not start");
    assert_eq!(raw.value().unwrap()["event"], "text");
    let left = Instant::now();
    drop(raw);
    assert_eq!(report_rx.recv_timeout(Duration::from_secs(3)).unwrap(), "BrokenPipe");
    eprintln!("cancel latency: the write failed {:?} after the client closed", left.elapsed());
    assert!(left.elapsed() < Duration::from_secs(1), "the next write fails within a second");
}

#[test]
fn long_text_is_cut_into_events_at_character_boundaries() {
    let long = "\u{e4}".repeat(30_000);
    let text = long.clone();
    let rig = rig("long", Limits::default(), Probe::new(move |_, _, out| out.text(&text).map_err(|e| e.to_string())));
    let (events, outcome) = collect(&rig.dir, &ai("q", 0), b"");
    assert!(outcome.ok, "{outcome:?}");
    let texts: Vec<&str> = events.iter().filter_map(|e| if let Event::Text { text } = e { Some(text.as_str()) } else { None }).collect();
    assert!(texts.len() >= 8, "{}", texts.len());
    assert!(texts.iter().all(|t| !t.is_empty() && t.len() <= MAX_CHUNK && t.chars().all(|c| c == '\u{e4}')));
    assert_eq!(texts.concat(), long);
}

#[test]
fn output_over_the_limit_stops_the_handler_and_ends_the_stream() {
    let (wrote_tx, wrote_rx) = channel::<usize>();
    let probe = Probe::new(move |_, _, out| {
        let piece = "a".repeat(512 * 1024);
        let mut wrote = 0usize;
        while wrote <= 64 * 1024 * 1024 && out.text(&piece).is_ok() {
            wrote += piece.len();
        }
        let _ = wrote_tx.send(wrote);
        Ok(())
    });
    let rig = rig("limit", Limits::default(), probe);
    let mut received = 0usize;
    let outcome = stream_request(&rig.dir, &ai("q", 0), b"", &mut |e| {
        if let Event::Text { text } = e {
            received += text.len();
        }
        Ok(())
    })
    .unwrap();
    assert!(!outcome.ok && outcome.message == "the answer is longer than the limit", "{outcome:?}");
    let wrote = wrote_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(wrote > 0 && wrote <= MAX_OUTPUT, "{wrote}");
    assert!(received <= MAX_OUTPUT, "{received}");
}

#[test]
fn a_note_reaches_the_client_apart_from_the_text() {
    let probe = Probe::new(|_, _, out| {
        out.note("waiting").map_err(|e| e.to_string())?;
        out.text("answer").map_err(|e| e.to_string())
    });
    let rig = rig("note", Limits::default(), probe);
    let (events, outcome) = collect(&rig.dir, &ai("q", 0), b"");
    assert_eq!(events, [Event::Note { text: "waiting".into() }, Event::Text { text: "answer".into() }]);
    assert!(outcome.ok);
}

#[test]
fn input_that_is_not_text_ends_the_stream_before_the_handler() {
    let rig = rig("text", Limits::default(), Probe::quiet());
    for body in [&[0xffu8, 0xfe][..], b"a\0b"] {
        let mut raw = Raw::open(&rig.dir);
        raw.write(&raw_header(TOKEN, "q", body.len()));
        assert_eq!(raw.value().unwrap()["event"], "ready");
        raw.write(body);
        let lines = raw.until_end();
        let end = end_of(&lines);
        assert!(end["ok"] == false && end["message"] == "the input is not text (it is not valid UTF-8 or has a NUL)", "{end}");
    }
    assert!(rig.probe.seen().is_empty());
}

/// A handler that knows only `handle`.
struct Plain;

impl Handler for Plain {
    fn handle(&self, _pane: Option<u64>, cmd: Cmd) -> Result<String, String> {
        match cmd {
            Cmd::Ai { prompt, .. } if prompt == "hello" => Ok("hello".into()),
            _ => Err("no".into()),
        }
    }
}

#[test]
fn a_handler_that_does_not_stream_still_answers_through_the_default() {
    let dir = folder("plain");
    let _server = serve_with(&dir, TOKEN.into(), Arc::new(Plain), Limits::default()).unwrap();
    let (events, outcome) = collect(&dir, &ai("hello", 0), b"");
    assert_eq!(events, [Event::Text { text: "hello".into() }]);
    assert!(outcome.ok);
    let (events, outcome) = collect(&dir, &ai("other", 0), b"");
    assert!(events.is_empty());
    assert_eq!(outcome, Outcome { ok: false, message: "no".into() });
}
