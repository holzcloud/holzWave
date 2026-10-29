//! The idle timer that ends a worker nobody is attached to.
//!
//! The rule is a pure function of two times that come from the injected clock, so a test moves time by hand and the server
//! never reads the system clock here. A worker is idle from the moment its last socket left (or from the moment it became
//! ready, when it was started without a socket) and ends when it has been idle for the whole timeout: 86,399 seconds of a
//! 86,400 second timeout keep it, 86,400 end it.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::supervisor::registry::Supervisor;

/// Whether a worker whose last socket left at `idle_since` (unix seconds) has been idle for `timeout` at `now`. A clock that
/// went back, or times too far apart to subtract, never end a worker.
pub fn expired(idle_since: i64, now: i64, timeout: Duration) -> bool {
    let timeout = i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX);
    now.checked_sub(idle_since).is_some_and(|passed| passed >= timeout)
}

/// Looks for idle workers every `interval` until `stop` is cancelled.
pub async fn run(supervisor: Supervisor, interval: Duration, stop: CancellationToken) {
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(interval) => {}
        }
        supervisor.sweep().await;
    }
}
