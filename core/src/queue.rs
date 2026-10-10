//! The bounded per-pane queue between the PTY reader thread and the engine thread: a fast producer is slowed down
//! instead of growing memory without limit.
//!
//! Two kinds of messages travel through it:
//!
//! - **Output**: chunks of bytes the PTY reader thread read from the child. They are bounded by *bytes*, not by
//!   message count: while the queued output is at or above the limit, [`PaneQueue::push_output`] blocks. The reader
//!   then stops reading, the kernel's PTY buffer fills and the child blocks in its own `write`. That is the
//!   intended backpressure; the queue never grows past the limit plus one chunk.
//! - **Control**: small messages from the shell (keys, resize, close). They are unbounded and never block, and
//!   [`PaneQueue::pop`] hands them out before any output, so input is never stuck behind a flood.
//!
//! The end of the output is part of the queue's state and not a control message, because control messages are
//! delivered first: [`PaneQueue::finish`] must stay ordered after every chunk that is already queued.
//!
//! The queue is generic over the control type, so it knows nothing about engines or keys. It uses a mutex and two
//! condition variables from the standard library; a plain `sync_channel` would block the thread that sends control
//! messages (a Tauri command on the main thread) and could not give input priority over output.
//!
//! A panic on one side must not take the other side down with it, so a poisoned lock is recovered: the queue's
//! state is only ever changed by short, non-panicking sections.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How many bytes of output may be queued before the producer has to wait.
pub const OUTPUT_LIMIT: usize = 1 << 20;

/// What the consumer got from [`PaneQueue::pop`].
#[derive(Debug, PartialEq, Eq)]
pub enum Popped<C> {
    /// A control message. Control is always handed out before output.
    Control(C),
    /// One chunk of output, in the order it was pushed.
    Output(Vec<u8>),
    /// The producer called [`PaneQueue::finish`] and everything it queued has been delivered. Repeats.
    Finished,
    /// Nothing arrived within the timeout.
    Timeout,
    /// The queue was closed and everything that remained has been delivered. Repeats.
    Closed,
}

struct Inner<C> {
    control: VecDeque<C>,
    output: VecDeque<Vec<u8>>,
    output_bytes: usize,
    high_water: usize,
    finished: bool,
    closed: bool,
}

pub struct PaneQueue<C> {
    inner: Mutex<Inner<C>>,
    /// Signalled when something the consumer waits for happened: a message, `finish` or `close`.
    consumer: Condvar,
    /// Signalled when room was made for the producer, or when the queue was closed.
    producer: Condvar,
    limit: usize,
}

impl<C> PaneQueue<C> {
    /// A queue with the default output limit of [`OUTPUT_LIMIT`] bytes.
    pub fn new() -> Self {
        Self::with_limit(OUTPUT_LIMIT)
    }

    /// A queue that blocks the producer while `bytes` or more bytes of output are queued. A limit of zero would
    /// block the producer forever, so it counts as one.
    pub fn with_limit(bytes: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                control: VecDeque::new(),
                output: VecDeque::new(),
                output_bytes: 0,
                high_water: 0,
                finished: false,
                closed: false,
            }),
            consumer: Condvar::new(),
            producer: Condvar::new(),
            limit: bytes.max(1),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner<C>> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Queue a control message. Never blocks. Returns false when the queue was closed, which means the pane is
    /// gone. Control is still accepted after [`finish`](Self::finish): the shell may type into, or close, a pane
    /// whose program has already exited.
    pub fn push_control(&self, control: C) -> bool {
        let mut inner = self.lock();
        if inner.closed {
            return false;
        }
        inner.control.push_back(control);
        self.consumer.notify_one();
        true
    }

    /// Queue a chunk of output. Called by the PTY reader thread.
    ///
    /// Blocks while the queued output is at or above the limit and returns when the consumer made room. Returns
    /// false when the queue was closed (the pane is gone) or finished (the reader has no more output to push).
    /// An empty chunk is accepted and not queued.
    pub fn push_output(&self, chunk: Vec<u8>) -> bool {
        let mut inner = self.lock();
        while inner.output_bytes >= self.limit && !inner.closed && !inner.finished {
            inner = self.producer.wait(inner).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        if inner.closed || inner.finished {
            return false;
        }
        if chunk.is_empty() {
            return true;
        }
        inner.output_bytes += chunk.len();
        inner.high_water = inner.high_water.max(inner.output_bytes);
        inner.output.push_back(chunk);
        self.consumer.notify_one();
        true
    }

    /// Take the next message, waiting up to `timeout`.
    ///
    /// The order is: control messages (first in, first out), then output chunks (first in, first out), then
    /// [`Popped::Closed`] if the queue was closed, then [`Popped::Finished`] if the producer finished, otherwise
    /// the call waits and returns [`Popped::Timeout`] when nothing arrived.
    pub fn pop(&self, timeout: Duration) -> Popped<C> {
        // A timeout too large to add to the clock simply waits without a deadline.
        let deadline = Instant::now().checked_add(timeout);
        let mut inner = self.lock();
        loop {
            if let Some(control) = inner.control.pop_front() {
                return Popped::Control(control);
            }
            if let Some(chunk) = inner.output.pop_front() {
                inner.output_bytes -= chunk.len();
                self.producer.notify_one();
                return Popped::Output(chunk);
            }
            if inner.closed {
                return Popped::Closed;
            }
            if inner.finished {
                return Popped::Finished;
            }
            match deadline {
                None => {
                    inner = self.consumer.wait(inner).unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Popped::Timeout;
                    }
                    inner = self
                        .consumer
                        .wait_timeout(inner, left)
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .0;
                }
            }
        }
    }

    /// The producer reached the end of the output (EOF, or a read error): no more output will come.
    ///
    /// Called once by the reader thread. Nothing that is queued is discarded: `pop` still returns every chunk, in
    /// order, and control messages keep being delivered before the next chunk. Only after the last chunk does it
    /// return [`Popped::Finished`].
    pub fn finish(&self) {
        let mut inner = self.lock();
        inner.finished = true;
        self.consumer.notify_all();
        self.producer.notify_all();
    }

    /// Shut the queue down from the consumer side: pushes return false from now on and a blocked producer is
    /// released. What is already queued is still delivered by `pop`, then it returns [`Popped::Closed`].
    pub fn close(&self) {
        let mut inner = self.lock();
        inner.closed = true;
        self.consumer.notify_all();
        self.producer.notify_all();
    }

    /// The most output bytes that were ever queued at once.
    pub fn high_water(&self) -> usize {
        self.lock().high_water
    }

    /// The output bytes queued right now.
    pub fn queued_bytes(&self) -> usize {
        self.lock().output_bytes
    }
}

impl<C> Default for PaneQueue<C> {
    fn default() -> Self {
        Self::new()
    }
}
