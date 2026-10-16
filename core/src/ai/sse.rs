//! The bounded stream framing of the provider dialects.
//!
//! Every dialect the product speaks arrives as a byte stream that the network cuts at arbitrary places. This file turns
//! those bytes into events, once, so that the wire adapters interpret events and never frame bytes themselves.
//!
//! - The decoder follows the HTML Standard, Server-sent events, event stream interpretation, with the deviations that
//!   keep a hostile or broken server harmless: every size is bounded, a failure is final, an event is dispatched only
//!   at its blank line, and the end of a stream never dispatches what is pending.
//! - **The client never reconnects.** A reconnect would send the prompt a second time (double billing, repeated side
//!   effects). So the decoder has no connection, no reconnection time and no last event id: the `id` and `retry` lines
//!   are parsed away without being read, and there is nothing to feed them to.
//! - The decoders do no I/O. They are pure functions of the bytes pushed into them and never read a clock; a caller
//!   that owns the connection pushes each chunk as it arrives and asks [`SseDecoder::finish`] when the body ends.
//!
//! The only dialect word in the file is the `[DONE]` data line behind [`SseEvent::is_done`].

use std::fmt;

use super::provider::{AiError, Detail, Flow};
use super::types::Size;

/// The longest line, without its terminator: 1 MiB.
pub const MAX_LINE: usize = 1_048_576;
/// The most an event may hold while it is pending: 4 MiB.
pub const MAX_EVENT: usize = 4_194_304;

// ---- limits, errors, the end of a stream ----------------------------------------------------------------

/// The two bounds of a decoder.
///
/// A line (without its terminator) may hold at most `line` bytes. The pending event (its data, one byte per data line
/// for the joining line feed, and its event name) may hold at most `event` bytes. Exactly at the limit passes, one more
/// fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most bytes in one line.
    pub line: usize,
    /// The most bytes in one pending event.
    pub event: usize,
}

impl Limits {
    /// [`MAX_LINE`] and [`MAX_EVENT`].
    pub const DEFAULT: Limits = Limits { line: MAX_LINE, event: MAX_EVENT };
}

/// Why a decoder gave up. The first error is final: the decoder does not look for the next event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// A line is longer than the limit.
    LineTooLong {
        /// The limit that was passed.
        limit: usize,
    },
    /// The pending event is larger than the limit.
    EventTooLarge {
        /// The limit that was passed.
        limit: usize,
    },
    /// The data or the name of an event is not UTF-8.
    InvalidUtf8,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::LineTooLong { limit } => write!(f, "a line of the stream is longer than {limit} bytes"),
            FrameError::EventTooLarge { limit } => write!(f, "an event of the stream is larger than {limit} bytes"),
            FrameError::InvalidUtf8 => f.write_str("the stream holds text that is not valid UTF-8"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<FrameError> for AiError {
    fn from(error: FrameError) -> Self {
        AiError::Protocol(Detail::new(&error.to_string()))
    }
}

/// How a stream ended, as far as the framing can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamEnd {
    /// Everything that arrived was a whole line and a whole event.
    Clean,
    /// Part of a line or of an event was pending, or the decoder had failed. Nothing pending was delivered.
    Truncated {
        /// How many bytes were dropped.
        discarded: usize,
    },
    /// The consumer answered [`Flow::Stop`], so the rest of the stream was not read.
    Stopped,
}

impl StreamEnd {
    /// Whether the stream ended cleanly.
    pub fn is_clean(self) -> bool {
        matches!(self, StreamEnd::Clean)
    }
}

/// Where a decoder is. A failed decoder keeps its error and how much it dropped.
#[derive(Clone, Copy, Debug)]
enum State {
    Running,
    Stopped,
    Failed { error: FrameError, dropped: usize },
}

impl State {
    fn name(self) -> &'static str {
        match self {
            State::Running => "running",
            State::Stopped => "stopped",
            State::Failed { .. } => "failed",
        }
    }
}

/// The verdict of `finish` for a decoder in `state` that still holds `pending` bytes.
fn verdict(state: State, pending: usize) -> StreamEnd {
    match state {
        State::Stopped => StreamEnd::Stopped,
        // A failed stream always lost at least the bytes that made it fail.
        State::Failed { dropped, .. } => StreamEnd::Truncated { discarded: dropped.max(1) },
        State::Running if pending == 0 => StreamEnd::Clean,
        State::Running => StreamEnd::Truncated { discarded: pending },
    }
}

// ---- the event ------------------------------------------------------------------------------------------

/// One event, borrowed from the decoder for the duration of the callback.
///
/// `Debug` prints sizes only: the data is model output and may hold an echoed secret.
#[derive(Clone, Copy)]
pub struct SseEvent<'a> {
    event: &'a str,
    data: &'a str,
}

impl<'a> SseEvent<'a> {
    fn new(event: &'a str, data: &'a str) -> Self {
        Self { event, data }
    }

    /// The event name; `message` when the stream named none.
    pub fn event(&self) -> &str {
        self.event
    }

    /// The data; several data lines are joined by a line feed, with no trailing line feed.
    pub fn data(&self) -> &str {
        self.data
    }

    /// Whether the data is exactly `[DONE]`, the end marker some dialects send.
    pub fn is_done(&self) -> bool {
        self.data == "[DONE]"
    }
}

impl fmt::Debug for SseEvent<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SseEvent")
            .field("event", &Size(self.event.len()))
            .field("data", &Size(self.data.len()))
            .finish()
    }
}

// ---- the server-sent-events decoder ---------------------------------------------------------------------

/// A decoder for a server-sent-events stream.
///
/// Push each chunk as it arrives, in any size: the result does not depend on where the network cut the stream. An event
/// leaves the decoder at its blank line and not before. The callback answers [`Flow`], so a cancel or a failed parse in
/// the adapter stops the decode within one event.
///
/// The memory is bounded by [`Limits`], checked before every append. After warm-up the buffers are reused and the
/// per-event path allocates nothing it does not already own.
pub struct SseDecoder {
    limits: Limits,
    /// The raw bytes of a line that has started and not ended.
    carry: Vec<u8>,
    /// The pending data: each data value followed by one line feed.
    data: Vec<u8>,
    /// The pending event name.
    name: Vec<u8>,
    /// A data field was seen since the last dispatch.
    data_seen: bool,
    /// The previous chunk ended with a carriage return, so a line feed that starts this chunk belongs to it.
    skip_lf: bool,
    /// The first line has not been processed yet.
    #[allow(dead_code)]
    first_line: bool,
    state: State,
}

impl Default for SseDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl SseDecoder {
    /// A decoder with the default limits.
    pub fn new() -> Self {
        Self::with_limits(Limits::DEFAULT)
    }

    /// A decoder with `limits`.
    pub fn with_limits(limits: Limits) -> Self {
        Self {
            limits,
            carry: Vec::new(),
            data: Vec::new(),
            name: Vec::new(),
            data_seen: false,
            skip_lf: false,
            first_line: true,
            state: State::Running,
        }
    }

    /// Takes one chunk and calls `on_event` for every event it completes, in order.
    ///
    /// An event is borrowed for the duration of the callback. An `Err` is final: the same error comes back from every
    /// later push, with no event and no attempt to find the next one. After `Ok(Flow::Stop)` later pushes deliver
    /// nothing and return `Ok(Flow::Stop)`. Events delivered before an error in the same chunk stay delivered.
    pub fn push(
        &mut self,
        chunk: &[u8],
        on_event: &mut dyn FnMut(SseEvent<'_>) -> Flow,
    ) -> Result<Flow, FrameError> {
        match self.state {
            State::Failed { error, .. } => return Err(error),
            State::Stopped => return Ok(Flow::Stop),
            State::Running => {}
        }
        match self.scan(chunk, on_event) {
            Ok(Flow::Continue) => Ok(Flow::Continue),
            Ok(Flow::Stop) => {
                self.release();
                self.state = State::Stopped;
                Ok(Flow::Stop)
            }
            Err(error) => {
                let dropped = self.buffered();
                self.release();
                self.state = State::Failed { error, dropped };
                Err(error)
            }
        }
    }

    /// The verdict at the end of the stream. Nothing that is pending is dispatched: the standard says an event needs
    /// its blank line, and a half event is how a cut stream looks.
    #[must_use]
    pub fn finish(self) -> StreamEnd {
        verdict(self.state, self.buffered())
    }

    /// The bytes held for a line or an event that is not finished.
    pub fn buffered(&self) -> usize {
        self.carry.len() + self.data.len() + self.name.len()
    }

    /// The bytes the decoder has reserved.
    pub fn capacity(&self) -> usize {
        self.carry.capacity() + self.data.capacity() + self.name.capacity()
    }

    /// Drops every buffer, so memory goes back after a failure or a stop.
    fn release(&mut self) {
        self.carry = Vec::new();
        self.data = Vec::new();
        self.name = Vec::new();
        self.data_seen = false;
    }

    /// Splits `chunk` into lines. Only the new bytes are scanned, so a slow drip costs linear time.
    fn scan(
        &mut self,
        chunk: &[u8],
        on_event: &mut dyn FnMut(SseEvent<'_>) -> Flow,
    ) -> Result<Flow, FrameError> {
        let mut rest = chunk;
        if self.skip_lf {
            self.skip_lf = false;
            if rest.first() == Some(&b'\n') {
                rest = &rest[1..];
            }
        }
        while !rest.is_empty() {
            let Some(end) = rest.iter().position(|&b| b == b'\n' || b == b'\r') else {
                // No terminator yet: keep the bytes, within the line bound.
                if self.carry.len() + rest.len() > self.limits.line {
                    return Err(FrameError::LineTooLong { limit: self.limits.line });
                }
                self.carry.extend_from_slice(rest);
                break;
            };
            let carriage_return = rest[end] == b'\r';
            let tail = &rest[..end];
            rest = &rest[end + 1..];
            if carriage_return {
                // A carriage return ends the line; a line feed right behind it belongs to it.
                match rest.first() {
                    Some(b'\n') => rest = &rest[1..],
                    Some(_) => {}
                    None => self.skip_lf = true,
                }
            }
            if self.line(tail, on_event)? == Flow::Stop {
                return Ok(Flow::Stop);
            }
        }
        Ok(Flow::Continue)
    }

    /// Finishes a line whose last part is `tail`: straight from the chunk when nothing was carried, else through the
    /// carry buffer, which is put back empty so its capacity is kept.
    fn line(
        &mut self,
        tail: &[u8],
        on_event: &mut dyn FnMut(SseEvent<'_>) -> Flow,
    ) -> Result<Flow, FrameError> {
        if self.carry.is_empty() {
            if tail.len() > self.limits.line {
                return Err(FrameError::LineTooLong { limit: self.limits.line });
            }
            return self.process(tail, on_event);
        }
        if self.carry.len() + tail.len() > self.limits.line {
            return Err(FrameError::LineTooLong { limit: self.limits.line });
        }
        self.carry.extend_from_slice(tail);
        let line = std::mem::take(&mut self.carry);
        let result = self.process(&line, on_event);
        self.carry = line;
        self.carry.clear();
        result
    }

    /// One complete line, by the rules of the standard.
    fn process(
        &mut self,
        line: &[u8],
        on_event: &mut dyn FnMut(SseEvent<'_>) -> Flow,
    ) -> Result<Flow, FrameError> {
        self.first_line = false;
        if line.is_empty() {
            return self.dispatch(on_event);
        }
        if line[0] == b':' {
            return Ok(Flow::Continue);
        }
        let (field, value) = match line.iter().position(|&b| b == b':') {
            Some(colon) => {
                let value = &line[colon + 1..];
                (&line[..colon], value.strip_prefix(b" ").unwrap_or(value))
            }
            None => (line, &line[line.len()..]),
        };
        match field {
            b"data" => {
                if self.data.len() + self.name.len() + value.len() + 1 > self.limits.event {
                    return Err(FrameError::EventTooLarge { limit: self.limits.event });
                }
                self.data.extend_from_slice(value);
                self.data.push(b'\n');
                self.data_seen = true;
            }
            b"event" => {
                if self.data.len() + value.len() > self.limits.event {
                    return Err(FrameError::EventTooLarge { limit: self.limits.event });
                }
                self.name.clear();
                self.name.extend_from_slice(value);
            }
            // `id`, `retry` and every other field are ignored without being read: the client never reconnects, so
            // there is nothing to feed them to.
            _ => {}
        }
        Ok(Flow::Continue)
    }

    /// A blank line: hands the pending event to the callback, or drops a name that has no data.
    fn dispatch(&mut self, on_event: &mut dyn FnMut(SseEvent<'_>) -> Flow) -> Result<Flow, FrameError> {
        if !self.data_seen {
            self.name.clear();
            return Ok(Flow::Continue);
        }
        // `data_seen` means the buffer holds at least the line feed of one data line.
        let data = std::str::from_utf8(&self.data[..self.data.len() - 1]).map_err(|_| FrameError::InvalidUtf8)?;
        let name = if self.name.is_empty() {
            "message"
        } else {
            std::str::from_utf8(&self.name).map_err(|_| FrameError::InvalidUtf8)?
        };
        let flow = on_event(SseEvent::new(name, data));
        self.data.clear();
        self.name.clear();
        self.data_seen = false;
        Ok(flow)
    }
}

impl fmt::Debug for SseDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SseDecoder")
            .field("state", &self.state.name())
            .field("buffered", &Size(self.buffered()))
            .field("limits", &self.limits)
            .finish()
    }
}
