//! The streaming half of the local socket: a request with a body and an answer that arrives in pieces.
//!
//! On the wire (the whole grammar is in the module documentation of the crate) the client writes one request line, the
//! server answers `ready` (or one refusal line), the client writes the announced number of bytes, and the server writes
//! `text` and `note` events and exactly one `end`. The server half ([`serve`]) reads the body through the buffered reader
//! that read the request line, so no byte is lost; the client half ([`stream_request`]) writes the request, the body once
//! `ready` came, and hands every event to a callback.

use std::io::{self, BufReader, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use interprocess::local_socket::traits::{RecvHalf as _, Stream as _};
use interprocess::local_socket::{RecvHalf, SendHalf};
use serde::{Deserialize, Serialize};

use crate::{BUSY_STREAMS, Cmd, Endpoint, Handler, Limits, MAX_LINE, Request, Response, Slot, answer, read_bounded_line};

/// The longest text of one `text` event, in bytes.
pub const MAX_CHUNK: usize = 8192;
/// The longest text of one `note` event, in bytes.
pub const MAX_NOTE: usize = 1024;
/// The most that one answer may hold in text and notes together, in bytes.
pub const MAX_OUTPUT: usize = 4 * 1024 * 1024;
/// The most that a client reads from one reply, in bytes of lines.
pub const MAX_REPLY: usize = 32 * 1024 * 1024;
/// The longest message of an `end` line, in bytes.
const MAX_END_MESSAGE: usize = 4096;

const INPUT_SHORT: &str = "the input ended before its announced length";
const INPUT_NOT_TEXT: &str = "the input is not text (it is not valid UTF-8 or has a NUL)";
const HANDLER_FAILED: &str = "holzWave failed while answering";
const INPUT_LATE: &str = "the input did not arrive in time";
const INPUT_EXTRA: &str = "unexpected data after the input";
const OUTPUT_LIMIT: &str = "the answer is longer than the limit";
/// How often the watcher looks whether the handler has returned.
const WATCH_POLL: Duration = Duration::from_millis(200);
/// The members of an `ai` request.
const AI_MEMBERS: [&str; 5] = ["token", "pane", "cmd", "prompt", "stdin_bytes"];

/// The first member of an `ai` request that the request does not have, shown cut at 40 characters and escaped, or `None`
/// (also for every other command). The request type would ignore such a member; this refuses it by name.
pub(crate) fn stray_member(value: &serde_json::Value) -> Option<String> {
    if value.get("cmd").and_then(|c| c.as_str()) != Some("ai") {
        return None;
    }
    let key = value.as_object()?.keys().find(|k| !AI_MEMBERS.contains(&k.as_str()))?;
    let shown: String = key.chars().take(40).collect();
    let escaped = format!("{shown:?}");
    let inner = escaped.strip_prefix('"').and_then(|e| e.strip_suffix('"')).unwrap_or(&escaped);
    Some(if key.chars().count() > 40 { format!("{inner}...") } else { inner.to_owned() })
}

/// One line the server writes after the request was accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The server is ready to read the body.
    Ready,
    /// A piece of the answer, for standard output.
    Text { text: String },
    /// A line of status, for standard error.
    Note { text: String },
    /// The last line: whether the request succeeded, and a message.
    End { ok: bool, message: String },
    /// An event this version does not know. Only read, never written.
    #[serde(other)]
    Unknown,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

type Hook = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct GoneState {
    gone: bool,
    finished: bool,
    hooks: Vec<Hook>,
}

/// Tells a handler that the client left, so it can stop its work.
#[derive(Default)]
pub struct Gone {
    state: Mutex<GoneState>,
}

impl Gone {
    /// A signal that has not fired.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the client has left (closed its end, sent something it must not, or a write to it failed).
    pub fn is_gone(&self) -> bool {
        lock(&self.state).gone
    }

    /// Run `hook` when the client leaves: at once when it has left already, else once when it does. A hook never runs after
    /// the handler returned.
    pub fn on_gone(&self, hook: impl FnOnce() + Send + 'static) {
        let run_now = {
            let mut s = lock(&self.state);
            if s.finished {
                return;
            }
            if !s.gone {
                s.hooks.push(Box::new(hook));
                return;
            }
            hook
        };
        run_hook(Box::new(run_now));
    }

    /// The client left: mark it and run the stored hooks, outside the lock. Does nothing after [`Gone::finish`].
    pub(crate) fn set(&self) {
        let hooks = {
            let mut s = lock(&self.state);
            if s.gone || s.finished {
                return;
            }
            s.gone = true;
            std::mem::take(&mut s.hooks)
        };
        for hook in hooks {
            run_hook(hook);
        }
    }

    /// The handler returned: no hook runs from here on.
    pub(crate) fn finish(&self) {
        let mut s = lock(&self.state);
        s.finished = true;
        s.hooks.clear();
    }
}

/// A hook that panics must not take the thread that fires it down.
fn run_hook(hook: Hook) {
    let _ = catch_unwind(AssertUnwindSafe(hook));
}

/// Where a handler writes its answer. Text goes to standard output of `wsh`, a note to standard error. Both fail once the
/// client is gone or a bound is hit.
pub trait Sink {
    fn text(&mut self, text: &str) -> io::Result<()>;
    fn note(&mut self, text: &str) -> io::Result<()>;
}

/// The pieces of `text`, none longer than `max` bytes (at least 4), cut at character boundaries.
fn pieces(text: &str, max: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut cut = rest.len().min(max);
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        let (head, tail) = rest.split_at(cut);
        out.push(head);
        rest = tail;
    }
    out
}

/// `text` cut at `max` bytes on a character boundary.
fn cut_to(text: &str, max: usize) -> &str {
    let mut cut = text.len().min(max);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    &text[..cut]
}

fn write_event(out: &mut impl Write, event: &Event) -> io::Result<()> {
    let mut line = serde_json::to_string(event).map_err(io::Error::other)?;
    line.push('\n');
    out.write_all(line.as_bytes())
}

struct WireSink<'a, W: Write> {
    out: &'a mut W,
    gone: &'a Gone,
    /// Bytes of text and notes written so far.
    written: usize,
    /// Whether a write was refused because the answer reached [`MAX_OUTPUT`].
    limit_hit: bool,
}

impl<W: Write> WireSink<'_, W> {
    fn send(&mut self, event: &Event, size: usize) -> io::Result<()> {
        if self.gone.is_gone() {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the client is gone"));
        }
        if self.written + size > MAX_OUTPUT {
            self.limit_hit = true;
            return Err(io::Error::other(OUTPUT_LIMIT));
        }
        write_event(self.out, event).inspect_err(|_| self.gone.set())?;
        self.written += size;
        Ok(())
    }
}

impl<W: Write> Sink for WireSink<'_, W> {
    fn text(&mut self, text: &str) -> io::Result<()> {
        for piece in pieces(text, MAX_CHUNK) {
            self.send(&Event::Text { text: piece.to_owned() }, piece.len())?;
        }
        Ok(())
    }

    fn note(&mut self, text: &str) -> io::Result<()> {
        let note = cut_to(text, MAX_NOTE);
        self.send(&Event::Note { text: note.to_owned() }, note.len())
    }
}

/// Watch the receive half while the handler runs: the client must send nothing after the body, so end of file, any byte or
/// an error means it left. Ends when the handler has returned.
fn watch(mut reader: BufReader<RecvHalf>, gone: &Gone, done: &AtomicBool) {
    let mut byte = [0u8; 64];
    loop {
        match reader.read(&mut byte) {
            Ok(_) => {
                if !done.load(Ordering::SeqCst) {
                    gone.set();
                }
                return;
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => {
                if done.load(Ordering::SeqCst) {
                    return;
                }
            }
            Err(_) => {
                if !done.load(Ordering::SeqCst) {
                    gone.set();
                }
                return;
            }
        }
    }
}

/// The server half of a stream: a stream slot, `ready`, the body, the handler with a watcher beside it, one `end`.
pub(crate) fn serve(
    mut reader: BufReader<RecvHalf>,
    mut send: SendHalf,
    request: Request,
    handler: &dyn Handler,
    limits: &Limits,
    streams: &Arc<AtomicUsize>,
) {
    let Some(slot) = Slot::take(streams, limits.streams) else {
        return answer(&mut send, false, BUSY_STREAMS);
    };
    if write_event(&mut send, &Event::Ready).is_err() {
        return;
    }
    let end = |send: &mut SendHalf, ok: bool, message: &str| {
        let _ = write_event(send, &Event::End { ok, message: cut_to(message, MAX_END_MESSAGE).to_owned() });
    };
    let announced = request.cmd.body_len();
    let mut body = Vec::with_capacity(announced);
    match (&mut reader).take(announced as u64).read_to_end(&mut body) {
        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
            return end(&mut send, false, INPUT_LATE);
        }
        Err(_) => return end(&mut send, false, INPUT_SHORT),
        Ok(_) if body.len() < announced => return end(&mut send, false, INPUT_SHORT),
        Ok(_) => {}
    }
    if !reader.buffer().is_empty() {
        return end(&mut send, false, INPUT_EXTRA);
    }
    let input = match String::from_utf8(body) {
        Ok(text) if !text.contains('\0') => text,
        _ => return end(&mut send, false, INPUT_NOT_TEXT),
    };
    let gone = Arc::new(Gone::new());
    let done = Arc::new(AtomicBool::new(false));
    // The watcher polls: its read times out every 200 ms so that it sees `done` (the result is ignored; named pipes have no
    // timeouts and there the watcher ends when the client closes, which it does as soon as it has read `end`).
    let _ = reader.get_ref().set_timeout(Some(WATCH_POLL));
    let watcher = {
        let (gone, done) = (gone.clone(), done.clone());
        thread::Builder::new().name("holzwave-ipc-watch".into()).spawn(move || watch(reader, &gone, &done))
    };
    let Ok(watcher) = watcher else {
        return end(&mut send, false, HANDLER_FAILED);
    };
    let (result, limit_hit) = {
        let mut sink = WireSink { out: &mut send, gone: &gone, written: 0, limit_hit: false };
        let result = catch_unwind(AssertUnwindSafe(|| handler.handle_stream(request.pane, request.cmd, input, &gone, &mut sink)));
        (result, sink.limit_hit)
    };
    gone.finish();
    done.store(true, Ordering::SeqCst);
    match result {
        Ok(Ok(())) if limit_hit => end(&mut send, false, OUTPUT_LIMIT),
        Ok(Ok(())) => end(&mut send, true, ""),
        Ok(Err(message)) => end(&mut send, false, &message),
        Err(_) => end(&mut send, false, HANDLER_FAILED),
    }
    drop(slot);
    let _ = watcher.join();
}

/// What a finished stream said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub ok: bool,
    pub message: String,
}

/// Send `request` (a command that streams) and `input`, and hand every text and note event to `on_event`. A refusal before
/// the stream starts is an [`Outcome`] that is not ok, and no input is sent. An error of the callback is returned unchanged
/// and drops the connection, which cancels the request on the server.
pub fn stream_request(
    dir: &Path,
    request: &Request,
    input: &[u8],
    on_event: &mut dyn FnMut(&Event) -> io::Result<()>,
) -> io::Result<Outcome> {
    stream_request_at(&Endpoint::from(dir), request, input, on_event)
}

/// [`stream_request`] to an [`Endpoint`].
pub fn stream_request_at(
    endpoint: &Endpoint,
    request: &Request,
    input: &[u8],
    on_event: &mut dyn FnMut(&Event) -> io::Result<()>,
) -> io::Result<Outcome> {
    let invalid = |why: &str| io::Error::new(io::ErrorKind::InvalidInput, why.to_owned());
    match &request.cmd {
        Cmd::Ai { stdin_bytes, .. } if *stdin_bytes as usize == input.len() => {}
        Cmd::Ai { .. } => return Err(invalid("the announced input length differs from the input")),
        _ => return Err(invalid("this command does not stream; use request")),
    }
    let mut line = serde_json::to_string(request).map_err(io::Error::other)?;
    line.push('\n');
    if line.len() > MAX_LINE {
        return Err(invalid("the request is too long"));
    }
    let (recv, mut send) = crate::connect_endpoint(endpoint)?.split();
    let mut reader = BufReader::new(recv);
    send.write_all(line.as_bytes())?;
    let first = read_bounded_line(&mut reader)?;
    if first.is_empty() {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the app closed the connection before the answer ended"));
    }
    let value: serde_json::Value =
        serde_json::from_str(&first).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the app sent no valid answer"))?;
    if value.get("event").is_none() {
        let refusal: Response = serde_json::from_value(value)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the app sent no valid answer"))?;
        return Ok(Outcome { ok: refusal.ok, message: refusal.message });
    }
    send.write_all(input)?;
    let mut total = 0usize;
    let mut next = Some(first);
    loop {
        let text = match next.take() {
            Some(t) => t,
            None => read_bounded_line(&mut reader)?,
        };
        total += text.len();
        if total > MAX_REPLY {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "the reply is longer than the limit"));
        }
        if text.is_empty() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the app closed the connection before the answer ended"));
        }
        let event: Event = serde_json::from_str(&text)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the app sent no valid answer"))?;
        match event {
            Event::Text { .. } | Event::Note { .. } => on_event(&event)?,
            Event::End { ok, message } => return Ok(Outcome { ok, message }),
            Event::Ready | Event::Unknown => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn gone_runs_each_hook_once_and_never_after_finish() {
        let count = Arc::new(AtomicUsize::new(0));
        let gone = Gone::new();
        let c = count.clone();
        gone.on_gone(move || {
            c.fetch_add(1, Ordering::SeqCst);
        });
        assert!(!gone.is_gone() && count.load(Ordering::SeqCst) == 0);
        gone.set();
        gone.set();
        assert!(gone.is_gone());
        assert_eq!(count.load(Ordering::SeqCst), 1, "once, however often the client leaves");
        let c = count.clone();
        gone.on_gone(move || {
            c.fetch_add(10, Ordering::SeqCst);
        });
        assert_eq!(count.load(Ordering::SeqCst), 11, "a hook added after the client left runs at once");

        let late = Gone::new();
        let c = count.clone();
        late.on_gone(move || {
            c.fetch_add(100, Ordering::SeqCst);
        });
        late.finish();
        late.set();
        let c = count.clone();
        late.on_gone(move || {
            c.fetch_add(100, Ordering::SeqCst);
        });
        assert!(!late.is_gone());
        assert_eq!(count.load(Ordering::SeqCst), 11, "nothing runs after the handler returned");

        let boom = Gone::new();
        boom.on_gone(|| std::panic::resume_unwind(Box::new("a hook that panics")));
        boom.set();
        assert!(boom.is_gone(), "a panicking hook does not stop the others or the thread");
    }

    #[test]
    fn the_events_have_the_documented_wire_shape() {
        let json = |e: &Event| serde_json::to_string(e).unwrap();
        assert_eq!(json(&Event::Ready), r#"{"event":"ready"}"#);
        assert_eq!(json(&Event::Text { text: "a\nb".into() }), r#"{"event":"text","text":"a\nb"}"#);
        assert_eq!(json(&Event::Note { text: "n".into() }), r#"{"event":"note","text":"n"}"#);
        assert_eq!(json(&Event::End { ok: false, message: "m".into() }), r#"{"event":"end","ok":false,"message":"m"}"#);
        assert_eq!(serde_json::from_str::<Event>(r#"{"event":"progress","pct":5}"#).unwrap(), Event::Unknown);
        assert!(serde_json::from_str::<Event>(r#"{"ok":false,"message":"busy"}"#).is_err(), "a refusal line is not an event");
    }

    #[test]
    fn the_split_of_a_text_keeps_characters_whole() {
        let text = format!("{}{}{}", "a".repeat(5), "ä".repeat(7), "😀".repeat(5));
        for max in [4usize, 5, 6, 7, 9, 16] {
            let parts = pieces(&text, max);
            assert!(parts.iter().all(|p| !p.is_empty() && p.len() <= max), "{max}");
            assert_eq!(parts.concat(), text, "{max}");
        }
        assert!(pieces("", 8).is_empty());
        assert_eq!(cut_to("aä", 2), "a");
        assert_eq!(cut_to("ab", 5), "ab");
    }

    use interprocess::local_socket::traits::Listener as _;
    use interprocess::local_socket::ListenerOptions;
    use std::path::PathBuf;

    fn folder(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hw-ipcf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn ai(n: u32) -> Request {
        Request { token: "t".into(), pane: None, cmd: Cmd::Ai { prompt: "q".into(), stdin_bytes: n } }
    }

    /// A server for one connection that reads the request line, writes `first`, reads `read_body` bytes, writes `rest`, and
    /// then either closes (`linger` false) or returns how many bytes the client sends before it closes.
    fn fake(dir: &Path, first: Vec<String>, read_body: usize, rest: Vec<String>, linger: bool) -> thread::JoinHandle<usize> {
        crate::make_private_dir(dir).unwrap();
        let listener = ListenerOptions::new().name(crate::name(dir).unwrap()).create_sync().unwrap();
        thread::spawn(move || {
            let Ok(stream) = listener.accept() else { return usize::MAX };
            let _ = stream.set_recv_timeout(Some(Duration::from_secs(3)));
            let (recv, mut send) = stream.split();
            let mut reader = BufReader::new(recv);
            if read_bounded_line(&mut reader).is_err() {
                return usize::MAX;
            }
            for line in &first {
                let _ = send.write_all(format!("{line}\n").as_bytes());
            }
            let mut body = vec![0u8; read_body];
            if read_body > 0 && reader.read_exact(&mut body).is_err() {
                return usize::MAX;
            }
            for line in &rest {
                if send.write_all(format!("{line}\n").as_bytes()).is_err() {
                    break;
                }
            }
            if !linger {
                return 0;
            }
            // What the client sends after that (nothing, when it behaves): bytes until it closes or the timeout.
            let mut extra = Vec::new();
            let _ = reader.read_to_end(&mut extra);
            extra.len()
        })
    }

    fn collect(dir: &Path, request: &Request, input: &[u8]) -> (Vec<Event>, io::Result<Outcome>) {
        let mut events = Vec::new();
        let outcome = stream_request(dir, request, input, &mut |e| {
            events.push(e.clone());
            Ok(())
        });
        (events, outcome)
    }

    #[test]
    fn the_client_ignores_unknown_events_and_returns_the_outcome() {
        let d = folder("a");
        let server = fake(
            &d,
            vec![r#"{"event":"ready"}"#.into()],
            2,
            vec![
                r#"{"event":"progress","pct":5}"#.into(),
                r#"{"event":"text","text":"hi "}"#.into(),
                r#"{"event":"note","text":"n"}"#.into(),
                r#"{"event":"end","ok":true,"message":"done"}"#.into(),
            ],
            true,
        );
        let (events, outcome) = collect(&d, &ai(2), b"ab");
        assert_eq!(events, [Event::Text { text: "hi ".into() }, Event::Note { text: "n".into() }]);
        assert_eq!(outcome.unwrap(), Outcome { ok: true, message: "done".into() });
        assert_eq!(server.join().unwrap(), 0);
    }

    #[test]
    fn a_reply_line_longer_than_the_limit_is_an_error() {
        let d = folder("b");
        let long = format!(r#"{{"event":"text","text":"{}"}}"#, "x".repeat(MAX_LINE));
        let server = fake(&d, vec![r#"{"event":"ready"}"#.into()], 0, vec![long], true);
        let (_, outcome) = collect(&d, &ai(0), b"");
        assert_eq!(outcome.unwrap_err().kind(), io::ErrorKind::InvalidData);
        let _ = server.join();

        // Many short lines that add up to more than the whole reply may hold.
        let d = folder("b2");
        let line = format!(r#"{{"event":"text","text":"{}"}}"#, "y".repeat(MAX_LINE / 2));
        let many = vec![line; MAX_REPLY / (MAX_LINE / 2) + 8];
        let server = fake(&d, vec![r#"{"event":"ready"}"#.into()], 0, many, true);
        let (_, outcome) = collect(&d, &ai(0), b"");
        let err = outcome.unwrap_err();
        assert!(err.kind() == io::ErrorKind::InvalidData && err.to_string().contains("reply is longer"), "{err}");
        let _ = server.join();
    }

    #[test]
    fn a_stream_that_closes_without_an_end_is_an_error() {
        let d = folder("c");
        let server = fake(&d, vec![r#"{"event":"ready"}"#.into()], 0, vec![r#"{"event":"text","text":"half"}"#.into()], false);
        let (events, outcome) = collect(&d, &ai(0), b"");
        assert_eq!(events, [Event::Text { text: "half".into() }]);
        assert_eq!(outcome.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        let _ = server.join();
    }

    #[test]
    fn a_refusal_line_is_an_outcome_and_sends_no_input() {
        let d = folder("d");
        let server = fake(&d, vec![r#"{"ok":false,"message":"refused: no"}"#.into()], 0, vec![], true);
        let (events, outcome) = collect(&d, &ai(5), b"hello");
        assert!(events.is_empty());
        assert_eq!(outcome.unwrap(), Outcome { ok: false, message: "refused: no".into() });
        assert_eq!(server.join().unwrap(), 0, "no byte arrived after the request line");
    }

    #[test]
    fn the_client_refuses_a_request_whose_input_length_differs() {
        let nowhere = folder("e");
        for (request, input) in [(ai(3), &b"ab"[..]), (ai(0), b"x"), (Request { token: "t".into(), pane: None, cmd: Cmd::Ping }, b"")] {
            let (events, outcome) = collect(&nowhere, &request, input);
            assert!(events.is_empty());
            assert_eq!(outcome.unwrap_err().kind(), io::ErrorKind::InvalidInput, "{request:?}");
        }
        let long = Request { token: "t".into(), pane: None, cmd: Cmd::Ai { prompt: "p".repeat(MAX_LINE), stdin_bytes: 0 } };
        assert_eq!(collect(&nowhere, &long, b"").1.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn a_stray_member_of_an_ai_request_is_named() {
        let clean = serde_json::json!({"token":"t","pane":1,"cmd":"ai","prompt":"q","stdin_bytes":0});
        assert_eq!(stray_member(&clean), None);
        let mut extra = clean.clone();
        extra["consent"] = serde_json::json!(true);
        assert_eq!(stray_member(&extra).as_deref(), Some("consent"));
        let mut long = clean.clone();
        long["k".repeat(200)] = serde_json::json!(1);
        let shown = stray_member(&long).unwrap();
        assert_eq!(shown, format!("{}...", "k".repeat(40)));
        let mut odd = clean.clone();
        odd["a\"\nb"] = serde_json::json!(1);
        assert_eq!(stray_member(&odd).as_deref(), Some("a\\\"\\nb"), "the name is escaped");
        let other = serde_json::json!({"token":"t","cmd":"ping","consent":true});
        assert_eq!(stray_member(&other), None, "only an ai request is held to its names");
    }
}
