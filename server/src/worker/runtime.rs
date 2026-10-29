//! The worker's run loop: setup, the self-check of its own privileges, and shutdown.

use std::fmt;
use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, PoisonError};

use holzwave_core::secrets::{MemoryStore, SecretStore};
use holzwave_host::commands::registry::{Mode, Outcome};
use holzwave_host::local_network::{Source, Unsupported};
use holzwave_host::seams::{Host, HostLimits, Platform};
use holzwave_host::state::{AppCore, ShellSpec};

use crate::wire::{Ctl, Frame, Note, PROTOCOL_VERSION, read_frame};
use crate::worker::dispatch::{self, ChannelSink, Item, QueueLimits};

/// The name of the worker's data directory inside the home.
pub const DATA_DIR: &str = ".holzwave";

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

pub use crate::worker::dispatch::Gate;

/// What the worker records for a test: the thread every command ran on, the panes it asked a full frame for.
#[derive(Debug, Default)]
pub struct Probe {
    calls: Mutex<Vec<(String, String)>>,
    repaired: Mutex<Vec<u64>>,
}

impl Probe {
    pub fn new() -> Arc<Probe> {
        Arc::new(Probe::default())
    }

    /// `(command, thread name)` for every command that ran, in the order they started.
    pub fn calls(&self) -> Vec<(String, String)> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The panes the worker asked a full frame for after it dropped frames, in order.
    pub fn repaired(&self) -> Vec<u64> {
        self.repaired.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// A command that is not in the table of the host, for a test: it runs where its mode says.
#[derive(Clone)]
pub struct HookRow {
    pub name: &'static str,
    pub mode: Mode,
    pub run: Arc<dyn Fn() -> Outcome + Send + Sync>,
}

impl HookRow {
    pub fn new(name: &'static str, mode: Mode, run: impl Fn() -> Outcome + Send + Sync + 'static) -> HookRow {
        HookRow { name, mode, run: Arc::new(run) }
    }
}

/// Switches of the integration tests, passed to [`run_with`]; the product passes the default, which changes nothing.
#[derive(Clone, Default)]
pub struct TestHooks {
    /// The writer thread waits at this gate before every frame.
    pub writer_gate: Option<Arc<Gate>>,
    /// Records the thread of every command and the panes asked for a full frame.
    pub probe: Option<Arc<Probe>>,
    /// Commands that are not in the table (a slow one, one that panics).
    pub rows: Vec<HookRow>,
    /// The marks of the outbound queue.
    pub queue: Option<QueueLimits>,
    /// Receives every log line the worker writes, as text.
    pub log: Option<Arc<Mutex<Vec<String>>>>,
}

impl fmt::Debug for TestHooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TestHooks(..)")
    }
}

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

/// The host of a person's core: the web platform, no local network source, no text scale, the caps of `Init`.
struct WebHost {
    secrets: Arc<dyn SecretStore>,
    limits: HostLimits,
}

impl Host for WebHost {
    fn platform(&self) -> Platform {
        Platform::Web
    }

    fn secrets(&self) -> Arc<dyn SecretStore> {
        self.secrets.clone()
    }

    fn local_network(&self) -> Arc<dyn Source> {
        Arc::new(Unsupported)
    }

    fn text_scale(&self) -> u32 {
        100
    }

    fn limits(&self) -> HostLimits {
        self.limits
    }

    fn local_processes(&self) -> bool {
        false
    }
}

fn cap(value: u32) -> Option<usize> {
    (value != 0).then(|| usize::try_from(value).unwrap_or(usize::MAX))
}

/// Runs the worker on `link` until the link ends.
pub fn run(link: UnixStream, env: WorkerEnv) -> RunEnd {
    run_with(link, env, TestHooks::default())
}

/// [`run`] with the switches of a test.
pub fn run_with(link: UnixStream, env: WorkerEnv, _hooks: TestHooks) -> RunEnd {
    let Ok(write_half) = link.try_clone() else { return RunEnd::Failed };
    let mut reader = link;
    let (out, items) = channel::<Item>();
    let writer = dispatch::spawn_item_writer(write_half, items);
    let end = serve(&mut reader, &env, &out);
    let _ = out.send(Item::Stop);
    let _ = writer.join();
    end
}

struct Running {
    core: Arc<AppCore>,
    jobs: Sender<dispatch::Job>,
}

fn serve(reader: &mut UnixStream, env: &WorkerEnv, out: &Sender<Item>) -> RunEnd {
    let mut running: Option<Running> = None;
    let end = loop {
        let frame = match read_frame(reader) {
            Ok(Some(frame)) => frame,
            Ok(None) => break RunEnd::LinkClosed,
            Err(_) => break RunEnd::Failed,
        };
        match (frame, running.as_ref()) {
            (Frame::Ctl(Ctl::Init { version, max_panes, max_scrollback_lines }), None) => {
                if version != PROTOCOL_VERSION {
                    break RunEnd::Failed;
                }
                let limits = HostLimits { max_panes: cap(max_panes), max_scrollback_lines: cap(max_scrollback_lines) };
                let Some(core) = build_core(env, out, limits) else { break RunEnd::Failed };
                let (jobs, queue) = channel();
                dispatch::spawn_dispatcher(core.clone(), queue, out.clone());
                running = Some(Running { core, jobs });
                let _ = out.send(Item::Frame(Frame::Note(Note::Ready)));
            }
            (Frame::Cmd(cmd), Some(running)) => match dispatch::accept(&cmd) {
                Ok(job) => {
                    let _ = running.jobs.send(job);
                }
                Err(refusal) => {
                    let _ = out.send(Item::Frame(refusal));
                }
            },
            (Frame::Cmd(cmd), None) => {
                let _ = out.send(Item::Frame(dispatch::refusal(cmd.socket, cmd.request, "the worker is not ready")));
            }
            _ => {}
        }
    };
    if let Some(running) = running {
        running.core.shutdown();
    }
    end
}

fn build_core(env: &WorkerEnv, out: &Sender<Item>, limits: HostLimits) -> Option<Arc<AppCore>> {
    let dir = env.home.join(DATA_DIR);
    if !dir.is_dir() {
        DirBuilder::new().mode(0o700).create(&dir).ok()?;
    }
    let host = WebHost { secrets: Arc::new(MemoryStore::default()), limits };
    let sink = Arc::new(ChannelSink::new(out.clone()));
    Some(AppCore::new_with_host(dir, sink, env.shell.clone(), None, None, Arc::new(host)))
}
