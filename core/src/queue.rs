//! The bounded per-pane queue between the PTY reader thread and the engine thread: a fast producer is slowed down
//! instead of growing memory without limit.
//!
//! Skeleton: the behaviour arrives with the implementation commit.

use std::marker::PhantomData;
use std::time::Duration;

/// How many bytes of output may be queued before the producer has to wait.
pub const OUTPUT_LIMIT: usize = 1 << 20;

/// What the consumer got from [`PaneQueue::pop`].
#[derive(Debug, PartialEq, Eq)]
pub enum Popped<C> {
    Control(C),
    Output(Vec<u8>),
    Finished,
    Timeout,
    Closed,
}

pub struct PaneQueue<C> {
    _control: PhantomData<fn() -> C>,
}

impl<C> PaneQueue<C> {
    pub fn new() -> Self {
        Self::with_limit(OUTPUT_LIMIT)
    }

    pub fn with_limit(_bytes: usize) -> Self {
        Self { _control: PhantomData }
    }

    pub fn push_control(&self, _control: C) -> bool {
        false
    }

    pub fn push_output(&self, _chunk: Vec<u8>) -> bool {
        false
    }

    pub fn pop(&self, _timeout: Duration) -> Popped<C> {
        Popped::Timeout
    }

    pub fn finish(&self) {}

    pub fn close(&self) {}

    pub fn high_water(&self) -> usize {
        0
    }

    pub fn queued_bytes(&self) -> usize {
        0
    }
}

impl<C> Default for PaneQueue<C> {
    fn default() -> Self {
        Self::new()
    }
}
