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
//! they are not tried again. A certificate that is not trusted ends the subscriptions with [`Fault::Certificate`], its own
//! fault with its own sentence, and not with the plain refusal of an upgrade. Once the tries are used, a pane's Enter, a call (`list`, `new`, `end`) or a new attach makes one
//! more try, and on success every waiting subscription is attached again; with no subscription left the hub simply connects
//! on the next request.
//!
//! **Silence.** A network can keep the TCP connection open and pass nothing (a proxy that froze, a NAT that forgot the
//! connection): no error and no close ever comes. Two rules end such a link as a broken one. The first is the beat: while a
//! subscription exists the hub beats every [`HubOptions::beat`] and the server answers at once, so a beat that is not answered
//! within [`HubOptions::beat_timeout`] ([`CALL_TIMEOUT`], 10 seconds) ends the link, which a silent network is noticed by within
//! about twenty seconds, even when pings still arrive. The second is the backstop for a connection with no beat to wait for: a
//! connection on which nothing at all was heard (no message, no ping, no pong) for [`HubOptions::silence`]
//! ([`proto::SILENCE_SECONDS`]) is dropped too. Either way no close frame is written into the link (a write into a dead link may
//! wait), the subscriptions go to `Reconnecting` and the tries, a call that waits fails as unreachable, and an idle hub is down
//! and connects again on the next request. What was typed between the moment the link went silent and the moment the hub
//! noticed was handed to a connection that looked alive; it is kept like everything else that is not acknowledged (below).
//!
//! **Typed input.** What a person types is numbered and kept: every input frame of a subscription that writes stays in one
//! buffer of that subscription until the server acknowledges it ([`Event::Ack`]), and so does every key typed while the
//! subscription attaches again or the connection is made again. The buffer holds at most [`KEEP_BYTES`] of typed bytes, none
//! older than [`HubOptions::input_keep_age`] ([`KEEP_AGE`], 30 seconds), only in memory, and every copy the hub makes of typed bytes
//! (the bytes it is handed, each kept frame, each encoded frame) is set to zero when it ends, because typed bytes can be
//! passwords. When the answer to an attach says that the subscription writes under the same epoch, the frames below the answer's
//! `next` are dropped (the server applied them) and the rest is sent again in order; the server applies a number once. In
//! every other case (another epoch, watching, free, the bounds, no try left, the subscription ending) the buffer is dropped and
//! the subscription is told [`SubEvent::NotSent`], so no key is lost unseen. A first attach has no earlier right to come back
//! to: what is typed meanwhile is not kept. An acknowledgement that reports a gap makes the hub send the frames after it again,
//! once for each such report.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::proto::{
    self, Answer, Chunk, ErrorWord, Event, MAX_ATTACHED, MAX_INPUT, MAX_PENDING, MAX_REPLY, MAX_SIZE, MAX_TAIL, MIN_SIZE, Op,
    Reply, Request, ResumeToken, SessionEntry, Stream, WriterView,
};
use super::ws::{Recv, WsClient, WsError, WsLimits};
use crate::server_link::{Fault, Origin, Token};
use crate::ssh::Reconnect;
use zeroize::{Zeroize, Zeroizing};

/// How long a call waits for the answer of the server before it gives up.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// The most bytes of snapshot one attach may promise: the top of the server's own bound.
pub const MAX_SNAPSHOT: u32 = 16 * 1024 * 1024;

/// The bytes one subscription may hold between the hub and its reader: a whole snapshot and some live output.
pub const SUB_QUEUE_BYTES: usize = 24 * 1024 * 1024;

/// The most bytes of typed input one subscription keeps until the server confirms them or the connection is back.
pub const KEEP_BYTES: usize = 64 * 1024;

/// How old a kept frame may be: after this the pane that typed it is told that it was not sent. It is also how long the server
/// keeps the right of a writer whose connection was lost, so a frame older than this could not be sent under it anyway.
pub const KEEP_AGE: Duration = Duration::from_secs(30);

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

    /// The server and its token for one connection. An endpoint whose sign-in can change between [`Endpoint::origin`] and
    /// [`Endpoint::token`] reads both at once here, so a token is never sent to a server it was not made for.
    fn credential(&self) -> Result<(Origin, Token), Fault> {
        let origin = self.origin().ok_or(Fault::SignedOut)?;
        Ok((origin, self.token()?))
    }

    /// A TLS configuration that trusts something other than the platform's store. Only a test passes one; `None` (the
    /// default) is the platform's own.
    fn tls(&self) -> Option<Arc<rustls::ClientConfig>> {
        None
    }

    /// The server refused the token this device sent: a 401 on the upgrade, or the close code 4401 (the token was revoked
    /// or ran out). The keeper of the token may check it and forget it; a token that a new sign-in put in its place since
    /// must stay. Called on the hub's thread, so it must not block. The default does nothing.
    fn refused(&self) {}
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
    /// How long the server has to answer a beat. A beat that is not answered in this time ends the connection as a broken one.
    pub beat_timeout: Duration,
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
    /// How long the connection may hear nothing at all (no message, no ping, no pong) before it is treated as broken.
    pub silence: Duration,
    /// How old a kept input frame may be before it is dropped and the pane says that it was not sent. Never above [`KEEP_AGE`].
    pub input_keep_age: Duration,
}

impl Default for HubOptions {
    fn default() -> Self {
        HubOptions {
            reconnect: Reconnect::default(),
            beat: Duration::from_secs(proto::BEAT_SECONDS),
            call_timeout: CALL_TIMEOUT,
            beat_timeout: CALL_TIMEOUT,
            max_snapshot: MAX_SNAPSHOT,
            sub_queue_bytes: SUB_QUEUE_BYTES,
            attached_poll: Duration::from_millis(2),
            resting_poll: Duration::from_millis(8),
            resting_after: Duration::from_secs(1),
            idle_poll: Duration::from_millis(25),
            idle_close: Duration::from_secs(60),
            silence: Duration::from_secs(proto::SILENCE_SECONDS),
            input_keep_age: KEEP_AGE,
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
            beat_timeout: self.beat_timeout.clamp(Duration::from_millis(50), Duration::from_secs(3600)),
            max_snapshot: self.max_snapshot.clamp(1024, MAX_SNAPSHOT),
            sub_queue_bytes: self.sub_queue_bytes.clamp(64 * 1024, 64 * 1024 * 1024),
            attached_poll: self.attached_poll.clamp(Duration::from_millis(1), Duration::from_millis(100)),
            resting_poll: self.resting_poll.clamp(Duration::from_millis(1), Duration::from_millis(100)),
            resting_after: self.resting_after.clamp(Duration::from_millis(10), Duration::from_secs(60)),
            idle_poll: self.idle_poll.clamp(Duration::from_millis(1), Duration::from_secs(1)),
            idle_close: self.idle_close.clamp(Duration::from_millis(100), Duration::from_secs(3600)),
            silence: self.silence.clamp(Duration::from_millis(50), Duration::from_secs(3600)),
            input_keep_age: self.input_keep_age.clamp(Duration::from_millis(10), KEEP_AGE),
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
        // The error itself is kept inside, so that a caller can still tell the case ([`HubError::of`]); it prints its sentence.
        crate::Error::Io(std::io::Error::other(error))
    }
}

impl HubError {
    /// The error of the hub that `error` carries, when it is one: what a pane's request to its session failed with.
    pub fn of(error: &crate::Error) -> Option<HubError> {
        match error {
            crate::Error::Io(io) => io.get_ref().and_then(|inner| inner.downcast_ref::<HubError>()).copied(),
            _ => None,
        }
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
    /// something older that the snapshot has to replace. `epoch` is the epoch of the lease at the time of the answer (it changes
    /// whenever the holder of the right changes); it is kept for what a pane may later do with it and nothing reads it yet.
    Attached { cols: u16, rows: u16, writer: WriterView, more: bool, snapshot: u32, again: bool, epoch: u32 },
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
    /// Input this client sent was dropped because the program does not read its input; said at most about once a second.
    InputDropped,
    /// `bytes` of typed input were not sent and are not kept: the pane watches or nobody writes, the bounds were passed, the
    /// right to write was not there when the connection came back, or no try is left. Never says what the bytes were.
    NotSent { bytes: usize },
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
    /// Input frames that were sent again: after a resumed attach, or because the server reported a gap.
    pub input_resent: u64,
    /// Bytes of typed input the hub holds now, in every subscription, until the server confirms them (a gauge, not a count).
    pub kept_input_bytes: u64,
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
    input_resent: AtomicU64,
    kept_input_bytes: AtomicU64,
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
            input_resent: n(&self.input_resent),
            kept_input_bytes: n(&self.kept_input_bytes),
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
    Input { id: u64, session: u32, bytes: Zeroizing<Vec<u8>>, at: Instant },
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
            resume: None,
            epoch: None,
            next_seq: 1,
            keep: Keep::default(),
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
    /// before they are sent otherwise (counts the bytes in [`HubStats::dropped_input_bytes`] and says [`SubEvent::NotSent`]).
    /// While the subscription attaches again or the connection is made again, the hub keeps them (see the module doc).
    pub fn input(&self, bytes: &[u8]) -> Result<(), HubError> {
        if bytes.is_empty() {
            return Ok(());
        }
        if let Route::Local(local) = &self.route {
            return local.input(bytes);
        }
        self.send(Cmd::Input { id: self.id, session: self.session, bytes: Zeroizing::new(bytes.to_vec()), at: Instant::now() })
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
    /// The token of the last attach answer: the proof, sent with the next attach of this subscription, that it is the same
    /// attachment as the one whose socket was lost. It is used once, so every answer replaces it (the old one is wiped as it is
    /// dropped); it lives in this struct and nowhere else: it is never logged, never written to disk and never printed.
    resume: Option<ResumeToken>,
    /// The epoch of the lease as the last attach answer or the last writer event said it: the epoch every input frame is
    /// written under.
    epoch: Option<u32>,
    /// The number of the next input frame: the `next` of the last attach answer, then one more for every frame kept.
    next_seq: u32,
    /// The input frames the server did not acknowledge yet, and the keys typed while there was no stream to send them on.
    keep: Keep,
}

impl fmt::Debug for Sub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The token is a bearer secret: only whether there is one is said, with the marker its own type prints.
        f.debug_struct("Sub")
            .field("session", &self.session)
            .field("phase", &self.phase)
            .field("writer", &self.writer)
            .field("resume", &self.resume.as_ref().map(|_| "ResumeToken(..)"))
            .field("epoch", &self.epoch)
            .field("next_seq", &self.next_seq)
            .field("keep", &self.keep)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The buffer of typed input
// ---------------------------------------------------------------------------------------------------------------------

/// One input frame that was typed and that the server has not confirmed: its number, the epoch it is written under, when it was
/// typed, and the typed bytes. The bytes are in a buffer that is set to zero when it is dropped.
struct Kept {
    seq: u32,
    epoch: u32,
    at: Instant,
    bytes: Zeroizing<Vec<u8>>,
}

impl fmt::Debug for Kept {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // What was typed can be a password: only its size is ever printed.
        f.debug_struct("Kept").field("seq", &self.seq).field("epoch", &self.epoch).field("bytes", &self.bytes.len()).finish()
    }
}

/// The one place where a kept frame ends: its bytes are set to zero in place, the whole buffer, before the frame is freed. Every
/// way out of the buffer (confirmed, too old, over the bound, not resumed, the subscription ending) goes through here. Returns how
/// many bytes the frame held.
fn wipe(mut frame: Kept) -> usize {
    let len = frame.bytes.len();
    Zeroize::zeroize(frame.bytes.as_mut_slice());
    release(frame);
    len
}

#[cfg(not(test))]
fn release(frame: Kept) {
    drop(frame);
}

/// A test can ask to be handed the wiped frames instead of having them freed, to look at their bytes.
#[cfg(test)]
fn release(frame: Kept) {
    let kept_for_the_test = WIPED.with(|list| match list.borrow_mut().as_mut() {
        Some(list) => {
            list.push(frame);
            None
        }
        None => Some(frame),
    });
    drop(kept_for_the_test);
}

#[cfg(test)]
thread_local! {
    static WIPED: std::cell::RefCell<Option<Vec<Kept>>> = const { std::cell::RefCell::new(None) };
}

/// The input of one subscription that the server did not confirm, oldest first, with its bounds. Memory only; never printed.
#[derive(Default)]
struct Keep {
    frames: VecDeque<Kept>,
    /// The typed bytes held, in every frame.
    bytes: usize,
    /// A write did not fit: nothing more is kept until the buffer is empty, so that what is sent is always a start of what was
    /// typed and never what came after a part that was left out.
    full: bool,
}

impl fmt::Debug for Keep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keep").field("frames", &self.frames.len()).field("bytes", &self.bytes).field("full", &self.full).finish()
    }
}

impl Drop for Keep {
    fn drop(&mut self) {
        self.clear();
    }
}

impl Keep {
    fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Keeps `bytes` as frame `seq` under `epoch`. `false` (nothing kept) when it does not fit in [`KEEP_BYTES`] or an earlier
    /// write did not fit.
    fn push(&mut self, seq: u32, epoch: u32, at: Instant, bytes: &[u8]) -> bool {
        if self.full || self.bytes.saturating_add(bytes.len()) > KEEP_BYTES {
            self.full = true;
            return false;
        }
        self.bytes += bytes.len();
        self.frames.push_back(Kept { seq, epoch, at, bytes: Zeroizing::new(bytes.to_vec()) });
        true
    }

    fn after_removal(&mut self) {
        if self.frames.is_empty() {
            self.full = false;
        }
    }

    /// Drops every frame up to and including `seq` (the server applied them). Returns the bytes dropped.
    fn confirm_through(&mut self, seq: u32) -> usize {
        let mut dropped = 0;
        while self.frames.front().is_some_and(|f| f.seq <= seq) {
            if let Some(frame) = self.frames.pop_front() {
                dropped += wipe(frame);
            }
        }
        self.bytes -= dropped;
        self.after_removal();
        dropped
    }

    /// Drops everything. Returns the bytes dropped.
    fn clear(&mut self) -> usize {
        let mut dropped = 0;
        while let Some(frame) = self.frames.pop_front() {
            dropped += wipe(frame);
        }
        self.bytes = 0;
        self.after_removal();
        dropped
    }

    /// Drops everything when the oldest frame is older than `max_age`: what comes after a frame that was left out must not be
    /// sent either. Returns the bytes dropped.
    fn expire(&mut self, now: Instant, max_age: Duration) -> usize {
        if self.frames.front().is_some_and(|f| now.saturating_duration_since(f.at) >= max_age) { self.clear() } else { 0 }
    }

    /// The epoch every frame is written under, when it is the same one for all of them.
    fn uniform_epoch(&self) -> Option<u32> {
        let first = self.frames.front()?.epoch;
        self.frames.iter().all(|f| f.epoch == first).then_some(first)
    }

    fn first_seq(&self) -> Option<u32> {
        self.frames.front().map(|f| f.seq)
    }

    fn last_seq(&self) -> Option<u32> {
        self.frames.back().map(|f| f.seq)
    }
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
    /// A resize, a takeover, a detach: nothing waits for the answer, and a late one changes nothing.
    Quiet,
    /// A beat: nothing waits for the answer, but one that does not come in time ends the connection.
    Beat,
}

/// Where the connection is.
#[derive(Clone, Copy)]
enum Mode {
    /// No connection, and nothing that needs one.
    Down,
    Up,
    /// Lost; try number `attempt` is due at `at`.
    Backoff { attempt: u32, at: Instant },
    /// The tries are used; a retry starts one more, and so does a request that needs the connection (a call, a connect or a
    /// new attach). With no subscription left the hub is `Down` instead.
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
        // A certificate that is not trusted has its own fault: another try changes nothing.
        WsError::Certificate => Some(Fault::Certificate),
        // Another failure of the secure connection, or a platform that refused the local network: another try changes nothing.
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
                    // A wait is never longer than a second, so that kept input that grows too old is told about on time.
                    let wait = at.saturating_duration_since(Instant::now()).min(Duration::from_secs(1));
                    match rx.recv_timeout(wait) {
                        Ok(cmd) => self.handle(cmd),
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => {
                            self.shutdown();
                            return;
                        }
                    }
                    self.expire_kept();
                    if matches!(self.mode, Mode::Backoff { .. }) && Instant::now() >= at {
                        self.try_again(attempt);
                    }
                }
            }
            self.publish();
        }
    }

    /// What the stats say about the input that is held now.
    fn publish(&self) {
        let held: usize = self.subs.values().map(|sub| sub.keep.bytes).sum();
        self.stats.kept_input_bytes.store(held as u64, Ordering::Relaxed);
    }

    fn shutdown(&mut self) {
        if let Some(mut ws) = self.ws.take() {
            let _ = ws.close(1000);
        }
        self.lose_all(HubError::Disconnected);
        self.fail_pending(HubError::Disconnected);
        self.publish();
    }

    // ---- commands ----------------------------------------------------------------------------------------------------

    fn handle(&mut self, cmd: Cmd) {
        self.last_activity = Instant::now();
        match cmd {
            Cmd::Connect(reply) => {
                let result = match self.mode {
                    Mode::Up => Ok(()),
                    Mode::Backoff { .. } => Err(HubError::Fault(Fault::Unreachable)),
                    Mode::Failed => self.try_once(),
                    Mode::Down => self.connect_now(),
                };
                let _ = reply.send(result);
            }
            Cmd::Call { op, reply } => self.call(op, reply),
            Cmd::Attach { sub } => self.attach(sub),
            Cmd::Input { id, session, bytes, at } => self.input(id, session, &bytes, at),
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
                    self.forget(session);
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
            Mode::Down | Mode::Failed => {
                let tried = if matches!(self.mode, Mode::Failed) { self.try_once() } else { self.connect_now() };
                if let Err(error) = tried {
                    let _ = reply.send(Err(error));
                    return;
                }
            }
            // The reconnect loop runs and tries again by itself soon.
            Mode::Backoff { .. } => {
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
            Mode::Failed => {
                // A new pane is a request for the connection: one try. On success every subscription is attached again.
                if self.try_once().is_err() && matches!(self.mode, Mode::Failed) {
                    self.tell(session, SubEvent::GaveUp);
                }
            }
        }
    }

    /// Typed bytes of a pane, typed at `at` (the age bound counts from then, not from when the thread got to them). A subscription that writes and has a right to come back to keeps them, numbered, and sends them at
    /// once when its stream is live; while it attaches again or the connection is made again they are only kept. Everything else
    /// is refused, and the pane is told so: no key is dropped unseen.
    fn input(&mut self, id: u64, session: u32, bytes: &[u8], at: Instant) {
        if !self.owns(id, session) {
            return;
        }
        let up = matches!(self.mode, Mode::Up);
        let can_wait = up || matches!(self.mode, Mode::Backoff { .. });
        let mut refused = 0usize;
        let mut first_new = None;
        for part in bytes.chunks(MAX_INPUT) {
            let Some(sub) = self.subs.get_mut(&session) else { break };
            // A right to come back to needs an epoch of an earlier answer, and that this client wrote under it. No frame goes out
            // under an epoch that was made up, and 0 is never a valid one; a number that cannot grow is not used.
            let seq = sub.next_seq;
            let kept = match sub.epoch {
                Some(epoch) if can_wait && sub.writer == WriterView::Writing && seq < u32::MAX => sub.keep.push(seq, epoch, at, part),
                _ => false,
            };
            if !kept {
                refused += part.len();
                continue;
            }
            sub.next_seq = seq + 1;
            first_new.get_or_insert(seq);
        }
        if refused > 0 {
            self.refuse(session, refused);
        }
        // What was kept now goes out when the stream is live: every frame before it was sent when the stream began, so these are the
        // only ones that were not. While the stream is not live they wait for the answer to the attach.
        let live = self.subs.get(&session).is_some_and(|sub| matches!(sub.phase, Phase::Snapshot { .. } | Phase::Live { .. }));
        if let Some(from) = first_new
            && up
            && live
        {
            self.transmit(session, from, false);
        }
    }

    /// Sends the kept frames with a number of `from` or more, in order, each in a buffer that is wiped when it is dropped. A
    /// frame that is sent again is counted as resent. A failed send ends the connection; what is kept stays.
    fn transmit(&mut self, session: u32, from: u32, resend: bool) {
        let mut failure = None;
        let mut sent = 0u64;
        if let (Some(sub), Some(ws)) = (self.subs.get(&session), self.ws.as_mut()) {
            for frame in sub.keep.frames.iter().filter(|f| f.seq >= from) {
                let Ok(encoded) = proto::encode_input_zeroizing(session, frame.epoch, frame.seq, &frame.bytes) else { continue };
                match ws.send_binary(&encoded) {
                    Ok(()) => sent += 1,
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
        }
        self.stats.input_frames.fetch_add(sent, Ordering::Relaxed);
        if resend {
            self.stats.input_resent.fetch_add(sent, Ordering::Relaxed);
        }
        if let Some(error) = failure {
            self.broken(&error);
        }
    }

    /// Typed bytes that are not sent and not kept: counted, and the subscription is told.
    fn refuse(&mut self, session: u32, bytes: usize) {
        self.stats.dropped_input_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        self.tell(session, SubEvent::NotSent { bytes });
    }

    /// Drops everything kept for `session` (for a reason the caller names by calling this) and tells the subscription.
    fn drop_kept(&mut self, session: u32) {
        let Some(sub) = self.subs.get_mut(&session) else { return };
        let bytes = sub.keep.clear();
        if bytes > 0 {
            self.refuse(session, bytes);
        }
    }

    /// Drops what has been kept for longer than the bound, in every subscription, and tells each.
    fn expire_kept(&mut self) {
        let (now, max_age) = (Instant::now(), self.options.input_keep_age);
        let late: Vec<(u32, usize)> = self
            .subs
            .iter_mut()
            .filter_map(|(session, sub)| {
                let bytes = sub.keep.expire(now, max_age);
                (bytes > 0).then_some((*session, bytes))
            })
            .collect();
        for (session, bytes) in late {
            self.refuse(session, bytes);
        }
    }

    // ---- sending -----------------------------------------------------------------------------------------------------

    /// Sends a request and files it under its number. `Err` gives the pending back when the connection broke.
    fn send(&mut self, op: Op, pending: Pending, deadline: Instant) -> Result<u64, Pending> {
        let id = self.next_id();
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

    /// The number of the next request.
    fn next_id(&mut self) -> u64 {
        let id = self.next_req;
        self.next_req = if id >= proto::MAX_ID { 1 } else { id + 1 };
        id
    }

    /// A request nobody waits the answer of.
    fn quiet(&mut self, op: Op) {
        if !matches!(self.mode, Mode::Up) || self.pending.len() >= MAX_PENDING {
            return;
        }
        let deadline = Instant::now() + self.options.call_timeout;
        let _ = self.send(op, Pending::Quiet, deadline);
    }

    /// The beat of an attached client: the server answers it at once, and an answer that is late ends the connection.
    fn beat(&mut self) {
        if !matches!(self.mode, Mode::Up) || self.pending.len() >= MAX_PENDING {
            return;
        }
        let deadline = Instant::now() + self.options.beat_timeout;
        let _ = self.send(Op::Beat { session: None }, Pending::Beat, deadline);
    }

    fn ask_attach(&mut self, session: u32) {
        let Some(sub) = self.subs.get(&session) else { return };
        let (id, tail) = (sub.id, sub.tail);
        // The token of the answer before this one: every attach of the subscription after the first (a reconnect, a resync, older
        // lines) presents it, so that a writer whose socket was lost gets its right back. It stays until the answer to this
        // request replaces it with the next one, so no attach of a subscription that was attached before goes without it.
        let resume = sub.resume.clone();
        let deadline = Instant::now() + self.options.call_timeout;
        match self.send(Op::Attach { session, tail, resume }, Pending::Attach { id, session }, deadline) {
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
            self.forget(session);
        }
    }

    /// The subscription of `session` is gone (detached, or nobody reads it any more): the server is told, so that it does not
    /// keep the attachment, and with it maybe the right to write, alive under the beat of this socket.
    fn forget(&mut self, session: u32) {
        if self.subs.remove(&session).is_some() {
            self.detach_on_server(session);
        }
        self.rest_if_unused();
    }

    /// Tells the server this device left `session`, when connected. Never dropped for the number of requests that wait:
    /// nothing waits for its answer, so it is not filed, and the answer is ignored when it comes.
    fn detach_on_server(&mut self, session: u32) {
        if !matches!(self.mode, Mode::Up) {
            return;
        }
        let text = Request { id: self.next_id(), op: Op::Detach { session } }.encode();
        if let Some(ws) = self.ws.as_mut()
            && let Err(error) = ws.send_text(&text)
        {
            self.broken(&error);
        }
    }

    /// With no subscription left there is nothing to reconnect for: a hub that waits for its next try, or that used its
    /// tries, is `Down`, so the next request connects at once instead of being refused.
    fn rest_if_unused(&mut self) {
        if self.subs.is_empty() && matches!(self.mode, Mode::Backoff { .. } | Mode::Failed) {
            self.mode = Mode::Down;
        }
    }

    /// Ends one subscription with `error`, and tells the server it is gone.
    fn end_sub(&mut self, session: u32, error: HubError) {
        if let Some(mut sub) = self.subs.remove(&session) {
            Self::tell_not_sent(&mut sub, self.options.sub_queue_bytes, &self.stats);
            sub.push(SubEvent::Lost(error), false, self.options.sub_queue_bytes, &self.stats);
            self.detach_on_server(session);
            self.rest_if_unused();
        }
    }

    fn lose_all(&mut self, error: HubError) {
        let bound = self.options.sub_queue_bytes;
        for (_, mut sub) in self.subs.drain() {
            Self::tell_not_sent(&mut sub, bound, &self.stats);
            sub.push(SubEvent::Lost(error), false, bound, &self.stats);
        }
    }

    /// What a subscription held that did not go out is dropped with it, and the pane that typed it is told.
    fn tell_not_sent(sub: &mut Sub, bound: usize, stats: &Cells) {
        let bytes = sub.keep.clear();
        if bytes > 0 {
            stats.dropped_input_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
            sub.push(SubEvent::NotSent { bytes }, false, bound, stats);
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
        let (origin, token) = self.endpoint.credential()?;
        let url = origin.socket_url("/ws/device").map_err(|_| Fault::Protocol)?;
        let connected = WsClient::connect(&url, &token, self.endpoint.tls(), &self.limits);
        // Only what the server said counts: an endpoint that has no token says signed out as well, and is not told.
        if matches!(connected, Err(WsError::Fault(Fault::SignedOut))) {
            self.endpoint.refused();
        }
        connected
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
            // No try is left: what was kept for the connection that did not come back is not sent, and the pane says so.
            self.drop_kept(session);
            self.tell(session, SubEvent::GaveUp);
        }
    }

    /// The connection is back: every subscription is attached again.
    fn reconnected(&mut self, ws: WsClient) {
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

    /// One try from `Failed` for a request that needs the connection (a call, a connect, a new attach). On success every
    /// subscription is attached again. On a failure that trying again may help the hub stays `Failed` and tells the panes
    /// nothing new: they already say that the tries are used and wait for Enter. A final failure ends them, as in the loop.
    fn try_once(&mut self) -> Result<(), HubError> {
        match self.open() {
            Ok(ws) => {
                self.reconnected(ws);
                Ok(())
            }
            Err(error) => {
                let final_fault = final_fault(&error);
                let fault = match &error {
                    WsError::Fault(fault) => *fault,
                    _ => final_fault.unwrap_or(Fault::Unreachable),
                };
                if let Some(fault) = final_fault {
                    self.lose_all(HubError::Fault(fault));
                    self.mode = Mode::Down;
                }
                self.rest_if_unused();
                Err(HubError::Fault(fault))
            }
        }
    }

    /// One try of the reconnect loop, after try number `attempt` was due (0 for the one try that a retry starts).
    fn try_again(&mut self, attempt: u32) {
        match self.open() {
            Ok(ws) => self.reconnected(ws),
            Err(error) => {
                if let Some(fault) = final_fault(&error) {
                    self.lose_all(HubError::Fault(fault));
                    self.mode = Mode::Down;
                } else if self.subs.is_empty() {
                    // Nobody waits for the connection any more.
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
                if code == proto::CLOSE_REVOKED {
                    self.endpoint.refused();
                }
                self.drop_connection(final_close(code));
            }
            Err(error) => self.broken(&error),
        }
    }

    fn housekeeping(&mut self) {
        self.expire_kept();
        let now = Instant::now();
        // A link that passes nothing at all is broken, even when the TCP connection says nothing of it.
        if self.ws.as_ref().is_some_and(|ws| now.saturating_duration_since(ws.heard()) >= self.options.silence) {
            self.drop_connection(None);
            return;
        }
        // Answers that never came.
        let late: Vec<u64> = self.pending.iter().filter(|(_, (_, deadline))| now >= *deadline).map(|(id, _)| *id).collect();
        for id in late {
            match self.pending.remove(&id) {
                // The server did not answer its own beat in time: the link is broken, whatever else it still passes.
                Some((Pending::Beat, _)) => {
                    self.drop_connection(None);
                    return;
                }
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
            self.beat();
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
            Pending::Quiet | Pending::Beat => {}
            Pending::Attach { id: sub, session } => {
                if !self.owns(sub, session) || !self.subs.get(&session).is_some_and(|s| s.phase == Phase::Asked { req: id }) {
                    return;
                }
                match result {
                    Err(word) => self.end_sub(session, HubError::Word(word)),
                    Ok(Answer::Attached { session: said, cols, rows, writer, more, snapshot, resume, epoch, next }) => {
                        let sane = said == session
                            && (MIN_SIZE.0..=MAX_SIZE).contains(&cols)
                            && (MIN_SIZE.1..=MAX_SIZE).contains(&rows)
                            && snapshot <= self.options.max_snapshot;
                        if !sane {
                            return self.stream_fault(session);
                        }
                        self.attached(session, cols, rows, writer, more, snapshot, resume, epoch, next);
                    }
                    Ok(_) => self.stream_fault(session),
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn attached(
        &mut self,
        session: u32,
        cols: u16,
        rows: u16,
        writer: WriterView,
        more: bool,
        snapshot: u32,
        resume: ResumeToken,
        epoch: u32,
        next: u32,
    ) {
        let max_age = self.options.input_keep_age;
        let Some(sub) = self.subs.get_mut(&session) else { return };
        let again = sub.attached_before;
        sub.attached_before = true;
        // What was kept is sent again only when the server says that this subscription writes under the very epoch the frames were
        // written under: then the right never moved, and the frames below `next` were applied already. Anything else (another
        // epoch, watching, free, a frame too old, a number the server does not expect) is dropped, and the pane says so.
        let mut lost = sub.keep.expire(Instant::now(), max_age);
        let mut resend = false;
        if !sub.keep.is_empty() {
            if writer == WriterView::Writing && sub.keep.uniform_epoch() == Some(epoch) {
                sub.keep.confirm_through(next.saturating_sub(1));
                if sub.keep.first_seq().is_some_and(|first| first != next) {
                    // A number is missing before what is kept: the server would take every frame for a gap.
                    lost += sub.keep.clear();
                }
                resend = !sub.keep.is_empty();
            } else {
                lost += sub.keep.clear();
            }
        }
        sub.writer = writer;
        // The new token replaces the old one, which is wiped as it is dropped.
        sub.resume = Some(resume);
        sub.epoch = Some(epoch);
        // Input is numbered from where the server says it is, and goes on after what is kept.
        sub.next_seq = sub.keep.last_seq().map_or(next, |last| last.saturating_add(1));
        sub.phase = if snapshot == 0 { Phase::Live { seq: 0 } } else { Phase::Snapshot { left: snapshot, seq: 0 } };
        self.tell(session, SubEvent::Attached { cols, rows, writer, more, snapshot, again, epoch });
        if lost > 0 {
            self.refuse(session, lost);
        }
        if resend {
            self.transmit(session, next, true);
        }
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
            Event::Writer { state, epoch, .. } => {
                let mut lost = 0;
                if let Some(sub) = self.subs.get_mut(&session) {
                    // The right moved, or this client does not have it: the server drops what is written under an epoch that is
                    // not its own now, so what is kept is not sent and the pane says so.
                    if state != WriterView::Writing || sub.keep.uniform_epoch().is_some_and(|kept| kept != epoch) {
                        lost = sub.keep.clear();
                    }
                    sub.writer = state;
                    sub.epoch = Some(epoch);
                }
                if lost > 0 {
                    self.refuse(session, lost);
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
                self.forget(session);
            }
            Event::Resync { .. } => self.resync(session),
            Event::Dropped { .. } => self.offer(session, SubEvent::InputDropped, true, bound),
            Event::Ack { seq, gap, .. } => {
                let Some(sub) = self.subs.get_mut(&session) else { return };
                // Every frame up to `seq` reached the program: they are not needed any more, and their bytes are wiped.
                sub.keep.confirm_through(seq);
                if !gap {
                    return;
                }
                // A gap means that the server did not get a frame (the socket's rate or a worker that was behind dropped it) and
                // got one after it. What is kept after `seq` is sent again, once for this report. With nothing kept the count goes
                // back to where the server is, so that what is typed next is applied instead of dropped; with a frame missing
                // from what is kept it cannot be filled, and the rest is not sent after a hole.
                match sub.keep.first_seq() {
                    None => sub.next_seq = seq.saturating_add(1),
                    Some(first) if first == seq.saturating_add(1) => self.transmit(session, first, true),
                    Some(_) => {
                        self.drop_kept(session);
                        if let Some(sub) = self.subs.get_mut(&session) {
                            sub.next_seq = seq.saturating_add(1);
                        }
                    }
                }
            }
        }
    }

    /// Offers an event that a subscription that has fallen behind may lose (the fresh attach carries the state again).
    fn offer(&mut self, session: u32, event: SubEvent, bounded: bool, bound: usize) {
        let Some(sub) = self.subs.get(&session) else { return };
        match sub.push(event, bounded, bound, &self.stats) {
            Pushed::Queued => {}
            Pushed::Gone => self.forget(session),
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
            Pushed::Gone => self.forget(session),
            Pushed::Full => self.overflow(session),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_certificate_is_final_with_its_own_fault_and_the_rest_keeps_its_meaning() {
        assert_eq!(final_fault(&WsError::Certificate), Some(Fault::Certificate));
        // A TLS failure that is no verdict on the certificate stays a refusal, as does the platform's local-network gate.
        assert_eq!(final_fault(&WsError::Tls), Some(Fault::Refused));
        assert_eq!(final_fault(&WsError::Blocked("no".into())), Some(Fault::Refused));
        assert_eq!(final_fault(&WsError::Fault(Fault::Unreachable)), None);
        assert_eq!(final_fault(&WsError::Fault(Fault::SignedOut)), Some(Fault::SignedOut));
        assert_eq!(final_fault(&WsError::Closed), None);
        assert_eq!(final_fault(&WsError::TooLarge), Some(Fault::Protocol));
    }

    #[test]
    fn the_debug_of_a_subscription_says_that_it_holds_a_token_and_never_what_the_token_is() {
        let token = ResumeToken::from_bytes([0xab; proto::RESUME_BYTES]);
        let hex = token.to_hex();
        let (tx, _rx) = mpsc::channel();
        let sub = Sub {
            id: 1,
            session: 3,
            tx,
            queued: Arc::new(AtomicUsize::new(0)),
            tail: None,
            phase: Phase::Live { seq: 0 },
            writer: WriterView::Writing,
            attached_before: true,
            streak: 0,
            resume: Some(token),
            epoch: Some(2),
            next_seq: 1,
            keep: Keep::default(),
        };
        let printed = format!("{sub:?} {:#?}", sub);
        assert!(!printed.contains(hex.as_str()), "{printed}");
        assert!(!printed.contains("abab"), "{printed}");
        assert!(printed.contains("ResumeToken(..)"), "{printed}");
        let none = Sub { resume: None, ..sub };
        assert!(format!("{none:?}").contains("resume: None"));
    }

    #[test]
    fn the_beat_must_be_answered_within_ten_seconds_and_the_bound_is_clamped() {
        assert_eq!(HubOptions::default().beat_timeout, Duration::from_secs(10));
        assert_eq!(HubOptions::default().clamped().beat_timeout, Duration::from_secs(10));
        let with = |beat_timeout: Duration| HubOptions { beat_timeout, ..HubOptions::default() }.clamped().beat_timeout;
        assert_eq!(with(Duration::ZERO), Duration::from_millis(50), "a beat is never due before it was sent");
        assert_eq!(with(Duration::from_millis(300)), Duration::from_millis(300));
        assert_eq!(with(Duration::from_secs(86_400)), Duration::from_secs(3600));
    }

    #[test]
    fn the_silence_bound_is_twice_the_ping_and_one_beat() {
        assert_eq!((proto::PING_SECONDS, proto::BEAT_SECONDS), (25, 10));
        assert_eq!(proto::SILENCE_SECONDS, 2 * proto::PING_SECONDS + proto::BEAT_SECONDS);
        assert_eq!(proto::SILENCE_SECONDS, 60);
        let defaults = HubOptions::default();
        assert_eq!(defaults.silence, Duration::from_secs(proto::SILENCE_SECONDS));
        assert_eq!(defaults.clamped().silence, Duration::from_secs(60));
        // The short bounds of the tests pass; zero, and anything that would turn the bound off, do not.
        let with = |silence: Duration| HubOptions { silence, ..HubOptions::default() }.clamped().silence;
        assert_eq!(with(Duration::from_millis(300)), Duration::from_millis(300));
        assert_eq!(with(Duration::ZERO), Duration::from_millis(50));
        assert_eq!(with(Duration::MAX), Duration::from_secs(3600));
        // The bound is longer than the beat, so an attached client that hears its beats answered is never taken for dead.
        assert!(defaults.silence > defaults.beat * 2);
    }
}
