//! The delta merger: many small pieces of streamed text become about sixty events a second.
//!
//! A provider can send a delta for every token, hundreds a second. Each one costs the page an event, a state change and
//! a repaint, and the page needs no more than a screen refresh. [`Coalescer`] merges adjacent deltas of one kind and
//! hands a batch out at most once per interval, with these rules:
//!
//! - the first text after a quiet time goes out at once (the leading edge), so the first word is never late;
//! - text that arrives within the interval of the last delivery is buffered, and [`Coalescer::due_at`] says when the
//!   buffer must go out; a helper thread calls [`Coalescer::take`] then, so buffered text never waits for the next
//!   piece to arrive;
//! - a change of kind (answer to reasoning or back) hands the buffered text out first, so the order of the stream is
//!   kept; the bytes are never changed, reordered or dropped;
//! - a buffer that reaches the byte bound goes out whole.
//!
//! At most one delivery happens per interval, plus one batch for each change of kind. The type reads no clock: the
//! caller passes the time of each call, which makes the rules testable with synthetic instants.

use std::time::{Duration, Instant};

/// What a delta says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeltaKind {
    /// A piece of the answer.
    Text,
    /// A piece of reasoning.
    Reasoning,
}

/// Merged text of one kind, ready to be delivered as one event.
#[derive(Clone, PartialEq, Eq)]
pub struct Batch {
    /// What the text is.
    pub kind: DeltaKind,
    /// The text, in the order it was pushed.
    pub text: String,
}

impl std::fmt::Debug for Batch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Batch").field("kind", &self.kind).field("bytes", &self.text.len()).finish()
    }
}

/// Merges deltas. One value serves one turn.
#[derive(Debug)]
pub struct Coalescer {
    interval: Duration,
    max_bytes: usize,
    kind: DeltaKind,
    buffer: String,
    last: Option<Instant>,
}

impl Coalescer {
    /// A merger that delivers at most once per `interval` and whose buffer never grows past `max_bytes` (a single push
    /// larger than that goes out whole).
    pub fn new(interval: Duration, max_bytes: usize) -> Coalescer {
        Coalescer { interval, max_bytes: max_bytes.max(1), kind: DeltaKind::Text, buffer: String::new(), last: None }
    }

    /// Takes a piece of text at time `now` and returns the batches to deliver now: none, one, or two (the buffered text
    /// of the other kind, then this one).
    pub fn push(&mut self, kind: DeltaKind, text: &str, now: Instant) -> Vec<Batch> {
        let mut out = Vec::new();
        if text.is_empty() {
            return out;
        }
        if !self.buffer.is_empty() && self.kind != kind {
            out.extend(self.flush(now));
        }
        self.kind = kind;
        self.buffer.push_str(text);
        let quiet = self.last.is_none_or(|last| now.saturating_duration_since(last) >= self.interval);
        if quiet || self.buffer.len() >= self.max_bytes {
            out.extend(self.flush(now));
        }
        out
    }

    /// When the buffered text must go out: the last delivery plus the interval. `None` while nothing is buffered.
    pub fn due_at(&self) -> Option<Instant> {
        if self.buffer.is_empty() { None } else { self.last.map(|last| last + self.interval) }
    }

    /// Delivers whatever is buffered, whether it is due or not. A turn calls this before any other event, so no delta
    /// can come after the event that supersedes it.
    pub fn take(&mut self, now: Instant) -> Option<Batch> {
        self.flush(now)
    }

    /// Whether nothing is buffered.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    fn flush(&mut self, now: Instant) -> Option<Batch> {
        if self.buffer.is_empty() {
            return None;
        }
        self.last = Some(now);
        Some(Batch { kind: self.kind, text: std::mem::take(&mut self.buffer) })
    }
}
