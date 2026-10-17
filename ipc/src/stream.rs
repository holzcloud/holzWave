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
use std::sync::{Mutex, MutexGuard, PoisonError};

use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{RecvHalf, SendHalf};
use serde::{Deserialize, Serialize};

use crate::{Cmd, Handler, MAX_LINE, Request, Response, read_bounded_line};

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
}

impl<W: Write> WireSink<'_, W> {
    fn send(&mut self, event: &Event) -> io::Result<()> {
        if self.gone.is_gone() {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the client is gone"));
        }
        write_event(self.out, event).inspect_err(|_| self.gone.set())
    }
}

impl<W: Write> Sink for WireSink<'_, W> {
    fn text(&mut self, text: &str) -> io::Result<()> {
        for piece in pieces(text, MAX_CHUNK) {
            self.send(&Event::Text { text: piece.to_owned() })?;
        }
        Ok(())
    }

    fn note(&mut self, text: &str) -> io::Result<()> {
        self.send(&Event::Note { text: cut_to(text, MAX_NOTE).to_owned() })
    }
}

/// The server half of a stream: `ready`, the body, the handler, one `end`.
pub(crate) fn serve(mut reader: BufReader<RecvHalf>, mut send: SendHalf, request: Request, handler: &dyn Handler) {
    if write_event(&mut send, &Event::Ready).is_err() {
        return;
    }
    let end = |send: &mut SendHalf, ok: bool, message: &str| {
        let _ = write_event(send, &Event::End { ok, message: cut_to(message, MAX_END_MESSAGE).to_owned() });
    };
    let announced = request.cmd.body_len();
    let mut body = Vec::with_capacity(announced);
    let read = (&mut reader).take(announced as u64).read_to_end(&mut body);
    if read.is_err() || body.len() < announced {
        return end(&mut send, false, INPUT_SHORT);
    }
    let input = match String::from_utf8(body) {
        Ok(text) if !text.contains('\0') => text,
        _ => return end(&mut send, false, INPUT_NOT_TEXT),
    };
    let gone = Gone::new();
    let result = {
        let mut sink = WireSink { out: &mut send, gone: &gone };
        catch_unwind(AssertUnwindSafe(|| handler.handle_stream(request.pane, request.cmd, input, &gone, &mut sink)))
    };
    gone.finish();
    match result {
        Ok(Ok(())) => end(&mut send, true, ""),
        Ok(Err(message)) => end(&mut send, false, &message),
        Err(_) => end(&mut send, false, HANDLER_FAILED),
    }
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
    let (recv, mut send) = crate::connect(dir)?.split();
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
    let mut next = Some(first);
    loop {
        let text = match next.take() {
            Some(t) => t,
            None => read_bounded_line(&mut reader)?,
        };
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
}
