//! The supervisor driven in a test: a scratch directory, a manual clock, a launcher and a client that reads what a browser
//! socket would.
//!
//! `SupEnv::start()` builds a supervisor over an `InProcessLauncher` (real workers on threads, `/bin/sh` as the fixed shell).
//! `ScriptedLauncher` builds one over workers that the test plays itself, frame by frame. `Client` wraps the two halves of an
//! attached socket and reads replies and events with a time limit. Tests pull this file in with
//! `#[path = "support/sup_env.rs"] mod sup_env;` next to `#[path = "support/scratch.rs"] mod scratch;`.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::Write;
use std::os::unix::net::UnixStream as StdStream;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use holzwave_host::state::ShellSpec;
use holzwave_server::clock::ManualClock;
use holzwave_server::store::AccountId;
use holzwave_server::supervisor::{
    AccountKey, AttachError, InProcessLauncher, Launch, LaunchError, LaunchRequest, LaunchedWorker, Outbound, SocketHandle,
    SocketRx, Supervisor, SupervisorSettings, WorkerLauncher,
};
use holzwave_server::wire::{CmdFrame, Ctl, Frame, Note, PROTOCOL_VERSION, read_frame, write_frame};
use holzwave_server::worker::runtime::WorkerMode;
use serde_json::Value;
use tokio::sync::watch;

use crate::scratch::Scratch;

/// How long a test waits for anything the supervisor should do.
pub const PATIENCE: Duration = Duration::from_secs(10);

/// What a `SupEnv` is started with.
#[derive(Default)]
pub struct SupOptions {
    /// Replaces the default settings (the pane cap of 64 and the default times).
    pub settings: Option<SupervisorSettings>,
}

/// A supervisor over real in-process workers.
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
        let shell = ShellSpec::new("/bin/sh", &["-c", &script]);
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
        {
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
                                let _ = ended_tx.send(Some(0));
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
pub fn expected_init(max_panes: u32, max_scrollback_lines: u32) -> Ctl {
    Ctl::Init { version: PROTOCOL_VERSION, max_panes, max_scrollback_lines }
}
