//! The dispatch of commands from the page to the application logic of the host, and the sink the core's events leave by.
//!
//! Every command is resolved with [`lookup`] in the command table of the host crate; this module holds no command name and
//! no list of its own. A command that the table marks as a waiting one runs on a small pool, every other one on one
//! dispatcher thread, so the order in which the page sent its sync commands is the order they run in. The reader of the
//! link never blocks here: a full queue is an answer, not a wait.
//!
//! Everything the worker writes to the supervisor goes through one [`Outbound`] queue and one writer thread. The queue is
//! counted in bytes: over the soft cap it drops the frame events of the panes (and remembers which panes lost one, so a
//! full frame is asked for when the queue is low again); over the hard cap it declares the link dead.

use std::cell::Cell;
use std::collections::{BTreeSet, VecDeque};
use std::io::{BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use holzwave_host::commands::registry::{Command, Mode, Outcome, Web, lookup, unavailable};
use holzwave_host::state::{AppCore, Out, Sink, Target};
use holzwave_host::windows::{CursorSpot, Place};
use serde::Deserialize;
use serde_json::Value;

use crate::log::{self, Level};
use crate::wire::{CmdFrame, Evt, ExitReason, Frame, Note, Res, ResBody, valid_event_name, write_frame};

/// The label of the one window of a person (every browser connection shows it).
pub const WINDOW: &str = "main";

/// How many sync commands wait for the dispatcher thread before the next one is refused.
pub const DISPATCH_QUEUE: usize = 2048;

/// How many threads run the commands that wait.
pub const POOL_THREADS: usize = 8;

/// How many waiting commands queue for the pool before the next one is refused.
pub const POOL_QUEUE: usize = 64;

/// The group of the commands that carry the keys, the mouse and the size of a pane; they never change the number of panes.
const TERMINAL_GROUP: &str = "terminal";

/// The longest command name in bytes; a longer one cannot be in the table.
pub const MAX_NAME_BYTES: usize = 64;

/// How long the writer waits for the supervisor to take bytes before it gives up on the link.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

// ---- gate, probe, log: the switches of a test -------------------------------------------------------------------------

/// A gate the writer thread waits at: closed by [`Gate::hold`], opened by [`Gate::release`]. A test closes it to stand in
/// for a supervisor that does not read.
#[derive(Debug, Default)]
pub struct Gate {
    held: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    /// An open gate.
    pub fn new() -> Arc<Gate> {
        Arc::new(Gate::default())
    }

    /// Close the gate: the writer stops before its next frame.
    pub fn hold(&self) {
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = true;
    }

    /// Open the gate.
    pub fn release(&self) {
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = false;
        self.changed.notify_all();
    }

    /// Wait until the gate is open.
    pub fn pass(&self) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        while *held {
            held = self.changed.wait(held).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

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

    pub(crate) fn record_call(&self, command: &str) {
        let thread = std::thread::current().name().unwrap_or("").to_owned();
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).push((command.to_owned(), thread));
    }

    pub(crate) fn record_repair(&self, pane: u64) {
        self.repaired.lock().unwrap_or_else(PoisonError::into_inner).push(pane);
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

/// Where the worker's log lines go: standard error, or a list a test reads.
#[derive(Clone, Default)]
pub struct WorkerLog {
    capture: Option<Arc<Mutex<Vec<String>>>>,
}

impl WorkerLog {
    /// A log that writes to standard error, or into `capture` when there is one.
    pub fn new(capture: Option<Arc<Mutex<Vec<String>>>>) -> WorkerLog {
        WorkerLog { capture }
    }

    /// One event: a fixed name and the facts to publish, never a secret and never text from a frame.
    pub fn event(&self, level: Level, name: &'static str, fields: &[(&'static str, &str)]) {
        match &self.capture {
            Some(lines) => {
                let line = log::line(SystemTime::now(), level, name, fields);
                lines.lock().unwrap_or_else(PoisonError::into_inner).push(line);
            }
            None => log::event(level, name, fields),
        }
    }
}

// ---- the outbound queue -----------------------------------------------------------------------------------------------

/// Where the soft cap of the outbound queue is: over it, frame events are dropped.
pub const SOFT_CAP: usize = 8 * 1024 * 1024;

/// Where the hard cap of the outbound queue is: over it, the link is declared dead.
pub const HARD_CAP: usize = 64 * 1024 * 1024;

/// Where the queue is low again: under it, a full frame is asked for every pane that lost one.
pub const LOW_WATER: usize = 1024 * 1024;

/// The three marks of the outbound queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueLimits {
    pub soft: usize,
    pub hard: usize,
    pub low: usize,
}

impl Default for QueueLimits {
    fn default() -> QueueLimits {
        QueueLimits { soft: SOFT_CAP, hard: HARD_CAP, low: LOW_WATER }
    }
}

/// What became of a frame offered to the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pushed {
    Queued,
    Dropped,
    Dead,
}

struct Inner {
    items: VecDeque<(Frame, usize)>,
    bytes: usize,
    lost: BTreeSet<u64>,
    dropped: u64,
    dead: bool,
    closed: bool,
}

type Repair = Box<dyn Fn(u64) + Send + Sync>;

/// The queue between everything that produces frames and the writer thread.
pub struct Outbound {
    limits: QueueLimits,
    inner: Mutex<Inner>,
    ready: Condvar,
    repair: Mutex<Option<Repair>>,
}

/// What a frame weighs in the queue: the bytes it will take on the link, near enough.
fn weight(frame: &Frame) -> usize {
    16 + match frame {
        Frame::Cmd(cmd) => cmd.json.len(),
        Frame::Res(res) => match &res.body {
            ResBody::Ok(t) | ResBody::Err(t) => t.len(),
            ResBody::Binary(b) => b.len(),
        },
        Frame::Evt(evt) => evt.name.len() + evt.text.len(),
        Frame::Ctl(_) | Frame::Note(_) => 0,
    }
}

/// The pane a frame event is for, read from the head of its text (the frame message starts with it).
fn pane_of(text: &str) -> Option<u64> {
    let digits = text.strip_prefix("{\"pane\":")?;
    let end = digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len());
    digits[..end].parse().ok()
}

/// The name of the event that carries a frame of a pane; the only one that may be dropped.
const FRAME_EVENT: &str = "frame";

fn is_frame_event(frame: &Frame) -> bool {
    matches!(frame, Frame::Evt(evt) if evt.name == FRAME_EVENT)
}

impl Outbound {
    pub fn new(limits: QueueLimits) -> Arc<Outbound> {
        Arc::new(Outbound {
            limits,
            inner: Mutex::new(Inner {
                items: VecDeque::new(),
                bytes: 0,
                lost: BTreeSet::new(),
                dropped: 0,
                dead: false,
                closed: false,
            }),
            ready: Condvar::new(),
            repair: Mutex::new(None),
        })
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn limits(&self) -> QueueLimits {
        self.limits
    }

    /// What to do for a pane that lost a frame, once the queue is low again.
    pub fn set_repair(&self, repair: impl Fn(u64) + Send + Sync + 'static) {
        *self.repair.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(repair));
    }

    /// Offers a frame. A frame event over the soft cap is dropped and its pane remembered; anything else is kept, and a
    /// queue over the hard cap is emptied, ends with one failure note and is dead.
    pub fn push(&self, frame: Frame) -> Pushed {
        let size = weight(&frame);
        let mut inner = self.inner();
        if inner.dead {
            return Pushed::Dead;
        }
        if inner.closed {
            return Pushed::Dropped;
        }
        if is_frame_event(&frame) && inner.bytes >= self.limits.soft {
            inner.dropped += 1;
            if let Frame::Evt(evt) = &frame
                && let Some(pane) = pane_of(&evt.text)
            {
                inner.lost.insert(pane);
            }
            return Pushed::Dropped;
        }
        inner.bytes += size;
        inner.items.push_back((frame, size));
        if inner.bytes > self.limits.hard {
            Self::kill(&mut inner);
            drop(inner);
            self.ready.notify_all();
            return Pushed::Dead;
        }
        drop(inner);
        self.ready.notify_one();
        Pushed::Queued
    }

    /// Offers the frame event of `pane` without building it when the queue is over the soft cap.
    pub fn offer_frame(&self, pane: u64, build: impl FnOnce() -> Option<Frame>) -> Pushed {
        {
            let mut inner = self.inner();
            if inner.dead {
                return Pushed::Dead;
            }
            if inner.bytes >= self.limits.soft && !inner.closed {
                inner.dropped += 1;
                inner.lost.insert(pane);
                return Pushed::Dropped;
            }
        }
        match build() {
            Some(frame) => self.push(frame),
            None => Pushed::Dropped,
        }
    }

    /// Empties the queue and leaves one failure note; nothing more is accepted.
    fn kill(inner: &mut Inner) {
        inner.items.clear();
        inner.lost.clear();
        let note = Frame::Note(Note::Exiting(ExitReason::Failure));
        let size = weight(&note);
        inner.items.push_back((note, size));
        inner.bytes = size;
        inner.dead = true;
        inner.closed = true;
    }

    /// Declares the link dead without a note (the writer could not write).
    pub fn fail(&self) {
        let mut inner = self.inner();
        inner.items.clear();
        inner.bytes = 0;
        inner.dead = true;
        inner.closed = true;
        drop(inner);
        self.ready.notify_all();
    }

    /// No more frames are accepted; the writer writes what is queued and ends.
    pub fn close(&self) {
        self.inner().closed = true;
        self.ready.notify_all();
    }

    /// The next frame, waiting for one. `None` when the queue is closed and empty. The flag says that the queue is empty
    /// after this frame, which is when the writer flushes.
    pub fn pop_wait(&self) -> Option<(Frame, bool)> {
        let mut inner = self.inner();
        let (frame, size) = loop {
            if let Some(item) = inner.items.pop_front() {
                break item;
            }
            if inner.closed {
                return None;
            }
            inner = self.ready.wait(inner).unwrap_or_else(PoisonError::into_inner);
        };
        inner.bytes = inner.bytes.saturating_sub(size);
        let empty = inner.items.is_empty();
        let lost = if inner.bytes < self.limits.low && !inner.lost.is_empty() {
            std::mem::take(&mut inner.lost)
        } else {
            BTreeSet::new()
        };
        drop(inner);
        if !lost.is_empty()
            && let Some(repair) = &*self.repair.lock().unwrap_or_else(PoisonError::into_inner)
        {
            for pane in lost {
                repair(pane);
            }
        }
        Some((frame, empty))
    }

    pub fn queued_bytes(&self) -> usize {
        self.inner().bytes
    }

    pub fn dropped_frames(&self) -> u64 {
        self.inner().dropped
    }

    pub fn is_dead(&self) -> bool {
        self.inner().dead
    }
}

/// The writer thread over the outbound queue: writes every frame to `link` until the queue is closed and empty, or dead.
/// A dead link is closed after its last note, so the reader of the run ends too.
pub fn spawn_writer(link: UnixStream, queue: Arc<Outbound>, gate: Option<Arc<Gate>>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("hw-writer".into())
        .spawn(move || {
            let _ = link.set_write_timeout(Some(WRITE_TIMEOUT));
            let mut out = BufWriter::new(link);
            loop {
                if let Some(gate) = &gate {
                    gate.pass();
                }
                let Some((frame, empty)) = queue.pop_wait() else { break };
                if write_frame(&mut out, &frame).is_err() || (empty && out.flush().is_err()) {
                    queue.fail();
                    break;
                }
            }
            let _ = out.flush();
            if queue.is_dead() {
                let _ = out.get_ref().shutdown(std::net::Shutdown::Both);
            }
        })
        .expect("the writer thread starts")
}

// ---- the sink of the core ---------------------------------------------------------------------------------------------

thread_local! {
    /// The socket the events of this thread are for; 0 is every attached socket.
    static PIN: Cell<u32> = const { Cell::new(0) };
}

/// While it lives, the events the core sends from this thread are for one socket only: the replay to a page that has just
/// attached. It is restored when dropped, also when the thread unwinds.
pub struct Pin {
    previous: u32,
}

impl Pin {
    pub fn to(socket: u32) -> Pin {
        Pin { previous: PIN.with(|pin| pin.replace(socket)) }
    }
}

impl Drop for Pin {
    fn drop(&mut self) {
        PIN.with(|pin| pin.set(self.previous));
    }
}

/// The value of the pane count before the first count is known.
const UNKNOWN: u64 = u64::MAX;

/// The sink of the core over the outbound queue: every event becomes one `Evt` frame with the text the desktop sends.
pub struct ServerSink {
    queue: Arc<Outbound>,
    panes: AtomicU64,
    log: WorkerLog,
}

impl ServerSink {
    pub fn new(queue: Arc<Outbound>) -> ServerSink {
        ServerSink::with_log(queue, WorkerLog::default())
    }

    pub fn with_log(queue: Arc<Outbound>, log: WorkerLog) -> ServerSink {
        ServerSink { queue, panes: AtomicU64::new(UNKNOWN), log }
    }

    /// Tells the supervisor how many panes there are, when that is not what it was told last.
    pub fn announce_panes(&self, count: usize) {
        let count = u64::try_from(count).unwrap_or(u64::MAX - 1);
        if self.panes.swap(count, Ordering::SeqCst) != count {
            let count = u32::try_from(count).unwrap_or(u32::MAX);
            self.queue.push(Frame::Note(Note::PaneCount(count)));
        }
    }

    fn event(&self, name: &'static str, out: &Out) -> Option<Frame> {
        if !valid_event_name(name) {
            self.log.event(Level::Error, "worker_event_refused", &[("reason", "name")]);
            return None;
        }
        match serde_json::to_string(out) {
            Ok(text) => Some(Frame::Evt(Evt { target: PIN.with(Cell::get), name: name.to_owned(), text })),
            Err(_) => {
                self.log.event(Level::Error, "worker_event_refused", &[("reason", "serialise")]);
                None
            }
        }
    }
}

impl Sink for ServerSink {
    fn send(&self, _target: Target, out: Out) {
        // The end of the app is the supervisor's to decide, never an event.
        if matches!(out, Out::Exit) {
            return;
        }
        let name = out.name();
        if let Out::Frame(message) = &out {
            self.queue.offer_frame(message.pane, || self.event(name, &out));
            return;
        }
        if let Some(frame) = self.event(name, &out) {
            self.queue.push(frame);
        }
        if let Out::Layout(view) = &out {
            self.announce_panes(view.workspaces.iter().map(|w| w.panes).sum());
        }
    }

    fn close_window(&self, _label: &str) {}

    fn open_window(&self, _label: &str, _place: &Place) -> Result<(), String> {
        Err("not available here".into())
    }

    fn cursor_spot(&self, _source: &str) -> CursorSpot {
        CursorSpot::Unknown
    }
}

// ---- the commands -----------------------------------------------------------------------------------------------------

/// The envelope of a command: its name and the arguments, which may be absent. A `null` is kept as a value so that it can
/// be refused, because a command takes an object.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    cmd: String,
    #[serde(default, deserialize_with = "present")]
    args: Option<Value>,
}

fn present<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// The answer frame of a refusal: the error value is a JSON string.
pub fn refusal(socket: u32, request: u64, sentence: &str) -> Frame {
    let body = serde_json::to_string(sentence).unwrap_or_else(|_| String::from("\"refused\""));
    Frame::Res(Res { socket, request, body: ResBody::Err(body) })
}

/// Whether `name` can be a command name at all: lower-case letters, digits and `_`, at most [`MAX_NAME_BYTES`].
fn plausible(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// What a command runs.
enum Work {
    Row(&'static Command, String),
    Hook(Arc<dyn Fn() -> Outcome + Send + Sync>),
}

/// A command that is accepted and waits to run.
struct Job {
    socket: u32,
    request: u64,
    name: String,
    work: Work,
    /// Whether the events it causes are for its socket alone (the page asking for the replay).
    pin: bool,
}

fn outcome_frame(socket: u32, request: u64, outcome: Outcome) -> Frame {
    let body = match outcome {
        Outcome::Ok(text) => ResBody::Ok(text),
        Outcome::Err(text) => ResBody::Err(text),
        Outcome::Binary(bytes) => ResBody::Binary(bytes),
    };
    Frame::Res(Res { socket, request, body })
}

/// Runs one job: the answer is always exactly one frame. A panic is caught, logged by the name of the command only, and
/// answered as an internal error.
fn run_job(job: Job, core: &AppCore, sink: &ServerSink, queue: &Outbound, probe: Option<&Probe>, log: &WorkerLog) {
    if let Some(probe) = probe {
        probe.record_call(&job.name);
    }
    let _pin = job.pin.then(|| Pin::to(job.socket));
    let run = catch_unwind(AssertUnwindSafe(|| match &job.work {
        Work::Row(command, args) => command.call(core, WINDOW, args),
        Work::Hook(run) => run(),
    }));
    // A command that is not about the keys and the mouse of a pane may have changed the number of panes (a split, a close, a
    // pane opened for a connection): the supervisor is told before the answer arrives.
    if let Work::Row(command, _) = &job.work
        && command.group != TERMINAL_GROUP
        && let Ok(view) = core.view_in(WINDOW)
    {
        sink.announce_panes(view.workspaces.iter().map(|w| w.panes).sum());
    }
    let frame = match run {
        Ok(outcome) => outcome_frame(job.socket, job.request, outcome),
        Err(_) => {
            log.event(Level::Error, "worker_command_panicked", &[("command", &job.name)]);
            refusal(job.socket, job.request, "internal error")
        }
    };
    queue.push(frame);
}

/// The threads that run commands, and the rules that let a command in.
pub struct Dispatcher {
    sync: SyncSender<Job>,
    pool: SyncSender<Job>,
    hooks: Vec<HookRow>,
    /// The name of the command that replays the state of the window to the socket that asks.
    replay: &'static str,
}

impl Dispatcher {
    /// Starts the dispatcher thread and the pool. `replay` names the command whose events are for its own socket only.
    pub fn start(
        core: &Arc<AppCore>,
        sink: &Arc<ServerSink>,
        queue: &Arc<Outbound>,
        hooks: Vec<HookRow>,
        probe: Option<Arc<Probe>>,
        log: WorkerLog,
        replay: &'static str,
    ) -> Dispatcher {
        let (sync, sync_jobs) = sync_channel::<Job>(DISPATCH_QUEUE);
        let (pool, pool_jobs) = sync_channel::<Job>(POOL_QUEUE);
        {
            let (core, sink, queue, probe, log) =
                (core.clone(), sink.clone(), queue.clone(), probe.clone(), log.clone());
            std::thread::Builder::new()
                .name("hw-dispatch".into())
                .spawn(move || {
                    while let Ok(job) = sync_jobs.recv() {
                        run_job(job, &core, &sink, &queue, probe.as_deref(), &log);
                    }
                })
                .expect("the dispatcher thread starts");
        }
        let pool_jobs = Arc::new(Mutex::new(pool_jobs));
        for n in 0..POOL_THREADS {
            let (core, sink, queue, probe, log, jobs) =
                (core.clone(), sink.clone(), queue.clone(), probe.clone(), log.clone(), pool_jobs.clone());
            std::thread::Builder::new()
                .name(format!("hw-pool-{n}"))
                .spawn(move || {
                    // One thread waits for the next job; the others wait for the lock.
                    while let Some(job) = take(&jobs) {
                        run_job(job, &core, &sink, &queue, probe.as_deref(), &log);
                    }
                })
                .expect("a pool thread starts");
        }
        Dispatcher { sync, pool, hooks, replay }
    }

    /// Reads the envelope of a command and queues it. The error is the answer to send at once.
    pub fn submit(&self, cmd: &CmdFrame) -> Result<(), Frame> {
        let (socket, request) = (cmd.socket, cmd.request);
        let refuse = |sentence: &str| refusal(socket, request, sentence);
        let envelope: Envelope =
            serde_json::from_str(&cmd.json).map_err(|_| refuse("the command is not valid"))?;
        if !plausible(&envelope.cmd) {
            return Err(refuse("unknown command"));
        }
        let (work, mode, pin) = match lookup(&envelope.cmd) {
            Some(command) => {
                if matches!(command.web, Web::Unavailable) {
                    return Err(outcome_frame(socket, request, unavailable()));
                }
                let args = match envelope.args {
                    None => String::from("{}"),
                    Some(value @ Value::Object(_)) => value.to_string(),
                    Some(_) => return Err(refuse("the arguments are not a JSON object")),
                };
                if !command.admits(args.len()) {
                    return Err(refuse("the arguments are too large"));
                }
                (Work::Row(command, args), command.mode, command.name == self.replay)
            }
            None => {
                let hook =
                    self.hooks.iter().find(|h| h.name == envelope.cmd).ok_or_else(|| refuse("unknown command"))?;
                (Work::Hook(hook.run.clone()), hook.mode, false)
            }
        };
        let job = Job { socket, request, name: envelope.cmd, work, pin };
        let lane = match mode {
            Mode::Sync => &self.sync,
            Mode::Async => &self.pool,
        };
        match lane.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(refuse("too many requests in progress")),
            Err(TrySendError::Disconnected(_)) => Err(refuse("the worker is ending")),
        }
    }
}

fn take(jobs: &Mutex<Receiver<Job>>) -> Option<Job> {
    let jobs = jobs.lock().unwrap_or_else(PoisonError::into_inner);
    jobs.recv().ok()
}
