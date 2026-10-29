//! The supervisor's side of the socket to the spawn helper.

use std::fmt;
use std::os::fd::OwnedFd;
use std::time::Duration;

use holzwave_spawn_helper::protocol::{Mode, Signal};
use tokio::sync::watch;

use crate::limits;

/// Why the helper could not do what was asked. One sentence; the helper's own words are fixed codes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperError(String);

impl HelperError {
    /// The sentence.
    pub fn sentence(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HelperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HelperError {}

/// How long the client waits.
#[derive(Clone, Copy, Debug)]
pub struct HelperTimeouts {
    /// For the answer to one request.
    pub request: Duration,
    /// For the first answer of a helper that was just started.
    pub start: Duration,
}

impl Default for HelperTimeouts {
    fn default() -> HelperTimeouts {
        HelperTimeouts { request: limits::HELPER_REQUEST_TIMEOUT, start: limits::HELPER_START_TIMEOUT }
    }
}

/// How to start the helper and what to tell it. Everything the helper needs it gets on its command line; it trusts nothing
/// else.
#[derive(Clone, Debug, Default)]
pub struct HelperSettings {
    /// The helper program.
    pub program: std::path::PathBuf,
    /// The worker program the helper starts for each person.
    pub worker_path: std::path::PathBuf,
    /// The directory that holds the homes.
    pub home_base: std::path::PathBuf,
    /// The first and last user id the helper may start workers as.
    pub uid_range: Option<(u32, u32)>,
    /// The process limit of each person.
    pub nproc: Option<u64>,
    /// The open-file limit of each person.
    pub nofile: Option<u64>,
    /// The most workers at once.
    pub max_workers: Option<u64>,
    /// The shell of the panes.
    pub shell: Option<std::path::PathBuf>,
    /// The seconds a wipe may take.
    pub wipe_timeout_seconds: Option<u64>,
    /// The waits.
    pub timeouts: HelperTimeouts,
}

/// A worker the helper started.
#[derive(Debug)]
pub struct Spawned {
    /// The process id of the worker.
    pub pid: i32,
    /// The supervisor's end of the link to the worker.
    pub link: OwnedFd,
    /// `Some(status)` once the helper said the worker ended.
    pub ended: watch::Receiver<Option<i32>>,
}

/// The client of the spawn helper.
#[derive(Clone)]
pub struct HelperClient {
    _private: (),
}

impl HelperClient {
    /// A client over a datagram socket whose other end is the helper (or a stand-in). Must be called inside a runtime.
    pub fn from_socket(_socket: OwnedFd, _timeouts: HelperTimeouts) -> std::io::Result<HelperClient> {
        Ok(HelperClient { _private: () })
    }

    /// Starts the helper with a sequenced-packet socket as its standard input and waits for its first answer.
    #[cfg(target_os = "linux")]
    pub async fn start(_settings: &HelperSettings) -> Result<HelperClient, HelperError> {
        Err(HelperError("the spawn helper client is not built yet".into()))
    }

    /// Whether the helper can still be reached.
    pub fn is_alive(&self) -> bool {
        false
    }

    /// Answers that carried an id nobody waited for.
    pub fn dropped_answers(&self) -> u64 {
        0
    }

    /// Asks whether the helper lives; the number of workers it holds.
    pub async fn ping(&self) -> Result<u32, HelperError> {
        Err(HelperError("the spawn helper client is not built yet".into()))
    }

    /// Starts a worker of the user `uid`.
    pub async fn spawn(&self, _uid: u32, _mode: Mode) -> Result<Spawned, HelperError> {
        Err(HelperError("the spawn helper client is not built yet".into()))
    }

    /// Signals every process of the user `uid`; how many were signalled.
    pub async fn kill_uid(&self, _uid: u32, _signal: Signal) -> Result<u32, HelperError> {
        Err(HelperError("the spawn helper client is not built yet".into()))
    }

    /// Removes the home of the user `uid`.
    pub async fn remove_home(&self, _uid: u32) -> Result<(), HelperError> {
        Err(HelperError("the spawn helper client is not built yet".into()))
    }
}
