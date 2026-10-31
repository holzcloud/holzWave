//! Starting a worker as the person's own Unix user.
//!
//! The supervisor never starts a worker itself. It asks a [`WorkerLauncher`] for one: the real server uses the spawn helper
//! (`HelperLauncher`), and a build with the `in-process-workers` feature can run the worker's loop on a thread of its own
//! process ([`InProcessLauncher`]) for development and for the tests. The trait is object safe and uses plain standard
//! library futures, so a test can bring a launcher of its own.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use tokio::net::UnixStream;
use tokio::sync::watch;

use holzwave_spawn_helper::protocol::{Mode, Signal};

use crate::supervisor::helper_client::HelperClient;
use crate::supervisor::registry::AccountKey;
use crate::worker::runtime::WorkerMode;

/// What a launcher is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaunchRequest {
    /// The person; the launcher starts the worker as exactly this user.
    pub account: AccountKey,
    /// What the worker does: serve the link, or wipe the store or the home and exit.
    pub mode: WorkerMode,
}

/// Why a worker could not be started. The text is one sentence and holds no byte that came from a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchError(String);

impl LaunchError {
    /// An error with a sentence.
    pub fn new(sentence: impl Into<String>) -> LaunchError {
        LaunchError(sentence.into())
    }

    /// The sentence.
    pub fn sentence(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LaunchError {}

/// A worker that was started.
#[derive(Debug)]
pub struct LaunchedWorker {
    /// The supervisor's end of the link to the worker. For a wipe mode the link is of no use and may be closed.
    pub link: UnixStream,
    /// `None` while the worker runs and `Some(status)` once it has ended: its exit code, or 128 plus the signal.
    pub ended: watch::Receiver<Option<i32>>,
}

/// The future every launcher method returns.
pub type Launch<'a, T> = Pin<Box<dyn Future<Output = Result<T, LaunchError>> + Send + 'a>>;

/// Starts workers and ends what is left of a person's processes.
pub trait WorkerLauncher: Send + Sync {
    /// Starts a worker for `request.account` in `request.mode`. At most one worker of a person runs at a time; a second
    /// request while one runs is refused with a sentence.
    fn launch(&self, request: LaunchRequest) -> Launch<'_, LaunchedWorker>;

    /// Kills every process of the user `uid`.
    fn kill_uid(&self, uid: u32) -> Launch<'_, ()>;

    /// Whether the thing behind the launcher (the spawn helper) can still be reached.
    fn helper_alive(&self) -> bool {
        true
    }
}

/// Starts workers through the spawn helper, which runs each as the person's own Unix user.
pub struct HelperLauncher {
    client: HelperClient,
}

impl HelperLauncher {
    /// A launcher over a helper client.
    pub fn new(client: HelperClient) -> HelperLauncher {
        HelperLauncher { client }
    }

    /// The client, for the calls the launcher trait does not carry.
    pub fn client(&self) -> &HelperClient {
        &self.client
    }
}

impl WorkerLauncher for HelperLauncher {
    fn launch(&self, request: LaunchRequest) -> Launch<'_, LaunchedWorker> {
        Box::pin(async move {
            // The user id of the account is the only thing about the person that goes to the helper.
            let uid = request.account.uid;
            let mode = match request.mode {
                WorkerMode::Serve => Mode::Serve,
                WorkerMode::WipeSecrets => Mode::WipeSecrets,
                WorkerMode::WipeHome => Mode::WipeHome,
            };
            let spawned = match self.client.spawn(uid, mode).await {
                Ok(spawned) => spawned,
                Err(error) => {
                    if error.may_have_started() {
                        let _ = self.client.kill_uid(uid, Signal::Kill).await;
                    }
                    return Err(LaunchError::new(error.sentence()));
                }
            };
            let stream = std::os::unix::net::UnixStream::from(spawned.link);
            stream.set_nonblocking(true).map_err(|_| LaunchError::new("the link to the worker could not be made non-blocking"))?;
            let link = UnixStream::from_std(stream).map_err(|_| LaunchError::new("the link to the worker could not be adopted"))?;
            Ok(LaunchedWorker { link, ended: spawned.ended })
        })
    }

    fn kill_uid(&self, uid: u32) -> Launch<'_, ()> {
        Box::pin(async move {
            self.client.kill_uid(uid, Signal::Kill).await.map(|_| ()).map_err(|e| LaunchError::new(e.sentence()))
        })
    }

    fn helper_alive(&self) -> bool {
        self.client.is_alive()
    }
}

#[cfg(feature = "in-process-workers")]
pub use in_process::InProcessLauncher;

#[cfg(feature = "in-process-workers")]
mod in_process {
    use std::collections::HashMap;
    use std::fs::DirBuilder;
    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::net::UnixStream as StdStream;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, PoisonError};

    use holzwave_host::state::ShellSpec;
    use tokio::sync::watch;

    use super::{Launch, LaunchError, LaunchRequest, LaunchedWorker, WorkerLauncher};
    use crate::worker::runtime::{self, WorkerEnv, WorkerMode};

    /// What the launcher remembers of one worker it started.
    struct Running {
        /// A handle on the worker's end of the link; shutting it down ends the worker's loop.
        link: StdStream,
        ended: watch::Receiver<Option<i32>>,
    }

    #[derive(Default)]
    struct Book {
        running: HashMap<u32, Running>,
        requests: Vec<(u32, WorkerMode)>,
        kills: Vec<u32>,
    }

    /// Runs each worker's loop on a thread of this process, over a socket pair. The homes are directories under one root,
    /// one per user id, each with mode 0700. There is no isolation between the people: this is for development and tests.
    pub struct InProcessLauncher {
        root: PathBuf,
        shell: ShellSpec,
        launches: AtomicUsize,
        book: Mutex<Book>,
        /// The switches of the integration tests that every worker of this launcher is started with; the default changes
        /// nothing.
        hooks: runtime::TestHooks,
    }

    impl InProcessLauncher {
        /// A launcher whose homes are `root/homes/<uid>` and whose panes run `shell`.
        pub fn new(root: impl Into<PathBuf>, shell: ShellSpec) -> InProcessLauncher {
            InProcessLauncher {
                root: root.into(),
                shell,
                launches: AtomicUsize::new(0),
                book: Mutex::new(Book::default()),
                hooks: runtime::TestHooks::default(),
            }
        }

        /// The same launcher, with every worker it starts run with `hooks` (the switches of a test).
        pub fn with_hooks(mut self, hooks: runtime::TestHooks) -> InProcessLauncher {
            self.hooks = hooks;
            self
        }

        /// How many workers were started, of any mode.
        pub fn launches(&self) -> usize {
            self.launches.load(Ordering::SeqCst)
        }

        /// The user id and mode of every request, in order.
        pub fn requests(&self) -> Vec<(u32, WorkerMode)> {
            self.book().requests.clone()
        }

        /// The user id of every `kill_uid`, in order.
        pub fn kills(&self) -> Vec<u32> {
            self.book().kills.clone()
        }

        /// The home of the user `uid`.
        pub fn home(&self, uid: u32) -> PathBuf {
            self.root.join("homes").join(uid.to_string())
        }

        fn book(&self) -> std::sync::MutexGuard<'_, Book> {
            self.book.lock().unwrap_or_else(PoisonError::into_inner)
        }

        fn make_dir(path: &Path) -> Result<(), LaunchError> {
            if path.is_dir() {
                return Ok(());
            }
            DirBuilder::new()
                .mode(0o700)
                .create(path)
                .map_err(|_| LaunchError::new("the home of the worker could not be made"))
        }

        fn start(&self, request: LaunchRequest) -> Result<LaunchedWorker, LaunchError> {
            let uid = request.account.uid;
            {
                let mut book = self.book();
                if book.running.get(&uid).is_some_and(|r| r.ended.borrow().is_none()) {
                    return Err(LaunchError::new("a worker of this person is still running"));
                }
                book.requests.push((uid, request.mode));
            }
            let homes = self.root.join("homes");
            Self::make_dir(&homes)?;
            let home = self.home(uid);
            Self::make_dir(&home)?;
            let tmp = home.join(".tmp");
            Self::make_dir(&tmp)?;
            let (ours, theirs) = StdStream::pair().map_err(|_| LaunchError::new("the socket pair could not be made"))?;
            let mine = ours.try_clone().map_err(|_| LaunchError::new("the socket could not be duplicated"))?;
            ours.set_nonblocking(true).map_err(|_| LaunchError::new("the socket could not be made non-blocking"))?;
            let (ended_tx, ended) = watch::channel(None);
            let env = WorkerEnv { home: home.clone(), tmp, shell: self.shell.clone(), user: format!("u{uid}") };
            let mode = request.mode;
            let own_uid = rustix::process::getuid().as_raw();
            let hooks = self.hooks.clone();
            std::thread::Builder::new()
                .name(format!("worker-{uid}"))
                .spawn(move || {
                    let status = match mode {
                        WorkerMode::Serve => match runtime::run_with(theirs, env, hooks) {
                            runtime::RunEnd::Failed => 1,
                            runtime::RunEnd::LinkClosed | runtime::RunEnd::Shutdown => 0,
                        },
                        WorkerMode::WipeSecrets => i32::from(runtime::wipe_secrets(&env.home, own_uid).is_err()),
                        WorkerMode::WipeHome => i32::from(runtime::wipe_home(&env.home, own_uid).is_err()),
                    };
                    let _ = ended_tx.send(Some(status));
                })
                .map_err(|_| LaunchError::new("the worker thread could not be started"))?;
            self.launches.fetch_add(1, Ordering::SeqCst);
            self.book().running.insert(uid, Running { link: mine, ended: ended.clone() });
            let link = tokio::net::UnixStream::from_std(ours).map_err(|_| LaunchError::new("the socket could not be adopted"))?;
            Ok(LaunchedWorker { link, ended })
        }
    }

    impl WorkerLauncher for InProcessLauncher {
        fn launch(&self, request: LaunchRequest) -> Launch<'_, LaunchedWorker> {
            Box::pin(async move { self.start(request) })
        }

        fn kill_uid(&self, uid: u32) -> Launch<'_, ()> {
            Box::pin(async move {
                let mut book = self.book();
                book.kills.push(uid);
                if let Some(running) = book.running.get(&uid) {
                    let _ = running.link.shutdown(std::net::Shutdown::Both);
                }
                Ok(())
            })
        }
    }
}
