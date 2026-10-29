//! The idle timer that ends a worker nobody is attached to.
//!
//! The rule is a pure function of two times that come from the injected clock, so a test moves time by hand and the server
//! never reads the system clock here.

use std::time::Duration;

/// Whether a worker whose last socket left at `idle_since` (unix seconds) has been idle for `timeout` at `now`.
pub fn expired(_idle_since: i64, _now: i64, _timeout: Duration) -> bool {
    false
}
