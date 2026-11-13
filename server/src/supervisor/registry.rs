//! The table of running workers and the people they belong to.
//!
//! One person has at most one worker. `Supervisor::attach` finds it or starts it (once, however many attaches arrive at the
//! same time), sends the caps of the settings, waits for the worker to say it is ready and joins the browser socket to it.
//! Every worker has three tasks: a reader that checks and routes what the worker says, a single writer that sends the frames
//! in the order they were queued, and a watcher that notices the end of the process. Workers of different people share
//! nothing but the launcher and the counters.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::runtime::Handle;
use tokio::sync::{Notify, oneshot, watch};
use tokio::task::JoinSet;
use tokio_util::codec::Decoder;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::clock::Clock;
use crate::limits;
use crate::log::{self, Level};
use crate::store::AccountId;
use crate::supervisor::fanout::{self, Counters, Delivery, Fault, Owners, Synthetic};
use crate::supervisor::idle;
use crate::supervisor::launcher::{LaunchRequest, LaunchedWorker, WorkerLauncher};
use crate::accounts::{AccountObserver, Ending};
use crate::supervisor::socket::{self, Attached, SocketCore, SocketHandle, SocketRx, SubmitError};
use crate::wire::{self, CmdFrame, Ctl, CtlOutcome, Frame, Note, PROTOCOL_VERSION, ResBody, Ses, StoreState};
use crate::worker::runtime::WorkerMode;
use crate::worker::sessions::SessionLimits;

/// The close code for a socket whose worker ended.
pub const CLOSE_WORKER_ENDED: u16 = 1012;

/// The close code for a socket when the server stops.
pub const CLOSE_SERVER_STOPPING: u16 = 1001;

/// The close code for a socket of an account that was disabled or deleted.
pub const CLOSE_ACCOUNT_ENDED: u16 = 4401;

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
    /// The most MiB of pictures one pane keeps, sent to the worker (0 is none).
    pub max_image_mib: u32,
    /// What a device may ask of a session that lives in the worker, sent to the worker.
    pub sessions: SessionLimits,
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
    /// How often the sweeper looks for idle workers.
    pub sweep_interval: Duration,
}

impl Default for SupervisorSettings {
    fn default() -> SupervisorSettings {
        SupervisorSettings {
            max_panes: limits::MAX_PANES,
            max_scrollback_lines: limits::MAX_SCROLLBACK_LINES,
            max_image_mib: limits::MAX_IMAGE_MIB,
            sessions: SessionLimits::DEFAULT,
            sockets_per_person: limits::SOCKETS_PER_PERSON as usize,
            socket_queue_bytes: limits::WS_QUEUE_BYTES as usize,
            idle_timeout: limits::WORKER_IDLE_TIMEOUT,
            ready_timeout: limits::WORKER_READY_TIMEOUT,
            shutdown_timeout: limits::WORKER_SHUTDOWN_TIMEOUT,
            control_timeout: limits::WORKER_CONTROL_TIMEOUT,
            wipe_timeout: limits::WORKER_WIPE_TIMEOUT,
            sweep_interval: limits::SWEEP_INTERVAL,
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

/// Why a control frame did not get an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CtlError {
    /// The supervisor refuses to send this frame; the sentence says why.
    Refused(&'static str),
    /// The worker did not answer in time.
    Timeout,
    /// The worker ended before it answered.
    WorkerGone,
    /// There are too many controls waiting for an answer.
    Busy,
    /// No worker could be started.
    Launch(String),
    /// The server is stopping.
    ShuttingDown,
}

impl fmt::Display for CtlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CtlError::Refused(sentence) => f.write_str(sentence),
            CtlError::Timeout => f.write_str("the worker did not answer in time"),
            CtlError::WorkerGone => f.write_str("the worker ended before it answered"),
            CtlError::Busy => f.write_str("too many controls are waiting"),
            CtlError::Launch(sentence) => f.write_str(sentence),
            CtlError::ShuttingDown => f.write_str("the server is stopping"),
        }
    }
}

impl std::error::Error for CtlError {}

/// Why a file control did not get an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileError {
    /// The frame is not one of the four file controls, or its id is used already.
    Refused,
    /// The person has no worker and none was to be started.
    NoWorker,
    /// The worker did not answer in time.
    Timeout,
    /// The worker ended before it answered.
    WorkerGone,
    /// Too many file frames wait for an answer, or the worker's queue is full.
    Busy,
    /// No worker could be started.
    Launch,
    /// The server is stopping.
    ShuttingDown,
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FileError::Refused => "this is not a file control",
            FileError::NoWorker => "the person has no worker",
            FileError::Timeout => "the worker did not answer in time",
            FileError::WorkerGone => "the worker ended before it answered",
            FileError::Busy => "too many file frames are waiting",
            FileError::Launch => "no worker could be started",
            FileError::ShuttingDown => "the server is stopping",
        })
    }
}

impl std::error::Error for FileError {}

/// Why a wipe did not happen. One sentence, no byte of any request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WipeError(pub String);

impl fmt::Display for WipeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for WipeError {}

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
    /// Answers to controls that nobody waited for.
    pub dropped_ctl_done: u64,
    /// File controls sent to workers.
    pub file_calls: u64,
    /// Answers to file controls that nobody waited for.
    pub dropped_file_answers: u64,
    /// Streams of a session that ended for a device socket because its queue had no room (the socket and the session went on).
    pub lost_streams: u64,
    /// Whether the spawn helper (or whatever starts the workers) can be reached.
    pub helper_alive: bool,
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

/// The control with the number `number` in place of the caller's. `None` for the four controls the supervisor owns.
fn with_number(control: Ctl, number: u64) -> Option<Ctl> {
    Some(match control {
        Ctl::Unlock { kind, key, .. } => Ctl::Unlock { ctl: number, kind, key },
        Ctl::Lock { .. } => Ctl::Lock { ctl: number },
        Ctl::ResetSecrets { .. } => Ctl::ResetSecrets { ctl: number },
        Ctl::Provision { slots, .. } => Ctl::Provision { ctl: number, slots },
        Ctl::SetSlot { slot, opening, .. } => Ctl::SetSlot { ctl: number, slot, opening },
        Ctl::RemoveSlot { kind, id, .. } => Ctl::RemoveSlot { ctl: number, kind, id },
        Ctl::Init { .. }
        | Ctl::Attach { .. }
        | Ctl::Detach { .. }
        | Ctl::Shutdown
        | Ctl::FileServe { .. }
        | Ctl::UploadOpen { .. }
        | Ctl::UploadPiece { .. }
        | Ctl::UploadClose { .. } => return None,
    })
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
    /// The worker is being ended: no socket may join it any more.
    ending: AtomicBool,
    done: watch::Sender<bool>,
    /// The controls that wait for their answer, by the number the supervisor gave them.
    waiters: Mutex<HashMap<u64, oneshot::Sender<CtlOutcome>>>,
    next_ctl: AtomicU64,
    /// The file controls that wait for their answer, by the id the control carries.
    file_waiters: Mutex<HashMap<u64, oneshot::Sender<ResBody>>>,
}

/// The first number the supervisor gives a control: high, so that a number a caller wrote never looks like it.
const FIRST_CTL_NUMBER: u64 = 1_000;

impl Worker {
    fn lock(&self) -> MutexGuard<'_, WorkerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    fn is_ending(&self) -> bool {
        self.ending.load(Ordering::SeqCst)
    }

    fn waiters(&self) -> MutexGuard<'_, HashMap<u64, oneshot::Sender<CtlOutcome>>> {
        self.waiters.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn file_waiters(&self) -> MutexGuard<'_, HashMap<u64, oneshot::Sender<ResBody>>> {
        self.file_waiters.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sends one file control and waits for the worker's answer. The frame carries the id the answer is matched by.
    async fn file(self: &Arc<Self>, control: Ctl) -> Result<ResBody, FileError> {
        let id = match &control {
            Ctl::FileServe { request, .. } => *request,
            Ctl::UploadOpen { id, .. } | Ctl::UploadPiece { id, .. } | Ctl::UploadClose { id, .. } => *id,
            _ => return Err(FileError::Refused),
        };
        let frame = encode(&Frame::Ctl(control)).map_err(|_| FileError::Refused)?;
        let (tx, rx) = oneshot::channel();
        {
            let mut waiters = self.file_waiters();
            if waiters.len() >= limits::FILE_WAITERS {
                return Err(FileError::Busy);
            }
            if waiters.contains_key(&id) {
                return Err(FileError::Refused);
            }
            waiters.insert(id, tx);
        }
        if self.is_dead() {
            self.file_waiters().remove(&id);
            return Err(FileError::WorkerGone);
        }
        match self.out.push_control(frame) {
            Ok(()) => Counters::bump(&self.shared.counters.file_calls),
            Err(SubmitError::Busy) => {
                self.file_waiters().remove(&id);
                return Err(FileError::Busy);
            }
            Err(_) => {
                self.file_waiters().remove(&id);
                return Err(FileError::WorkerGone);
            }
        }
        match tokio::time::timeout(limits::FILE_CALL_TIMEOUT, rx).await {
            Ok(Ok(body)) => Ok(body),
            Ok(Err(_)) => Err(FileError::WorkerGone),
            Err(_) => {
                self.file_waiters().remove(&id);
                Err(FileError::Timeout)
            }
        }
    }

    /// Starts the end of this worker: no socket may join it, the sockets it has close with `close` if given, and it is asked
    /// to shut down.
    fn begin_end(&self, close: Option<(u16, &'static str)>) {
        self.ending.store(true, Ordering::SeqCst);
        if let Some((code, reason)) = close {
            let sockets: Vec<Arc<SocketCore>> = self.lock().sockets.drain().map(|(_, s)| s).collect();
            for socket in sockets {
                socket.queue.close(code, reason);
            }
        }
        let _ = self.push_control(Ctl::Shutdown);
    }

    /// Waits for the worker to be gone, at most `limit`. True when it ended in time.
    async fn wait_done(&self, limit: Duration) -> bool {
        let mut done = self.done.subscribe();
        matches!(tokio::time::timeout(limit, done.wait_for(|done| *done)).await, Ok(Ok(_)))
    }

    /// Sends a control that has an answer and waits for it. The number in `control` is replaced by one of the supervisor's.
    async fn control(self: &Arc<Self>, control: Ctl) -> Result<CtlOutcome, CtlError> {
        let (tx, rx) = oneshot::channel();
        let number = {
            let mut waiters = self.waiters();
            if waiters.len() >= limits::PENDING_CONTROLS {
                return Err(CtlError::Busy);
            }
            let number = self.next_ctl.fetch_add(1, Ordering::SeqCst);
            waiters.insert(number, tx);
            number
        };
        let Some(frame) = with_number(control, number) else {
            self.waiters().remove(&number);
            return Err(CtlError::Refused("this control belongs to the supervisor"));
        };
        if self.push_control(frame).is_err() {
            self.waiters().remove(&number);
            return Err(CtlError::WorkerGone);
        }
        match tokio::time::timeout(self.shared.settings.control_timeout, rx).await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(_)) => Err(CtlError::WorkerGone),
            Err(_) => {
                self.waiters().remove(&number);
                Err(CtlError::Timeout)
            }
        }
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
        self.waiters().clear();
        self.file_waiters().clear();
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

    /// Joins a new socket to this worker: the socket of a page, which the worker replays its window to, or the socket of a device,
    /// which speaks sessions only and is never told of the window.
    fn add_socket(self: &Arc<Self>, device: bool) -> Result<(SocketHandle, SocketRx), AttachError> {
        let mut state = self.lock();
        if self.is_dead() || self.is_ending() {
            return Err(AttachError::Launch("the worker ended while the socket attached".into()));
        }
        if state.sockets.len() >= self.shared.settings.sockets_per_person {
            return Err(AttachError::TooManySockets);
        }
        state.next_socket += 1;
        let id = state.next_socket;
        let core = Arc::new(SocketCore::new(id, self.shared.settings.socket_queue_bytes, device));
        state.sockets.insert(id, core.clone());
        state.idle_since = None;
        // A device socket sends no `Attach`: the worker counts the pages that are attached, and a device is not one.
        if !device && self.push_control(Ctl::Attach { socket: id }).is_err() {
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
            let Some(removed) = state.sockets.remove(&socket) else { continue };
            if state.sockets.is_empty() {
                state.idle_since = Some(self.shared.clock.unix());
            }
            if removed.device {
                // The socket of a device leaves every session it was in; the worker's pages know nothing of it.
                if let Ok(frame) = encode(&Frame::Ses(Ses::Gone { socket })) {
                    let _ = self.out.push_control(frame);
                }
                continue;
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
            Frame::Res(res) if fanout::is_file_answer(&res) => {
                let waiter = self.file_waiters().remove(&res.request);
                fanout::deliver_file_res(res, waiter, &self.shared.counters);
            }
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
            Frame::Note(Note::CtlDone { ctl, outcome }) => match self.waiters().remove(&ctl) {
                Some(waiter) => {
                    let _ = waiter.send(outcome);
                }
                None => Counters::bump(&self.shared.counters.dropped_ctl_done),
            },
            Frame::Note(Note::PaneCount(_)) => {}
            // A request about sessions is the supervisor's to send, never a worker's.
            Frame::Ses(ses) if ses.is_request() => return Err(Fault::WrongDirection),
            // The answers, the events and the bytes of a session go to the device socket they name.
            Frame::Ses(ses) => {
                let mut state = self.lock();
                match fanout::deliver_ses(ses, &state.sockets, &self.shared.counters) {
                    Delivery::Done => {}
                    Delivery::Overflow(socket) => self.drop_sockets(&mut state, &[socket]),
                    Delivery::Lost { socket, session } => {
                        // The worker ends that stream only, so that it stops writing what nobody takes; the socket stays
                        // attached and keeps its place in the lease until it attaches again.
                        if let Ok(frame) = encode(&Frame::Ses(Ses::Lost { socket, session })) {
                            let _ = self.out.push_control(frame);
                        }
                    }
                }
            }
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
        if origin.device {
            return Err(SubmitError::BadCommand);
        }
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

    fn ses(&self, socket: u32, ses: Ses) -> Result<(), SubmitError> {
        let state = self.lock();
        match state.sockets.get(&socket) {
            Some(origin) if origin.device => {}
            Some(_) => return Err(SubmitError::BadCommand),
            None => return Err(SubmitError::Closed),
        }
        drop(state);
        let frame = encode(&Frame::Ses(ses))?;
        // Input is a stream and never answered: a worker that is behind drops it rather than queueing more.
        self.out.push_commands(vec![frame])
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
                // A link that breaks is a link that ends: a frame cut off by either is a fault of the worker.
                Ok(0) | Err(_) => match codec.decode_eof(&mut buf) {
                    Ok(Some(payload)) => break Ok(Some(payload)),
                    Ok(None) => break Ok(None),
                    Err(_) => break Err(Fault::Malformed),
                },
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
            _ => break,
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

    /// The worker, if one runs and is not being ended.
    fn usable(&self) -> Option<Arc<Worker>> {
        self.current().clone().filter(|w| !w.is_dead() && !w.is_ending())
    }
}

struct Shared {
    launcher: Arc<dyn WorkerLauncher>,
    settings: SupervisorSettings,
    clock: Arc<dyn Clock>,
    slots: Mutex<HashMap<AccountId, Arc<Slot>>>,
    counters: Counters,
    shutting_down: AtomicBool,
    /// The last id given to a file control; ids are never 0 and never repeat while the server runs.
    file_ids: AtomicU64,
    /// Cancelled at shutdown: stops the sweeper.
    stop: CancellationToken,
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
                file_ids: AtomicU64::new(0),
                stop: CancellationToken::new(),
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
        if let Some(worker) = slot.usable() {
            return Ok(worker);
        }
        // A worker that is being ended holds this lock until it is gone, so a new one never overlaps it.
        let _launching = slot.launch.lock().await;
        if self.shared.shutting_down.load(Ordering::SeqCst) {
            return Err(AttachError::ShuttingDown);
        }
        if let Some(worker) = slot.usable() {
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
            ending: AtomicBool::new(false),
            done: watch::channel(false).0,
            waiters: Mutex::new(HashMap::new()),
            next_ctl: AtomicU64::new(FIRST_CTL_NUMBER),
            file_waiters: Mutex::new(HashMap::new()),
        });
        let init = Ctl::Init {
            version: PROTOCOL_VERSION,
            max_panes: shared.settings.max_panes,
            max_scrollback_lines: shared.settings.max_scrollback_lines,
            max_image_mib: shared.settings.max_image_mib,
            sessions: shared.settings.sessions,
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
        self.attach_kind(account, false).await
    }

    /// Attaches a new socket of a device of `account`, starting the person's worker if there is none. It counts as one of the
    /// person's sockets (the cap is shared with the pages) and keeps the worker from going idle, but it is told nothing of the
    /// window: it speaks sessions through [`SocketHandle::ses`] and hears [`crate::supervisor::Outbound::Ses`].
    pub async fn attach_device(&self, account: AccountKey) -> Result<(SocketHandle, SocketRx), AttachError> {
        self.attach_kind(account, true).await
    }

    async fn attach_kind(&self, account: AccountKey, device: bool) -> Result<(SocketHandle, SocketRx), AttachError> {
        let mut last = AttachError::Launch("the worker ended while the socket attached".into());
        for _ in 0..3 {
            let worker = self.ensure_worker(account).await?;
            match worker.add_socket(device) {
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
            dropped_ctl_done: Counters::get(&counters.dropped_ctl_done),
            file_calls: Counters::get(&counters.file_calls),
            dropped_file_answers: Counters::get(&counters.dropped_file_answers),
            lost_streams: Counters::get(&counters.lost_streams),
            helper_alive: self.shared.launcher.helper_alive(),
            ..Stats::default()
        };
        for worker in slots.iter().filter_map(|slot| slot.live()) {
            stats.workers += 1;
            stats.sockets += worker.lock().sockets.len();
        }
        stats
    }

    /// Starts a worker for `account` if there is none, without attaching a socket. Its idle time starts when it is ready.
    pub async fn ensure(&self, account: AccountKey) -> Result<(), AttachError> {
        self.ensure_worker(account).await.map(|_| ())
    }

    /// Sends a control frame that belongs to the unlock plumbing to the worker of `account` and returns what the worker
    /// answered, starting the worker if there is none. `Init`, `Attach`, `Detach` and `Shutdown` belong to the supervisor and
    /// are refused. A `Lock` for a person without a worker is done: there is no key to wipe.
    pub async fn ctl(&self, account: AccountKey, control: Ctl) -> Result<CtlOutcome, CtlError> {
        if matches!(
            control,
            Ctl::Init { .. }
                | Ctl::Attach { .. }
                | Ctl::Detach { .. }
                | Ctl::Shutdown
                | Ctl::FileServe { .. }
                | Ctl::UploadOpen { .. }
                | Ctl::UploadPiece { .. }
                | Ctl::UploadClose { .. }
        ) {
            return Err(CtlError::Refused("this control belongs to the supervisor"));
        }
        let existing = self.shared.slots().get(&account.id).and_then(|slot| slot.usable());
        let worker = match existing {
            Some(worker) => worker,
            None if matches!(control, Ctl::Lock { .. }) => return Ok(CtlOutcome::Done),
            None => self.ensure_worker(account).await.map_err(|e| match e {
                AttachError::ShuttingDown => CtlError::ShuttingDown,
                other => CtlError::Launch(other.to_string()),
            })?,
        };
        worker.control(control).await
    }

    /// A new id for a file control: not 0, and never given twice while the server runs.
    pub fn file_id(&self) -> u64 {
        self.shared.file_ids.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Sends one of the four file controls (`FileServe`, `UploadOpen`, `UploadPiece`, `UploadClose`) to the worker of
    /// `account` and returns the answer the worker gave: its status and body, which the caller treats as untrusted. The
    /// control carries the id the answer is matched by (see [`Supervisor::file_id`]); an upload uses one id for all its frames
    /// and has one frame in flight at a time. With `start` false a person with no worker gets [`FileError::NoWorker`] and no
    /// worker is started; the download route has no business starting one, because a person with no worker has no grants.
    pub async fn file_call(&self, account: AccountKey, control: Ctl, start: bool) -> Result<ResBody, FileError> {
        if !matches!(
            control,
            Ctl::FileServe { .. } | Ctl::UploadOpen { .. } | Ctl::UploadPiece { .. } | Ctl::UploadClose { .. }
        ) {
            return Err(FileError::Refused);
        }
        if self.shared.shutting_down.load(Ordering::SeqCst) {
            return Err(FileError::ShuttingDown);
        }
        let existing = self.shared.slots().get(&account.id).and_then(|slot| slot.usable());
        let worker = match existing {
            Some(worker) => worker,
            None if !start => return Err(FileError::NoWorker),
            None => self.ensure_worker(account).await.map_err(|e| match e {
                AttachError::ShuttingDown => FileError::ShuttingDown,
                _ => FileError::Launch,
            })?,
        };
        worker.file(control).await
    }

    /// What the worker last said about the store of `account`; `None` while no worker runs.
    pub fn store_state(&self, account: AccountKey) -> Option<StoreState> {
        let slot = self.shared.slots().get(&account.id).cloned()?;
        *slot.store.borrow()
    }

    /// A channel that carries the store state of `account` whenever it changes.
    pub fn watch_store_state(&self, account: AccountKey) -> watch::Receiver<Option<StoreState>> {
        self.slot(account).store.subscribe()
    }

    /// Ends the workers whose last socket left at least the idle timeout ago. The number of workers ended.
    pub async fn sweep(&self) -> usize {
        let now = self.shared.clock.unix();
        let timeout = self.shared.settings.idle_timeout;
        let slots: Vec<Arc<Slot>> = self.shared.slots().values().cloned().collect();
        let mut ending = JoinSet::new();
        for slot in slots {
            let idle = slot.usable().is_some_and(|worker| {
                let state = worker.lock();
                state.sockets.is_empty() && state.idle_since.is_some_and(|since| idle::expired(since, now, timeout))
            });
            if idle {
                let supervisor = self.clone();
                ending.spawn(async move { supervisor.end_idle(&slot, now).await });
            }
        }
        let mut ended = 0;
        while let Some(result) = ending.join_next().await {
            if result.unwrap_or(false) {
                ended += 1;
            }
        }
        ended
    }

    /// Ends the worker of `slot` if it is still idle, once nobody is starting or ending it.
    async fn end_idle(&self, slot: &Arc<Slot>, now: i64) -> bool {
        let _launching = slot.launch.lock().await;
        let Some(worker) = slot.usable() else { return false };
        {
            // Decided and marked under one lock, so that no socket joins in between.
            let state = worker.lock();
            let idle = state.sockets.is_empty()
                && state.idle_since.is_some_and(|since| idle::expired(since, now, self.shared.settings.idle_timeout));
            if !idle {
                return false;
            }
            worker.ending.store(true, Ordering::SeqCst);
        }
        log::event(Level::Info, "supervisor.worker_idle", &[("account", &slot.key.id.to_string())]);
        worker.begin_end(None);
        if !worker.wait_done(self.shared.settings.shutdown_timeout).await {
            worker.die();
            let _ = self.shared.launcher.kill_uid(slot.key.uid).await;
        }
        true
    }

    /// Runs [`Supervisor::sweep`] every `sweep_interval` until [`Supervisor::shutdown`].
    pub fn start_sweeper(&self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(idle::run(self.clone(), self.shared.settings.sweep_interval, self.shared.stop.clone()))
    }

    /// Removes the secret store of `account`: through the running worker, or through a worker in wipe mode.
    pub async fn wipe_secrets(&self, account: AccountKey) -> Result<(), WipeError> {
        let slot = self.slot(account);
        loop {
            if self.shared.shutting_down.load(Ordering::SeqCst) {
                return Err(WipeError("the server is stopping".into()));
            }
            if slot.usable().is_some() {
                return match self.ctl(account, Ctl::ResetSecrets { ctl: 0 }).await {
                    Ok(CtlOutcome::Done) => Ok(()),
                    Ok(_) => Err(WipeError("the secret store could not be removed".into())),
                    Err(error) => Err(WipeError(error.to_string())),
                };
            }
            let launching = slot.launch.lock().await;
            if slot.usable().is_some() {
                // A worker started while this waited: it does the wipe.
                drop(launching);
                continue;
            }
            return self.wipe_launch(&slot, WorkerMode::WipeSecrets).await;
        }
    }

    /// Removes everything in the home of `account`. A running worker is ended first.
    pub async fn wipe_home(&self, account: AccountKey) -> Result<(), WipeError> {
        if self.shared.shutting_down.load(Ordering::SeqCst) {
            return Err(WipeError("the server is stopping".into()));
        }
        let slot = self.slot(account);
        let _launching = slot.launch.lock().await;
        if let Some(worker) = slot.live() {
            worker.begin_end(Some((CLOSE_WORKER_ENDED, "the worker ended")));
            if !worker.wait_done(self.shared.settings.shutdown_timeout).await {
                worker.die();
                let _ = self.shared.launcher.kill_uid(slot.key.uid).await;
            }
        }
        self.wipe_launch(&slot, WorkerMode::WipeHome).await
    }

    /// Starts a worker in a wipe mode and waits for its process to end. The caller holds the launch lock of the slot.
    async fn wipe_launch(&self, slot: &Arc<Slot>, mode: WorkerMode) -> Result<(), WipeError> {
        let launched = self
            .shared
            .launcher
            .launch(LaunchRequest { account: slot.key, mode })
            .await
            .map_err(|e| WipeError(e.to_string()))?;
        let LaunchedWorker { link, mut ended } = launched;
        // A wipe worker has nothing to say on its link.
        drop(link);
        let finished = tokio::time::timeout(self.shared.settings.wipe_timeout, ended.wait_for(|status| status.is_some()))
            .await
            .map(|result| result.map(|status| *status));
        match finished {
            Ok(Ok(Some(0))) => Ok(()),
            Ok(_) => Err(WipeError("the wipe did not finish: the worker ended with an error".into())),
            Err(_) => {
                let _ = self.shared.launcher.kill_uid(slot.key.uid).await;
                Err(WipeError("the wipe did not finish in time".into()))
            }
        }
    }

    /// Ends every worker: the sockets close with 1001, each worker is asked to shut down, and one that does not close its
    /// link in time is killed by user id. A second call does nothing.
    pub async fn shutdown(&self) {
        if self.shared.shutting_down.swap(true, Ordering::SeqCst) {
            return;
        }
        self.shared.stop.cancel();
        let slots: Vec<Arc<Slot>> = self.shared.slots().values().cloned().collect();
        let mut ending = JoinSet::new();
        for slot in slots {
            let supervisor = self.clone();
            ending.spawn(async move {
                // A worker that is still being started finishes first (and its start sees the flag next time).
                let _launching = slot.launch.lock().await;
                let Some(worker) = slot.live() else { return };
                worker.begin_end(Some((CLOSE_SERVER_STOPPING, "the server is stopping")));
                if !worker.wait_done(supervisor.shared.settings.shutdown_timeout).await {
                    worker.die();
                    let _ = supervisor.shared.launcher.kill_uid(slot.key.uid).await;
                }
            });
        }
        while ending.join_next().await.is_some() {}
    }
}

impl Supervisor {
    /// Ends the worker of an account that was disabled or deleted: its sockets close with 4401, the worker is asked to shut
    /// down, and whatever is left of the user's processes is killed after the bound. The home and the store stay.
    async fn end_account(&self, slot: Arc<Slot>, uid: u32) {
        // Waits for a start that is in progress, so that its worker is ended too.
        let _launching = slot.launch.lock().await;
        let Some(worker) = slot.live() else { return };
        log::event(Level::Info, "supervisor.account_ended", &[("account", &slot.key.id.to_string())]);
        worker.begin_end(Some((CLOSE_ACCOUNT_ENDED, "the account was ended")));
        if !worker.wait_done(self.shared.settings.shutdown_timeout).await {
            worker.die();
        }
        let _ = self.shared.launcher.kill_uid(uid).await;
    }
}

/// A disabled or deleted account loses its worker and its sockets; its home and its store stay where they are.
impl AccountObserver for Supervisor {
    fn account_ended(&self, account: AccountId, uid: u32, _ending: Ending) {
        // An account that never had a worker has no slot: there is nothing to end.
        let Some(slot) = self.shared.slots().get(&account).cloned() else { return };
        let supervisor = self.clone();
        self.shared.runtime.spawn(async move { supervisor.end_account(slot, uid).await });
    }
}
