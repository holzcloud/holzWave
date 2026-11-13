//! The session host: programs that live in a person's worker, outside every window, and the clients that attach to them.
//!
//! A session is one headless pane of the core: a program on a PTY with an engine that draws for no page. It keeps running
//! when no client is attached, and it does not outlive its worker (a worker that ends, by its idle time or with the server,
//! ends its sessions). A client attaches and gets the screen and a bounded tail of the scrollback as one atomic snapshot of
//! the engine, then every byte the program writes after that, with no byte missed or repeated across the join. The host is
//! the one place that sees every attachment, so it owns the writer lease ([`crate::worker::lease`]) and applies it to input
//! and resizes before anything reaches the program.
//!
//! A client is anything that can take bytes: it implements [`Attachment`], and the host calls it from its own threads. A
//! method of an attachment never blocks and never calls back into the host; a push that cannot be taken is reported by
//! returning `false`, and the host then ends that attachment's stream with `resync` and goes on. The host never waits for
//! a client.
//!
//! The ids of sessions are a key in this host's own table, and a host is one person's. Another person's session id is an id
//! this host has not made: it is `not found`, the same answer as an id that never existed.
//!
//! # Threads
//!
//! One housekeeping thread per host: it frees leases whose holder was silent for the whole time, ends sessions whose program
//! ended, and sleeps until the next deadline (an idle host wakes for nothing). One reader thread per attachment moves the
//! snapshot and the live bytes to the attachment and ends with the stream. A session's lock is never held while the host's
//! table is locked, and the sink of a pane (which runs on the pane's engine thread) never takes a session's lock: the
//! engine thread may be the one a locked session is waiting for.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use holzwave_core::Error as PaneError;
use holzwave_core::layout::PaneId;
use holzwave_core::pane::{MAX_DIM, MAX_TAPS, MIN_COLS, MIN_ROWS, PaneEvent, PaneHandle, PaneSpec, TapEnd, TapId, TapMsg, TapReceiver};

use crate::worker::lease::{Changes, Input, Lease, Refused, Who, WriterState};

/// The most bytes of a snapshot chunk or of a live chunk one call of an attachment carries. A snapshot is cut into chunks of
/// this size; a live chunk is never larger because the pane reads its PTY in chunks of this size.
pub const CHUNK_BYTES: usize = 64 * 1024;

/// The most bytes of input one call takes.
pub const MAX_INPUT_BYTES: usize = 64 * 1024;

/// The longest title of a session in the list, in bytes.
pub const MAX_TITLE_BYTES: usize = 256;

/// The longest working directory of a session in the list, in bytes.
pub const MAX_CWD_BYTES: usize = 4096;

/// The most sessions a list carries. The pane cap of a worker is at most 1024 (`workers.max_panes`).
pub const MAX_LISTED: usize = 1024;

/// How long a session whose program ended stays in the list for the clients that are attached, in milliseconds. It leaves at
/// once when the last attachment detaches.
pub const EXITED_GRACE_MS: u64 = 10_000;

/// The bytes that may wait for one attachment's reader, per attachment: a client that falls further behind loses its stream
/// and is told to attach again.
pub const TAP_QUEUE_BYTES: usize = 4 * 1024 * 1024;

/// How long the reader of a stream waits for the program's exit status after the stream ended with its pane.
const EXIT_STATUS_WAIT: Duration = Duration::from_secs(2);

/// How long the longest sleep of the housekeeping thread is, whatever it knows of the deadlines.
const LONGEST_SLEEP: Duration = Duration::from_secs(60);

/// How short the sleep of the housekeeping thread may be, so that a deadline that has just passed is not a busy loop.
const SHORTEST_SLEEP: Duration = Duration::from_millis(5);

/// The number a session's pane is given, added to the session id: a label for the pane's thread names, distinct from the
/// numbers of the layout's panes.
const PANE_LABEL_BASE: PaneId = 1 << 40;

// ---- the numbers of the four keys --------------------------------------------------------------------------------------------

/// The four settings that bound what a client may ask of a session (the `remote` section of the configuration).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionLimits {
    /// The rows of scrollback that come with the screen when a client does not say how many it wants.
    pub attach_tail_lines: u32,
    /// The most rows of scrollback any attach may bring, whatever the client asks; never above the pane's own scrollback.
    pub history_max_lines: u32,
    /// The most bytes a snapshot may have; a larger one is refused, never cut.
    pub snapshot_max_bytes: u32,
    /// How long the holder of a session may be silent before the right to write is free, in milliseconds.
    pub writer_timeout_ms: u32,
}

impl SessionLimits {
    /// The defaults of the four keys.
    pub const DEFAULT: SessionLimits = SessionLimits {
        attach_tail_lines: 1000,
        history_max_lines: 10_000,
        snapshot_max_bytes: 4 * 1024 * 1024,
        writer_timeout_ms: 30_000,
    };
}

impl Default for SessionLimits {
    fn default() -> SessionLimits {
        SessionLimits::DEFAULT
    }
}

// ---- errors, answers ---------------------------------------------------------------------------------------------------------

/// Why a request of a client is not done. A closed set of words: no echo of anything the client sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SesError {
    /// No such session on this host (an id that was never made, that has ended, or that another person's host made).
    NotFound,
    /// The request is understood and not allowed: not the writer, a snapshot over its cap, a pane that cannot start.
    Refused,
    /// The session cannot answer now.
    Busy,
    /// A cap is reached: the panes of the person, or the attachments of one session.
    TooMany,
    /// The request is not valid.
    BadRequest,
}

impl SesError {
    /// The word the device socket answers with.
    pub fn word(self) -> &'static str {
        match self {
            SesError::NotFound => "not_found",
            SesError::Refused => "refused",
            SesError::Busy => "busy",
            SesError::TooMany => "too_many",
            SesError::BadRequest => "bad_request",
        }
    }
}

impl fmt::Display for SesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

impl std::error::Error for SesError {}

/// One line of the list of sessions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: u32,
    /// The program's own title, cleaned and bounded; empty when it set none.
    pub title: String,
    /// The program's working directory as the shell reported it; empty when it did not.
    pub cwd: String,
    pub cols: u16,
    pub rows: u16,
    /// The start, in seconds since the Unix epoch.
    pub started: u64,
    /// How many clients are attached.
    pub attached: u16,
    /// Whether an attachment holds the right to write.
    pub writer_held: bool,
}

/// What an attach answers: the grid the client sets before it feeds the snapshot, its own view of the right to write, whether
/// older lines exist beyond the tail that was sent, and how many bytes of snapshot follow in chunks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attached {
    pub cols: u16,
    pub rows: u16,
    pub writer: WriterState,
    pub more: bool,
    pub snapshot_bytes: u32,
}

// ---- the seams: attachment, clock, spawner -----------------------------------------------------------------------------------

/// A client of a session, as the host sees it. Every method returns at once and never calls the host.
pub trait Attachment: Send + Sync {
    /// The next chunk of the snapshot, `seq` counting from 0. `false` when it cannot be taken now (a full queue).
    fn snapshot_chunk(&self, session: u32, seq: u32, bytes: &[u8]) -> bool;
    /// The next bytes the program wrote, `seq` continuing the count of the snapshot. `false` when they cannot be taken.
    fn live(&self, session: u32, seq: u32, bytes: &[u8]) -> bool;
    /// This attachment's own view of the right to write changed.
    fn writer(&self, session: u32, state: WriterState);
    /// The grid of the session changed.
    fn size(&self, session: u32, cols: u16, rows: u16);
    /// The program ended, with its exit status when it could be read.
    fn exited(&self, session: u32, code: Option<u32>);
    /// The stream of this attachment was lost: attach again for a fresh snapshot.
    fn resync(&self, session: u32);
}

/// The time the lease and the grace are measured in: milliseconds from some point, never going back.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// The real clock: monotonic milliseconds since it was made.
pub struct MonotonicClock {
    start: Instant,
}

impl MonotonicClock {
    pub fn new() -> MonotonicClock {
        MonotonicClock { start: Instant::now() }
    }
}

impl Default for MonotonicClock {
    fn default() -> MonotonicClock {
        MonotonicClock::new()
    }
}

impl Clock for MonotonicClock {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// A place under the person's pane cap, held by a session while it lives and given back when it is dropped.
pub type Place = Box<dyn Send + Sync>;

/// What starts a session: the program (the person's shell in the product, a script in a test) and the place under the cap.
pub trait Spawner: Send + Sync {
    /// How a session of `cols` x `rows` starts. The host makes it headless whatever this says.
    fn spec(&self, cols: u16, rows: u16) -> PaneSpec;
    /// A place under the pane cap, or `None` when the person has as many panes and sessions as the cap allows.
    fn take_place(&self) -> Option<Place>;
}

// ---- the host ----------------------------------------------------------------------------------------------------------------

/// What a pane told, kept apart from the session so that the pane's own thread never needs the session's lock.
#[derive(Default)]
struct Meta {
    state: Mutex<MetaState>,
    changed: Condvar,
}

#[derive(Default)]
struct MetaState {
    title: String,
    cwd: String,
    /// `Some` once the program ended; the inner `None` is an exit status that could not be read.
    exit: Option<Option<u32>>,
}

impl Meta {
    fn lock(&self) -> MutexGuard<'_, MetaState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn exit(&self) -> Option<Option<u32>> {
        self.lock().exit
    }

    /// The exit status once there is one, waiting at most `wait` for it.
    fn wait_exit(&self, wait: Duration) -> Option<Option<u32>> {
        let deadline = Instant::now() + wait;
        let mut state = self.lock();
        loop {
            if let Some(exit) = state.exit {
                return Some(exit);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            state = self.changed.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}

/// Removes control characters and cuts at a character boundary at or below `max` bytes.
fn clean(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

enum Event {
    /// The program of a session ended.
    Exited(u32),
    /// A deadline may have changed.
    Wake,
    /// The host ends.
    Stop,
}

/// The end of a stream on purpose. The reader holds it while it hands anything to its attachment, and whoever ends the stream
/// takes it to do so: once [`Halt::set`] has returned, nothing of this stream reaches the attachment any more. That is what
/// lets an attach answer a client right after it ended the client's old stream, with no old byte after the answer.
#[derive(Default)]
struct Halt(Mutex<bool>);

impl Halt {
    /// Ends the stream; waits for a push that is under way, which never blocks (see [`Attachment`]).
    fn set(&self) {
        *lock(&self.0) = true;
    }

    fn is_set(&self) -> bool {
        *lock(&self.0)
    }

    /// Runs `push` unless the stream was ended, and keeps it from being ended while `push` runs; `None` when it was ended.
    fn unless<R>(&self, push: impl FnOnce() -> R) -> Option<R> {
        let halted = lock(&self.0);
        if *halted { None } else { Some(push()) }
    }
}

/// The flags of one attachment's reader thread.
struct Stream {
    tap: TapId,
    /// Set by whoever ends the stream on purpose: the reader then says nothing more.
    stop: Arc<Halt>,
    /// Set by the reader as its last act.
    done: Arc<AtomicBool>,
    /// Set by whoever tells this attachment that the program ended, so that it is told once.
    told_exit: Arc<AtomicBool>,
}

struct Member {
    client: u64,
    who: Who,
    att: Arc<dyn Attachment>,
    /// `None` between `create` and the first `attach`, and after an overflow.
    stream: Option<Stream>,
}

struct Session {
    id: u32,
    pane: PaneHandle,
    lease: Lease,
    members: Vec<Member>,
    cols: u16,
    rows: u16,
    /// The scrollback the pane was started with, which no attach can exceed.
    scrollback: usize,
    started: u64,
    meta: Arc<Meta>,
    /// The time the program's end was seen.
    exited_at: Option<u64>,
    _place: Place,
}

#[derive(Default)]
struct Table {
    sessions: BTreeMap<u32, Arc<Mutex<Session>>>,
    /// The next session id; ids are never reused while the host lives.
    next_id: u32,
    /// The next attachment identity of the lease.
    next_who: u64,
}

type CountHook = Box<dyn Fn(usize) + Send + Sync>;

struct Shared {
    table: Mutex<Table>,
    spawner: Arc<dyn Spawner>,
    limits: SessionLimits,
    tap_queue_bytes: usize,
    clock: Arc<dyn Clock>,
    events: Sender<Event>,
    on_count: Mutex<Option<CountHook>>,
}

/// The sessions of one person's worker.
pub struct SessionHost {
    shared: Arc<Shared>,
}

/// Settings of a host that are not the four keys.
#[derive(Clone, Copy, Debug)]
pub struct HostSettings {
    pub limits: SessionLimits,
    /// The bytes that may wait for one attachment's reader ([`TAP_QUEUE_BYTES`]).
    pub tap_queue_bytes: usize,
}

impl Default for HostSettings {
    fn default() -> HostSettings {
        HostSettings { limits: SessionLimits::DEFAULT, tap_queue_bytes: TAP_QUEUE_BYTES }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// What a failed attach of a tap is, as the client is told.
fn tap_error(error: &PaneError) -> SesError {
    match error {
        PaneError::Pty(text) if text == "pane busy" => SesError::Busy,
        PaneError::Pty(text) if text.starts_with("too many taps") => SesError::TooMany,
        PaneError::Pty(text) if text == "the pane is closed" => SesError::NotFound,
        _ => SesError::Refused,
    }
}

impl SessionHost {
    /// A host with the real clock and the default settings.
    pub fn new(spawner: Arc<dyn Spawner>, limits: SessionLimits) -> SessionHost {
        SessionHost::start(spawner, HostSettings { limits, ..HostSettings::default() }, Arc::new(MonotonicClock::new()))
    }

    /// A host with its own clock and settings, which is how a test steps time.
    pub fn start(spawner: Arc<dyn Spawner>, settings: HostSettings, clock: Arc<dyn Clock>) -> SessionHost {
        let (events, inbox) = mpsc::channel();
        let shared = Arc::new(Shared {
            table: Mutex::new(Table { sessions: BTreeMap::new(), next_id: 1, next_who: 1 }),
            spawner,
            limits: settings.limits,
            tap_queue_bytes: settings.tap_queue_bytes,
            clock,
            events,
            on_count: Mutex::new(None),
        });
        let work = shared.clone();
        std::thread::Builder::new()
            .name("hw-sessions".into())
            .spawn(move || housekeeping(&work, &inbox))
            .expect("the housekeeping thread starts");
        SessionHost { shared }
    }

    /// Calls `hook` with the number of sessions whenever it changes (after a create, an end, a removal).
    pub fn set_count_hook(&self, hook: impl Fn(usize) + Send + Sync + 'static) {
        *lock(&self.shared.on_count) = Some(Box::new(hook));
    }

    /// How many sessions the host holds, ended ones that are still listed included.
    pub fn len(&self) -> usize {
        lock(&self.shared.table).sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The limits the host was started with.
    pub fn limits(&self) -> SessionLimits {
        self.shared.limits
    }

    /// The running sessions and the ended ones that are still listed, in the order of their ids.
    pub fn list(&self) -> Vec<SessionInfo> {
        let mut out = Vec::new();
        for session in self.shared.all() {
            let s = lock(&session);
            let meta = s.meta.lock();
            out.push(SessionInfo {
                id: s.id,
                title: meta.title.clone(),
                cwd: meta.cwd.clone(),
                cols: s.cols,
                rows: s.rows,
                started: s.started,
                attached: u16::try_from(s.members.len()).unwrap_or(u16::MAX),
                writer_held: s.lease.holder().is_some(),
            });
            if out.len() >= MAX_LISTED {
                break;
            }
        }
        out
    }

    /// Starts a session of `cols` x `rows` for `client` and makes `client` its writer. The creator is attached without a
    /// stream: it gets the screen and the bytes with its first [`SessionHost::attach`].
    pub fn create(&self, client: u64, att: Arc<dyn Attachment>, cols: u16, rows: u16) -> Result<u32, SesError> {
        self.shared.create(client, att, cols, rows)
    }

    /// Attaches `client` to `session` and brings it the screen and `tail` rows of scrollback (the configured default when
    /// `None`, clamped to the history cap and to the pane's own scrollback). `reply` is called exactly once, with the answer,
    /// and only after it has returned does the first chunk of the snapshot go to the attachment, so that the client reads the
    /// answer first. A client that is attached already and attaches again keeps its place in the lease and gets a new stream:
    /// that is how it loads older lines.
    pub fn attach(
        &self,
        client: u64,
        att: Arc<dyn Attachment>,
        session: u32,
        tail: Option<u32>,
        reply: impl FnOnce(Result<Attached, SesError>),
    ) {
        self.shared.attach(client, att, session, tail, reply);
    }

    /// `client` leaves `session`. When it held the right to write the right is free and the others are told.
    pub fn detach(&self, client: u64, session: u32) -> Result<(), SesError> {
        let found = self.shared.find(session).ok_or(SesError::NotFound)?;
        self.shared.detach_member(&found, client);
        Ok(())
    }

    /// The stream of `client` in `session` could not be delivered and the client was told to attach again: the stream ends,
    /// and the client stays attached with its place in the lease, as when its own reader loses the stream. An attach of the
    /// client gives it a new one.
    pub fn drop_stream(&self, client: u64, session: u32) -> Result<(), SesError> {
        let found = self.shared.find(session).ok_or(SesError::NotFound)?;
        let mut guard = lock(&found);
        let s = &mut *guard;
        let Some(member) = s.members.iter_mut().find(|m| m.client == client) else { return Ok(()) };
        if let Some(stream) = member.stream.take() {
            stream.stop.set();
            s.pane.detach_tap(stream.tap);
        }
        Ok(())
    }

    /// The socket of `client` ended: it leaves every session.
    pub fn gone(&self, client: u64) {
        for session in self.shared.all() {
            self.shared.detach_member(&session, client);
        }
    }

    /// Input of `client` for `session`: it reaches the program when `client` holds the right to write and is dropped when it
    /// does not. It is also dropped, whole, while the program leaves more than
    /// [`holzwave_core::pane::MAX_RAW_BACKLOG`] bytes of its input unread: the pane refuses it, so a sender cannot grow the
    /// worker's memory by writing to a program that does not read.
    pub fn input(&self, client: u64, session: u32, bytes: &[u8]) -> Result<Input, SesError> {
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(SesError::BadRequest);
        }
        let found = self.shared.find(session).ok_or(SesError::NotFound)?;
        let mut s = lock(&found);
        let now = self.shared.clock.now_ms();
        let Some(who) = s.members.iter().find(|m| m.client == client).map(|m| m.who) else { return Ok(Input::Dropped) };
        if s.lease.input(who, now) == Input::Dropped || s.exited_at.is_some() {
            return Ok(Input::Dropped);
        }
        if s.pane.write_raw(bytes).is_err() {
            return Ok(Input::Dropped);
        }
        Ok(Input::Accepted)
    }

    /// Resizes `session` to `cols` x `rows` for `client`, which must hold the right to write; every attachment is told.
    pub fn resize(&self, client: u64, session: u32, cols: u16, rows: u16) -> Result<(), SesError> {
        let found = self.shared.find(session).ok_or(SesError::NotFound)?;
        let mut s = lock(&found);
        let now = self.shared.clock.now_ms();
        let who = s.members.iter().find(|m| m.client == client).map(|m| m.who).ok_or(SesError::Refused)?;
        if !s.lease.resize_allowed(who) || s.exited_at.is_some() {
            return Err(SesError::Refused);
        }
        let (cols, rows) = (cols.clamp(MIN_COLS, MAX_DIM), rows.clamp(MIN_ROWS, MAX_DIM));
        s.pane.resize(cols, rows, (0, 0)).map_err(|_| SesError::Refused)?;
        s.cols = cols;
        s.rows = rows;
        s.lease.beat(who, now);
        for member in &s.members {
            member.att.size(session, cols, rows);
        }
        Ok(())
    }

    /// `client`, attached to `session`, takes the right to write; every attachment is told its own view.
    pub fn takeover(&self, client: u64, session: u32) -> Result<(), SesError> {
        let found = self.shared.find(session).ok_or(SesError::NotFound)?;
        let mut s = lock(&found);
        let now = self.shared.clock.now_ms();
        let who = s.members.iter().find(|m| m.client == client).map(|m| m.who).ok_or(SesError::Refused)?;
        let changes = match s.lease.takeover(who, now) {
            Ok(changes) => changes,
            Err(Refused::NotAttached) => return Err(SesError::Refused),
        };
        tell_writers(&s, &changes);
        drop(s);
        let _ = self.shared.events.send(Event::Wake);
        Ok(())
    }

    /// A sign of life of `client` in `session`; only the holder's counts.
    pub fn beat(&self, client: u64, session: u32) -> Result<(), SesError> {
        let found = self.shared.find(session).ok_or(SesError::NotFound)?;
        let mut s = lock(&found);
        let now = self.shared.clock.now_ms();
        if let Some(who) = s.members.iter().find(|m| m.client == client).map(|m| m.who) {
            s.lease.beat(who, now);
        }
        Ok(())
    }

    /// Ends the program of `session`. The attachments hear `exited` when it has ended.
    pub fn end(&self, session: u32) -> Result<(), SesError> {
        let found = self.shared.find(session).ok_or(SesError::NotFound)?;
        lock(&found).pane.close();
        Ok(())
    }

    /// Frees the leases whose holder was silent for the whole time and removes the ended sessions whose grace is over, as
    /// of the host's clock now. The housekeeping thread does this by itself; a test calls it to step time.
    pub fn sweep(&self) {
        self.shared.sweep(self.shared.clock.now_ms());
    }

    /// Ends every program and forgets every session. Nothing is told to the attachments: the worker is ending.
    pub fn shutdown(&self) {
        let sessions: Vec<_> = {
            let mut table = lock(&self.shared.table);
            std::mem::take(&mut table.sessions).into_values().collect()
        };
        for session in &sessions {
            let s = lock(session);
            for member in &s.members {
                if let Some(stream) = &member.stream {
                    stream.stop.set();
                }
            }
            s.pane.close();
        }
        drop(sessions);
        self.shared.counted();
    }
}

impl Drop for SessionHost {
    fn drop(&mut self) {
        self.shutdown();
        let _ = self.shared.events.send(Event::Stop);
    }
}

/// Every attachment hears its own view of the change.
fn tell_writers(session: &Session, changes: &Changes) {
    for (who, state) in changes {
        if let Some(member) = session.members.iter().find(|m| m.who == *who) {
            member.att.writer(session.id, *state);
        }
    }
}

impl Shared {
    fn find(&self, id: u32) -> Option<Arc<Mutex<Session>>> {
        lock(&self.table).sessions.get(&id).cloned()
    }

    fn all(&self) -> Vec<Arc<Mutex<Session>>> {
        lock(&self.table).sessions.values().cloned().collect()
    }

    /// Tells the count hook how many sessions there are now.
    fn counted(&self) {
        let count = lock(&self.table).sessions.len();
        if let Some(hook) = &*lock(&self.on_count) {
            hook(count);
        }
    }

    fn create(&self, client: u64, att: Arc<dyn Attachment>, cols: u16, rows: u16) -> Result<u32, SesError> {
        let place = self.spawner.take_place().ok_or(SesError::TooMany)?;
        let (id, who) = {
            let mut table = lock(&self.table);
            let id = table.next_id;
            table.next_id = id.checked_add(1).ok_or(SesError::TooMany)?;
            let who = Who(table.next_who);
            table.next_who += 1;
            (id, who)
        };
        let (cols, rows) = (cols.clamp(MIN_COLS, MAX_DIM), rows.clamp(MIN_ROWS, MAX_DIM));
        let mut spec = self.spawner.spec(cols, rows);
        spec.headless = true;
        let scrollback = spec.scrollback;
        let meta = Arc::new(Meta::default());
        let sink = {
            let meta = meta.clone();
            let events = self.events.clone();
            move |event: PaneEvent| match event {
                PaneEvent::Title(title) => meta.lock().title = clean(&title, MAX_TITLE_BYTES),
                PaneEvent::Cwd(cwd) => meta.lock().cwd = clean(&cwd, MAX_CWD_BYTES),
                PaneEvent::Exited { code } => {
                    meta.lock().exit = Some(code);
                    meta.changed.notify_all();
                    let _ = events.send(Event::Exited(id));
                }
                PaneEvent::Failed(_) => {
                    meta.lock().exit = Some(None);
                    meta.changed.notify_all();
                    let _ = events.send(Event::Exited(id));
                }
                _ => {}
            }
        };
        let pane = PaneHandle::spawn_ghostty(PANE_LABEL_BASE + PaneId::from(id), spec, sink).map_err(|_| SesError::Refused)?;
        let now = self.clock.now_ms();
        let mut lease = Lease::new(u64::from(self.limits.writer_timeout_ms));
        lease.attach(who, true, now);
        let session = Session {
            id,
            pane,
            lease,
            members: vec![Member { client, who, att, stream: None }],
            cols,
            rows,
            scrollback,
            started: unix_now(),
            meta,
            exited_at: None,
            _place: place,
        };
        lock(&self.table).sessions.insert(id, Arc::new(Mutex::new(session)));
        self.counted();
        let _ = self.events.send(Event::Wake);
        Ok(id)
    }

    fn attach(
        &self,
        client: u64,
        att: Arc<dyn Attachment>,
        id: u32,
        tail: Option<u32>,
        reply: impl FnOnce(Result<Attached, SesError>),
    ) {
        match self.attach_inner(client, att.clone(), id, tail) {
            Ok((attached, start)) => {
                reply(Ok(attached));
                start.run(att, id);
            }
            Err(error) => reply(Err(error)),
        }
    }

    fn attach_inner(
        &self,
        client: u64,
        att: Arc<dyn Attachment>,
        id: u32,
        tail: Option<u32>,
    ) -> Result<(Attached, StreamStart), SesError> {
        let found = self.find(id).ok_or(SesError::NotFound)?;
        let mut s = lock(&found);
        if s.exited_at.is_some() || s.meta.exit().is_some() {
            return Err(SesError::NotFound);
        }
        let now = self.clock.now_ms();
        let existing = s.members.iter().position(|m| m.client == client);
        if existing.is_none() && s.members.len() >= MAX_TAPS {
            return Err(SesError::TooMany);
        }
        let wanted = tail.unwrap_or(self.limits.attach_tail_lines).min(self.limits.history_max_lines);
        let tail_rows = usize::try_from(wanted).unwrap_or(usize::MAX).min(s.scrollback);
        // The old stream of this client (a reattach) ends first, so that it cannot hand a byte to the new one's place: once
        // `set` returns, its reader pushes nothing more, so nothing of it follows the answer below.
        if let Some(at) = existing
            && let Some(stream) = s.members[at].stream.take()
        {
            stream.stop.set();
            s.pane.detach_tap(stream.tap);
        }
        let max_bytes = usize::try_from(self.limits.snapshot_max_bytes).unwrap_or(usize::MAX);
        let tap = s.pane.attach_tap(tail_rows, max_bytes, self.tap_queue_bytes).map_err(|e| tap_error(&e))?;
        let who = match existing {
            Some(at) => {
                s.members[at].att = att;
                s.members[at].who
            }
            None => {
                let who = {
                    let mut table = lock(&self.table);
                    let who = Who(table.next_who);
                    table.next_who += 1;
                    who
                };
                // The first entry of the change is the new attachment's own view, which the answer carries.
                let changes = s.lease.attach(who, false, now);
                s.members.push(Member { client, who, att, stream: None });
                let others: Changes = changes.into_iter().filter(|(w, _)| *w != who).collect();
                tell_writers(&s, &others);
                who
            }
        };
        let stream = Stream {
            tap: tap.id,
            stop: Arc::new(Halt::default()),
            done: Arc::new(AtomicBool::new(false)),
            told_exit: Arc::new(AtomicBool::new(false)),
        };
        let start = StreamStart {
            snapshot: tap.snapshot,
            rx: tap.rx,
            tap: tap.id,
            pane: s.pane.clone(),
            meta: s.meta.clone(),
            stop: stream.stop.clone(),
            done: stream.done.clone(),
            told_exit: stream.told_exit.clone(),
        };
        let at = s.members.iter().position(|m| m.who == who).ok_or(SesError::NotFound)?;
        s.members[at].stream = Some(stream);
        let attached = Attached {
            cols: tap.size.0,
            rows: tap.size.1,
            writer: s.lease.view(who).unwrap_or(WriterState::Free),
            more: tap.history_rows > tail_rows,
            snapshot_bytes: u32::try_from(start.snapshot.len()).map_err(|_| SesError::Refused)?,
        };
        Ok((attached, start))
    }

    /// `client` leaves one session, if it is attached to it.
    fn detach_member(&self, found: &Arc<Mutex<Session>>, client: u64) {
        let mut s = lock(found);
        let Some(at) = s.members.iter().position(|m| m.client == client) else { return };
        let member = s.members.remove(at);
        if let Some(stream) = member.stream {
            stream.stop.set();
            s.pane.detach_tap(stream.tap);
        }
        let changes = s.lease.detach(member.who);
        tell_writers(&s, &changes);
        let remove = s.exited_at.is_some() && s.members.is_empty();
        let id = s.id;
        drop(s);
        if remove {
            self.remove(&[id]);
        }
    }

    /// The program of session `id` ended: every attachment is told, and a session nobody is attached to leaves at once.
    fn session_exited(&self, id: u32) {
        let Some(found) = self.find(id) else { return };
        let mut s = lock(&found);
        if s.exited_at.is_some() {
            return;
        }
        s.exited_at = Some(self.clock.now_ms());
        let code = s.meta.exit().flatten();
        for member in &s.members {
            match &member.stream {
                // The reader of a running stream tells its attachment itself, after the last byte it delivers.
                Some(stream) if !stream.done.load(Ordering::SeqCst) => {}
                Some(stream) => {
                    if !stream.told_exit.swap(true, Ordering::SeqCst) {
                        member.att.exited(id, code);
                    }
                }
                None => member.att.exited(id, code),
            }
        }
        let empty = s.members.is_empty();
        drop(s);
        if empty {
            self.remove(&[id]);
        }
    }

    fn remove(&self, ids: &[u32]) {
        let removed: Vec<_> = {
            let mut table = lock(&self.table);
            ids.iter().filter_map(|id| table.sessions.remove(id)).collect()
        };
        if removed.is_empty() {
            return;
        }
        // Dropping the sessions gives their places back; the panes of ended sessions are gone already.
        drop(removed);
        self.counted();
    }

    /// The leases whose holder was silent for the whole time are free, and the ended sessions past their grace are removed.
    fn sweep(&self, now: u64) {
        let mut gone = Vec::new();
        for found in self.all() {
            let mut s = lock(&found);
            let changes = s.lease.sweep(now);
            if !changes.is_empty() {
                tell_writers(&s, &changes);
            }
            if let Some(at) = s.exited_at
                && now.saturating_sub(at) >= EXITED_GRACE_MS
            {
                gone.push(s.id);
            }
        }
        if !gone.is_empty() {
            self.remove(&gone);
        }
    }

    /// How long the housekeeping thread may sleep: until the earliest deadline of a lease or of a grace.
    fn next_wait(&self, now: u64) -> Duration {
        let mut earliest: Option<u64> = None;
        for found in self.all() {
            let s = lock(&found);
            let deadlines = [s.lease.deadline(), s.exited_at.map(|at| at.saturating_add(EXITED_GRACE_MS))];
            for deadline in deadlines.into_iter().flatten() {
                earliest = Some(earliest.map_or(deadline, |e| e.min(deadline)));
            }
        }
        match earliest {
            None => LONGEST_SLEEP,
            Some(deadline) => Duration::from_millis(deadline.saturating_sub(now)).clamp(SHORTEST_SLEEP, LONGEST_SLEEP),
        }
    }
}

/// What the reader of a new stream owns.
struct StreamStart {
    snapshot: Vec<u8>,
    rx: TapReceiver,
    tap: TapId,
    pane: PaneHandle,
    meta: Arc<Meta>,
    stop: Arc<Halt>,
    done: Arc<AtomicBool>,
    told_exit: Arc<AtomicBool>,
}

impl StreamStart {
    /// Starts the reader thread: the snapshot in chunks, then every byte the tap delivers.
    fn run(self, att: Arc<dyn Attachment>, session: u32) {
        let spawned = std::thread::Builder::new().name(format!("hw-stream-{session}")).spawn(move || self.read(&*att, session));
        if spawned.is_err() {
            // No thread, no stream: the tap is dropped with the closure and the client attaches again.
        }
    }

    /// Every push to the attachment, and every word about the stream, happens inside [`Halt::unless`]: a stream that was ended
    /// on purpose (a reattach, a detach) says nothing more, not even a `resync` that would undo the new attach.
    fn read(self, att: &dyn Attachment, session: u32) {
        let lost = |start: &StreamStart| {
            start.pane.detach_tap(start.tap);
            start.stop.unless(|| att.resync(session));
        };
        let mut seq: u32 = 0;
        let mut healthy = true;
        for chunk in self.snapshot.chunks(CHUNK_BYTES) {
            match self.stop.unless(|| att.snapshot_chunk(session, seq, chunk)) {
                None => {
                    self.finish(att, session);
                    return;
                }
                Some(true) => seq = seq.wrapping_add(1),
                Some(false) => {
                    lost(&self);
                    healthy = false;
                    break;
                }
            }
        }
        loop {
            // A snapshot that could not be sent ends the stream before it starts; so does a stream ended on purpose.
            if !healthy || self.stop.is_set() {
                break;
            }
            match self.rx.recv(Duration::from_millis(250)) {
                Some(TapMsg::Bytes(bytes)) => match self.stop.unless(|| att.live(session, seq, &bytes)) {
                    None => break,
                    Some(true) => seq = seq.wrapping_add(1),
                    Some(false) => {
                        lost(&self);
                        break;
                    }
                },
                Some(TapMsg::End(TapEnd::Overflow)) => {
                    self.stop.unless(|| att.resync(session));
                    break;
                }
                Some(TapMsg::End(TapEnd::Closed)) => {
                    // The pane ended after the last byte it delivered, unless this stream was ended on purpose; the status of
                    // the program arrives from the pane's own thread a moment later.
                    if !self.stop.is_set() {
                        let _ = self.meta.wait_exit(EXIT_STATUS_WAIT);
                    }
                    break;
                }
                None => {}
            }
        }
        self.finish(att, session);
    }

    /// The last act of the reader: it is done, and an attachment that was not told that the program ended is told now.
    fn finish(&self, att: &dyn Attachment, session: u32) {
        self.done.store(true, Ordering::SeqCst);
        if let Some(exit) = self.meta.exit() {
            self.stop.unless(|| {
                if !self.told_exit.swap(true, Ordering::SeqCst) {
                    att.exited(session, exit);
                }
            });
        }
    }
}

/// The housekeeping thread: events of the panes and deadlines of the leases.
fn housekeeping(shared: &Shared, inbox: &Receiver<Event>) {
    loop {
        let wait = shared.next_wait(shared.clock.now_ms());
        match inbox.recv_timeout(wait) {
            Ok(Event::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Ok(Event::Exited(id)) => shared.session_exited(id),
            Ok(Event::Wake) | Err(RecvTimeoutError::Timeout) => {}
        }
        shared.sweep(shared.clock.now_ms());
    }
}
