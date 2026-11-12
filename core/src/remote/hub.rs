//! The hub: the one socket of a device to a holzWave server, and everything that rides on it.
//!
//! A device has one connection to its server. Every session the device shows, the list of sessions and the creation and the
//! end of a session go over it, so there is one reader, one beat, one reconnect and one table of answers, and no pane reaches
//! into any of them. A pane asks the hub for a [`Subscription`] and gets [`SubEvent`]s in order; it never sees a message of the
//! vocabulary ([`super::proto`]) or the socket.
//!
//! **Threads.** One thread owns the [`WsClient`]: it reads with a short timeout, sends what the callers hand it through a
//! channel, matches answers to requests by number, and routes the binary frames of the server to the subscription of the
//! session they name. The timeout is short while a subscription exists, so a typed byte leaves within a few milliseconds, and
//! long otherwise. Connecting is blocking by design (the platform verifier of a certificate can take a long time): it happens
//! on this thread and nowhere else, never on the caller's.
//!
//! **Trust.** Everything the server sends is untrusted. A text over the bound of the vocabulary, an answer or an event with a
//! member that is not in the vocabulary, a frame with an unknown tag, a size outside the clamps of a pane, a snapshot longer
//! than the answer promised or than [`HubOptions::max_snapshot`], and a sequence number that is not the next one are each
//! refused: the stream of that session ends with a fixed fault, or (for a message that cannot be read at all) the connection
//! does. Nothing is allocated for a number the server announces; bytes are held only as they arrive, in a queue per
//! subscription that is counted in bytes and never grows past [`HubOptions::sub_queue_bytes`]: a subscription that cannot keep
//! up is attached again from a fresh snapshot, never buffered without bound.
//!
//! **Faults.** A call or a subscription ends with a [`HubError`], a closed set whose every case has one fixed sentence. No
//! sentence holds a byte the server wrote, and neither the token nor a byte of terminal output is ever logged or printed.
//!
//! **Reconnect.** A lost connection that a person has subscriptions on is made again with the same shape as an SSH pane
//! ([`Reconnect`]: five tries, three seconds apart), with a fresh token from the [`Endpoint`] for every try, and every
//! subscription is attached again from a fresh snapshot with the tail it last used. A token the server refuses (a 401 on the
//! upgrade or the close code 4401), a feature that is off, a refused upgrade and a certificate that is not trusted are final:
//! they are not tried again.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::proto::{
    self, Answer, Chunk, ErrorWord, Event, MAX_ATTACHED, MAX_INPUT, MAX_PENDING, MAX_REPLY, MAX_SIZE, MAX_TAIL, MIN_SIZE, Op,
    Reply, Request, SessionEntry, Stream, WriterView,
};
use super::ws::{Recv, WsClient, WsError, WsLimits};
use crate::server_link::{Fault, Origin, Token};
use crate::ssh::Reconnect;

/// How long a call waits for the answer of the server before it gives up.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// The most bytes of snapshot one attach may promise: the top of the server's own bound.
pub const MAX_SNAPSHOT: u32 = 16 * 1024 * 1024;

/// The bytes one subscription may hold between the hub and its reader: a whole snapshot and some live output.
pub const SUB_QUEUE_BYTES: usize = 24 * 1024 * 1024;

/// What an event that carries no bytes counts as in the queue of a subscription.
const EVENT_COST: usize = 64;

/// How many attaches in a row, made because the reader fell behind or the server asked, a subscription may need before it
/// is ended: a reader that never catches up is not helped by another snapshot.
pub const MAX_RESYNC_STREAK: u32 = 5;

/// How long the thread sleeps when it has nothing to read and nothing to do at all.
const LONG_WAIT: Duration = Duration::from_secs(3600);

/// What the hub needs to know about the server it talks to. The caller owns the custody of the credential: the hub asks for
/// a fresh [`Token`] for every connection and keeps none, so one long-lived hub survives a sign-out and a new sign-in.
pub trait Endpoint: Send + Sync {
    /// The server now, or `None` when the person is signed out.
    fn origin(&self) -> Option<Origin>;

    /// The token of this device, read from where it is kept. [`Fault::SignedOut`] when there is none.
    fn token(&self) -> Result<Token, Fault>;

    /// A TLS configuration that trusts something other than the platform's store. Only a test passes one; `None` (the
    /// default) is the platform's own.
    fn tls(&self) -> Option<Arc<rustls::ClientConfig>> {
        None
    }
}

/// The settings of a hub. Every wait and every size has a bound.
#[derive(Clone, Debug)]
pub struct HubOptions {
    /// How often and how fast a lost connection is made again.
    pub reconnect: Reconnect,
    /// How often a client that is attached says that it is alive.
    pub beat: Duration,
    /// How long a call waits for its answer.
    pub call_timeout: Duration,
    /// The most bytes of snapshot an attach may promise.
    pub max_snapshot: u32,
    /// The most bytes one subscription may hold unread.
    pub sub_queue_bytes: usize,
    /// How long the thread waits for the socket while a subscription exists: a typed byte waits at most this long.
    pub attached_poll: Duration,
    /// How long the thread waits for the socket while a subscription exists and nothing was typed or shown for
    /// [`HubOptions::resting_after`]: the first byte after a pause waits at most this long, and then the pane is quick again.
    pub resting_poll: Duration,
    /// How long nothing has to move for the thread to rest.
    pub resting_after: Duration,
    /// How long the thread waits for the socket with no subscription.
    pub idle_poll: Duration,
    /// How long a connection with no subscription and no call stays open.
    pub idle_close: Duration,
}

impl Default for HubOptions {
    fn default() -> Self {
        HubOptions {
            reconnect: Reconnect::default(),
            beat: Duration::from_secs(proto::BEAT_SECONDS),
            call_timeout: CALL_TIMEOUT,
            max_snapshot: MAX_SNAPSHOT,
            sub_queue_bytes: SUB_QUEUE_BYTES,
            attached_poll: Duration::from_millis(2),
            resting_poll: Duration::from_millis(8),
            resting_after: Duration::from_secs(1),
            idle_poll: Duration::from_millis(25),
            idle_close: Duration::from_secs(60),
        }
    }
}

impl HubOptions {
    /// The same options with every value moved into its allowed range, so a caller that built the struct by hand cannot turn
    /// a bound off.
    fn clamped(&self) -> HubOptions {
        HubOptions {
            reconnect: Reconnect { attempts: self.reconnect.attempts.min(100), delay: self.reconnect.delay.min(Duration::from_secs(3600)) },
            beat: self.beat.clamp(Duration::from_millis(5), Duration::from_secs(60)),
            call_timeout: self.call_timeout.clamp(Duration::from_millis(50), Duration::from_secs(60)),
            max_snapshot: self.max_snapshot.clamp(1024, MAX_SNAPSHOT),
            sub_queue_bytes: self.sub_queue_bytes.clamp(64 * 1024, 64 * 1024 * 1024),
            attached_poll: self.attached_poll.clamp(Duration::from_millis(1), Duration::from_millis(100)),
            resting_poll: self.resting_poll.clamp(Duration::from_millis(1), Duration::from_millis(100)),
            resting_after: self.resting_after.clamp(Duration::from_millis(10), Duration::from_secs(60)),
            idle_poll: self.idle_poll.clamp(Duration::from_millis(1), Duration::from_secs(1)),
            idle_close: self.idle_close.clamp(Duration::from_millis(100), Duration::from_secs(3600)),
        }
    }
}

/// Why a call or a subscription ended. A closed set; the sentence is fixed and holds nothing the server wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HubError {
    /// A fault of the link: signed out, off, unreachable, refused, or a rule of the protocol broken by the server.
    Fault(Fault),
    /// The server said no, with one of its fixed words.
    Word(ErrorWord),
    /// The answer did not come in time.
    Timeout,
    /// Too many requests wait for the server, or the socket is attached to as many sessions as it may be.
    Busy,
    /// The session is already shown on this device.
    AlreadyAttached,
    /// The reader of the subscription cannot keep up with the session.
    Slow,
    /// The hub was told to end its connection, or ended.
    Disconnected,
    /// The pane that held the subscription was closed.
    Closed,
}

impl HubError {
    /// The one fixed sentence of the error.
    pub fn sentence(self) -> &'static str {
        match self {
            HubError::Fault(fault) => fault.sentence(),
            HubError::Word(ErrorWord::NotFound) => "That session is not on the server any more.",
            HubError::Word(ErrorWord::Refused) => "The server did not allow that.",
            HubError::Word(ErrorWord::Busy) => "The server is busy. Try again in a moment.",
            HubError::Word(ErrorWord::TooMany) => "The server has as many sessions open as it allows.",
            HubError::Word(ErrorWord::BadRequest) => "The server did not accept the request.",
            HubError::Word(ErrorWord::Disabled) => "Remote sessions are off on this server.",
            HubError::Timeout => "The server did not answer in time.",
            HubError::Busy => "Too many requests are waiting for the server.",
            HubError::AlreadyAttached => "That session is already open on this computer.",
            HubError::Slow => "This pane could not keep up with the session.",
            HubError::Disconnected => "The connection to the server was ended.",
            HubError::Closed => "The pane was closed.",
        }
    }
}

impl fmt::Display for HubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.sentence())
    }
}

impl std::error::Error for HubError {}

impl From<Fault> for HubError {
    fn from(fault: Fault) -> Self {
        HubError::Fault(fault)
    }
}

impl From<HubError> for crate::Error {
    fn from(error: HubError) -> Self {
        crate::Error::Io(std::io::Error::other(error.sentence()))
    }
}

/// What made a subscription attach again on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The server asked for a fresh start, or the reader fell behind.
    Resync,
    /// The person asked for older lines: a longer tail.
    Older,
}

/// What a subscription is told, in the order it happened. The last one is always [`SubEvent::Lost`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubEvent {
    /// The attach was accepted. The session's grid, the writer view of this client and whether older lines exist come first;
    /// then `snapshot` bytes of [`SubEvent::Snapshot`] follow, then live bytes. `again` is false for the first attach of the
    /// subscription and true for every later one (a reconnect, a resync, older lines): the engine of the reader then holds
    /// something older that the snapshot has to replace.
    Attached { cols: u16, rows: u16, writer: WriterView, more: bool, snapshot: u32, again: bool },
    /// A chunk of the screen at the time of the attach.
    Snapshot(Vec<u8>),
    /// Bytes the program wrote after it.
    Live(Vec<u8>),
    /// This client's own view of the right to write changed.
    Writer(WriterView),
    /// The grid of the session changed (inside the clamps of a pane).
    Size(u16, u16),
    /// The program ended, with its status when it could be read. Nothing follows but [`SubEvent::Lost`] when the pane is closed.
    Exited(Option<u32>),
    /// The stream is being replaced: the attach that follows is the answer.
    Reattaching(Cause),
    /// The connection was lost and is being made again: attempt `attempt` of `of`, the next one in `in_secs` seconds.
    Reconnecting { attempt: u32, of: u32, in_secs: u64 },
    /// The tries are used: the subscription waits for [`SubHandle::retry`].
    GaveUp,
    /// The subscription is over and will not be attached again.
    Lost(HubError),
}

/// The bytes an event counts for in the queue of its subscription.
fn cost(event: &SubEvent) -> usize {
    EVENT_COST
        + match event {
            SubEvent::Snapshot(bytes) | SubEvent::Live(bytes) => bytes.len(),
            _ => 0,
        }
}

/// What the hub counted, for tests and diagnostics. Nothing in it is a byte of terminal output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HubStats {
    /// Connections that were opened.
    pub connects: u64,
    /// Messages of the server that were read (text and binary).
    pub messages: u64,
    /// Bytes of snapshot and of live output handed to subscriptions.
    pub bytes_delivered: u64,
    /// Chunks that named a session nobody is attached to, or that belonged to a stream that was replaced.
    pub dropped_chunks: u64,
    /// Binary frames of input that were sent, and bytes of input that were dropped before they were sent.
    pub input_frames: u64,
    pub dropped_input_bytes: u64,
    /// Streams that were ended because the server broke a rule.
    pub protocol_faults: u64,
    /// Beats sent.
    pub beats: u64,
    /// The most bytes any subscription held unread at once.
    pub queue_high_water: u64,
    /// Attaches that were made again because a reader fell behind.
    pub overflows: u64,
}

#[derive(Default)]
struct Cells {
    connects: AtomicU64,
    messages: AtomicU64,
    bytes_delivered: AtomicU64,
    dropped_chunks: AtomicU64,
    input_frames: AtomicU64,
    dropped_input_bytes: AtomicU64,
    protocol_faults: AtomicU64,
    beats: AtomicU64,
    queue_high_water: AtomicU64,
    overflows: AtomicU64,
}

impl Cells {
    fn get(&self) -> HubStats {
        let n = |c: &AtomicU64| c.load(Ordering::Relaxed);
        HubStats {
            connects: n(&self.connects),
            messages: n(&self.messages),
            bytes_delivered: n(&self.bytes_delivered),
            dropped_chunks: n(&self.dropped_chunks),
            input_frames: n(&self.input_frames),
            dropped_input_bytes: n(&self.dropped_input_bytes),
            protocol_faults: n(&self.protocol_faults),
            beats: n(&self.beats),
            queue_high_water: n(&self.queue_high_water),
            overflows: n(&self.overflows),
        }
    }
}

fn bump(cell: &AtomicU64) {
    cell.fetch_add(1, Ordering::Relaxed);
}

/// What the callers tell the thread.
enum Cmd {
    Connect(Sender<Result<(), HubError>>),
    Call { op: Op, reply: Sender<Result<Answer, HubError>> },
    Attach { sub: Sub },
    Input { id: u64, session: u32, bytes: Vec<u8> },
    Resize { id: u64, session: u32, cols: u16, rows: u16 },
    Takeover { id: u64, session: u32 },
    Reattach { id: u64, session: u32, tail: Option<u32>, cause: Cause },
    Detach { id: u64, session: u32 },
    Retry,
    Disconnect,
}

/// The one connection of a device. Cheap to clone; the last clone and the last subscription ending end the thread.
#[derive(Clone)]
pub struct Hub {
    inner: Arc<HubInner>,
}

struct HubInner {
    endpoint: Arc<dyn Endpoint>,
    cmd: Sender<Cmd>,
    stats: Arc<Cells>,
    next_sub: AtomicU64,
    options: HubOptions,
}

impl fmt::Debug for Hub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hub").finish_non_exhaustive()
    }
}

impl Hub {
    /// A hub for `endpoint`. Nothing is connected until something is asked.
    pub fn new(endpoint: Arc<dyn Endpoint>, options: HubOptions) -> Hub {
        let options = options.clamped();
        let (cmd, rx) = mpsc::channel();
        let stats = Arc::new(Cells::default());
        let core = Core::new(endpoint.clone(), options.clone(), stats.clone());
        // A thread that cannot start leaves a hub whose every call says it is disconnected.
        let _ = thread::Builder::new().name("hw-hub".into()).spawn(move || core.run(&rx));
        Hub { inner: Arc::new(HubInner { endpoint, cmd, stats, next_sub: AtomicU64::new(1), options }) }
    }

    /// Opens the connection now, so that a refusal is known before anything is asked. Waits at most the call timeout.
    pub fn connect(&self) -> Result<(), HubError> {
        let (tx, rx) = mpsc::channel();
        self.inner.cmd.send(Cmd::Connect(tx)).map_err(|_| HubError::Disconnected)?;
        rx.recv_timeout(self.inner.options.call_timeout).map_err(|_| HubError::Timeout)?
    }

    fn call(&self, op: Op) -> Result<Answer, HubError> {
        let (tx, rx) = mpsc::channel();
        self.inner.cmd.send(Cmd::Call { op, reply: tx }).map_err(|_| HubError::Disconnected)?;
        // The thread answers a call that is not answered in time itself; the margin is for the thread being busy connecting.
        match rx.recv_timeout(self.inner.options.call_timeout + Duration::from_secs(1)) {
            Ok(answer) => answer,
            Err(RecvTimeoutError::Timeout) => Err(HubError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(HubError::Disconnected),
        }
    }

    /// The sessions of the person.
    pub fn list(&self) -> Result<Vec<SessionEntry>, HubError> {
        match self.call(Op::List)? {
            Answer::List(list) => Ok(list),
            _ => Err(HubError::Fault(Fault::Protocol)),
        }
    }

    /// Makes a session of `cols` by `rows` cells. The caller is its writer; the session id is what a pane names to show it.
    pub fn new_session(&self, cols: u16, rows: u16) -> Result<(u32, WriterView), HubError> {
        match self.call(Op::New { cols, rows })? {
            Answer::Created { session, writer } => Ok((session, writer)),
            _ => Err(HubError::Fault(Fault::Protocol)),
        }
    }

    /// Ends the program of a session.
    pub fn end(&self, session: u32) -> Result<(), HubError> {
        match self.call(Op::End { session })? {
            Answer::Done => Ok(()),
            _ => Err(HubError::Fault(Fault::Protocol)),
        }
    }

    /// Attaches to `session`: the screen and `tail` rows of scrollback (the server's default when `None`, never above
    /// [`MAX_TAIL`]), then live bytes. Returns at once; the connection is made on the hub's thread and its progress and any
    /// failure arrive as events. A session can be shown once on a device: a second subscription to it ends at once with
    /// [`HubError::AlreadyAttached`].
    pub fn attach(&self, session: u32, tail: Option<u32>) -> Subscription {
        let id = self.inner.next_sub.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let sub = Sub {
            id,
            session,
            tx: tx.clone(),
            queued: queued.clone(),
            tail: tail.map(|t| t.min(MAX_TAIL)),
            phase: Phase::Wanting,
            writer: WriterView::Watching,
            attached_before: false,
            streak: 0,
        };
        let handle =
            SubHandle { id, session, route: Route::Hub(self.inner.cmd.clone()), wake: tx.clone(), queued: queued.clone() };
        if let Err(mpsc::SendError(Cmd::Attach { sub })) = self.inner.cmd.send(Cmd::Attach { sub }) {
            sub.push(SubEvent::Lost(HubError::Disconnected), false, usize::MAX, &self.inner.stats);
        }
        Subscription { handle, rx, queued }
    }

    /// The name of the server for a debug print: its host, or a word when the person is signed out. Never more.
    pub fn host(&self) -> String {
        self.inner.endpoint.origin().map_or_else(|| "(signed out)".to_owned(), |origin| origin.host().to_owned())
    }

    /// Ends the connection. Every subscription ends as signed out ([`Fault::SignedOut`]) and no session is ended. A later
    /// call or attach connects again, with whatever the endpoint then says.
    pub fn disconnect(&self) {
        let _ = self.inner.cmd.send(Cmd::Disconnect);
    }

    /// What the hub counted so far.
    pub fn stats(&self) -> HubStats {
        self.inner.stats.get()
    }
}

/// The reading end of a subscription, owned by the thread that reads it. The handle that sends is [`SubHandle`].
pub struct Subscription {
    handle: SubHandle,
    rx: Receiver<SubEvent>,
    queued: Arc<AtomicUsize>,
}

impl Subscription {
    /// The handle that sends input, a resize and the rest; it can be cloned and used from any thread.
    pub fn handle(&self) -> SubHandle {
        self.handle.clone()
    }

    /// The next event, waiting at most `wait`; `None` when nothing came. After [`SubEvent::Lost`] the subscription is over.
    pub fn recv(&self, wait: Duration) -> Option<SubEvent> {
        match self.rx.recv_timeout(wait) {
            Ok(event) => {
                self.queued.fetch_sub(cost(&event), Ordering::SeqCst);
                Some(event)
            }
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(SubEvent::Lost(HubError::Disconnected)),
        }
    }

    /// The session this subscription is for.
    pub fn session(&self) -> u32 {
        self.handle.session
    }
}

/// Where the requests of a subscription go: the thread of a hub, or a host of sessions in this process.
#[derive(Clone)]
enum Route {
    Hub(Sender<Cmd>),
    Local(Arc<dyn LocalRoute>),
}

/// The requests of a subscription whose session lives in this process (the server's own worker, which shows a session in a
/// pane of its window without a socket). Each call returns at once; what it causes arrives as [`SubEvent`]s through the
/// subscription's [`SubFeed`], in the same order and with the same meaning as a hub delivers them, so the link of a pane
/// cannot tell the two apart.
pub trait LocalRoute: Send + Sync {
    /// Typed bytes for the program (never empty).
    fn input(&self, bytes: &[u8]) -> Result<(), HubError>;
    /// Asks for the session to take this size; only the writer's is applied.
    fn resize(&self, cols: u16, rows: u16) -> Result<(), HubError>;
    /// Takes the right to write.
    fn takeover(&self) -> Result<(), HubError>;
    /// Attaches again from a fresh snapshot with `tail` rows: [`SubEvent::Reattaching`] first, then the answer.
    fn reattach(&self, tail: Option<u32>, cause: Cause) -> Result<(), HubError>;
    /// Leaves the session; it goes on.
    fn detach(&self);
}

/// The feeding end of a subscription made by [`Subscription::local`]: whoever holds the session hands its events over here.
/// The bytes of the snapshot and the live bytes are bounded like a hub's queue; the small events always pass.
#[derive(Clone)]
pub struct SubFeed {
    tx: Sender<SubEvent>,
    queued: Arc<AtomicUsize>,
    bound: usize,
}

impl SubFeed {
    /// Queues `event`. `false` when a snapshot chunk or live bytes would take the queue past its bound (nothing is queued
    /// then), or when nobody reads the subscription any more.
    pub fn push(&self, event: SubEvent) -> bool {
        let bounded = matches!(event, SubEvent::Snapshot(_) | SubEvent::Live(_));
        let cost = cost(&event);
        if bounded && self.queued.load(Ordering::SeqCst).saturating_add(cost) > self.bound {
            return false;
        }
        self.queued.fetch_add(cost, Ordering::SeqCst);
        if self.tx.send(event).is_err() {
            self.queued.fetch_sub(cost, Ordering::SeqCst);
            return false;
        }
        true
    }

    /// The bytes the reader has not taken yet.
    pub fn queued(&self) -> usize {
        self.queued.load(Ordering::SeqCst)
    }

    /// The bound of the queue in bytes.
    pub fn bound(&self) -> usize {
        self.bound
    }
}

impl Subscription {
    /// A subscription to `session` whose requests go to `route` and whose events are handed over through the returned
    /// [`SubFeed`], with a queue of at most `bound` bytes of snapshot and output. Nothing is attached by this call.
    pub fn local(session: u32, route: Arc<dyn LocalRoute>, bound: usize) -> (Subscription, SubFeed) {
        let (tx, rx) = mpsc::channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let handle = SubHandle { id: 0, session, route: Route::Local(route), wake: tx.clone(), queued: queued.clone() };
        (Subscription { handle, rx, queued: queued.clone() }, SubFeed { tx, queued, bound })
    }
}

/// What a pane sends over its subscription. Cheap to clone.
#[derive(Clone)]
pub struct SubHandle {
    id: u64,
    session: u32,
    route: Route,
    /// The subscription's own way to end itself ([`SubHandle::close`]).
    wake: Sender<SubEvent>,
    queued: Arc<AtomicUsize>,
}

impl fmt::Debug for SubHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubHandle").field("session", &self.session).finish_non_exhaustive()
    }
}

impl SubHandle {
    fn send(&self, cmd: Cmd) -> Result<(), HubError> {
        match &self.route {
            Route::Hub(tx) => tx.send(cmd).map_err(|_| HubError::Disconnected),
            Route::Local(_) => Ok(()),
        }
    }

    /// Typed bytes for the program. They reach it only while this client holds the right to write: the hub drops them
    /// before they are sent otherwise (and counts the bytes in [`HubStats::dropped_input_bytes`]).
    pub fn input(&self, bytes: &[u8]) -> Result<(), HubError> {
        if bytes.is_empty() {
            return Ok(());
        }
        if let Route::Local(local) = &self.route {
            return local.input(bytes);
        }
        self.send(Cmd::Input { id: self.id, session: self.session, bytes: bytes.to_vec() })
    }

    /// Asks the server to resize the session; it does only for the writer.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), HubError> {
        if let Route::Local(local) = &self.route {
            return local.resize(cols, rows);
        }
        self.send(Cmd::Resize { id: self.id, session: self.session, cols, rows })
    }

    /// Takes the right to write.
    pub fn takeover(&self) -> Result<(), HubError> {
        if let Route::Local(local) = &self.route {
            return local.takeover();
        }
        self.send(Cmd::Takeover { id: self.id, session: self.session })
    }

    /// Attaches again from a fresh snapshot with `tail` rows of scrollback.
    pub fn reattach(&self, tail: Option<u32>, cause: Cause) -> Result<(), HubError> {
        let tail = tail.map(|t| t.min(MAX_TAIL));
        if let Route::Local(local) = &self.route {
            return local.reattach(tail, cause);
        }
        self.send(Cmd::Reattach { id: self.id, session: self.session, tail, cause })
    }

    /// One more try after the tries were used ([`SubEvent::GaveUp`]). A session in this process has no connection to try.
    pub fn retry(&self) -> Result<(), HubError> {
        self.send(Cmd::Retry)
    }

    /// Leaves the session; it goes on.
    pub fn detach(&self) {
        if let Route::Local(local) = &self.route {
            return local.detach();
        }
        let _ = self.send(Cmd::Detach { id: self.id, session: self.session });
    }

    /// Detaches from the session (it goes on) and wakes the reader that waits, which then reads [`SubEvent::Lost`] with
    /// [`HubError::Closed`].
    pub fn close(&self) {
        self.detach();
        // Counted like every event, so that reading it leaves the count right.
        self.queued.fetch_add(EVENT_COST, Ordering::SeqCst);
        let _ = self.wake.send(SubEvent::Lost(HubError::Closed));
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The thread
// ---------------------------------------------------------------------------------------------------------------------

/// Where a subscription is in its attach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Not asked yet: there is no connection to ask on.
    Wanting,
    /// The attach request `req` was sent and its answer has not come. Chunks of an older stream may still arrive: they are dropped.
    Asked { req: u64 },
    /// The snapshot: `left` more bytes are promised, the next chunk has number `seq`.
    Snapshot { left: u32, seq: u32 },
    /// Live bytes, the next chunk has number `seq`.
    Live { seq: u32 },
}

struct Sub {
    id: u64,
    session: u32,
    tx: Sender<SubEvent>,
    queued: Arc<AtomicUsize>,
    /// The tail the last attach asked for.
    tail: Option<u32>,
    phase: Phase,
    /// This client's view of the right to write, kept in step with what the server said.
    writer: WriterView,
    /// Whether an attach was ever accepted: the next one is `again`.
    attached_before: bool,
    /// Attaches in a row that were made because the reader fell behind or the server asked.
    streak: u32,
}

/// What became of an event offered to a subscription.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pushed {
    Queued,
    /// The queue is at its bound: nothing was queued.
    Full,
    /// Nobody reads the subscription any more.
    Gone,
}

impl Sub {
    /// Queues `event`. `bounded` events are refused when they would pass the bound; the others are a few bytes and a
    /// subscription must always be told them.
    fn push(&self, event: SubEvent, bounded: bool, bound: usize, stats: &Cells) -> Pushed {
        let cost = cost(&event);
        let held = self.queued.load(Ordering::SeqCst);
        if bounded && held.saturating_add(cost) > bound {
            return Pushed::Full;
        }
        let now = self.queued.fetch_add(cost, Ordering::SeqCst) + cost;
        stats.queue_high_water.fetch_max(now as u64, Ordering::Relaxed);
        if self.tx.send(event).is_err() {
            self.queued.fetch_sub(cost, Ordering::SeqCst);
            return Pushed::Gone;
        }
        Pushed::Queued
    }
}

/// What is waiting for an answer.
enum Pending {
    Call(Sender<Result<Answer, HubError>>),
    Attach { id: u64, session: u32 },
    /// A beat, a resize, a takeover, a detach: nothing waits for the answer.
    Quiet,
}

/// Where the connection is.
#[derive(Clone, Copy)]
enum Mode {
    /// No connection, and nothing that needs one.
    Down,
    Up,
    /// Lost; try number `attempt` is due at `at`.
    Backoff { attempt: u32, at: Instant },
    /// The tries are used; a retry starts one more.
    Failed,
}

struct Core {
    endpoint: Arc<dyn Endpoint>,
    options: HubOptions,
    limits: WsLimits,
    stats: Arc<Cells>,
    ws: Option<WsClient>,
    mode: Mode,
    subs: HashMap<u32, Sub>,
    pending: HashMap<u64, (Pending, Instant)>,
    next_req: u64,
    last_beat: Instant,
    last_activity: Instant,
}

/// What a close code means for the connection: the fault that makes it final, or `None` when trying again may help.
fn final_close(code: u16) -> Option<Fault> {
    match code {
        proto::CLOSE_REVOKED => Some(Fault::SignedOut),
        proto::CLOSE_PROTOCOL | proto::CLOSE_TYPE | proto::CLOSE_ENCODING | proto::CLOSE_POLICY | proto::CLOSE_TOO_LARGE => {
            Some(Fault::Protocol)
        }
        _ => None,
    }
}

/// What a failed connect or a broken connection means: the fault that makes it final, or `None` when trying again may help.
fn final_fault(error: &WsError) -> Option<Fault> {
    match error {
        WsError::Fault(Fault::Unreachable) | WsError::Closed => None,
        WsError::Fault(fault) => Some(*fault),
        // A message over the bound is the server breaking a rule of the protocol.
        WsError::TooLarge => Some(Fault::Protocol),
        // A certificate that is not trusted, or a platform that refused the local network: another try changes nothing.
        WsError::Tls | WsError::Blocked(_) => Some(Fault::Refused),
    }
}

impl Core {
    fn new(endpoint: Arc<dyn Endpoint>, options: HubOptions, stats: Arc<Cells>) -> Core {
        let limits = WsLimits::new(MAX_REPLY + 4096, MAX_REPLY + 4096);
        let now = Instant::now();
        Core {
            endpoint,
            options,
            limits,
            stats,
            ws: None,
            mode: Mode::Down,
            subs: HashMap::new(),
            pending: HashMap::new(),
            next_req: 1,
            last_beat: now,
            last_activity: now,
        }
    }

    fn run(mut self, rx: &Receiver<Cmd>) {
        loop {
            match self.mode {
                Mode::Up => {
                    loop {
                        match rx.try_recv() {
                            Ok(cmd) => self.handle(cmd),
                            Err(mpsc::TryRecvError::Empty) => break,
                            Err(mpsc::TryRecvError::Disconnected) => {
                                self.shutdown();
                                return;
                            }
                        }
                    }
                    if matches!(self.mode, Mode::Up) {
                        self.pump();
                        self.housekeeping();
                    }
                }
                Mode::Down | Mode::Failed => match rx.recv_timeout(LONG_WAIT) {
                    Ok(cmd) => self.handle(cmd),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        self.shutdown();
                        return;
                    }
                },
                Mode::Backoff { attempt, at } => {
                    match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                        Ok(cmd) => self.handle(cmd),
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => {
                            self.shutdown();
                            return;
                        }
                    }
                    if matches!(self.mode, Mode::Backoff { .. }) && Instant::now() >= at {
                        self.try_again(attempt);
                    }
                }
            }
        }
    }

    fn shutdown(&mut self) {
        if let Some(mut ws) = self.ws.take() {
            let _ = ws.close(1000);
        }
        self.lose_all(HubError::Disconnected);
        self.fail_pending(HubError::Disconnected);
    }

    // ---- commands ----------------------------------------------------------------------------------------------------

    fn handle(&mut self, cmd: Cmd) {
        self.last_activity = Instant::now();
        match cmd {
            Cmd::Connect(reply) => {
                let result = match self.mode {
                    Mode::Up => Ok(()),
                    Mode::Backoff { .. } | Mode::Failed => Err(HubError::Fault(Fault::Unreachable)),
                    Mode::Down => self.connect_now(),
                };
                let _ = reply.send(result);
            }
            Cmd::Call { op, reply } => self.call(op, reply),
            Cmd::Attach { sub } => self.attach(sub),
            Cmd::Input { id, session, bytes } => self.input(id, session, &bytes),
            Cmd::Resize { id, session, cols, rows } => {
                if self.owns(id, session) {
                    self.quiet(Op::Resize { session, cols, rows });
                }
            }
            Cmd::Takeover { id, session } => {
                if self.owns(id, session) {
                    self.quiet(Op::Takeover { session });
                }
            }
            Cmd::Reattach { id, session, tail, cause } => {
                if self.owns(id, session) {
                    if let Some(sub) = self.subs.get_mut(&session) {
                        sub.tail = tail;
                    }
                    self.reattach(session, cause);
                }
            }
            Cmd::Detach { id, session } => {
                if self.owns(id, session) {
                    self.subs.remove(&session);
                    if matches!(self.mode, Mode::Up) {
                        self.quiet(Op::Detach { session });
                    }
                }
            }
            Cmd::Retry => {
                if matches!(self.mode, Mode::Failed) {
                    self.try_again(0);
                }
            }
            Cmd::Disconnect => {
                if let Some(mut ws) = self.ws.take() {
                    let _ = ws.close(1000);
                }
                self.fail_pending(HubError::Disconnected);
                self.lose_all(HubError::Fault(Fault::SignedOut));
                self.mode = Mode::Down;
            }
        }
    }

    /// Whether subscription `id` is the one attached to `session` now.
    fn owns(&self, id: u64, session: u32) -> bool {
        self.subs.get(&session).is_some_and(|sub| sub.id == id)
    }

    fn call(&mut self, op: Op, reply: Sender<Result<Answer, HubError>>) {
        match self.mode {
            Mode::Down => {
                if let Err(error) = self.connect_now() {
                    let _ = reply.send(Err(error));
                    return;
                }
            }
            Mode::Backoff { .. } | Mode::Failed => {
                let _ = reply.send(Err(HubError::Fault(Fault::Unreachable)));
                return;
            }
            Mode::Up => {}
        }
        if self.pending.len() >= MAX_PENDING {
            let _ = reply.send(Err(HubError::Busy));
            return;
        }
        let deadline = Instant::now() + self.options.call_timeout;
        match self.send(op, Pending::Call(reply), deadline) {
            Ok(_) => {}
            Err(Pending::Call(reply)) => {
                let _ = reply.send(Err(HubError::Fault(Fault::Unreachable)));
            }
            Err(_) => {}
        }
    }

    fn attach(&mut self, sub: Sub) {
        let session = sub.session;
        let bound = self.options.sub_queue_bytes;
        if self.subs.contains_key(&session) {
            sub.push(SubEvent::Lost(HubError::AlreadyAttached), false, bound, &self.stats);
            return;
        }
        if self.subs.len() >= MAX_ATTACHED {
            sub.push(SubEvent::Lost(HubError::Busy), false, bound, &self.stats);
            return;
        }
        self.subs.insert(session, sub);
        match self.mode {
            Mode::Up => self.ask_attach(session),
            Mode::Down => {
                // The connection is made now; on success every subscription (this one included) is attached.
                let _ = self.connect_now();
            }
            Mode::Backoff { attempt, .. } => {
                let (of, in_secs) = (self.options.reconnect.attempts, self.options.reconnect.delay.as_secs());
                self.tell(session, SubEvent::Reconnecting { attempt, of, in_secs });
            }
            Mode::Failed => self.tell(session, SubEvent::GaveUp),
        }
    }

    fn input(&mut self, id: u64, session: u32, bytes: &[u8]) {
        if !self.owns(id, session) || !matches!(self.mode, Mode::Up) {
            return;
        }
        let writing = self
            .subs
            .get(&session)
            .is_some_and(|sub| sub.writer == WriterView::Writing && matches!(sub.phase, Phase::Snapshot { .. } | Phase::Live { .. }));
        if !writing {
            self.stats.dropped_input_bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
            return;
        }
        for part in bytes.chunks(MAX_INPUT) {
            let Ok(frame) = proto::encode_input(session, part) else { continue };
            let Some(ws) = self.ws.as_mut() else { return };
            if let Err(error) = ws.send_binary(&frame) {
                self.broken(&error);
                return;
            }
            bump(&self.stats.input_frames);
        }
    }

    // ---- sending -----------------------------------------------------------------------------------------------------

    /// Sends a request and files it under its number. `Err` gives the pending back when the connection broke.
    fn send(&mut self, op: Op, pending: Pending, deadline: Instant) -> Result<u64, Pending> {
        let id = self.next_req;
        self.next_req = if id >= proto::MAX_ID { 1 } else { id + 1 };
        let text = Request { id, op }.encode();
        let Some(ws) = self.ws.as_mut() else { return Err(pending) };
        match ws.send_text(&text) {
            Ok(()) => {
                self.pending.insert(id, (pending, deadline));
                Ok(id)
            }
            Err(error) => {
                self.broken(&error);
                Err(pending)
            }
        }
    }

    /// A request nobody waits the answer of.
    fn quiet(&mut self, op: Op) {
        if !matches!(self.mode, Mode::Up) || self.pending.len() >= MAX_PENDING {
            return;
        }
        let deadline = Instant::now() + self.options.call_timeout;
        let _ = self.send(op, Pending::Quiet, deadline);
    }

    fn ask_attach(&mut self, session: u32) {
        let Some(sub) = self.subs.get(&session) else { return };
        let (id, tail) = (sub.id, sub.tail);
        let deadline = Instant::now() + self.options.call_timeout;
        match self.send(Op::Attach { session, tail }, Pending::Attach { id, session }, deadline) {
            Ok(req) => {
                if let Some(sub) = self.subs.get_mut(&session) {
                    sub.phase = Phase::Asked { req };
                }
            }
            Err(_) => {
                if let Some(sub) = self.subs.get_mut(&session) {
                    sub.phase = Phase::Wanting;
                }
            }
        }
    }

    fn reattach(&mut self, session: u32, cause: Cause) {
        if !matches!(self.mode, Mode::Up) {
            return;
        }
        self.tell(session, SubEvent::Reattaching(cause));
        self.ask_attach(session);
    }

    /// Offers a small event to one subscription (they are not held to the bound).
    fn tell(&mut self, session: u32, event: SubEvent) {
        let bound = self.options.sub_queue_bytes;
        if let Some(sub) = self.subs.get(&session)
            && sub.push(event, false, bound, &self.stats) == Pushed::Gone
        {
            self.subs.remove(&session);
        }
    }

    /// Ends one subscription with `error`, and tells the server it is gone.
    fn end_sub(&mut self, session: u32, error: HubError) {
        if let Some(sub) = self.subs.remove(&session) {
            sub.push(SubEvent::Lost(error), false, self.options.sub_queue_bytes, &self.stats);
            if matches!(self.mode, Mode::Up) {
                self.quiet(Op::Detach { session });
            }
        }
    }

    fn lose_all(&mut self, error: HubError) {
        let bound = self.options.sub_queue_bytes;
        for (_, sub) in self.subs.drain() {
            sub.push(SubEvent::Lost(error), false, bound, &self.stats);
        }
    }

    fn fail_pending(&mut self, error: HubError) {
        for (_, (pending, _)) in self.pending.drain() {
            if let Pending::Call(reply) = pending {
                let _ = reply.send(Err(error));
            }
        }
    }

    // ---- connecting --------------------------------------------------------------------------------------------------

    /// Opens the connection and attaches every subscription. On failure that cannot be helped by trying again the
    /// subscriptions end; on one that can, they wait for the tries.
    fn connect_now(&mut self) -> Result<(), HubError> {
        match self.open() {
            Ok(ws) => {
                self.ws = Some(ws);
                self.mode = Mode::Up;
                self.last_beat = Instant::now();
                bump(&self.stats.connects);
                let sessions: Vec<u32> = self.subs.keys().copied().collect();
                for session in sessions {
                    self.ask_attach(session);
                }
                Ok(())
            }
            Err(error) => {
                let final_fault = final_fault(&error);
                let fault = match &error {
                    WsError::Fault(fault) => *fault,
                    _ => final_fault.unwrap_or(Fault::Unreachable),
                };
                match final_fault {
                    Some(fault) => {
                        self.lose_all(HubError::Fault(fault));
                        self.mode = Mode::Down;
                    }
                    None if self.subs.is_empty() => self.mode = Mode::Down,
                    None => self.begin_backoff(),
                }
                Err(HubError::Fault(fault))
            }
        }
    }

    /// The connection itself: the origin, a fresh token, the upgrade.
    fn open(&self) -> Result<WsClient, WsError> {
        let origin = self.endpoint.origin().ok_or(Fault::SignedOut)?;
        let token: Token = self.endpoint.token()?;
        let url = origin.socket_url("/ws/device").map_err(|_| Fault::Protocol)?;
        WsClient::connect(&url, &token, self.endpoint.tls(), &self.limits)
    }

    fn begin_backoff(&mut self) {
        let policy = self.options.reconnect;
        if policy.attempts == 0 {
            self.give_up();
            return;
        }
        self.mode = Mode::Backoff { attempt: 1, at: Instant::now() + policy.delay };
        self.announce(1);
    }

    fn announce(&mut self, attempt: u32) {
        let (of, in_secs) = (self.options.reconnect.attempts, self.options.reconnect.delay.as_secs());
        let sessions: Vec<u32> = self.subs.keys().copied().collect();
        for session in sessions {
            self.tell(session, SubEvent::Reconnecting { attempt, of, in_secs });
        }
    }

    fn give_up(&mut self) {
        self.mode = Mode::Failed;
        let sessions: Vec<u32> = self.subs.keys().copied().collect();
        for session in sessions {
            self.tell(session, SubEvent::GaveUp);
        }
    }

    /// One try of the reconnect loop, after try number `attempt` was due (0 for the one try that a retry starts).
    fn try_again(&mut self, attempt: u32) {
        match self.open() {
            Ok(ws) => {
                self.ws = Some(ws);
                self.mode = Mode::Up;
                self.last_beat = Instant::now();
                bump(&self.stats.connects);
                let sessions: Vec<u32> = self.subs.keys().copied().collect();
                for session in sessions {
                    if let Some(sub) = self.subs.get_mut(&session) {
                        sub.phase = Phase::Wanting;
                    }
                    self.ask_attach(session);
                }
            }
            Err(error) => {
                if let Some(fault) = final_fault(&error) {
                    self.lose_all(HubError::Fault(fault));
                    self.mode = Mode::Down;
                } else if attempt == 0 || attempt >= self.options.reconnect.attempts {
                    self.give_up();
                } else {
                    let next = attempt + 1;
                    self.mode = Mode::Backoff { attempt: next, at: Instant::now() + self.options.reconnect.delay };
                    self.announce(next);
                }
            }
        }
    }

    /// The connection is gone (an error on the socket, or the peer closed it).
    fn broken(&mut self, error: &WsError) {
        self.drop_connection(final_fault(error));
    }

    fn drop_connection(&mut self, final_fault: Option<Fault>) {
        self.ws = None;
        self.fail_pending(HubError::Fault(final_fault.unwrap_or(Fault::Unreachable)));
        for sub in self.subs.values_mut() {
            sub.phase = Phase::Wanting;
        }
        match final_fault {
            Some(fault) => {
                self.lose_all(HubError::Fault(fault));
                self.mode = Mode::Down;
            }
            None if self.subs.is_empty() => self.mode = Mode::Down,
            None => self.begin_backoff(),
        }
    }

    // ---- reading -----------------------------------------------------------------------------------------------------

    fn pump(&mut self) {
        let wait = if self.subs.is_empty() {
            self.options.idle_poll
        } else if self.last_activity.elapsed() >= self.options.resting_after {
            self.options.resting_poll
        } else {
            self.options.attached_poll
        };
        let Some(ws) = self.ws.as_mut() else { return };
        match ws.recv(wait) {
            Ok(Recv::Timeout) => {}
            Ok(Recv::Text(text)) => {
                // Answers and events do not wake the thread up: the beat is answered too, and it is not a reason to be quick.
                bump(&self.stats.messages);
                self.text(&text);
            }
            Ok(Recv::Binary(bytes)) => {
                self.last_activity = Instant::now();
                bump(&self.stats.messages);
                self.binary(&bytes);
            }
            Ok(Recv::Closed(code)) => {
                self.drop_connection(final_close(code));
            }
            Err(error) => self.broken(&error),
        }
    }

    fn housekeeping(&mut self) {
        let now = Instant::now();
        // Answers that never came.
        let late: Vec<u64> = self.pending.iter().filter(|(_, (_, deadline))| now >= *deadline).map(|(id, _)| *id).collect();
        for id in late {
            match self.pending.remove(&id) {
                Some((Pending::Call(reply), _)) => {
                    let _ = reply.send(Err(HubError::Timeout));
                }
                Some((Pending::Attach { id: sub, session }, _)) if self.owns(sub, session) => {
                    self.end_sub(session, HubError::Timeout);
                }
                _ => {}
            }
        }
        if !self.subs.is_empty() && now.duration_since(self.last_beat) >= self.options.beat {
            self.last_beat = now;
            bump(&self.stats.beats);
            self.quiet(Op::Beat { session: None });
        }
        if self.subs.is_empty() && self.pending.is_empty() && now.duration_since(self.last_activity) >= self.options.idle_close {
            if let Some(mut ws) = self.ws.take() {
                let _ = ws.close(1000);
            }
            self.mode = Mode::Down;
        }
    }

    /// A text message of the server. One that cannot be read at all ends the connection: nothing can be said of what it meant.
    fn text(&mut self, text: &str) {
        let reply = match Reply::parse(text) {
            Ok(reply) => reply,
            Err(_) => return self.protocol_break(),
        };
        match reply {
            Reply::Answer { id, result } => self.answer(id, result),
            Reply::Event(event) => self.event(event),
        }
    }

    fn protocol_break(&mut self) {
        bump(&self.stats.protocol_faults);
        if let Some(mut ws) = self.ws.take() {
            let _ = ws.close(proto::CLOSE_PROTOCOL);
        }
        self.drop_connection(Some(Fault::Protocol));
    }

    fn answer(&mut self, id: u64, result: Result<Answer, ErrorWord>) {
        let Some((pending, _)) = self.pending.remove(&id) else { return };
        match pending {
            Pending::Call(reply) => {
                let _ = reply.send(result.map_err(HubError::Word));
            }
            Pending::Quiet => {}
            Pending::Attach { id: sub, session } => {
                if !self.owns(sub, session) || !self.subs.get(&session).is_some_and(|s| s.phase == Phase::Asked { req: id }) {
                    return;
                }
                match result {
                    Err(word) => self.end_sub(session, HubError::Word(word)),
                    Ok(Answer::Attached { session: said, cols, rows, writer, more, snapshot }) => {
                        let sane = said == session
                            && (MIN_SIZE.0..=MAX_SIZE).contains(&cols)
                            && (MIN_SIZE.1..=MAX_SIZE).contains(&rows)
                            && snapshot <= self.options.max_snapshot;
                        if !sane {
                            return self.stream_fault(session);
                        }
                        self.attached(session, cols, rows, writer, more, snapshot);
                    }
                    Ok(_) => self.stream_fault(session),
                }
            }
        }
    }

    fn attached(&mut self, session: u32, cols: u16, rows: u16, writer: WriterView, more: bool, snapshot: u32) {
        let Some(sub) = self.subs.get_mut(&session) else { return };
        let again = sub.attached_before;
        sub.attached_before = true;
        sub.writer = writer;
        sub.phase = if snapshot == 0 { Phase::Live { seq: 0 } } else { Phase::Snapshot { left: snapshot, seq: 0 } };
        self.tell(session, SubEvent::Attached { cols, rows, writer, more, snapshot, again });
    }

    /// The server broke a rule on one stream: that subscription ends, the connection and the others go on.
    fn stream_fault(&mut self, session: u32) {
        bump(&self.stats.protocol_faults);
        self.end_sub(session, HubError::Fault(Fault::Protocol));
    }

    fn event(&mut self, event: Event) {
        let session = event.session();
        if !self.subs.contains_key(&session) {
            return;
        }
        let bound = self.options.sub_queue_bytes;
        match event {
            Event::Writer { state, .. } => {
                if let Some(sub) = self.subs.get_mut(&session) {
                    sub.writer = state;
                }
                self.offer(session, SubEvent::Writer(state), true, bound);
            }
            Event::Size { cols, rows, .. } => {
                if !(MIN_SIZE.0..=MAX_SIZE).contains(&cols) || !(MIN_SIZE.1..=MAX_SIZE).contains(&rows) {
                    return self.stream_fault(session);
                }
                self.offer(session, SubEvent::Size(cols, rows), true, bound);
            }
            Event::Exited { code, .. } => {
                self.tell(session, SubEvent::Exited(code));
                // The session is over: the place it held on the server is given back.
                self.subs.remove(&session);
                self.quiet(Op::Detach { session });
            }
            Event::Resync { .. } => self.resync(session),
        }
    }

    /// Offers an event that a subscription that has fallen behind may lose (the fresh attach carries the state again).
    fn offer(&mut self, session: u32, event: SubEvent, bounded: bool, bound: usize) {
        let Some(sub) = self.subs.get(&session) else { return };
        match sub.push(event, bounded, bound, &self.stats) {
            Pushed::Queued => {}
            Pushed::Gone => {
                self.subs.remove(&session);
            }
            Pushed::Full => self.overflow(session),
        }
    }

    /// The subscription holds as many unread bytes as it may: it is attached again, from a fresh snapshot.
    fn overflow(&mut self, session: u32) {
        bump(&self.stats.overflows);
        self.resync(session);
    }

    fn resync(&mut self, session: u32) {
        let Some(sub) = self.subs.get_mut(&session) else { return };
        sub.streak += 1;
        if sub.streak > MAX_RESYNC_STREAK {
            return self.end_sub(session, HubError::Slow);
        }
        self.reattach(session, Cause::Resync);
    }

    /// A binary frame of the server: a chunk of a stream.
    fn binary(&mut self, frame: &[u8]) {
        let Ok(chunk) = Chunk::parse(frame) else { return self.protocol_break() };
        let session = chunk.session;
        let bound = self.options.sub_queue_bytes;
        let Some(sub) = self.subs.get_mut(&session) else {
            bump(&self.stats.dropped_chunks);
            return;
        };
        let len = chunk.bytes.len();
        // What the stream may be at: the snapshot first, as many bytes as the answer promised, then the live bytes.
        let next = match (sub.phase, chunk.stream) {
            (Phase::Asked { .. } | Phase::Wanting, _) => {
                bump(&self.stats.dropped_chunks);
                return;
            }
            (Phase::Snapshot { left, seq }, Stream::Snapshot) if chunk.seq == seq && len as u64 <= u64::from(left) => {
                let left = left - len as u32;
                if left == 0 { Phase::Live { seq: seq.wrapping_add(1) } } else { Phase::Snapshot { left, seq: seq.wrapping_add(1) } }
            }
            (Phase::Live { seq }, Stream::Live) if chunk.seq == seq => Phase::Live { seq: seq.wrapping_add(1) },
            _ => return self.stream_fault(session),
        };
        let event = match chunk.stream {
            Stream::Snapshot => SubEvent::Snapshot(chunk.bytes.to_vec()),
            Stream::Live => SubEvent::Live(chunk.bytes.to_vec()),
        };
        match sub.push(event, true, bound, &self.stats) {
            Pushed::Queued => {
                sub.phase = next;
                self.stats.bytes_delivered.fetch_add(len as u64, Ordering::Relaxed);
                // A reader that gets live bytes through has caught up: the streak of fresh starts is over.
                if matches!(next, Phase::Live { .. }) && chunk.stream == Stream::Live {
                    sub.streak = 0;
                }
            }
            Pushed::Gone => {
                self.subs.remove(&session);
            }
            Pushed::Full => self.overflow(session),
        }
    }
}
