//! What a pane tells the window when a program wants attention: the bell (docs/design.md section 6.23).
//!
//! A program can ring the bell as fast as it can write, so the pane lets one event through per [`GAP`]. The gate is a
//! UI hint, never a decision about security or content: a bell that is held back is simply not shown again.

use std::time::{Duration, Instant};

/// The least time between two attention events of one kind from one pane.
pub const GAP: Duration = Duration::from_millis(500);

/// Lets one call through per [`GAP`]. The reference time is the last call that passed, not the last attempt, so a
/// steady flood still gets two per second and cannot keep the gate closed by trying.
#[derive(Clone, Copy, Debug, Default)]
pub struct Gate {
    last_pass: Option<Instant>,
}

impl Gate {
    /// A gate that lets the first call through.
    pub const fn new() -> Self {
        Self { last_pass: None }
    }

    /// Whether the event at `now` goes through. An instant earlier than the last pass does not (and never panics).
    pub fn pass(&mut self, now: Instant) -> bool {
        let open = match self.last_pass {
            None => true,
            Some(last) => now.checked_duration_since(last).is_some_and(|since| since >= GAP),
        };
        if open {
            self.last_pass = Some(now);
        }
        open
    }
}
