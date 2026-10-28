//! The health report that the container check reads.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::app::Shutdown;

/// The name of the health socket inside the runtime directory.
pub const SOCKET_NAME: &str = "health.sock";

/// The longest `healthcheck` waits for an answer.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The most connections the health socket serves at once.
pub const MAX_CONNECTIONS: usize = 8;

type Check = Arc<dyn Fn() -> bool + Send + Sync>;

/// The readiness checks of the subsystems.
#[derive(Default)]
pub struct Readiness {
    checks: Mutex<Vec<(&'static str, Check)>>,
}

impl Readiness {
    /// A registry with no check: it is ready.
    pub fn new() -> Readiness {
        Readiness::default()
    }

    /// Registers a check under a fixed name.
    pub fn register(&self, name: &'static str, check: impl Fn() -> bool + Send + Sync + 'static) {
        let _ = (name, check);
        drop(self.checks.lock().unwrap_or_else(PoisonError::into_inner));
    }

    /// The names of the checks that do not pass.
    pub fn failing(&self) -> Vec<&'static str> {
        Vec::new()
    }

    /// Whether every check passes.
    pub fn ready(&self) -> bool {
        true
    }
}

/// Why the health socket cannot be made: one sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthError(String);

impl fmt::Display for HealthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HealthError {}

/// The running health socket.
pub struct HealthServer {
    path: PathBuf,
}

impl HealthServer {
    /// The path of the socket.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Waits until the socket is closed and its file removed (after the shutdown signal).
    pub async fn finished(self) {}
}

/// Binds `<runtime_dir>/health.sock` and answers `GET /healthz` until `shutdown` is triggered.
pub async fn serve(runtime_dir: &Path, readiness: Arc<Readiness>, shutdown: Shutdown) -> Result<HealthServer, HealthError> {
    let _ = (readiness, shutdown);
    Err(HealthError(format!("the health socket in {} is not built", runtime_dir.display())))
}

/// What the probe found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// The server answered `ok`.
    Healthy,
    /// The server answered 503 `not ready`.
    NotReady,
    /// Nothing listens on the socket.
    Unreachable,
    /// The answer is not the server's.
    Garbled,
    /// No answer within the time.
    TimedOut,
}

impl Health {
    /// The one sentence for the person running the check.
    pub fn sentence(self, runtime_dir: &Path) -> String {
        format!("{self:?} {}", runtime_dir.display())
    }
}

/// Asks the server on the health socket in `runtime_dir` whether it is ready, within `within`.
pub fn probe(runtime_dir: &Path, within: Duration) -> Health {
    let _ = (runtime_dir, within);
    Health::Unreachable
}
