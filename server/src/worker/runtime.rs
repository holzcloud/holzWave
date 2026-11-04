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
use crate::worker::files::{FileHooks, Files};
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
    /// The switches of the file routes: the idle time of a grant, a full disk, counters.
    pub files: FileHooks,
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

/// The picture cap of a pane in bytes. Unlike the other caps 0 is a cap: no pictures. `None` when the number of bytes does
/// not fit (it cannot with the range of the key, but the worker does not assume what the supervisor checked).
fn image_cap(mib: u32) -> Option<usize> {
    usize::try_from(mib).ok()?.checked_mul(1024 * 1024)
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
    /// The grants and the uploads.
    files: Files,
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
            (Frame::Ctl(Ctl::Init { version, max_panes, max_scrollback_lines, max_image_mib }), None) => {
                if version != PROTOCOL_VERSION {
                    break fail(queue, log, "protocol version");
                }
                let Some(max_image_bytes) = image_cap(max_image_mib) else { break fail(queue, log, "setup") };
                let limits = HostLimits {
                    max_panes: cap(max_panes),
                    max_scrollback_lines: cap(max_scrollback_lines),
                    // 0 is not "no cap" here: it is a cap of no bytes, and the host keeps no picture under it.
                    max_image_bytes: Some(max_image_bytes),
                };
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
            (Frame::Ctl(Ctl::FileServe { request, what, token, offset, length }), Some(running)) => {
                running.files.serve(request, what, &token, offset, length);
            }
            (Frame::Ctl(Ctl::UploadOpen { id, size, folder, name }), Some(running)) => {
                running.files.upload_open(id, size, folder, name);
            }
            (Frame::Ctl(Ctl::UploadPiece { id, bytes }), Some(running)) => running.files.upload_piece(id, bytes),
            (Frame::Ctl(Ctl::UploadClose { id, commit }), Some(running)) => running.files.upload_close(id, commit),
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
        Ctl::Init { .. }
        | Ctl::Attach { .. }
        | Ctl::Detach { .. }
        | Ctl::Shutdown
        | Ctl::FileServe { .. }
        | Ctl::UploadOpen { .. }
        | Ctl::UploadPiece { .. }
        | Ctl::UploadClose { .. } => return,
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
    // The spawn helper names the temporary directory (`$HOME/.tmp`) in the environment and makes nothing; the person's own
    // worker makes it, private, so that a shell's `$TMPDIR` is a directory that is there.
    if !env.tmp.is_dir() {
        DirBuilder::new().mode(0o700).create(&env.tmp).ok()?;
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
    let files = Files::new(&core, queue, &env.home, hooks.files.clone(), log.clone());
    sink.set_observer(files.observer());
    Some(Running { core, sink, secrets, dispatcher, files, told: StoreState::Absent })
}

// ---- the self-check, the modes and the wipes ----------------------------------------------------------------------------

/// The ids the worker runs with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub uid: u32,
    pub gid: u32,
}

/// The text after `name` (such as `Uid:`) on the line of `text` that starts with it.
fn status_field<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines().find_map(|line| line.strip_prefix(name))
}

/// Four ids that all name the same user or group: real, effective, saved and file system. `what` is `user` or `group`.
fn same_ids(text: &str, name: &str, what: &str, zero: &str) -> Result<u32, String> {
    let ids: Vec<u32> = status_field(text, name)
        .map(|rest| rest.split_whitespace().map(str::parse).collect::<Result<Vec<u32>, _>>())
        .and_then(Result::ok)
        .filter(|ids| ids.len() == 4)
        .ok_or_else(|| format!("the status has no readable {what} ids"))?;
    if ids[0] != ids[1] {
        return Err(format!("the real and effective {what} ids differ"));
    }
    if ids[2] != ids[1] || ids[3] != ids[1] {
        return Err(format!("the saved or file system {what} id differs from the effective one"));
    }
    if ids[1] == 0 {
        return Err(zero.to_owned());
    }
    Ok(ids[1])
}

/// One capability set, which must be empty.
fn empty_capabilities(text: &str, name: &str, what: &str) -> Result<(), String> {
    let value = status_field(text, name)
        .map(str::trim)
        .filter(|v| v.len() == 16 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .and_then(|v| u64::from_str_radix(v, 16).ok())
        .ok_or_else(|| format!("the status has no readable {what} capability set"))?;
    if value == 0 { Ok(()) } else { Err(format!("the worker holds {what} capabilities")) }
}

/// Reads the text of `/proc/self/status`: the worker runs only as its person with nothing but its own ids, no group, no
/// capability and with `no_new_privs`. The error is one sentence, and no sentence repeats a byte of the text.
pub fn check_status(text: &str) -> Result<Identity, String> {
    let uid = same_ids(text, "Uid:", "user", "the worker runs as the superuser")?;
    let gid = same_ids(text, "Gid:", "group", "the worker runs with the root group")?;
    let groups = status_field(text, "Groups:").ok_or("the status has no group list")?;
    let mut listed = 0usize;
    for group in groups.split_whitespace() {
        group.parse::<u32>().map_err(|_| "the group list is not readable")?;
        listed += 1;
    }
    if listed > 0 {
        return Err("the worker has supplementary groups".into());
    }
    empty_capabilities(text, "CapInh:", "inheritable")?;
    empty_capabilities(text, "CapPrm:", "permitted")?;
    empty_capabilities(text, "CapEff:", "effective")?;
    empty_capabilities(text, "CapAmb:", "ambient")?;
    match status_field(text, "NoNewPrivs:").map(str::trim) {
        Some("1") => {}
        Some("0") => return Err("no_new_privs is not set".into()),
        _ => return Err("the status has no readable no_new_privs line".into()),
    }
    Ok(Identity { uid, gid })
}

/// The descriptors the worker may have: 0, 1, 2 and the link on 3, and the directory descriptor `own` that the list was
/// read through.
pub fn check_descriptors(open: &[u32], own: Option<u32>) -> Result<(), String> {
    if !open.contains(&LINK_DESCRIPTOR) {
        return Err(format!("descriptor {LINK_DESCRIPTOR}, the link, is not open"));
    }
    for descriptor in open {
        if *descriptor <= LINK_DESCRIPTOR || Some(*descriptor) == own {
            continue;
        }
        return Err(format!("descriptor {descriptor} is open; only 0, 1, 2 and the link on {LINK_DESCRIPTOR} may be"));
    }
    Ok(())
}

/// The descriptor the helper passes the socket on.
pub const LINK_DESCRIPTOR: u32 = 3;

/// The home must be an absolute path to a real directory, owned by `uid`, with mode 0700.
pub fn check_home(home: &Path, uid: u32) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    if !home.is_absolute() {
        return Err("HOME is not an absolute path".into());
    }
    let meta = std::fs::symlink_metadata(home).map_err(|_| "HOME cannot be read".to_owned())?;
    if meta.file_type().is_symlink() {
        return Err("HOME is a link".into());
    }
    if !meta.is_dir() {
        return Err("HOME is not a directory".into());
    }
    if meta.uid() != uid {
        return Err("HOME does not belong to the worker's user".into());
    }
    let mode = meta.mode() & 0o7777;
    if mode & 0o077 != 0 {
        return Err("HOME is open to the group or to others".into());
    }
    if mode != 0o700 {
        return Err("HOME does not have mode 0700".into());
    }
    Ok(())
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
    pub fn parse(name: &str) -> Option<WorkerMode> {
        match name {
            "serve" => Some(WorkerMode::Serve),
            "wipe_secrets" => Some(WorkerMode::WipeSecrets),
            "wipe_home" => Some(WorkerMode::WipeHome),
            _ => None,
        }
    }
}

/// Removes the store file and its siblings. The home must pass [`check_home`]; a data directory that is a link is refused,
/// so nothing outside the home is touched.
pub fn wipe_secrets(home: &Path, uid: u32) -> Result<(), String> {
    check_home(home, uid)?;
    let data = home.join(DATA_DIR);
    match std::fs::symlink_metadata(&data) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("the data directory cannot be read".into()),
        Ok(meta) if meta.file_type().is_symlink() => return Err("the data directory is a link".into()),
        Ok(meta) if !meta.is_dir() => return Err("the data directory is not a directory".into()),
        Ok(_) => {}
    }
    crate::worker::secretstore::remove_store_files(&crate::worker::secretstore::store_path(home))
        .map_err(|_| "the secret store files could not all be removed".to_owned())
}

/// Removes everything inside the home and leaves the home. Links are removed as links; nothing is followed.
pub fn wipe_home(home: &Path, uid: u32) -> Result<(), String> {
    check_home(home, uid)?;
    let entries = std::fs::read_dir(home).map_err(|_| "HOME cannot be listed".to_owned())?;
    let mut failed = 0usize;
    for entry in entries {
        let Ok(entry) = entry else {
            failed += 1;
            continue;
        };
        let path = entry.path();
        let removed = match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&path),
            Ok(_) => std::fs::remove_file(&path),
            Err(e) => Err(e),
        };
        if removed.is_err() {
            failed += 1;
        }
    }
    if failed == 0 { Ok(()) } else { Err(format!("{failed} entries in HOME could not be removed")) }
}

/// The longest user name taken from the environment.
const MAX_USER_BYTES: usize = 64;

fn absolute(get: &dyn Fn(&str) -> Option<String>, name: &str) -> Result<PathBuf, String> {
    let value = get(name).filter(|v| !v.is_empty()).ok_or_else(|| format!("{name} is not set"))?;
    let path = PathBuf::from(value);
    if path.is_absolute() { Ok(path) } else { Err(format!("{name} is not an absolute path")) }
}

impl WorkerEnv {
    /// The environment the helper set, read through `get`. Anything missing or malformed is a refusal.
    pub fn from_vars(get: &dyn Fn(&str) -> Option<String>) -> Result<WorkerEnv, String> {
        let home = absolute(get, "HOME")?;
        let tmp = absolute(get, "TMPDIR")?;
        absolute(get, "SHELL")?;
        let user = get("USER").filter(|u| !u.is_empty()).ok_or("USER is not set")?;
        if user.len() > MAX_USER_BYTES || user.bytes().any(|b| b.is_ascii_control() || b == b'/' || b == b' ') {
            return Err("USER is not a user name".into());
        }
        Ok(WorkerEnv { home, tmp, shell: ShellSpec::System, user })
    }

    /// The environment of this process.
    pub fn from_process() -> Result<WorkerEnv, String> {
        WorkerEnv::from_vars(&|name| std::env::var(name).ok())
    }
}

/// The numbers of the open descriptors of this process, and the number of the directory descriptor the list was read
/// through.
#[cfg(target_os = "linux")]
fn open_descriptors() -> Result<(Vec<u32>, u32), String> {
    use std::os::fd::AsRawFd;
    let unreadable = || "the open descriptors cannot be listed".to_owned();
    let handle = rustix::fs::open(
        "/proc/self/fd",
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|_| unreadable())?;
    let own = u32::try_from(handle.as_raw_fd()).map_err(|_| unreadable())?;
    let mut dir = rustix::fs::Dir::new(handle).map_err(|_| unreadable())?;
    let mut open = Vec::new();
    while let Some(entry) = dir.read() {
        let entry = entry.map_err(|_| unreadable())?;
        if let Some(number) = entry.file_name().to_str().ok().and_then(|n| n.parse::<u32>().ok()) {
            open.push(number);
        }
    }
    Ok((open, own))
}

/// The checks the worker makes before anything else: its ids, groups, capabilities and `no_new_privs` from the kernel's own
/// account, the descriptors it holds, and that it cannot be dumped or traced. Returns its ids for the check of the home.
#[cfg(target_os = "linux")]
pub fn self_check() -> Result<Identity, String> {
    use rustix::process::{DumpableBehavior, dumpable_behavior, set_dumpable_behavior};
    let status = std::fs::read_to_string("/proc/self/status").map_err(|_| "the process status cannot be read".to_owned())?;
    let identity = check_status(&status)?;
    let (open, own) = open_descriptors()?;
    check_descriptors(&open, Some(own))?;
    // The exec that started this process reset the state, so it is set here and read back.
    set_dumpable_behavior(DumpableBehavior::NotDumpable).map_err(|_| "the process cannot be made undumpable".to_owned())?;
    if dumpable_behavior() != Ok(DumpableBehavior::NotDumpable) {
        return Err("the process is dumpable".into());
    }
    Ok(identity)
}
