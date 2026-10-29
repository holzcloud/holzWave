//! The supervisor: the network-facing process that starts, tracks and stops one worker per person.
//!
//! [`registry`] owns the workers and the sockets that are attached to them, [`socket`] holds the bounded queue of every
//! browser socket, [`fanout`] turns what a worker says into what the page expects, [`idle`] ends workers nobody uses,
//! [`launcher`] starts them and [`helper_client`] speaks to the spawn helper that does it as the person's own user.

pub mod fanout;
pub mod helper_client;
pub mod idle;
pub mod launcher;
pub mod registry;
pub mod socket;

pub use helper_client::{HelperClient, HelperError, HelperSettings, HelperTimeouts, Spawned};
pub use launcher::{HelperLauncher, Launch, LaunchError, LaunchRequest, LaunchedWorker, WorkerLauncher};
#[cfg(feature = "in-process-workers")]
pub use launcher::InProcessLauncher;
pub use registry::{AccountKey, AccountStats, AttachError, CtlError, Stats, Supervisor, SupervisorSettings, WipeError};
pub use socket::{Outbound, SocketHandle, SocketRx, SubmitError};
