//! The table of running workers and the people they belong to.
//!
//! One person has at most one worker. `Supervisor::attach` finds it or starts it (once, however many attaches arrive at the
//! same time), sends the caps of the settings, waits for the worker to say it is ready and joins the browser socket to it.
//! Every worker has three tasks: a reader that checks and routes what the worker says, a single writer that sends the frames
//! in the order they were queued, and a watcher that notices the end of the process. Workers of different people share
//! nothing but the launcher and the counters.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::runtime::Handle;
use tokio::sync::{Notify, oneshot, watch};
use tokio_util::codec::Decoder;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::clock::Clock;
use crate::limits;
use crate::log::{self, Level};
use crate::store::AccountId;
use crate::supervisor::fanout::{self, Counters, Fault, Owners, Synthetic};
use crate::supervisor::launcher::{LaunchRequest, LaunchedWorker, WorkerLauncher};
use crate::supervisor::socket::{self, Attached, SocketCore, SocketHandle, SocketRx, SubmitError};
use crate::wire::{self, CmdFrame, Ctl, Frame, Note, PROTOCOL_VERSION, StoreState};
use crate::worker::runtime::WorkerMode;

/// The close code for a socket whose worker ended.
pub const CLOSE_WORKER_ENDED: u16 = 1012;

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
    /// Messages for a socket of a person that is not attached any more.
    pub dropped_unknown_socket: u64,
    /// Frame events dropped because a socket's queue was full.
    pub dropped_frames: u64,
    /// Answers to requests the supervisor made up itself, dropped on purpose.
    pub dropped_synthetic: u64,
    /// Sockets closed with 1013 because their queue was full.
    pub overflow_closes: u64,
    /// Workers that were declared dead because of what they sent or because the link broke.
    pub worker_deaths: u64,
}

/// What the supervisor knows about one person.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountStats {
    /// Whether a worker runs.
    pub worker_running: bool,
    /// Sockets attached now.
    pub sockets: usize,
    /// The bytes waiting in the queues of the sockets.
    pub queued_bytes: usize,
    /// The frames waiting for the worker's writer.
    pub worker_queue_frames: usize,
    /// The bytes waiting for the worker's writer.
    pub worker_queue_bytes: usize,
    /// The sizes remembered for panes that another socket owns.
    pub remembered_sizes: usize,
    /// The panes that have an owner.
    pub owned_panes: usize,
}

// ---- the queue of frames to a worker ---------------------------------------------------------------------------------

struct WorkQueueState {
    /// Whole frames, length prefix included.
    items: VecDeque<Zeroizing<Vec<u8>>>,
    /// The frames and bytes queued or being written (counted until the write is done).
    frames: usize,
    bytes: usize,
    closed: bool,
}

/// The frames waiting for the single writer of a worker, bounded in frames and in bytes.
struct WorkQueue {
    state: Mutex<WorkQueueState>,
    ready: Notify,
}

impl WorkQueue {
    fn new() -> WorkQueue {
        WorkQueue {
            state: Mutex::new(WorkQueueState { items: VecDeque::new(), frames: 0, bytes: 0, closed: false }),
            ready: Notify::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, WorkQueueState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues commands together, or none of them: `Busy` when they do not fit.
    fn push_commands(&self, frames: Vec<Zeroizing<Vec<u8>>>) -> Result<(), SubmitError> {
        let total: usize = frames.iter().map(|f| f.len()).sum();
        let mut state = self.lock();
        if state.closed {
            return Err(SubmitError::Closed);
        }
        if state.frames + frames.len() > limits::WORKER_QUEUE_FRAMES || state.bytes + total > limits::WORKER_QUEUE_BYTES {
            return Err(SubmitError::Busy);
        }
        state.frames += frames.len();
        state.bytes += total;
        state.items.extend(frames);
        drop(state);
        self.ready.notify_one();
        Ok(())
    }

    /// Queues a control frame, which may use a small reserve beyond the bounds of the commands.
    fn push_control(&self, frame: Zeroizing<Vec<u8>>) -> Result<(), SubmitError> {
        let mut state = self.lock();
        if state.closed {
            return Err(SubmitError::Closed);
        }
        if state.frames + 1 > limits::WORKER_QUEUE_FRAMES + limits::WORKER_QUEUE_CONTROL_RESERVE
            || state.bytes + frame.len() > limits::WORKER_QUEUE_BYTES + limits::WORKER_QUEUE_CONTROL_BYTES
        {
            return Err(SubmitError::Busy);
        }
        state.frames += 1;
        state.bytes += frame.len();
        state.items.push_back(frame);
        drop(state);
        self.ready.notify_one();
        Ok(())
    }

    /// The frames and bytes that wait or are being written.
    fn load(&self) -> (usize, usize) {
        let state = self.lock();
        (state.frames, state.bytes)
    }

    fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        state.items.clear();
        state.frames = 0;
        state.bytes = 0;
        drop(state);
        self.ready.notify_one();
    }

    async fn pop(&self) -> Option<Zeroizing<Vec<u8>>> {
        loop {
            {
                let mut state = self.lock();
                if state.closed {
                    return None;
                }
                if let Some(frame) = state.items.pop_front() {
                    return Some(frame);
                }
            }
            self.ready.notified().await;
        }
    }

    /// A frame of `len` bytes was written.
    fn done(&self, len: usize) {
        let mut state = self.lock();
        state.frames = state.frames.saturating_sub(1);
        state.bytes = state.bytes.saturating_sub(len);
    }
}

/// The frame as it goes on the link: the length prefix and the payload. The buffer is wiped when it is dropped.
fn encode(frame: &Frame) -> Result<Zeroizing<Vec<u8>>, SubmitError> {
    let mut out = Zeroizing::new(Vec::new());
    wire::write_frame(&mut *out, frame).map_err(|_| SubmitError::TooLarge)?;
    Ok(out)
}

// ---- the worker ------------------------------------------------------------------------------------------------------

struct WorkerState {
    sockets: HashMap<u32, Arc<SocketCore>>,
    next_socket: u32,
    /// Who owns the size of each pane.
    owners: Owners,
    /// The last request number the supervisor made up itself, counted from [`limits::SYNTHETIC_REQUEST_BASE`].
    synthetic: u64,
    /// When the last socket left (or the worker became ready with none), in unix seconds.
    idle_since: Option<i64>,
}

/// One running worker.
struct Worker {
    shared: Arc<Shared>,
    slot: Arc<Slot>,
    state: Mutex<WorkerState>,
    out: WorkQueue,
    cancel: CancellationToken,
    dead: AtomicBool,
    done: watch::Sender<bool>,
}

impl Worker {
    fn lock(&self) -> MutexGuard<'_, WorkerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    fn push_control(&self, control: Ctl) -> Result<(), SubmitError> {
        self.out.push_control(encode(&Frame::Ctl(control))?)
    }

    /// The frame of a `resize` the supervisor makes itself.
    fn synthetic_frame(state: &mut WorkerState, resize: &Synthetic) -> Result<Zeroizing<Vec<u8>>, SubmitError> {
        state.synthetic += 1;
        let json = fanout::command_json("resize", &resize.args());
        let request = limits::SYNTHETIC_REQUEST_BASE + state.synthetic;
        encode(&Frame::Cmd(CmdFrame { socket: resize.socket, request, json }))
    }

    /// The worker is gone: its sockets close, its record leaves the table and its tasks stop.
    fn die(self: &Arc<Self>) {
        if self.dead.swap(true, Ordering::SeqCst) {
            return;
        }
        Counters::bump(&self.shared.counters.worker_deaths);
        self.cancel.cancel();
        self.out.close();
        let sockets: Vec<Arc<SocketCore>> = self.lock().sockets.drain().map(|(_, s)| s).collect();
        for socket in sockets {
            socket.queue.close(CLOSE_WORKER_ENDED, "the worker ended");
        }
        {
            let mut current = self.slot.current();
            if current.as_ref().is_some_and(|w| Arc::ptr_eq(w, self)) {
                *current = None;
            }
        }
        self.slot.store.send_replace(None);
        self.done.send_replace(true);
    }

    /// Joins a new socket to this worker.
    fn add_socket(self: &Arc<Self>) -> Result<(SocketHandle, SocketRx), AttachError> {
        let mut state = self.lock();
        if self.is_dead() {
            return Err(AttachError::Launch("the worker ended while the socket attached".into()));
        }
        if state.sockets.len() >= self.shared.settings.sockets_per_person {
            return Err(AttachError::TooManySockets);
        }
        state.next_socket += 1;
        let id = state.next_socket;
        let core = Arc::new(SocketCore::new(id, self.shared.settings.socket_queue_bytes));
        state.sockets.insert(id, core.clone());
        state.idle_since = None;
        if self.push_control(Ctl::Attach { socket: id }).is_err() {
            state.sockets.remove(&id);
            return Err(AttachError::Launch("the worker is not taking frames".into()));
        }
        drop(state);
        let worker: Arc<dyn Attached> = self.clone();
        Ok(socket::halves(core, worker))
    }

    /// Takes sockets out of the table, tells the worker, and hands the panes they owned on. A socket that is not in the
    /// table (it left already) is skipped.
    fn drop_sockets(&self, state: &mut WorkerState, sockets: &[u32]) {
        for &socket in sockets {
            if state.sockets.remove(&socket).is_none() {
                continue;
            }
            if state.sockets.is_empty() {
                state.idle_since = Some(self.shared.clock.unix());
            }
            let _ = self.push_control(Ctl::Detach { socket });
            // The panes the socket owned go to the socket with the latest size, and that size is applied.
            for resize in state.owners.left(socket) {
                if let Ok(frame) = Self::synthetic_frame(state, &resize) {
                    let _ = self.out.push_commands(vec![frame]);
                }
            }
        }
    }

    /// A frame from the worker, already decoded. The error says what was wrong with it.
    fn handle(self: &Arc<Self>, frame: Frame, ready: &mut Option<oneshot::Sender<()>>) -> Result<(), Fault> {
        // Until the worker says it is ready it may only say that, or that it is ending.
        if ready.is_some() && !matches!(frame, Frame::Note(Note::Ready | Note::Exiting(_))) {
            return Err(Fault::OutOfOrder);
        }
        match frame {
            Frame::Cmd(_) | Frame::Ctl(_) => return Err(Fault::WrongDirection),
            Frame::Res(res) => {
                let mut state = self.lock();
                let overflowed = fanout::deliver_res(res, &state.sockets, &self.shared.counters);
                self.drop_sockets(&mut state, &overflowed);
            }
            Frame::Evt(evt) => {
                fanout::check_event_name(&evt.name)?;
                let mut state = self.lock();
                let overflowed = fanout::deliver_evt(evt, &state.sockets, &self.shared.counters);
                self.drop_sockets(&mut state, &overflowed);
            }
            Frame::Note(Note::Ready) => match ready.take() {
                Some(ready) => {
                    let _ = ready.send(());
                }
                None => return Err(Fault::OutOfOrder),
            },
            Frame::Note(Note::StoreState(state)) => {
                self.slot.store.send_replace(Some(state));
            }
            Frame::Note(Note::Exiting(_)) => self.die(),
            Frame::Note(Note::PaneCount(_) | Note::CtlDone { .. }) => {}
        }
        Ok(())
    }

    /// The worker broke the rules of the link: the person's processes are killed and the worker is declared dead, without
    /// letting a new worker start in between.
    async fn violation(self: &Arc<Self>, fault: Fault) {
        log::event(
            Level::Warn,
            "supervisor.worker_fault",
            &[("account", &self.slot.key.id.to_string()), ("fault", fault.word())],
        );
        // Held when no worker is being started, so that a new worker is not killed by this clean-up.
        let launching = self.slot.launch.try_lock().ok();
        self.die();
        let _ = self.shared.launcher.kill_uid(self.slot.key.uid).await;
        drop(launching);
    }
}

impl Attached for Worker {
    fn submit(&self, socket: u32, request: u64, command: &str, args: &str) -> Result<(), SubmitError> {
        if !fanout::check_command_name(command) {
            return Err(SubmitError::BadCommand);
        }
        if request >= limits::SYNTHETIC_REQUEST_BASE {
            return Err(SubmitError::BadRequest);
        }
        let mut state = self.lock();
        let Some(origin) = state.sockets.get(&socket).cloned() else { return Err(SubmitError::Closed) };
        let plan = state.owners.assess(socket, command, args);
        if plan.answer {
            // A resize from a socket that does not own the pane is answered here and goes no further.
            state.owners.commit(socket, &plan);
            if self.shared.counters.push(&origin, fanout::null_answer(request)) {
                self.drop_sockets(&mut state, &[socket]);
            }
            return Ok(());
        }
        let mut frames = Vec::with_capacity(3);
        if let Some(resize) = &plan.before {
            frames.push(Self::synthetic_frame(&mut state, resize)?);
        }
        let json = fanout::command_json(command, args);
        frames.push(encode(&Frame::Cmd(CmdFrame { socket, request, json }))?);
        if let Some(resize) = &plan.after {
            frames.push(Self::synthetic_frame(&mut state, resize)?);
        }
        self.out.push_commands(frames)?;
        state.owners.commit(socket, &plan);
        Ok(())
    }

    fn detach(&self, socket: u32) {
        let mut state = self.lock();
        self.drop_sockets(&mut state, &[socket]);
    }
}

/// Reads the link of one worker until it ends, and routes what it says.
async fn read_loop(worker: Arc<Worker>, mut read: OwnedReadHalf, ready: oneshot::Sender<()>) {
    let mut codec = wire::codec();
    let mut buf = Default::default();
    let mut ready = Some(ready);
    loop {
        // `Ok(None)` is the end of the link, `Err` is a frame the layout does not allow.
        let next: Result<Option<_>, Fault> = loop {
            match codec.decode(&mut buf) {
                Ok(Some(payload)) => break Ok(Some(payload)),
                Ok(None) => {}
                Err(_) => break Err(Fault::Malformed),
            }
            let read_some = tokio::select! {
                () = worker.cancel.cancelled() => return,
                n = read.read_buf(&mut buf) => n,
            };
            match read_some {
                Ok(0) => match codec.decode_eof(&mut buf) {
                    Ok(Some(payload)) => break Ok(Some(payload)),
                    Ok(None) => break Ok(None),
                    Err(_) => break Err(Fault::Malformed),
                },
                Err(_) => break Ok(None),
                Ok(_) => {}
            }
        };
        let outcome = match next {
            Ok(Some(payload)) => match Frame::from_payload(&payload) {
                Ok(frame) => worker.handle(frame, &mut ready),
                Err(_) => Err(Fault::Malformed),
            },
            Ok(None) => break,
            Err(fault) => Err(fault),
        };
        if let Err(fault) = outcome {
            worker.violation(fault).await;
            return;
        }
    }
    worker.die();
}

/// Writes the queued frames to the link, one after the other.
async fn write_loop(worker: Arc<Worker>, mut write: OwnedWriteHalf) {
    loop {
        let frame = tokio::select! {
            () = worker.cancel.cancelled() => return,
            frame = worker.out.pop() => frame,
        };
        let Some(frame) = frame else { return };
        match tokio::time::timeout(limits::WORKER_WRITE_TIMEOUT, write.write_all(&frame)).await {
            Ok(Ok(())) => worker.out.done(frame.len()),
            Ok(Err(_)) | Err(_) => break,
        }
    }
    worker.die();
}

/// Notices the end of the worker's process.
async fn watch_loop(worker: Arc<Worker>, mut ended: watch::Receiver<Option<i32>>) {
    tokio::select! {
        () = worker.cancel.cancelled() => {}
        _ = ended.wait_for(|status| status.is_some()) => worker.die(),
    }
}

// ---- the table -------------------------------------------------------------------------------------------------------

/// What the supervisor keeps for one person.
struct Slot {
    key: AccountKey,
    /// Held while a worker is being started, so that a person never has two.
    launch: tokio::sync::Mutex<()>,
    current: Mutex<Option<Arc<Worker>>>,
    store: watch::Sender<Option<StoreState>>,
}

impl Slot {
    fn current(&self) -> MutexGuard<'_, Option<Arc<Worker>>> {
        self.current.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The worker, if one runs.
    fn live(&self) -> Option<Arc<Worker>> {
        self.current().clone().filter(|w| !w.is_dead())
    }
}

struct Shared {
    launcher: Arc<dyn WorkerLauncher>,
    settings: SupervisorSettings,
    clock: Arc<dyn Clock>,
    slots: Mutex<HashMap<AccountId, Arc<Slot>>>,
    counters: Counters,
    shutting_down: AtomicBool,
    #[allow(dead_code)]
    runtime: Handle,
}

impl Shared {
    fn slots(&self) -> MutexGuard<'_, HashMap<AccountId, Arc<Slot>>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The supervisor.
#[derive(Clone)]
pub struct Supervisor {
    shared: Arc<Shared>,
}

impl Supervisor {
    /// A supervisor that starts workers through `launcher`. It must be made inside a tokio runtime.
    pub fn new(launcher: Arc<dyn WorkerLauncher>, settings: SupervisorSettings, clock: Arc<dyn Clock>) -> Supervisor {
        Supervisor {
            shared: Arc::new(Shared {
                launcher,
                settings,
                clock,
                slots: Mutex::new(HashMap::new()),
                counters: Counters::default(),
                shutting_down: AtomicBool::new(false),
                runtime: Handle::current(),
            }),
        }
    }

    fn slot(&self, account: AccountKey) -> Arc<Slot> {
        let mut slots = self.shared.slots();
        slots
            .entry(account.id)
            .or_insert_with(|| {
                Arc::new(Slot {
                    key: account,
                    launch: tokio::sync::Mutex::new(()),
                    current: Mutex::new(None),
                    store: watch::channel(None).0,
                })
            })
            .clone()
    }

    /// The worker of `account`, started if there is none.
    async fn ensure_worker(&self, account: AccountKey) -> Result<Arc<Worker>, AttachError> {
        if self.shared.shutting_down.load(Ordering::SeqCst) {
            return Err(AttachError::ShuttingDown);
        }
        let slot = self.slot(account);
        if let Some(worker) = slot.live() {
            return Ok(worker);
        }
        let _launching = slot.launch.lock().await;
        if let Some(worker) = slot.live() {
            return Ok(worker);
        }
        self.start_worker(&slot).await
    }

    /// Starts a worker, sends the caps and waits for it to be ready.
    async fn start_worker(&self, slot: &Arc<Slot>) -> Result<Arc<Worker>, AttachError> {
        let shared = &self.shared;
        let launched = shared
            .launcher
            .launch(LaunchRequest { account: slot.key, mode: WorkerMode::Serve })
            .await
            .map_err(|e| AttachError::Launch(e.to_string()))?;
        let LaunchedWorker { link, ended } = launched;
        let (read, write) = link.into_split();
        let worker = Arc::new(Worker {
            shared: shared.clone(),
            slot: slot.clone(),
            state: Mutex::new(WorkerState {
                sockets: HashMap::new(),
                next_socket: 0,
                owners: Owners::default(),
                synthetic: 0,
                idle_since: None,
            }),
            out: WorkQueue::new(),
            cancel: CancellationToken::new(),
            dead: AtomicBool::new(false),
            done: watch::channel(false).0,
        });
        let init = Ctl::Init {
            version: PROTOCOL_VERSION,
            max_panes: shared.settings.max_panes,
            max_scrollback_lines: shared.settings.max_scrollback_lines,
        };
        worker.push_control(init).map_err(|_| AttachError::Launch("the worker could not be started".into()))?;
        let (ready_tx, ready_rx) = oneshot::channel();
        tokio::spawn(write_loop(worker.clone(), write));
        tokio::spawn(read_loop(worker.clone(), read, ready_tx));
        tokio::spawn(watch_loop(worker.clone(), ended));
        match tokio::time::timeout(shared.settings.ready_timeout, ready_rx).await {
            Ok(Ok(())) => {}
            _ => {
                worker.die();
                let _ = shared.launcher.kill_uid(slot.key.uid).await;
                log::event(Level::Warn, "supervisor.worker_not_ready", &[("account", &slot.key.id.to_string())]);
                return Err(AttachError::Launch("the worker did not become ready".into()));
            }
        }
        worker.lock().idle_since = Some(shared.clock.unix());
        *slot.current() = Some(worker.clone());
        Ok(worker)
    }

    /// Attaches a new browser socket of `account`, starting the person's worker if there is none.
    pub async fn attach(&self, account: AccountKey) -> Result<(SocketHandle, SocketRx), AttachError> {
        let mut last = AttachError::Launch("the worker ended while the socket attached".into());
        for _ in 0..3 {
            let worker = self.ensure_worker(account).await?;
            match worker.add_socket() {
                Ok(halves) => return Ok(halves),
                Err(AttachError::TooManySockets) => return Err(AttachError::TooManySockets),
                Err(other) => last = other,
            }
        }
        Err(last)
    }

    /// What the supervisor knows about `account`.
    pub fn account_stats(&self, account: AccountKey) -> AccountStats {
        let slot = self.shared.slots().get(&account.id).cloned();
        let Some(worker) = slot.and_then(|slot| slot.live()) else { return AccountStats::default() };
        let (worker_queue_frames, worker_queue_bytes) = worker.out.load();
        let state = worker.lock();
        AccountStats {
            worker_running: true,
            sockets: state.sockets.len(),
            queued_bytes: state.sockets.values().map(|s| s.queue.bytes()).sum(),
            worker_queue_frames,
            worker_queue_bytes,
            remembered_sizes: state.owners.remembered(),
            owned_panes: state.owners.owned(),
        }
    }

    /// The totals.
    pub fn stats(&self) -> Stats {
        let slots: Vec<Arc<Slot>> = self.shared.slots().values().cloned().collect();
        let counters = &self.shared.counters;
        let mut stats = Stats {
            dropped_unknown_socket: Counters::get(&counters.dropped_unknown_socket),
            dropped_frames: Counters::get(&counters.dropped_frames),
            dropped_synthetic: Counters::get(&counters.dropped_synthetic),
            overflow_closes: Counters::get(&counters.overflow_closes),
            worker_deaths: Counters::get(&counters.worker_deaths),
            ..Stats::default()
        };
        for worker in slots.iter().filter_map(|slot| slot.live()) {
            stats.workers += 1;
            stats.sockets += worker.lock().sockets.len();
        }
        stats
    }

    /// Asks every worker to end.
    pub async fn shutdown(&self) {
        self.shared.shutting_down.store(true, Ordering::SeqCst);
        let slots: Vec<Arc<Slot>> = self.shared.slots().values().cloned().collect();
        for worker in slots.iter().filter_map(|slot| slot.live()) {
            let _ = worker.push_control(Ctl::Shutdown);
        }
    }
}
