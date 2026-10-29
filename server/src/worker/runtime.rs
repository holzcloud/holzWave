//! The worker's run loop: setup, the self-check of its own privileges, and shutdown.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use holzwave_host::state::ShellSpec;

/// What the worker needs to know about its surroundings, and nothing a frame can change.
#[derive(Clone, Debug)]
pub struct WorkerEnv {
    /// The person's home directory. The worker's data directory is `.holzwave` inside it.
    pub home: PathBuf,
    /// The temporary directory.
    pub tmp: PathBuf,
    /// The program a pane runs.
    pub shell: ShellSpec,
}

/// Switches of the integration tests; the product passes the default.
#[derive(Clone, Debug, Default)]
pub struct TestHooks {}

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunEnd {
    /// The link reached end of file.
    LinkClosed,
    /// The supervisor asked for the end.
    Shutdown,
    /// The worker gave up.
    Failed,
}

/// Runs the worker on `link` until the link ends.
pub fn run(link: UnixStream, env: WorkerEnv) -> RunEnd {
    run_with(link, env, TestHooks::default())
}

/// [`run`] with the switches of a test.
pub fn run_with(_link: UnixStream, _env: WorkerEnv, _hooks: TestHooks) -> RunEnd {
    RunEnd::Failed
}
