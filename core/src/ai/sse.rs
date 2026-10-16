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
//! The one rule that must exist exactly once lives here too: **a tool call from a stream that did not end cleanly is
//! discarded**. [`ToolAssembler`] collects the fragments a dialect sends, parses the arguments only when a call is
//! complete, and releases the calls only after the stream ended cleanly with its terminal event.
//!
//! The only dialect word in the file is the `[DONE]` data line behind [`SseEvent::is_done`].

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use super::provider::{AiError, Detail, Flow};
use super::types::{Size, ToolCall, json_size};

/// The UTF-8 byte order mark, dropped at the start of an event stream.
const BYTE_ORDER_MARK: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// The longest line, without its terminator: 1 MiB.
pub const MAX_LINE: usize = 1_048_576;
/// The most an event may hold while it is pending: 4 MiB.
pub const MAX_EVENT: usize = 4_194_304;
/// The most argument text one turn may accumulate for its tool calls: 4 MiB.
pub const MAX_TOOL_ARGS: usize = 4_194_304;
/// The most tool calls one turn may make.
pub const MAX_TOOL_CALLS: usize = 64;
/// The longest id or name of a tool call, in bytes.
pub const MAX_TOOL_FIELD: usize = 256;

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

impl State {
    /// The answer a push gives without looking at its bytes, when the decoder is no longer running.
    fn gate(self) -> Option<Result<Flow, FrameError>> {
        match self {
            State::Running => None,
            State::Stopped => Some(Ok(Flow::Stop)),
            State::Failed { error, .. } => Some(Err(error)),
        }
    }
}

/// The state a decoder moves to after a step that did not simply continue; `pending` is how much it was holding.
fn settled(result: &Result<Flow, FrameError>, pending: usize) -> Option<State> {
    match result {
        Ok(Flow::Continue) => None,
        Ok(Flow::Stop) => Some(State::Stopped),
        Err(error) => Some(State::Failed { error: *error, dropped: pending }),
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
    /// The first line has not been processed yet; it may start with a byte order mark.
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
        if let Some(answer) = self.state.gate() {
            return answer;
        }
        let result = self.scan(chunk, on_event);
        if let Some(state) = settled(&result, self.buffered()) {
            self.release();
            self.state = state;
        }
        result
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
        // A line feed that starts the chunk belongs to the carriage return that ended the last one. An empty chunk
        // says nothing, so the flag waits for the first byte that does.
        if self.skip_lf && !rest.is_empty() {
            self.skip_lf = false;
            if rest[0] == b'\n' {
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
        let mut line = line;
        if self.first_line {
            // The standard drops one byte order mark at the start of the stream, and only there.
            self.first_line = false;
            line = line.strip_prefix(&BYTE_ORDER_MARK).unwrap_or(line);
        }
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

// ---- the newline-delimited JSON decoder -----------------------------------------------------------------

/// A decoder for a stream of records that a line feed ends, the framing of the Ollama native stream.
///
/// A record is delivered without its line feed and without one carriage return in front of it; empty records are
/// skipped. The bytes are not validated as UTF-8: the JSON parser of the adapter does that. A record is delivered only
/// with its line feed, so a last record without one is a cut stream and is reported by [`NdjsonDecoder::finish`]. A lone
/// carriage return does not end a record, because a raw one cannot occur inside a JSON string.
///
/// The bound is on the record. Like [`SseDecoder`] it stays failed after its first error, and it does not scan a byte
/// twice.
pub struct NdjsonDecoder {
    limit: usize,
    carry: Vec<u8>,
    state: State,
}

impl Default for NdjsonDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl NdjsonDecoder {
    /// A decoder with a record bound of [`MAX_EVENT`].
    pub fn new() -> Self {
        Self::with_limit(MAX_EVENT)
    }

    /// A decoder that accepts records of at most `limit` bytes.
    pub fn with_limit(limit: usize) -> Self {
        Self { limit, carry: Vec::new(), state: State::Running }
    }

    /// Takes one chunk and calls `on_line` for every record it completes, in order. The record is borrowed for the
    /// call. An `Err` is final; after `Ok(Flow::Stop)` later pushes deliver nothing and return `Ok(Flow::Stop)`.
    pub fn push(
        &mut self,
        chunk: &[u8],
        on_line: &mut dyn FnMut(&[u8]) -> Flow,
    ) -> Result<Flow, FrameError> {
        if let Some(answer) = self.state.gate() {
            return answer;
        }
        let result = self.scan(chunk, on_line);
        if let Some(state) = settled(&result, self.buffered()) {
            self.carry = Vec::new();
            self.state = state;
        }
        result
    }

    /// The verdict at the end of the stream. A partial record is never delivered.
    #[must_use]
    pub fn finish(self) -> StreamEnd {
        verdict(self.state, self.buffered())
    }

    /// The bytes held for a record that is not finished.
    pub fn buffered(&self) -> usize {
        self.carry.len()
    }

    /// The bytes the decoder has reserved.
    pub fn capacity(&self) -> usize {
        self.carry.capacity()
    }

    fn scan(&mut self, chunk: &[u8], on_line: &mut dyn FnMut(&[u8]) -> Flow) -> Result<Flow, FrameError> {
        let mut rest = chunk;
        while !rest.is_empty() {
            let Some(end) = rest.iter().position(|&b| b == b'\n') else {
                // No line feed yet. The pending bytes may be a record of `limit` bytes and the carriage return of its
                // CRLF, so one byte more is allowed when it is a carriage return.
                let total = self.carry.len() + rest.len();
                let ends_in_cr = rest.last() == Some(&b'\r');
                if total > self.limit && !(total == self.limit.saturating_add(1) && ends_in_cr) {
                    return Err(FrameError::LineTooLong { limit: self.limit });
                }
                self.carry.extend_from_slice(rest);
                break;
            };
            let tail = &rest[..end];
            rest = &rest[end + 1..];
            if self.record(tail, on_line)? == Flow::Stop {
                return Ok(Flow::Stop);
            }
        }
        Ok(Flow::Continue)
    }

    /// Finishes a record whose last part is `tail`: straight from the chunk when nothing was carried, else through the
    /// carry buffer, which is put back empty so its capacity is kept.
    fn record(&mut self, tail: &[u8], on_line: &mut dyn FnMut(&[u8]) -> Flow) -> Result<Flow, FrameError> {
        if self.carry.is_empty() {
            return self.emit(tail, on_line);
        }
        let ends_in_cr = tail.last().or(self.carry.last()) == Some(&b'\r');
        if self.carry.len() + tail.len() - usize::from(ends_in_cr) > self.limit {
            return Err(FrameError::LineTooLong { limit: self.limit });
        }
        self.carry.extend_from_slice(tail);
        let line = std::mem::take(&mut self.carry);
        let result = self.emit(&line, on_line);
        self.carry = line;
        self.carry.clear();
        result
    }

    /// One complete record, with the carriage return of a CRLF still on it.
    fn emit(&mut self, line: &[u8], on_line: &mut dyn FnMut(&[u8]) -> Flow) -> Result<Flow, FrameError> {
        let record = line.strip_suffix(b"\r").unwrap_or(line);
        if record.len() > self.limit {
            return Err(FrameError::LineTooLong { limit: self.limit });
        }
        if record.is_empty() {
            return Ok(Flow::Continue);
        }
        Ok(on_line(record))
    }
}

impl fmt::Debug for NdjsonDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NdjsonDecoder")
            .field("state", &self.state.name())
            .field("buffered", &Size(self.buffered()))
            .field("limit", &self.limit)
            .finish()
    }
}

// ---- tool fragments -------------------------------------------------------------------------------------
//
// Tool calls are assembled here, next to the framing, so that the rule "a call from a stream that did not end cleanly
// is discarded" exists once and every wire adapter gets it by using this type.

/// Why a tool fragment was refused. Every fault ends the turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolFault {
    /// More calls than [`MAX_TOOL_CALLS`] (or than the assembler's limit).
    TooMany,
    /// An id or name over [`MAX_TOOL_FIELD`] bytes, or more argument text than the limit.
    TooLarge,
    /// A missing name, or a control character in an id or a name.
    BadField,
    /// A slot changed its id or name, or two calls claim the same id.
    Conflict,
    /// A fragment or a completion for a slot that is already complete.
    ClosedSlot,
    /// A completion for a slot that was never opened.
    UnknownSlot,
    /// The arguments are not one JSON object.
    InvalidArgs,
}

impl fmt::Display for ToolFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ToolFault::TooMany => "the model made more tool calls than one turn allows",
            ToolFault::TooLarge => "a tool call is larger than one turn allows",
            ToolFault::BadField => "a tool call has a missing name or an id or name that is not allowed",
            ToolFault::Conflict => "a tool call changed its id or name, or two tool calls share an id",
            ToolFault::ClosedSlot => "a tool call was changed after it was complete",
            ToolFault::UnknownSlot => "a tool call was completed that was never started",
            ToolFault::InvalidArgs => "the arguments of a tool call are not one valid JSON object",
        })
    }
}

impl std::error::Error for ToolFault {}

impl From<ToolFault> for AiError {
    fn from(fault: ToolFault) -> Self {
        AiError::Protocol(Detail::new(&fault.to_string()))
    }
}

/// Tool calls that were dropped because the stream did not end cleanly. The turn failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Discarded {
    /// How many calls (open or complete) were dropped.
    pub dropped: usize,
}

impl fmt::Display for Discarded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the stream did not end cleanly, so its tool calls were discarded ({})", self.dropped)
    }
}

impl std::error::Error for Discarded {}

impl From<Discarded> for AiError {
    fn from(discarded: Discarded) -> Self {
        AiError::Protocol(Detail::new(&discarded.to_string()))
    }
}

/// A call that has started and is not complete.
struct OpenCall {
    slot: u32,
    id: Option<String>,
    name: Option<String>,
    args: String,
}

/// Collects the tool calls of one turn.
///
/// A slot is the dialect's own index (a content block index, a `tool_calls` index, an output index). An adapter feeds
/// the fragments it reads, completes a slot at the dialect's end-of-call marker, and at the end of the stream calls
/// [`ToolAssembler::finish`] with the verdict of the decoder and whether the dialect's terminal event was seen.
///
/// The rules for adapters:
///
/// - An empty id or name counts as absent. A fault is a protocol error and ends the turn.
/// - Deliver [`StreamEvent::ToolCall`](super::provider::StreamEvent::ToolCall) only from the vector `finish` returns,
///   after the dialect's terminal event. `Err(Discarded)` is a failed turn, never `Ok`.
///
/// Everything here is bounded, because the stream is untrusted: the argument text in total, the number of calls and the
/// size of an id or a name. Arguments are parsed only when a call is completed, so half a call is never parsed.
/// `Debug` prints counts and sizes, never ids, names or arguments.
pub struct ToolAssembler {
    /// Calls that have started, in the order opened.
    open: Vec<OpenCall>,
    /// Calls that are complete, in the order completed.
    ready: Vec<ToolCall>,
    /// The slots already completed.
    closed: Vec<u32>,
    /// The argument bytes accepted so far.
    args_total: usize,
    max_args: usize,
    max_calls: usize,
    /// Makes minted ids unique across turns and restarts. It is not a secret.
    nonce: u64,
    /// How many ids were minted.
    minted: u64,
    /// A fault happened, so nothing is released.
    faulted: bool,
}

impl Default for ToolAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolAssembler {
    /// An assembler bounded by [`MAX_TOOL_ARGS`] and [`MAX_TOOL_CALLS`].
    pub fn new() -> Self {
        Self::with_limits(MAX_TOOL_ARGS, MAX_TOOL_CALLS)
    }

    /// An assembler that accepts at most `max_args` bytes of argument text and `max_calls` calls.
    pub fn with_limits(max_args: usize, max_calls: usize) -> Self {
        Self {
            open: Vec::new(),
            ready: Vec::new(),
            closed: Vec::new(),
            args_total: 0,
            max_args,
            max_calls,
            nonce: fresh_nonce(),
            minted: 0,
            faulted: false,
        }
    }

    /// A piece of a call: its id and name when the dialect sends them, and a piece of the argument text. The first
    /// fragment of a slot opens it.
    pub fn fragment(
        &mut self,
        slot: u32,
        id: Option<&str>,
        name: Option<&str>,
        args: &str,
    ) -> Result<(), ToolFault> {
        let result = self.add_fragment(slot, id, name, args);
        self.faulted |= result.is_err();
        result
    }

    /// The call in `slot` is complete: its arguments are parsed now.
    pub fn complete(&mut self, slot: u32) -> Result<(), ToolFault> {
        let result = match self.open.iter().position(|call| call.slot == slot) {
            Some(index) => self.complete_at(index),
            None if self.closed.contains(&slot) => Err(ToolFault::ClosedSlot),
            None => Err(ToolFault::UnknownSlot),
        };
        self.faulted |= result.is_err();
        result
    }

    /// Completes every open slot, in the order opened, and stops at the first fault.
    pub fn complete_all(&mut self) -> Result<(), ToolFault> {
        while !self.open.is_empty() {
            let result = self.complete_at(0);
            if result.is_err() {
                self.faulted = true;
                return result;
            }
        }
        Ok(())
    }

    /// A call that arrives in one piece, with parsed arguments.
    pub fn whole(&mut self, id: Option<&str>, name: &str, args: Value) -> Result<(), ToolFault> {
        let result = self.add_whole(id, name, args);
        self.faulted |= result.is_err();
        result
    }

    /// How many calls have started and are not complete.
    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    /// How many calls are complete and held.
    pub fn ready_count(&self) -> usize {
        self.ready.len()
    }

    /// The end of the turn. The calls are released, complete and parsed, in the order completed, only when the decoder
    /// reported a clean end, the dialect's terminal event was seen, no call is still open and no fault happened. In
    /// every other case they are dropped and the turn has failed.
    pub fn finish(self, end: StreamEnd, terminal_seen: bool) -> Result<Vec<ToolCall>, Discarded> {
        if end.is_clean() && terminal_seen && self.open.is_empty() && !self.faulted {
            Ok(self.ready)
        } else {
            Err(Discarded { dropped: self.open.len() + self.ready.len() })
        }
    }

    fn add_fragment(&mut self, slot: u32, id: Option<&str>, name: Option<&str>, args: &str) -> Result<(), ToolFault> {
        if self.closed.contains(&slot) {
            return Err(ToolFault::ClosedSlot);
        }
        let id = id.filter(|text| !text.is_empty());
        let name = name.filter(|text| !text.is_empty());
        check_field(id)?;
        check_field(name)?;
        if self.args_total.saturating_add(args.len()) > self.max_args {
            return Err(ToolFault::TooLarge);
        }
        let index = match self.open.iter().position(|call| call.slot == slot) {
            Some(index) => index,
            None => {
                if self.open.len() + self.ready.len() >= self.max_calls {
                    return Err(ToolFault::TooMany);
                }
                self.open.push(OpenCall { slot, id: None, name: None, args: String::new() });
                self.open.len() - 1
            }
        };
        if let Some(id) = id {
            let first = match self.open[index].id.as_deref() {
                Some(known) if known != id => return Err(ToolFault::Conflict),
                Some(_) => false,
                None => true,
            };
            if first {
                if self.id_taken(id) {
                    return Err(ToolFault::Conflict);
                }
                self.open[index].id = Some(id.to_owned());
            }
        }
        if let Some(name) = name {
            match self.open[index].name.as_deref() {
                Some(known) if known != name => return Err(ToolFault::Conflict),
                Some(_) => {}
                None => self.open[index].name = Some(name.to_owned()),
            }
        }
        self.open[index].args.push_str(args);
        self.args_total += args.len();
        Ok(())
    }

    fn complete_at(&mut self, index: usize) -> Result<(), ToolFault> {
        let call = &self.open[index];
        if call.name.is_none() {
            return Err(ToolFault::BadField);
        }
        let args = parse_args(&call.args)?;
        let OpenCall { slot, id, name, .. } = self.open.remove(index);
        let id = match id {
            Some(id) => id,
            None => self.mint(),
        };
        self.closed.push(slot);
        self.ready.push(ToolCall { id, name: name.unwrap_or_default(), args });
        Ok(())
    }

    fn add_whole(&mut self, id: Option<&str>, name: &str, args: Value) -> Result<(), ToolFault> {
        let id = id.filter(|text| !text.is_empty());
        if name.is_empty() {
            return Err(ToolFault::BadField);
        }
        check_field(id)?;
        check_field(Some(name))?;
        if !args.is_object() {
            return Err(ToolFault::InvalidArgs);
        }
        if self.open.len() + self.ready.len() >= self.max_calls {
            return Err(ToolFault::TooMany);
        }
        let size = json_size(&args).0;
        if self.args_total.saturating_add(size) > self.max_args {
            return Err(ToolFault::TooLarge);
        }
        if id.is_some_and(|id| self.id_taken(id)) {
            return Err(ToolFault::Conflict);
        }
        let id = match id {
            Some(id) => id.to_owned(),
            None => self.mint(),
        };
        self.args_total += size;
        self.ready.push(ToolCall { id, name: name.to_owned(), args });
        Ok(())
    }

    /// Whether a call of this turn already has the provider id `id`.
    fn id_taken(&self, id: &str) -> bool {
        self.open.iter().any(|call| call.id.as_deref() == Some(id)) || self.ready.iter().any(|call| call.id == id)
    }

    /// An id for a call the provider did not name: `call_`, the nonce in sixteen hex digits, an underscore, a counter.
    fn mint(&mut self) -> String {
        let id = format!("call_{:016x}_{}", self.nonce, self.minted);
        self.minted += 1;
        id
    }
}

impl fmt::Debug for ToolAssembler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolAssembler")
            .field("open", &self.open.len())
            .field("ready", &self.ready.len())
            .field("args", &Size(self.args_total))
            .field("faulted", &self.faulted)
            .finish()
    }
}

/// An id or a name must be short and free of control characters, which could rewrite a terminal or split a log line.
fn check_field(value: Option<&str>) -> Result<(), ToolFault> {
    match value {
        Some(text) if text.len() > MAX_TOOL_FIELD => Err(ToolFault::TooLarge),
        Some(text) if text.chars().any(char::is_control) => Err(ToolFault::BadField),
        _ => Ok(()),
    }
}

/// The arguments of a call: white space around them is dropped, nothing becomes an empty object, and anything else
/// must be one JSON object.
fn parse_args(text: &str) -> Result<Value, ToolFault> {
    let text = text.trim_matches(|c: char| c.is_ascii_whitespace());
    if text.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    match serde_json::from_str::<Value>(text) {
        Ok(value @ Value::Object(_)) => Ok(value),
        _ => Err(ToolFault::InvalidArgs),
    }
}

/// The nonce of an assembler: random, so ids are unique across turns and restarts. If the system has no random numbers
/// the clock and a process-wide counter stand in; the nonce only has to make ids unique, it is not a secret.
fn fresh_nonce() -> u64 {
    let mut raw = [0u8; 8];
    if getrandom::fill(&mut raw).is_ok() {
        return u64::from_le_bytes(raw);
    }
    static FALLBACK: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_nanos() as u64);
    nanos.rotate_left(17) ^ FALLBACK.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
}
