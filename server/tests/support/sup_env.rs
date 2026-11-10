//! The supervisor driven in a test: a scratch directory, a manual clock, a launcher and a client that reads what a browser
//! socket would.
//!
//! `SupEnv::start()` builds a supervisor over an `InProcessLauncher` (real workers on threads, `/bin/sh` as the fixed shell).
//! `ScriptedLauncher` builds one over workers that the test plays itself, frame by frame. `Client` wraps the two halves of an
//! attached socket and reads replies and events with a time limit. Tests pull this file in with
//! `#[path = "support/sup_env.rs"] mod sup_env;` next to `#[path = "support/scratch.rs"] mod scratch;`.
#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::io::{IoSlice, IoSliceMut, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream as StdStream;
#[cfg(feature = "in-process-workers")]
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

#[cfg(feature = "in-process-workers")]
use holzwave_host::state::ShellSpec;
use holzwave_spawn_helper::protocol::{self, Answer, ExitedEvent, Mode, Request};
use holzwave_server::clock::ManualClock;
use holzwave_server::store::AccountId;
use holzwave_server::supervisor::{
    AccountKey, HelperClient, HelperLauncher, HelperTimeouts, Launch, LaunchError, LaunchRequest,
    LaunchedWorker, Outbound, SocketHandle, SocketRx, Supervisor, SupervisorSettings, WorkerLauncher,
};
#[cfg(feature = "in-process-workers")]
use holzwave_server::supervisor::{AttachError, InProcessLauncher};
use holzwave_server::wire::{CmdFrame, Ctl, Frame, Note, PROTOCOL_VERSION, read_frame, write_frame};
use holzwave_server::worker::runtime::WorkerMode;
use holzwave_server::worker::sessions::SessionLimits;
use serde_json::Value;
use tokio::sync::watch;

#[cfg(feature = "in-process-workers")]
use crate::scratch::Scratch;

/// How long a test waits for anything the supervisor should do.
pub const PATIENCE: Duration = Duration::from_secs(10);

/// What a `SupEnv` is started with.
#[cfg(feature = "in-process-workers")]
#[derive(Default)]
pub struct SupOptions {
    /// Replaces the default settings (the pane cap of 64 and the default times).
    pub settings: Option<SupervisorSettings>,
    /// Replaces the fixed shell of every pane (which writes its process id, prints `ready` and then echoes).
    pub shell: Option<ShellSpec>,
}

/// A supervisor over real in-process workers.
#[cfg(feature = "in-process-workers")]
pub struct SupEnv {
    pub scratch: Scratch,
    pub clock: Arc<ManualClock>,
    pub launcher: Arc<InProcessLauncher>,
    pub supervisor: Supervisor,
    /// The file every fixed shell appends its process id to.
    pub pid_file: PathBuf,
}

/// The first moment of every test clock.
pub const START: i64 = 1_800_000_000;

#[cfg(feature = "in-process-workers")]
impl SupEnv {
    /// A supervisor with the default settings.
    pub async fn start() -> SupEnv {
        SupEnv::start_with(SupOptions::default()).await
    }

    /// A supervisor with the given options.
    pub async fn start_with(options: SupOptions) -> SupEnv {
        let scratch = Scratch::new("sup");
        let pid_file = scratch.join("shells.pid");
        let script = format!("echo $$ >> '{}'; echo ready; exec cat", pid_file.display());
        let shell = options.shell.unwrap_or_else(|| ShellSpec::new("/bin/sh", &["-c", &script]));
        let launcher = Arc::new(InProcessLauncher::new(scratch.path().to_path_buf(), shell));
        let clock = Arc::new(ManualClock::new(START));
        let settings = options.settings.unwrap_or_default();
        let supervisor = Supervisor::new(launcher.clone(), settings, clock.clone());
        SupEnv { scratch, clock, launcher, supervisor, pid_file }
    }

    /// The key of account `id` that runs as user `uid`.
    pub fn account(&self, id: i64, uid: u32) -> AccountKey {
        account(id, uid)
    }

    /// Attaches a socket and wraps it.
    pub async fn try_client(&self, account: AccountKey) -> Result<Client, AttachError> {
        let (handle, rx) = self.supervisor.attach(account).await?;
        Ok(Client::new(handle, rx))
    }

    /// Attaches a socket and wraps it; the attach must succeed.
    pub async fn client(&self, account: AccountKey) -> Client {
        match self.try_client(account).await {
            Ok(client) => client,
            Err(e) => panic!("attach failed: {e}"),
        }
    }

    /// The process ids the fixed shell wrote, in order.
    pub fn shell_pids(&self) -> Vec<i32> {
        std::fs::read_to_string(&self.pid_file)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    }
}

/// The key of account `id` that runs as user `uid`.
pub fn account(id: i64, uid: u32) -> AccountKey {
    AccountKey { id: AccountId::from_raw(id), uid }
}

/// Whether the process `pid` still exists (a zombie that was not reaped counts as existing).
pub fn process_exists(pid: i32) -> bool {
    let Some(pid) = rustix::process::Pid::from_raw(pid) else { return false };
    rustix::process::test_kill_process(pid).is_ok()
}

/// What a command answered.
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    /// `{"id":N,"ok":...}` with the value.
    Ok(Value),
    /// `{"id":N,"err":...}` with the value.
    Err(Value),
}

/// The request number of a text message, if it is an answer.
pub fn answer_id(text: &str) -> Option<u64> {
    let value: Value = serde_json::from_str(text).ok()?;
    value.get("id")?.as_u64()
}

/// Both halves of an attached socket, read the way a browser would.
pub struct Client {
    pub handle: SocketHandle,
    pub rx: SocketRx,
    next: u64,
    backlog: VecDeque<Outbound>,
    log: Vec<String>,
}

/// One line for a message: the kind and the size, never the content.
pub fn describe(message: &Outbound) -> String {
    match message {
        Outbound::Text(text) => {
            let event = serde_json::from_str::<Value>(text)
                .ok()
                .and_then(|v| v.get("event").and_then(Value::as_str).map(str::to_owned));
            match (event, answer_id(text)) {
                (Some(name), _) => format!("Text event {name} ({} bytes)", text.len()),
                (None, Some(id)) => format!("Text answer {id} ({} bytes)", text.len()),
                (None, None) => format!("Text ({} bytes)", text.len()),
            }
        }
        Outbound::Binary(bytes) => format!("Binary ({} bytes)", bytes.len()),
        Outbound::Close { code, .. } => format!("Close {code}"),
        Outbound::Ses(_) => "Ses frame".to_owned(),
    }
}

impl Client {
    pub fn new(handle: SocketHandle, rx: SocketRx) -> Client {
        Client { handle, rx, next: 1, backlog: VecDeque::new(), log: Vec::new() }
    }

    /// One line for every message taken from the socket so far, in order.
    pub fn log(&self) -> &[String] {
        &self.log
    }

    /// The number of the socket.
    pub fn id(&self) -> u32 {
        self.handle.id()
    }

    /// Submits a command and returns its request number.
    pub fn send(&mut self, command: &str, args: &str) -> u64 {
        let request = self.next;
        self.next += 1;
        self.handle.submit(request, command, args).unwrap_or_else(|e| panic!("submit {command}: {e:?}"));
        request
    }

    /// The next message, from what was set aside first, or `None` when none came within `timeout`.
    pub async fn next_message(&mut self, timeout: Duration) -> Option<Outbound> {
        if let Some(message) = self.backlog.pop_front() {
            return Some(message);
        }
        match tokio::time::timeout(timeout, self.rx.recv()).await {
            Ok(Some(message)) => {
                self.log.push(describe(&message));
                Some(message)
            }
            Ok(None) | Err(_) => None,
        }
    }

    /// Reads until `wanted` accepts a message, setting every other message aside (they stay for later reads).
    pub async fn wait_for(&mut self, timeout: Duration, wanted: impl Fn(&Outbound) -> bool) -> Option<Outbound> {
        if let Some(at) = self.backlog.iter().position(&wanted) {
            return self.backlog.remove(at);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, self.rx.recv()).await {
                Ok(Some(message)) => {
                    self.log.push(describe(&message));
                    if wanted(&message) {
                        return Some(message);
                    }
                    self.backlog.push_back(message);
                }
                Ok(None) | Err(_) => return None,
            }
        }
    }

    /// The answer to request `request`.
    pub async fn wait_reply(&mut self, request: u64) -> Reply {
        let found = self
            .wait_for(PATIENCE, |m| matches!(m, Outbound::Text(t) if answer_id(t) == Some(request)))
            .await
            .unwrap_or_else(|| panic!("no answer to request {request} within {PATIENCE:?}"));
        let Outbound::Text(text) = found else { unreachable!() };
        let value: Value = serde_json::from_str(&text).expect("an answer is JSON");
        match (value.get("ok"), value.get("err")) {
            (Some(ok), None) => Reply::Ok(ok.clone()),
            (None, Some(err)) => Reply::Err(err.clone()),
            _ => panic!("an answer has exactly one of ok and err: {text}"),
        }
    }

    /// Sends a command and waits for its answer.
    pub async fn call(&mut self, command: &str, args: &str) -> Reply {
        let request = self.send(command, args);
        self.wait_reply(request).await
    }

    /// Sends a command that must succeed and returns its result.
    pub async fn ok(&mut self, command: &str, args: &str) -> Value {
        match self.call(command, args).await {
            Reply::Ok(value) => value,
            Reply::Err(value) => panic!("{command} failed: {value}"),
        }
    }

    /// The next event named `name` whose payload satisfies `wanted`, as the whole message `{"event","payload"}` parsed.
    pub async fn event_where(&mut self, name: &str, timeout: Duration, wanted: impl Fn(&Value) -> bool) -> Option<Value> {
        let found = self
            .wait_for(timeout, |m| match m {
                Outbound::Text(t) => serde_json::from_str::<Value>(t)
                    .ok()
                    .is_some_and(|v| v.get("event").and_then(Value::as_str) == Some(name) && wanted(&v["payload"])),
                _ => false,
            })
            .await?;
        let Outbound::Text(text) = found else { unreachable!() };
        serde_json::from_str(&text).ok()
    }

    /// The next event named `name`.
    pub async fn next_event(&mut self, name: &str, timeout: Duration) -> Option<Value> {
        self.event_where(name, timeout, |_| true).await
    }

    /// The next binary message.
    pub async fn binary(&mut self, timeout: Duration) -> Option<Vec<u8>> {
        match self.wait_for(timeout, |m| matches!(m, Outbound::Binary(_))).await? {
            Outbound::Binary(bytes) => Some(bytes),
            _ => None,
        }
    }

    /// The code of the close message, once it comes.
    pub async fn close_code(&mut self, timeout: Duration) -> Option<u16> {
        match self.wait_for(timeout, |m| matches!(m, Outbound::Close { .. })).await? {
            Outbound::Close { code, .. } => Some(code),
            _ => None,
        }
    }

    /// Every text message that is waiting right now, without waiting for more.
    pub async fn drain_texts(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(message) = self.next_message(Duration::from_millis(50)).await {
            if let Outbound::Text(t) = message {
                out.push(t);
            }
        }
        out
    }
}

// ---- the scripted worker -----------------------------------------------------------------------------------------------

/// A gate the reader thread of a script waits at while the test stands in for a worker that does not read.
#[derive(Default)]
struct Pause {
    held: Mutex<bool>,
    changed: Condvar,
}

impl Pause {
    fn pass(&self) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        while *held {
            held = self.changed.wait(held).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn set(&self, held: bool) {
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = held;
        self.changed.notify_all();
    }
}

/// The worker end of one launch, played by the test.
pub struct ScriptHandle {
    writer: Arc<Mutex<StdStream>>,
    frames: Receiver<Result<Frame, String>>,
    seen: Vec<Frame>,
    ended: Arc<watch::Sender<Option<i32>>>,
    /// Set by `close_link`: the closed link is then not read as the end of the process.
    quiet: Arc<AtomicBool>,
    pause: Arc<Pause>,
    pub uid: u32,
    pub mode: WorkerMode,
}

impl ScriptHandle {
    /// The next frame the supervisor sent, or `None` when none came within `timeout`.
    pub fn next(&mut self, timeout: Duration) -> Option<Frame> {
        match self.frames.recv_timeout(timeout) {
            Ok(Ok(frame)) => {
                self.seen.push(frame.clone());
                Some(frame)
            }
            Ok(Err(e)) => panic!("the supervisor wrote a frame the script cannot read: {e}"),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => None,
        }
    }

    /// Reads frames until `wanted` accepts one (or the time is up).
    pub fn wait_for(&mut self, what: &str, timeout: Duration, wanted: impl Fn(&Frame) -> bool) -> Frame {
        if let Some(found) = self.seen.iter().find(|f| wanted(f)) {
            return found.clone();
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.next(left) {
                Some(frame) if wanted(&frame) => return frame,
                Some(_) => {}
                None => panic!("waited {timeout:?} for {what}; the supervisor wrote {:?}", self.seen),
            }
        }
    }

    /// Reads whatever the supervisor sends for `quiet` more, then returns everything seen so far.
    pub fn settle(&mut self, quiet: Duration) -> &[Frame] {
        while self.next(quiet).is_some() {}
        &self.seen
    }

    /// Every frame seen so far.
    pub fn seen(&self) -> &[Frame] {
        &self.seen
    }

    /// Every command seen so far, in order.
    pub fn cmds(&self) -> Vec<CmdFrame> {
        self.seen
            .iter()
            .filter_map(|f| match f {
                Frame::Cmd(c) => Some(c.clone()),
                _ => None,
            })
            .collect()
    }

    /// Every control seen so far, in order.
    pub fn ctls(&self) -> Vec<Ctl> {
        self.seen
            .iter()
            .filter_map(|f| match f {
                Frame::Ctl(c) => Some(c.clone()),
                _ => None,
            })
            .collect()
    }

    /// Writes one frame to the supervisor.
    pub fn send(&self, frame: Frame) {
        let mut writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        write_frame(&mut *writer, &frame).expect("the frame is written to the supervisor");
    }

    /// Writes raw bytes to the supervisor, for the frames a well-behaved worker never sends.
    pub fn send_raw(&self, bytes: &[u8]) {
        let mut writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        writer.write_all(bytes).expect("the bytes are written to the supervisor");
        let _ = writer.flush();
    }

    /// Stops reading what the supervisor writes.
    pub fn pause(&self) {
        self.pause.set(true);
    }

    /// Reads again.
    pub fn resume(&self) {
        self.pause.set(false);
    }

    /// Closes the link as a worker that ended would.
    pub fn close(&self) {
        let writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = writer.shutdown(std::net::Shutdown::Both);
        let _ = self.ended.send(Some(0));
    }

    /// Closes the link without saying that the process ended, as a worker that died in the middle of a frame leaves it.
    pub fn close_link(&self) {
        self.quiet.store(true, Ordering::SeqCst);
        let writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = writer.shutdown(std::net::Shutdown::Both);
    }

    /// Says the process ended (the helper's exit event) without closing the link.
    pub fn exit(&self, status: i32) {
        let _ = self.ended.send(Some(status));
    }
}

/// A launcher whose workers are played by the test.
pub struct ScriptedLauncher {
    state: Mutex<ScriptedState>,
    ready: Condvar,
    auto_ready: Mutex<bool>,
    send_buffer: Mutex<Option<usize>>,
}

#[derive(Default)]
struct ScriptedState {
    launches: usize,
    scripts: VecDeque<ScriptHandle>,
    kills: Vec<u32>,
    requests: Vec<(u32, WorkerMode)>,
    refuse: Option<String>,
}

impl ScriptedLauncher {
    pub fn new() -> Arc<ScriptedLauncher> {
        Arc::new(ScriptedLauncher {
            state: Mutex::new(ScriptedState::default()),
            ready: Condvar::new(),
            auto_ready: Mutex::new(true),
            send_buffer: Mutex::new(None),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, ScriptedState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many workers were started.
    pub fn launches(&self) -> usize {
        self.state().launches
    }

    /// The user id of every `kill_uid`.
    pub fn kills(&self) -> Vec<u32> {
        self.state().kills.clone()
    }

    /// The user id and mode of every launch request.
    pub fn requests(&self) -> Vec<(u32, WorkerMode)> {
        self.state().requests.clone()
    }

    /// Whether the script answers `Init` with `Ready` by itself (the default).
    pub fn set_auto_ready(&self, on: bool) {
        *self.auto_ready.lock().unwrap_or_else(PoisonError::into_inner) = on;
    }

    /// Makes the next launches fail with this sentence (or work again with `None`).
    pub fn refuse_with(&self, sentence: Option<&str>) {
        self.state().refuse = sentence.map(str::to_owned);
    }

    /// Sets a small send buffer on the supervisor's end of every link made from now on.
    pub fn set_send_buffer(&self, bytes: usize) {
        *self.send_buffer.lock().unwrap_or_else(PoisonError::into_inner) = Some(bytes);
    }

    /// The script of the next launch that nobody took yet, waiting for it.
    pub fn script(&self, timeout: Duration) -> ScriptHandle {
        let deadline = Instant::now() + timeout;
        let mut state = self.state();
        loop {
            if let Some(script) = state.scripts.pop_front() {
                return script;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "no worker was launched within {timeout:?}");
            state = self.ready.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    fn start(&self, request: LaunchRequest) -> Result<LaunchedWorker, LaunchError> {
        if let Some(sentence) = self.state().refuse.clone() {
            return Err(LaunchError::new(sentence));
        }
        let (ours, theirs) = StdStream::pair().map_err(|_| LaunchError::new("no socket pair"))?;
        if let Some(bytes) = *self.send_buffer.lock().unwrap_or_else(PoisonError::into_inner) {
            let _ = rustix::net::sockopt::set_socket_send_buffer_size(&ours, bytes);
        }
        ours.set_nonblocking(true).map_err(|_| LaunchError::new("not non-blocking"))?;
        let (ended_tx, ended) = watch::channel(None);
        let ended_tx = Arc::new(ended_tx);
        let writer = Arc::new(Mutex::new(theirs.try_clone().map_err(|_| LaunchError::new("no clone"))?));
        let (tx, frames) = channel();
        let pause = Arc::new(Pause::default());
        let auto = *self.auto_ready.lock().unwrap_or_else(PoisonError::into_inner);
        // A serving worker that sees its link close ends; a wipe worker has no use for its link, and the test says when it ends.
        let serving = request.mode == WorkerMode::Serve;
        let quiet = Arc::new(AtomicBool::new(false));
        {
            let quiet = quiet.clone();
            let (mut reading, writer, pause, ended_tx) = (theirs, writer.clone(), pause.clone(), ended_tx.clone());
            std::thread::Builder::new()
                .name("script-reader".into())
                .spawn(move || {
                    loop {
                        pause.pass();
                        match read_frame(&mut reading) {
                            Ok(Some(frame)) => {
                                if auto && matches!(frame, Frame::Ctl(Ctl::Init { .. })) {
                                    let mut w = writer.lock().unwrap_or_else(PoisonError::into_inner);
                                    let _ = write_frame(&mut *w, &Frame::Note(Note::Ready));
                                }
                                if tx.send(Ok(frame)).is_err() {
                                    return;
                                }
                            }
                            Ok(None) => {
                                if serving && !quiet.load(Ordering::SeqCst) {
                                    let _ = ended_tx.send(Some(0));
                                }
                                return;
                            }
                            Err(e) => {
                                let _ = tx.send(Err(e.to_string()));
                                return;
                            }
                        }
                    }
                })
                .map_err(|_| LaunchError::new("no reader thread"))?;
        }
        let link = tokio::net::UnixStream::from_std(ours).map_err(|_| LaunchError::new("not adopted"))?;
        let mut state = self.state();
        state.launches += 1;
        state.requests.push((request.account.uid, request.mode));
        state.scripts.push_back(ScriptHandle {
            writer,
            frames,
            seen: Vec::new(),
            ended: ended_tx,
            quiet,
            pause,
            uid: request.account.uid,
            mode: request.mode,
        });
        self.ready.notify_all();
        Ok(LaunchedWorker { link, ended })
    }
}

impl WorkerLauncher for ScriptedLauncher {
    fn launch(&self, request: LaunchRequest) -> Launch<'_, LaunchedWorker> {
        Box::pin(async move { self.start(request) })
    }

    fn kill_uid(&self, uid: u32) -> Launch<'_, ()> {
        Box::pin(async move {
            self.state().kills.push(uid);
            Ok(())
        })
    }
}

/// A supervisor over scripted workers.
pub struct ScriptEnv {
    pub clock: Arc<ManualClock>,
    pub launcher: Arc<ScriptedLauncher>,
    pub supervisor: Supervisor,
}

impl ScriptEnv {
    pub async fn start() -> ScriptEnv {
        ScriptEnv::start_with(SupervisorSettings::default()).await
    }

    pub async fn start_with(settings: SupervisorSettings) -> ScriptEnv {
        let clock = Arc::new(ManualClock::new(START));
        let launcher = ScriptedLauncher::new();
        let supervisor = Supervisor::new(launcher.clone(), settings, clock.clone());
        ScriptEnv { clock, launcher, supervisor }
    }

    /// Attaches a socket; the first attach of a person also takes the script of the worker it started.
    pub async fn client(&self, account: AccountKey) -> Client {
        let (handle, rx) = self.supervisor.attach(account).await.unwrap_or_else(|e| panic!("attach failed: {e}"));
        Client::new(handle, rx)
    }
}

/// The `Init` the supervisor must send first.
pub fn expected_init(max_panes: u32, max_scrollback_lines: u32, max_image_mib: u32) -> Ctl {
    Ctl::Init {
        version: PROTOCOL_VERSION,
        max_panes,
        max_scrollback_lines,
        max_image_mib,
        sessions: SessionLimits::DEFAULT,
    }
}


// ---- the fake spawn helper ---------------------------------------------------------------------------------------------

/// What the fake helper does with the next request instead of the normal answer.
pub enum Next {
    /// The normal answer, but with exactly these descriptors (none, or too many) instead of the worker's end.
    WithFds(Vec<OwnedFd>),
}

#[derive(Default)]
struct FakeConfig {
    /// Answer nothing.
    silent: bool,
    /// Keep this many answers and send them in the opposite order.
    hold: usize,
    held: Vec<(Vec<u8>, Option<OwnedFd>)>,
    next: VecDeque<Next>,
    /// Workers do not answer `Init` with `Ready`.
    no_ready: bool,
    /// The status of the exit event that follows a wipe.
    wipe_status: i32,
}

struct FakeInner {
    control: Mutex<Option<Arc<OwnedFd>>>,
    requests: Mutex<Vec<Request>>,
    config: Mutex<FakeConfig>,
    workers: Mutex<HashMap<i32, (u32, StdStream)>>,
    next_pid: AtomicI32,
    stop: AtomicBool,
    send_lock: Mutex<()>,
    _turn: Turn,
}

/// On systems other than Linux only one fake helper lives at a time. The tests pass descriptors through datagram sockets
/// there, and macOS closes sockets of other threads' pairs while the descriptors of a test that ended are collected
/// (a link shows end of stream a moment after it was handed out, only when helper tests run side by side). On Linux the
/// tests run side by side, as everywhere else.
struct Turn;

#[cfg(not(target_os = "linux"))]
static TURN: AtomicBool = AtomicBool::new(false);

impl Turn {
    fn take() -> Turn {
        #[cfg(not(target_os = "linux"))]
        while TURN.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            std::thread::sleep(Duration::from_millis(2));
        }
        Turn
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        #[cfg(not(target_os = "linux"))]
        TURN.store(false, Ordering::SeqCst);
    }
}

/// The socket type of the control socket: the helper's own, where the system has it.
pub fn datagram_type() -> rustix::net::SocketType {
    if cfg!(target_os = "linux") { rustix::net::SocketType::SEQPACKET } else { rustix::net::SocketType::DGRAM }
}

/// A pair of connected sockets of the control socket's type.
pub fn datagram_pair() -> (OwnedFd, OwnedFd) {
    let (a, b) = rustix::net::socketpair(rustix::net::AddressFamily::UNIX, datagram_type(), rustix::net::SocketFlags::empty(), None)
        .expect("a socket pair");
    for fd in [&a, &b] {
        rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::CLOEXEC).expect("close-on-exec");
    }
    (a, b)
}

/// A stand-in for the spawn helper: a thread on the other end of a datagram socket pair that speaks the helper's own types.
/// It records every request and answers by default the way the helper does (a worker is a thread that says `Ready` to `Init`).
pub struct FakeHelper {
    inner: Arc<FakeInner>,
}

impl FakeHelper {
    /// A fake and the end of the socket that the client takes.
    pub fn start() -> (FakeHelper, OwnedFd) {
        let turn = Turn::take();
        let (client_end, helper_end) = datagram_pair();
        let _ = rustix::net::sockopt::set_socket_timeout(&helper_end, rustix::net::sockopt::Timeout::Recv, Some(Duration::from_millis(50)));
        // Datagram sockets of some systems queue very little by default; the tests send hundreds of requests at once.
        for fd in [&helper_end, &client_end] {
            let _ = rustix::net::sockopt::set_socket_send_buffer_size(fd, 1 << 20);
            let _ = rustix::net::sockopt::set_socket_recv_buffer_size(fd, 1 << 20);
        }
        let inner = Arc::new(FakeInner {
            control: Mutex::new(Some(Arc::new(helper_end))),
            requests: Mutex::new(Vec::new()),
            config: Mutex::new(FakeConfig::default()),
            workers: Mutex::new(HashMap::new()),
            next_pid: AtomicI32::new(4242),
            stop: AtomicBool::new(false),
            send_lock: Mutex::new(()),
            _turn: turn,
        });
        let thread_inner = inner.clone();
        std::thread::Builder::new().name("fake-helper".into()).spawn(move || thread_inner.serve()).expect("the fake helper thread");
        (FakeHelper { inner }, client_end)
    }

    /// Every request the fake received, in order.
    pub fn requests(&self) -> Vec<Request> {
        self.inner.requests.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The user id and mode of every `spawn`, in order.
    pub fn spawns(&self) -> Vec<(u64, Mode)> {
        self.requests()
            .into_iter()
            .filter_map(|r| match r {
                Request::Spawn { uid, mode } => Some((uid, mode)),
                _ => None,
            })
            .collect()
    }

    /// The user id of every `kill_uid`.
    pub fn kills(&self) -> Vec<u64> {
        self.requests()
            .into_iter()
            .filter_map(|r| match r {
                Request::KillUid { uid, .. } => Some(uid),
                _ => None,
            })
            .collect()
    }

    fn config(&self) -> std::sync::MutexGuard<'_, FakeConfig> {
        self.inner.config.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Answers nothing from now on (or again).
    pub fn set_silent(&self, silent: bool) {
        self.config().silent = silent;
    }

    /// Keeps the next `count` answers and sends them together in the opposite order.
    pub fn hold_answers(&self, count: usize) {
        self.config().hold = count;
    }

    /// The next request is answered the normal way but with these descriptors.
    pub fn override_next(&self, next: Next) {
        self.config().next.push_back(next);
    }

    /// Workers started from now on stay silent after `Init`.
    pub fn set_no_ready(&self, on: bool) {
        self.config().no_ready = on;
    }

    /// The exit status that follows a wipe worker.
    pub fn set_wipe_status(&self, status: i32) {
        self.config().wipe_status = status;
    }

    /// Sends one datagram, with descriptors, whatever the client expects.
    pub fn send_raw(&self, bytes: &[u8], fds: Vec<OwnedFd>) {
        self.inner.send(bytes, &fds);
    }

    /// The worker `pid` ends with `status`: the helper sends its event and the worker's link closes.
    pub fn exit(&self, pid: i32, status: i32) {
        let found = self.inner.workers.lock().unwrap_or_else(PoisonError::into_inner).remove(&pid);
        if let Some((uid, link)) = found {
            let _ = link.shutdown(std::net::Shutdown::Both);
            self.inner.send(&ExitedEvent::new(pid, uid, status, None).to_bytes(), &[]);
        }
    }

    /// The process numbers of the workers that run.
    pub fn pids(&self) -> Vec<i32> {
        let mut pids: Vec<i32> = self.inner.workers.lock().unwrap_or_else(PoisonError::into_inner).keys().copied().collect();
        pids.sort_unstable();
        pids
    }

    /// The helper goes away: its end of the socket closes.
    pub fn close(&self) {
        // A datagram socket has no end of stream on every system: an empty message says "closed", as the helper's own
        // read of an empty message does.
        self.inner.send(&[], &[]);
        self.inner.stop.store(true, Ordering::SeqCst);
        self.inner.control.lock().unwrap_or_else(PoisonError::into_inner).take();
        // The thread notices at its next timeout and drops its own handle.
        std::thread::sleep(Duration::from_millis(150));
    }
}

impl Drop for FakeHelper {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        self.inner.control.lock().unwrap_or_else(PoisonError::into_inner).take();
        let workers: Vec<_> = self.inner.workers.lock().unwrap_or_else(PoisonError::into_inner).drain().collect();
        for (_, (_, link)) in workers {
            let _ = link.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl FakeInner {
    fn control(&self) -> Option<Arc<OwnedFd>> {
        self.control.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn serve(self: Arc<Self>) {
        let Some(control) = self.control() else { return };
        let mut buf = vec![0u8; 16 * 1024];
        while !self.stop.load(Ordering::SeqCst) {
            let received = {
                let mut iov = [IoSliceMut::new(&mut buf)];
                rustix::net::recvmsg(control.as_fd(), &mut iov, &mut rustix::net::RecvAncillaryBuffer::default(), rustix::net::RecvFlags::empty())
            };
            match received {
                Ok(msg) if msg.bytes == 0 => return,
                Ok(msg) => {
                    let bytes = buf[..msg.bytes].to_vec();
                    self.handle(&bytes);
                }
                Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {}
                Err(_) => return,
            }
        }
    }

    /// One datagram out, with descriptors.
    fn send(&self, bytes: &[u8], fds: &[OwnedFd]) {
        let _guard = self.send_lock.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(control) = self.control() else { return };
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(2))];
        let mut ancillary = rustix::net::SendAncillaryBuffer::new(&mut space);
        let borrowed: Vec<_> = fds.iter().map(|f| f.as_fd()).collect();
        if !borrowed.is_empty() {
            ancillary.push(rustix::net::SendAncillaryMessage::ScmRights(&borrowed));
        }
        let _ = rustix::net::sendmsg(control.as_fd(), &[IoSlice::new(bytes)], &mut ancillary, rustix::net::SendFlags::empty());
    }

    /// An answer, with the rules of the config: silent, held and reversed, or with other descriptors.
    fn answer(&self, bytes: Vec<u8>, fd: Option<OwnedFd>) {
        let mut config = self.config.lock().unwrap_or_else(PoisonError::into_inner);
        if config.silent {
            return;
        }
        let (bytes, fds) = match config.next.pop_front() {
            Some(Next::WithFds(fds)) => (bytes, fds),
            None => (bytes, fd.into_iter().collect()),
        };
        if config.hold > 0 {
            // Only one descriptor per held answer in the tests that hold.
            config.held.push((bytes, fds.into_iter().next()));
            if config.held.len() == config.hold {
                let held: Vec<_> = config.held.drain(..).rev().collect();
                config.hold = 0;
                drop(config);
                for (bytes, fd) in held {
                    self.send(&bytes, &fd.into_iter().collect::<Vec<_>>());
                }
            }
            return;
        }
        drop(config);
        self.send(&bytes, &fds);
    }

    fn handle(&self, bytes: &[u8]) {
        let Ok((id, request)) = protocol::parse(bytes) else { return };
        self.requests.lock().unwrap_or_else(PoisonError::into_inner).push(request.clone());
        match request {
            Request::Ping => {
                let workers = self.workers.lock().unwrap_or_else(PoisonError::into_inner).len() as u32;
                self.answer(Answer::pong(id, workers).to_bytes(), None);
            }
            Request::KillUid { uid, .. } => {
                self.answer(Answer::signalled(id, 1).to_bytes(), None);
                let killed: Vec<(i32, StdStream)> = {
                    let mut workers = self.workers.lock().unwrap_or_else(PoisonError::into_inner);
                    let pids: Vec<i32> = workers.iter().filter(|(_, (u, _))| u64::from(*u) == uid).map(|(p, _)| *p).collect();
                    pids.into_iter().filter_map(|p| workers.remove(&p).map(|(_, link)| (p, link))).collect()
                };
                for (pid, link) in killed {
                    let _ = link.shutdown(std::net::Shutdown::Both);
                    self.send(&ExitedEvent::new(pid, uid as u32, 137, Some(9)).to_bytes(), &[]);
                }
            }
            Request::RemoveHome { .. } => self.answer(Answer::done(id).to_bytes(), None),
            Request::Spawn { uid, mode } => {
                let pid = self.next_pid.fetch_add(1, Ordering::SeqCst);
                let (ours, theirs) = StdStream::pair().expect("a worker socket pair");
                let keep = theirs.try_clone().expect("a clone");
                self.workers.lock().unwrap_or_else(PoisonError::into_inner).insert(pid, (uid as u32, keep));
                let (no_ready, wipe_status) = {
                    let config = self.config.lock().unwrap_or_else(PoisonError::into_inner);
                    (config.no_ready, config.wipe_status)
                };
                if mode == Mode::Serve {
                    std::thread::Builder::new()
                        .name(format!("fake-worker-{pid}"))
                        .spawn(move || scripted_worker(theirs, no_ready))
                        .expect("a fake worker thread");
                    self.answer(Answer::spawned(id, pid).to_bytes(), Some(OwnedFd::from(ours)));
                } else {
                    // A wipe worker does its work and ends: the helper reports the status.
                    drop(theirs);
                    self.answer(Answer::spawned(id, pid).to_bytes(), Some(OwnedFd::from(ours)));
                    self.workers.lock().unwrap_or_else(PoisonError::into_inner).remove(&pid);
                    self.send(&ExitedEvent::new(pid, uid as u32, wipe_status, None).to_bytes(), &[]);
                }
            }
        }
    }
}

/// A worker that says `Ready` to `Init` and ends at `Shutdown`.
fn scripted_worker(mut link: StdStream, no_ready: bool) {
    loop {
        match read_frame(&mut link) {
            Ok(Some(Frame::Ctl(Ctl::Init { .. }))) if !no_ready => {
                let _ = write_frame(&mut link, &Frame::Note(Note::Ready));
            }
            Ok(Some(Frame::Ctl(Ctl::Shutdown))) => {
                let _ = link.shutdown(std::net::Shutdown::Both);
                return;
            }
            Ok(Some(_)) => {}
            _ => return,
        }
    }
}

/// A supervisor over the spawn-helper launcher and a fake helper.
pub struct HelperEnv {
    pub clock: Arc<ManualClock>,
    pub fake: FakeHelper,
    pub launcher: Arc<HelperLauncher>,
    pub supervisor: Supervisor,
}

impl HelperEnv {
    pub async fn start() -> HelperEnv {
        HelperEnv::start_with(SupervisorSettings::default(), HelperTimeouts::default()).await
    }

    pub async fn start_with(settings: SupervisorSettings, timeouts: HelperTimeouts) -> HelperEnv {
        let (fake, end) = FakeHelper::start();
        let client = HelperClient::from_socket(end, timeouts).expect("the client");
        let launcher = Arc::new(HelperLauncher::new(client));
        let clock = Arc::new(ManualClock::new(START));
        let supervisor = Supervisor::new(launcher.clone(), settings, clock.clone());
        HelperEnv { clock, fake, launcher, supervisor }
    }

    pub async fn client(&self, account: AccountKey) -> Client {
        let (handle, rx) = self.supervisor.attach(account).await.unwrap_or_else(|e| panic!("attach failed: {e}"));
        Client::new(handle, rx)
    }
}
