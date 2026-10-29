//! The worker's run loop: setup from the first frame, the commands and controls that follow, and the end.
//!
//! One thread reads the link and never blocks on anything but the link: a command is handed to the dispatcher, a control
//! is a short call on the secret store, and everything the worker says goes through the outbound queue to the writer
//! thread. The run ends when the link ends, when the supervisor asks for it, or when the worker cannot go on.

use std::fmt;
use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use holzwave_core::secrets::SecretStore;
use holzwave_host::local_network::{Source, Unsupported};
use holzwave_host::seams::{Host, HostLimits, Platform};
use holzwave_host::state::{AppCore, ShellSpec};

use crate::log::Level;
use crate::wire::{Ctl, ExitReason, Frame, Note, PROTOCOL_VERSION, StoreState, read_frame};
pub use crate::worker::dispatch::{Gate, HookRow, Probe, QueueLimits};
use crate::worker::dispatch::{self, Dispatcher, Outbound, Pin, ServerSink, WorkerLog};
pub use crate::worker::secretstore::DATA_DIR;
use crate::worker::secretstore::SecretsHandle;

/// The command a page sends when it is ready for the state of the window; the events it causes are for that page alone.
pub const REPLAY_COMMAND: &str = "window_ready";

/// What the worker needs to know about its surroundings, and nothing a frame can change.
#[derive(Clone, Debug)]
pub struct WorkerEnv {
    /// The person's home directory. The worker's data directory is `.holzwave` inside it.
    pub home: PathBuf,
    /// The temporary directory.
    pub tmp: PathBuf,
    /// The program a pane runs.
    pub shell: ShellSpec,
    /// The name of the person's account, as the helper set it.
    pub user: String,
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

/// The host of a person's core: the web platform, the store of the controls, no local network source, no text scale, the
/// caps of `Init`.
struct WebHost {
    secrets: Arc<SecretsHandle>,
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
pub fn run_with(link: UnixStream, env: WorkerEnv, hooks: TestHooks) -> RunEnd {
    let Ok(write_half) = link.try_clone() else { return RunEnd::Failed };
    let queue = Outbound::new(hooks.queue.unwrap_or_default());
    let writer = dispatch::spawn_writer(write_half, queue.clone(), hooks.writer_gate.clone());
    let mut reader = link;
    let log = WorkerLog::new(hooks.log.clone());
    let end = serve(&mut reader, &env, &queue, &hooks, &log);
    queue.close();
    let _ = writer.join();
    if queue.is_dead() { RunEnd::Failed } else { end }
}

/// Everything that exists once `Init` has come.
struct Running {
    core: Arc<AppCore>,
    sink: Arc<ServerSink>,
    secrets: Arc<SecretsHandle>,
    dispatcher: Dispatcher,
    /// The store state the supervisor was told last.
    told: StoreState,
}

/// Ends the run with a failure note.
fn fail(queue: &Outbound, log: &WorkerLog, why: &'static str) -> RunEnd {
    log.event(Level::Warn, "worker_failed", &[("reason", why)]);
    queue.push(Frame::Note(Note::Exiting(ExitReason::Failure)));
    RunEnd::Failed
}

fn serve(
    reader: &mut UnixStream,
    env: &WorkerEnv,
    queue: &Arc<Outbound>,
    hooks: &TestHooks,
    log: &WorkerLog,
) -> RunEnd {
    let mut running: Option<Running> = None;
    let end = loop {
        let frame = match read_frame(reader) {
            Ok(Some(frame)) => frame,
            Ok(None) => break RunEnd::LinkClosed,
            Err(_) => break fail(queue, log, "bad frame"),
        };
        match (frame, running.as_mut()) {
            (Frame::Ctl(Ctl::Init { version, max_panes, max_scrollback_lines }), None) => {
                if version != PROTOCOL_VERSION {
                    break fail(queue, log, "protocol version");
                }
                let limits = HostLimits { max_panes: cap(max_panes), max_scrollback_lines: cap(max_scrollback_lines) };
                let Some(started) = start(env, queue, hooks, log, limits) else { break fail(queue, log, "setup") };
                queue.push(Frame::Note(Note::Ready));
                let state = started.secrets.state();
                queue.push(Frame::Note(Note::StoreState(state)));
                if let Ok(view) = started.core.view_in(dispatch::WINDOW) {
                    started.sink.announce_panes(view.workspaces.iter().map(|w| w.panes).sum());
                }
                running = Some(Running { told: state, ..started });
            }
            (Frame::Cmd(cmd), Some(running)) => {
                if let Err(answer) = running.dispatcher.submit(&cmd) {
                    queue.push(answer);
                }
            }
            (Frame::Cmd(cmd), None) => {
                queue.push(dispatch::refusal(cmd.socket, cmd.request, "the worker is not ready"));
            }
            (Frame::Ctl(Ctl::Shutdown), Some(running)) => {
                running.core.shutdown();
                queue.push(Frame::Note(Note::Exiting(ExitReason::Clean)));
                break RunEnd::Shutdown;
            }
            (Frame::Ctl(Ctl::Attach { socket }), Some(running)) => {
                // The replay of titles and directories is for the page that attached alone.
                let _pin = Pin::to(socket);
                let _ = running.core.window_ready_in(dispatch::WINDOW);
            }
            (Frame::Ctl(Ctl::Detach { .. }), Some(_)) => {}
            (Frame::Ctl(Ctl::Init { .. }), Some(_)) => break fail(queue, log, "second init"),
            (Frame::Ctl(control), Some(running)) => control_store(running, queue, control),
            // A second Init, a frame before Init, or a frame that only the worker writes.
            _ => break fail(queue, log, "unexpected frame"),
        }
    };
    if let Some(running) = running {
        running.core.shutdown();
    }
    end
}

/// A control of the secret store: the change, the new state when it changed, and the answer.
fn control_store(running: &mut Running, queue: &Outbound, control: Ctl) {
    let (ctl, outcome) = match control {
        Ctl::Unlock { ctl, kind, key } => (ctl, running.secrets.unlock(kind, &key.to_kek())),
        Ctl::Lock { ctl } => (ctl, running.secrets.lock()),
        Ctl::ResetSecrets { ctl } => (ctl, running.secrets.reset()),
        Ctl::Provision { ctl, slots } => {
            let keys: Vec<_> = slots.iter().map(|s| (s.kind, s.id, s.key.to_kek())).collect();
            (ctl, running.secrets.provision(&keys))
        }
        Ctl::SetSlot { ctl, slot, opening } => {
            let opening_kek = opening.as_ref().map(|o| (o.kind, o.key.to_kek()));
            let outcome = running.secrets.set_slot(
                slot.kind,
                slot.id,
                &slot.key.to_kek(),
                opening_kek.as_ref().map(|(kind, kek)| (*kind, kek)),
            );
            (ctl, outcome)
        }
        Ctl::RemoveSlot { ctl, kind, id } => (ctl, running.secrets.remove_slot(kind, id)),
        // The other controls are handled by the loop.
        Ctl::Init { .. } | Ctl::Attach { .. } | Ctl::Detach { .. } | Ctl::Shutdown => return,
    };
    let state = running.secrets.state();
    if state != running.told {
        running.told = state;
        queue.push(Frame::Note(Note::StoreState(state)));
    }
    queue.push(Frame::Note(Note::CtlDone { ctl, outcome }));
}

/// Builds the core of the person and the threads that run its commands.
fn start(
    env: &WorkerEnv,
    queue: &Arc<Outbound>,
    hooks: &TestHooks,
    log: &WorkerLog,
    limits: HostLimits,
) -> Option<Running> {
    let dir = env.home.join(DATA_DIR);
    if !dir.is_dir() {
        DirBuilder::new().mode(0o700).create(&dir).ok()?;
    }
    let secrets = Arc::new(SecretsHandle::open(&env.home));
    let host = WebHost { secrets: secrets.clone(), limits };
    let sink = Arc::new(ServerSink::with_log(queue.clone(), log.clone()));
    let core = AppCore::new_with_host(dir, sink.clone(), env.shell.clone(), None, None, Arc::new(host));
    let repair_core = Arc::downgrade(&core);
    let probe = hooks.probe.clone();
    queue.set_repair(move |pane| {
        if let Some(probe) = &probe {
            probe.record_repair(pane);
        }
        if let Some(core) = repair_core.upgrade() {
            let _ = core.request_full(pane);
        }
    });
    let dispatcher =
        Dispatcher::start(&core, &sink, queue, hooks.rows.clone(), hooks.probe.clone(), log.clone(), REPLAY_COMMAND);
    Some(Running { core, sink, secrets, dispatcher, told: StoreState::Absent })
}

// ---- the self-check, the modes and the wipes ----------------------------------------------------------------------------

/// The ids the worker runs with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub uid: u32,
    pub gid: u32,
}

/// Reads the text of `/proc/self/status`: the worker runs only as its person with nothing but its own ids, no group, no
/// capability and with `no_new_privs`. The error is one sentence, and no sentence repeats a byte of the text.
pub fn check_status(_text: &str) -> Result<Identity, String> {
    Err("not built".into())
}

/// The descriptors the worker may have: 0, 1, 2 and the link on 3, and the directory descriptor `own` that the list was
/// read through.
pub fn check_descriptors(_open: &[u32], _own: Option<u32>) -> Result<(), String> {
    Err("not built".into())
}

/// The home must be an absolute path to a real directory, owned by `uid`, with mode 0700.
pub fn check_home(_home: &Path, _uid: u32) -> Result<(), String> {
    Err("not built".into())
}

/// What a worker process is asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerMode {
    /// Serve the person's core on the link.
    Serve,
    /// Remove the person's secret store and exit.
    WipeSecrets,
    /// Remove everything in the person's home and exit.
    WipeHome,
}

impl WorkerMode {
    /// The mode a `--mode` value names.
    pub fn parse(_name: &str) -> Option<WorkerMode> {
        None
    }
}

/// Removes the store file and its siblings.
pub fn wipe_secrets(_home: &Path, _uid: u32) -> Result<(), String> {
    Err("not built".into())
}

/// Removes everything inside the home and leaves the home.
pub fn wipe_home(_home: &Path, _uid: u32) -> Result<(), String> {
    Err("not built".into())
}

impl WorkerEnv {
    /// The environment the helper set, read through `get`. Anything missing or malformed is a refusal.
    pub fn from_vars(_get: &dyn Fn(&str) -> Option<String>) -> Result<WorkerEnv, String> {
        Err("not built".into())
    }
}
