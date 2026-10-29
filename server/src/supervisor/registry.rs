//! The table of running workers and the people they belong to.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use crate::clock::Clock;
use crate::limits;
use crate::store::AccountId;
use crate::supervisor::launcher::WorkerLauncher;
use crate::supervisor::socket::{SocketHandle, SocketRx};

/// A person, as the supervisor knows them: the account and the operating-system user it runs as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AccountKey {
    /// The account.
    pub id: AccountId,
    /// The user id the person's worker runs as.
    pub uid: u32,
}

/// The sizes and times the supervisor works with.
#[derive(Clone, Debug)]
pub struct SupervisorSettings {
    /// The most panes of one person, sent to the worker.
    pub max_panes: u32,
    /// The most scrollback lines of a pane, sent to the worker.
    pub max_scrollback_lines: u32,
    /// The most sockets one person has attached at once.
    pub sockets_per_person: usize,
    /// The bytes one socket may have queued before it is dropped from or closed.
    pub socket_queue_bytes: usize,
    /// How long a worker lives after the last socket left.
    pub idle_timeout: Duration,
    /// How long a new worker may take to be ready.
    pub ready_timeout: Duration,
    /// How long a worker may take to close its link after a shutdown request.
    pub shutdown_timeout: Duration,
    /// How long the answer to a control frame may take.
    pub control_timeout: Duration,
    /// How long a wipe may take.
    pub wipe_timeout: Duration,
}

impl Default for SupervisorSettings {
    fn default() -> SupervisorSettings {
        SupervisorSettings {
            max_panes: limits::MAX_PANES,
            max_scrollback_lines: limits::MAX_SCROLLBACK_LINES,
            sockets_per_person: limits::SOCKETS_PER_PERSON as usize,
            socket_queue_bytes: limits::WS_QUEUE_BYTES as usize,
            idle_timeout: limits::WORKER_IDLE_TIMEOUT,
            ready_timeout: limits::WORKER_READY_TIMEOUT,
            shutdown_timeout: limits::WORKER_SHUTDOWN_TIMEOUT,
            control_timeout: limits::WORKER_CONTROL_TIMEOUT,
            wipe_timeout: limits::WORKER_WIPE_TIMEOUT,
        }
    }
}

/// Why a socket could not be attached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachError {
    /// The person has the most sockets already.
    TooManySockets,
    /// The worker could not be started; the sentence says why.
    Launch(String),
    /// The server is stopping.
    ShuttingDown,
}

impl fmt::Display for AttachError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AttachError::TooManySockets => f.write_str("too many connections"),
            AttachError::Launch(sentence) => f.write_str(sentence),
            AttachError::ShuttingDown => f.write_str("the server is stopping"),
        }
    }
}

impl std::error::Error for AttachError {}

/// What the supervisor counts, over all people.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Workers that run now.
    pub workers: usize,
    /// Sockets attached now.
    pub sockets: usize,
}

/// What the supervisor knows about one person.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountStats {
    /// Whether a worker runs.
    pub worker_running: bool,
    /// Sockets attached now.
    pub sockets: usize,
}

/// The supervisor.
#[derive(Clone)]
pub struct Supervisor {
    _private: Arc<()>,
}

impl Supervisor {
    /// A supervisor that starts workers through `launcher`. It must be made inside a tokio runtime.
    pub fn new(_launcher: Arc<dyn WorkerLauncher>, _settings: SupervisorSettings, _clock: Arc<dyn Clock>) -> Supervisor {
        Supervisor { _private: Arc::new(()) }
    }

    /// Attaches a new browser socket of `account`, starting the person's worker if there is none.
    pub async fn attach(&self, _account: AccountKey) -> Result<(SocketHandle, SocketRx), AttachError> {
        Err(AttachError::Launch("the supervisor is not built yet".into()))
    }

    /// What the supervisor knows about `account`.
    pub fn account_stats(&self, _account: AccountKey) -> AccountStats {
        AccountStats::default()
    }

    /// The totals.
    pub fn stats(&self) -> Stats {
        Stats::default()
    }

    /// Ends every worker and closes every socket.
    pub async fn shutdown(&self) {}
}
