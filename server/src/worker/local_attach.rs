//! The sessions of the worker as the person's own window sees them: a pane of the browser's window that shows a session
//! attaches to the worker's [`SessionHost`] in this process, with no socket, no token and no sign-in.
//!
//! The browser draws no terminal of its own: the worker renders every pane of its window, and a pane that shows a session
//! is one of them. Its link is the same [`ServerLink`] a desktop pane runs on (the notices, the reset before a snapshot, the
//! grid rule and the writer rule are that code, not a copy of it); only the way to the session differs. Where a desktop's
//! subscription goes through the hub and its socket, this one goes straight to the session host as one more
//! [`Attachment`]. The host is the one place that sees every attachment, so one writer is enforced across this window and
//! every native device of the person.
//!
//! **Identities.** Every pane's link is an attachment with a client number of its own, taken from a counter that starts at
//! [`LOCAL_CLIENT_BASE`]. Socket numbers are small `u32` values that start at 1, so no pane's number is ever the number of
//! a socket, and two panes of one window on one session are two attachments. A session made for a pane is claimed by the
//! number its pane will use: the claim waits in the service until the pane attaches, and the host keeps the right to write
//! of a client that attaches again.
//!
//! **Life against the silence rule.** A device says it is alive with a beat every [`BEAT_SECONDS`]; a pane of the window
//! has no socket and sends no beat. Its proof of life is a browser tab: while at least one browser socket of the person is
//! attached to this worker (the supervisor's `Attach` and `Detach` controls, which device sockets never send), the service
//! beats for every pane that is attached, every beat interval. All tabs of one person share the panes of the window, so
//! they are one writer identity: an attached tab keeps the right to write of a pane that holds it even when nobody types.
//! When the last tab goes the beats stop and the lease's own silence rule frees the right after its time, exactly as for a
//! device whose beats stopped; a reload inside that time keeps it. Closing the pane frees it at once. Nothing of the lease
//! or of the host changes for this: a beat counts only for the holder, as it always did.
//!
//! **Falling behind.** A pane whose reader does not keep up loses its stream as every attachment does (the host says
//! `resync`). The service attaches it again from a fresh snapshot once the pane has taken what it was handed, and ends the
//! subscription as too slow after [`MAX_RESYNC_STREAK`] fresh starts in a row, as the hub does.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use holzwave_core::conn::Link;
use holzwave_core::pane::{PaneSink, RemoteAttach, RemoteTarget};
use holzwave_core::remote::hub::{Cause, HubError, LocalRoute, MAX_RESYNC_STREAK, SUB_QUEUE_BYTES, SubEvent, SubFeed, Subscription};
use holzwave_core::remote::link::{Renew, ServerLink};
use holzwave_core::remote::proto::{BEAT_SECONDS, ErrorWord, SessionEntry, WriterView};
use holzwave_host::sessions::{SessionFault, SessionRow, SessionService};

use crate::wire::Ctl;
use crate::worker::lease::WriterState;
use crate::worker::sessions::{Attachment, SesError, SessionHost};

/// The first client number of a pane's attachment. Socket numbers are `u32` and start at 1, so every number from here on
/// is one no socket can have.
pub const LOCAL_CLIENT_BASE: u64 = 1 << 40;

/// The name of the server for the panes of its own window: the window was loaded from it.
pub const THIS_SERVER: &str = "This server";

/// How often the service looks whether a pane that fell behind has taken what it was handed.
const DRAIN_POLL: Duration = Duration::from_millis(20);

/// Whether `client` is the number of a pane's attachment (and so never the number of a socket).
pub fn is_local_client(client: u64) -> bool {
    client >= LOCAL_CLIENT_BASE
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---- the browser sockets ------------------------------------------------------------------------------------------------

/// The browser sockets of the person that are attached to this worker, as the supervisor's controls say. A set of socket
/// numbers, not a count: a `Detach` of a socket that was never attached, or that left already, changes nothing.
#[derive(Default)]
pub struct BrowserSockets {
    attached: Mutex<BTreeSet<u32>>,
}

impl BrowserSockets {
    pub fn new() -> Arc<BrowserSockets> {
        Arc::new(BrowserSockets::default())
    }

    /// Reads a control of the supervisor: `Attach` adds its socket, `Detach` takes it away, every other control is not about
    /// sockets. The supervisor sends neither for the socket of a device.
    pub fn control(&self, ctl: &Ctl) {
        match ctl {
            Ctl::Attach { socket } => {
                lock(&self.attached).insert(*socket);
            }
            Ctl::Detach { socket } => {
                lock(&self.attached).remove(socket);
            }
            _ => {}
        }
    }

    /// How many browser sockets are attached.
    pub fn count(&self) -> usize {
        lock(&self.attached).len()
    }
}

// ---- the service --------------------------------------------------------------------------------------------------------

/// The settings of the service that are not the session host's.
#[derive(Clone, Copy, Debug)]
pub struct LocalOptions {
    /// The most bytes of snapshot and output that may wait for one pane's reader ([`SUB_QUEUE_BYTES`], as for a hub).
    pub queue_bytes: usize,
    /// How often the service beats for the panes while a browser socket is attached: the device client's interval.
    pub beat: Duration,
}

impl Default for LocalOptions {
    fn default() -> LocalOptions {
        LocalOptions { queue_bytes: SUB_QUEUE_BYTES, beat: Duration::from_secs(BEAT_SECONDS) }
    }
}

enum Job {
    /// A pane's stream was lost: attach it again once it has taken what it was handed.
    Resync { tie: Arc<Tie>, generation: u64 },
    Stop,
}

struct Shared {
    host: OnceLock<Weak<SessionHost>>,
    browsers: Arc<BrowserSockets>,
    next_client: AtomicU64,
    /// The client number a session made for a pane is claimed by, until that pane attaches.
    pending: Mutex<BTreeMap<u32, u64>>,
    /// The attachments of panes that are attached: (client, session).
    live: Mutex<BTreeSet<(u64, u32)>>,
    options: LocalOptions,
    jobs: Mutex<Sender<Job>>,
    resyncs: AtomicU64,
}

impl Shared {
    fn host(&self) -> Option<Arc<SessionHost>> {
        self.host.get().and_then(Weak::upgrade)
    }

    fn next_client(&self) -> u64 {
        self.next_client.fetch_add(1, Ordering::SeqCst)
    }

    fn job(&self, job: Job) {
        let _ = lock(&self.jobs).send(job);
    }

    fn tick(&self) {
        if self.browsers.count() == 0 {
            return;
        }
        let Some(host) = self.host() else { return };
        let live: Vec<(u64, u32)> = lock(&self.live).iter().copied().collect();
        for (client, session) in live {
            let _ = host.beat(client, session);
        }
    }
}

/// The worker's sessions for the panes of its own window: the [`SessionService`] the core of the worker uses, and the
/// beats that show the life of its panes. Made before the session host (the core it starts sessions with needs the
/// service first) and bound to it once it exists.
pub struct LocalSessions {
    shared: Arc<Shared>,
}

impl LocalSessions {
    /// A service that counts `browsers` as the proof of life of its panes. Its thread beats once per beat interval and
    /// attaches the panes that fell behind again; it ends with the service.
    pub fn new(browsers: Arc<BrowserSockets>, options: LocalOptions) -> Arc<LocalSessions> {
        let (jobs, inbox) = mpsc::channel();
        let shared = Arc::new(Shared {
            host: OnceLock::new(),
            browsers,
            next_client: AtomicU64::new(LOCAL_CLIENT_BASE),
            pending: Mutex::new(BTreeMap::new()),
            live: Mutex::new(BTreeSet::new()),
            options,
            jobs: Mutex::new(jobs),
            resyncs: AtomicU64::new(0),
        });
        let work = shared.clone();
        std::thread::Builder::new()
            .name("hw-local-sessions".into())
            .spawn(move || run(&work, &inbox))
            .expect("the thread of the local sessions starts");
        Arc::new(LocalSessions { shared })
    }

    /// The session host whose sessions this service shows. Bound once; a later call changes nothing.
    pub fn bind(&self, host: &Arc<SessionHost>) {
        let _ = self.shared.host.set(Arc::downgrade(host));
    }

    /// One beat for every attached pane, when at least one browser socket is attached; nothing otherwise. The thread of the
    /// service calls it once per beat interval; a test calls it against the host's manual clock.
    pub fn tick(&self) {
        self.shared.tick();
    }

    /// How many panes are attached to a session.
    pub fn attached_panes(&self) -> usize {
        lock(&self.shared.live).len()
    }

    /// How many times a pane's stream was lost because the pane fell behind.
    pub fn resyncs(&self) -> u64 {
        self.shared.resyncs.load(Ordering::Relaxed)
    }

    fn host(&self) -> Result<Arc<SessionHost>, SessionFault> {
        self.shared.host().ok_or(SessionFault::Refused)
    }
}

impl Drop for LocalSessions {
    fn drop(&mut self) {
        self.shared.job(Job::Stop);
    }
}

/// The thread of the service: the beat interval, and the panes that wait to be attached again.
fn run(shared: &Arc<Shared>, inbox: &Receiver<Job>) {
    let beat = shared.options.beat.max(Duration::from_millis(5));
    let mut next_beat = Instant::now() + beat;
    let mut waiting: Vec<(Arc<Tie>, u64)> = Vec::new();
    loop {
        let now = Instant::now();
        let mut wait = next_beat.saturating_duration_since(now);
        if !waiting.is_empty() {
            wait = wait.min(DRAIN_POLL);
        }
        match inbox.recv_timeout(wait) {
            Ok(Job::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Ok(Job::Resync { tie, generation }) => waiting.push((tie, generation)),
            Err(RecvTimeoutError::Timeout) => {}
        }
        if Instant::now() >= next_beat {
            shared.tick();
            next_beat = Instant::now() + beat;
        }
        waiting.retain(|(tie, generation)| !tie.try_resync(*generation));
    }
}

fn hub_error(error: SesError) -> HubError {
    HubError::Word(match error {
        SesError::NotFound => ErrorWord::NotFound,
        SesError::Refused => ErrorWord::Refused,
        SesError::Busy => ErrorWord::Busy,
        SesError::TooMany => ErrorWord::TooMany,
        SesError::BadRequest => ErrorWord::BadRequest,
    })
}

fn fault(error: SesError) -> SessionFault {
    match error {
        SesError::NotFound => SessionFault::NotFound,
        SesError::TooMany => SessionFault::TooMany,
        SesError::Refused | SesError::Busy => SessionFault::Refused,
        SesError::BadRequest => SessionFault::Protocol,
    }
}

fn view(state: WriterState) -> WriterView {
    match state {
        WriterState::Writing => WriterView::Writing,
        WriterState::Watching => WriterView::Watching,
        WriterState::Free => WriterView::Free,
    }
}

impl SessionService for LocalSessions {
    fn name(&self) -> String {
        THIS_SERVER.to_owned()
    }

    fn list(&self) -> Result<Vec<SessionRow>, SessionFault> {
        let host = self.host()?;
        let mut rows: Vec<SessionRow> = host
            .list()
            .into_iter()
            .map(|info| {
                SessionRow::from(SessionEntry {
                    id: info.id,
                    title: info.title,
                    cwd: info.cwd,
                    cols: info.cols,
                    rows: info.rows,
                    started: info.started,
                    attached: info.attached,
                    writer_held: info.writer_held,
                })
            })
            .collect();
        rows.sort_by_key(|row| (row.started, row.id));
        Ok(rows)
    }

    fn create(&self, cols: u16, rows: u16) -> Result<u32, SessionFault> {
        let host = self.host()?;
        let client = self.shared.next_client();
        let session = host.create(client, Arc::new(Unclaimed), cols, rows).map_err(fault)?;
        let mut pending = lock(&self.shared.pending);
        // Claims of sessions that are gone (made and never shown) are forgotten here, so the table stays as small as the
        // list of sessions.
        let listed: BTreeSet<u32> = host.list().into_iter().map(|info| info.id).collect();
        pending.retain(|id, _| listed.contains(id));
        pending.insert(session, client);
        Ok(session)
    }

    fn end(&self, session: u32) -> Result<(), SessionFault> {
        let host = self.host()?;
        host.end(session).map_err(fault)?;
        // A claim whose pane never attached is let go, so the ended session leaves the list as soon as nobody is attached.
        let claim = lock(&self.shared.pending).remove(&session);
        if let Some(client) = claim {
            let _ = host.detach(client, session);
        }
        Ok(())
    }

    fn target(&self, session: u32) -> Result<RemoteTarget, SessionFault> {
        Ok(RemoteTarget { session, tail: None, attach: Arc::new(LocalAttach { shared: self.shared.clone() }) })
    }
}

/// The attachment of a session between its creation and the first attach of its pane: it is told nothing anybody reads.
struct Unclaimed;

impl Attachment for Unclaimed {
    fn snapshot_chunk(&self, _: u32, _: u32, _: &[u8]) -> bool {
        true
    }
    fn live(&self, _: u32, _: u32, _: &[u8]) -> bool {
        true
    }
    fn writer(&self, _: u32, _: WriterState) {}
    fn size(&self, _: u32, _: u16, _: u16) {}
    fn exited(&self, _: u32, _: Option<u32>) {}
    fn resync(&self, _: u32) {}
}

// ---- a pane's way to a session ------------------------------------------------------------------------------------------

/// What a pane's spec names to show a session of this worker.
struct LocalAttach {
    shared: Arc<Shared>,
}

impl RemoteAttach for LocalAttach {
    fn link(&self, session: u32, tail: Option<u32>, size: (u16, u16), sink: PaneSink) -> holzwave_core::Result<Box<dyn Link>> {
        let claimed = lock(&self.shared.pending).remove(&session);
        let client = claimed.unwrap_or_else(|| self.shared.next_client());
        let shared = self.shared.clone();
        let sub = subscribe(&shared, client, session, tail);
        let renew: Renew = Arc::new(move || subscribe(&shared, client, session, tail));
        Ok(Box::new(ServerLink::new(sub, tail, size, sink, renew)))
    }

    fn host(&self) -> String {
        THIS_SERVER.to_owned()
    }
}

/// A subscription of `client` to `session`, attached at once: its first events are the answer and the snapshot.
fn subscribe(shared: &Arc<Shared>, client: u64, session: u32, tail: Option<u32>) -> Subscription {
    let tie = Arc::new(Tie {
        shared: shared.clone(),
        client,
        session,
        feed: OnceLock::new(),
        generation: Mutex::new(0),
        tail: Mutex::new(tail),
        streak: AtomicU32::new(0),
        closed: AtomicBool::new(false),
    });
    let (sub, feed) = Subscription::local(session, Arc::new(TieRoute(tie.clone())), shared.options.queue_bytes);
    let _ = tie.feed.set(feed);
    tie.attach(0, false);
    sub
}

/// One pane's subscription to one session: the feed of its events and the number of its current stream. Every attach makes
/// a new stream; what an older one still hands over is dropped, so nothing of it lands after the reset of a newer one.
struct Tie {
    shared: Arc<Shared>,
    client: u64,
    session: u32,
    feed: OnceLock<SubFeed>,
    /// The number of the current stream. A push happens with this lock held, so a stream that is replaced cannot push
    /// after the event that announces its replacement.
    generation: Mutex<u64>,
    /// The tail the last attach asked for.
    tail: Mutex<Option<u32>>,
    /// Fresh starts in a row because the pane fell behind.
    streak: AtomicU32,
    closed: AtomicBool,
}

impl Tie {
    /// Hands `event` over when it belongs to the current stream. An event of a replaced stream is dropped and counts as
    /// taken, so its stream ends quietly.
    fn pass(&self, generation: u64, event: SubEvent) -> bool {
        let current = lock(&self.generation);
        if *current != generation {
            return true;
        }
        self.feed.get().is_some_and(|feed| feed.push(event))
    }

    /// Starts a new stream: `Reattaching` is the last event the pane sees before its answer, and every later push of an
    /// older stream is dropped.
    fn begin(&self, cause: Cause) -> u64 {
        let mut current = lock(&self.generation);
        *current += 1;
        if let Some(feed) = self.feed.get() {
            feed.push(SubEvent::Reattaching(cause));
        }
        *current
    }

    /// Attaches the stream `generation`. The answer (or the end of the subscription) is the stream's first event.
    fn attach(self: &Arc<Self>, generation: u64, again: bool) {
        let Some(host) = self.shared.host() else {
            self.pass(generation, SubEvent::Lost(HubError::Disconnected));
            return;
        };
        let att = Arc::new(PaneAttachment { tie: self.clone(), generation });
        let tail = *lock(&self.tail);
        host.attach(self.client, att, self.session, tail, |result| match result {
            Ok(attached) => {
                // The pane may have closed while it was being attached (a fresh start races the close): then the new
                // attachment leaves at once instead of staying behind for a subscription nobody reads. `closed` is looked at
                // under the same lock `leave` sets it under, so either `leave` removes what is inserted here or this sees it.
                let closed = {
                    let mut live = lock(&self.shared.live);
                    let closed = self.closed.load(Ordering::SeqCst);
                    if !closed {
                        live.insert((self.client, self.session));
                    }
                    closed
                };
                if closed {
                    let _ = host.detach(self.client, self.session);
                    return;
                }
                self.pass(
                    generation,
                    SubEvent::Attached {
                        cols: attached.cols,
                        rows: attached.rows,
                        writer: view(attached.writer),
                        more: attached.more,
                        snapshot: attached.snapshot_bytes,
                        again,
                    },
                );
            }
            Err(error) => {
                self.pass(generation, SubEvent::Lost(hub_error(error)));
            }
        });
    }

    /// The thread of the service asks whether the pane that fell behind can be attached again now: `true` when this is done
    /// with (attached, ended, or replaced by a newer stream), `false` while the pane still reads what it was handed.
    fn try_resync(self: &Arc<Self>, generation: u64) -> bool {
        if self.closed.load(Ordering::SeqCst) || *lock(&self.generation) != generation {
            return true;
        }
        let Some(feed) = self.feed.get() else { return true };
        if feed.queued() > feed.bound() / 2 {
            return false;
        }
        if self.streak.fetch_add(1, Ordering::SeqCst) + 1 > MAX_RESYNC_STREAK {
            // A reader that never catches up is not helped by another snapshot.
            self.leave();
            self.pass(generation, SubEvent::Lost(HubError::Slow));
            return true;
        }
        let next = self.begin(Cause::Resync);
        self.attach(next, true);
        true
    }

    fn leave(&self) {
        {
            let mut live = lock(&self.shared.live);
            self.closed.store(true, Ordering::SeqCst);
            live.remove(&(self.client, self.session));
        }
        if let Some(host) = self.shared.host() {
            let _ = host.detach(self.client, self.session);
        }
    }
}

/// The requests of a pane's subscription, made on the session host directly. Like the hub, a request whose refusal only
/// the session's events tell (a resize or a takeover that changes nothing) answers `Ok`.
struct TieRoute(Arc<Tie>);

impl TieRoute {
    fn host(&self) -> Result<Arc<SessionHost>, HubError> {
        self.0.shared.host().ok_or(HubError::Disconnected)
    }
}

impl LocalRoute for TieRoute {
    fn input(&self, bytes: &[u8]) -> Result<(), HubError> {
        let tie = &self.0;
        self.host()?.input(tie.client, tie.session, bytes).map(|_| ()).map_err(hub_error)
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), HubError> {
        let _ = self.host()?.resize(self.0.client, self.0.session, cols, rows);
        Ok(())
    }

    fn takeover(&self) -> Result<(), HubError> {
        let _ = self.host()?.takeover(self.0.client, self.0.session);
        Ok(())
    }

    fn reattach(&self, tail: Option<u32>, cause: Cause) -> Result<(), HubError> {
        let tie = &self.0;
        if tie.closed.load(Ordering::SeqCst) {
            return Err(HubError::Closed);
        }
        *lock(&tie.tail) = tail;
        let generation = tie.begin(cause);
        tie.attach(generation, true);
        Ok(())
    }

    fn detach(&self) {
        self.0.leave();
    }
}

/// A pane's stream as the session host sees it: everything it is handed becomes an event of the pane's subscription.
struct PaneAttachment {
    tie: Arc<Tie>,
    generation: u64,
}

impl Attachment for PaneAttachment {
    fn snapshot_chunk(&self, _session: u32, _seq: u32, bytes: &[u8]) -> bool {
        self.tie.pass(self.generation, SubEvent::Snapshot(bytes.to_vec()))
    }

    fn live(&self, _session: u32, _seq: u32, bytes: &[u8]) -> bool {
        let taken = self.tie.pass(self.generation, SubEvent::Live(bytes.to_vec()));
        if taken {
            // A reader that gets live bytes through has caught up: the streak of fresh starts is over.
            self.tie.streak.store(0, Ordering::SeqCst);
        }
        taken
    }

    fn writer(&self, _session: u32, state: WriterState) {
        self.tie.pass(self.generation, SubEvent::Writer(view(state)));
    }

    fn size(&self, _session: u32, cols: u16, rows: u16) {
        self.tie.pass(self.generation, SubEvent::Size(cols, rows));
    }

    fn exited(&self, _session: u32, code: Option<u32>) {
        self.tie.pass(self.generation, SubEvent::Exited(code));
    }

    fn resync(&self, _session: u32) {
        if *lock(&self.tie.generation) == self.generation {
            self.tie.shared.resyncs.fetch_add(1, Ordering::Relaxed);
        }
        // Never the host's thread's work: the service attaches the pane again once it has read what it holds.
        self.tie.shared.job(Job::Resync { tie: self.tie.clone(), generation: self.generation });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pane_number_is_never_a_socket_number() {
        assert!(LOCAL_CLIENT_BASE > u64::from(u32::MAX));
        assert!(!is_local_client(u64::from(u32::MAX)));
        assert!(is_local_client(LOCAL_CLIENT_BASE));
    }

    #[test]
    fn the_browser_sockets_are_a_set_that_only_attach_and_detach_change() {
        let sockets = BrowserSockets::new();
        sockets.control(&Ctl::Detach { socket: 3 });
        assert_eq!(sockets.count(), 0, "a socket that never attached is not taken away");
        sockets.control(&Ctl::Attach { socket: 3 });
        sockets.control(&Ctl::Attach { socket: 3 });
        assert_eq!(sockets.count(), 1, "the same socket counts once");
        sockets.control(&Ctl::Attach { socket: 4 });
        sockets.control(&Ctl::Shutdown);
        assert_eq!(sockets.count(), 2);
        sockets.control(&Ctl::Detach { socket: 3 });
        sockets.control(&Ctl::Detach { socket: 3 });
        assert_eq!(sockets.count(), 1);
    }

    #[test]
    fn the_beat_of_the_panes_is_the_device_clients_beat() {
        assert_eq!(LocalOptions::default().beat, Duration::from_secs(BEAT_SECONDS));
        assert_eq!(LocalOptions::default().queue_bytes, SUB_QUEUE_BYTES);
        assert_eq!(BEAT_SECONDS * 1000, crate::worker::lease::BEAT_INTERVAL_MS);
    }
}
