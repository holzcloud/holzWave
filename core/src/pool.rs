//! One SSH connection per saved connection, shared by everything that needs the host.
//!
//! A [`Connection`] owns the login of one saved SSH profile. The file browser ([`crate::files::SftpSource`]) and the
//! command runner ([`crate::ssh::SshExec`]) take their channels from it instead of logging in on their own, so one host
//! in use by a file browser, a monitor and the agent costs one TCP connection and one password or one-time-code prompt.
//! A [`SessionPool`] keeps one connection per profile id and generation of the connection list
//! ([`crate::connections::Connections::session`]); [`Connection::standalone`] makes a connection with a runtime of its
//! own, which is what `SshExec::new` and `SftpSource::new` use, so each of them still works without a pool.
//!
//! Five rules make the sharing safe. Each one is a test in `tests/pool.rs`.
//!
//! 1. **Every consumer of a channel drains at once.** `russh` hands data to a channel with a bounded `send(..).await` in
//!    the session task, so one channel whose reader stops stalls every channel of the connection. The consumers here
//!    read continuously into bounded buffers; a consumer added later (the shell, a forwarded socket) must do the same
//!    and close its channel when its buffer is full, never queue without a bound.
//! 2. **The host-key policy belongs to the opener.** An [`Open::Pinned`] opener (a monitor, the agent, an exec) checks
//!    the host key of the host and of every jump host strictly: it never *learns* a key, so an unknown key is refused
//!    and the known hosts file stays as it was. An [`Open::Interactive`] opener (a file browser, later a pane) keeps
//!    trust on first use, because a person's own act is what learns a key. A session made by either opener serves both
//!    afterwards, since by then the key was learned by the person's own act or was known before. A key that changed is
//!    refused for both, loudly.
//! 3. **A login runs on the pool's runtime, never on the caller's.** A channel handle is runtime-neutral, but the
//!    session task of `russh` is spawned on the runtime that runs the login: a caller that ends (a pane's thread, a
//!    one-off runtime) must not take the session with it. Logins are single flight (one at a time per connection, every
//!    waiter gets the same result) and bounded to [`OPEN_TIMEOUT`].
//! 4. **A background opener waits after a failure.** After a refused login ([`COOLDOWN_FATAL`]) or a lost connection
//!    ([`COOLDOWN_LOST`]) an [`Open::Pinned`] open answers with the same reason without any network attempt, so a wrong
//!    password is not tried every second against a server that locks accounts. An [`Open::Interactive`] open is never
//!    delayed: a person who presses Enter or clicks a folder is never made to wait.
//! 5. **Idle and bounds.** A session with no lease and no use for [`IDLE`] is closed, and at most [`MAX_CONNECTIONS`]
//!    connections are kept.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use russh::Disconnect;
use russh::client::Handle;
use russh_sftp::client::SftpSession;

use crate::ssh::{Client, Outcome, SshTarget, open, pin_hosts};
use crate::{Error, Result};

/// A session with no lease and no use for this long is closed by the reaper.
pub const IDLE: Duration = Duration::from_secs(120);
/// The most connections a pool keeps.
pub const MAX_CONNECTIONS: usize = 16;
/// How long a background opener waits after a refused login, a changed or refused host key or an unusable key.
pub const COOLDOWN_FATAL: Duration = Duration::from_secs(15);
/// How long a background opener waits after a lost or timed-out connection.
pub const COOLDOWN_LOST: Duration = Duration::from_secs(3);
/// How long one login (connect, host key, authentication, jump hosts) may take.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(20);
/// How often the reaper looks for idle sessions.
const REAP_EVERY: Duration = Duration::from_secs(30);
/// The bound of closing a session politely.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// Who opens the session, which decides the host-key rule (see the module documentation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Open {
    /// A person's own open: the host-key policy of the target (trust on first use), and never made to wait.
    Interactive,
    /// A command runner's open: strict on the host and every jump host, and cooled down after a failure.
    Pinned,
}

/// Why a session could not be opened. The reasons are the ones `ssh::open` produced and never hold a credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenError {
    /// True for a refused login, a changed or refused host key and an unusable key; false for a lost or timed-out
    /// connection.
    pub fatal: bool,
    pub why: String,
}

impl OpenError {
    fn lost(why: impl Into<String>) -> Self {
        Self { fatal: false, why: why.into() }
    }

    fn fatal(why: impl Into<String>) -> Self {
        Self { fatal: true, why: why.into() }
    }

    fn ended() -> Self {
        Self::lost("the connection task ended")
    }

    /// How long a background opener waits after this failure.
    fn cooldown(&self) -> Duration {
        if self.fatal { COOLDOWN_FATAL } else { COOLDOWN_LOST }
    }
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.why)
    }
}

impl std::error::Error for OpenError {}

/// A runtime that is shut down without waiting when its last owner lets go, so no owner can panic by dropping it inside
/// an async context.
struct OwnedRuntime(Option<tokio::runtime::Runtime>);

impl OwnedRuntime {
    fn build(workers: usize, name: &str) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .enable_all()
            .thread_name(name)
            .build()
            .map_err(|e| Error::Pty(format!("ssh runtime: {e}")))?;
        Ok(Self(Some(runtime)))
    }

    fn handle(&self) -> tokio::runtime::Handle {
        match &self.0 {
            Some(runtime) => runtime.handle().clone(),
            // Only `drop` empties the cell.
            None => unreachable!("the runtime is only taken when it is dropped"),
        }
    }
}

impl Drop for OwnedRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

/// One login: the `russh` handle and the SFTP subsystem made on it, once, when a file browser first needs it.
pub struct Session {
    handle: Handle<Client>,
    sftp: tokio::sync::Mutex<Option<Arc<SftpSession>>>,
    /// Set when the session was handed to its first user.
    handed_out: AtomicBool,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("closed", &self.is_closed()).finish_non_exhaustive()
    }
}

impl Session {
    fn new(handle: Handle<Client>) -> Self {
        Self { handle, sftp: tokio::sync::Mutex::new(None), handed_out: AtomicBool::new(false) }
    }

    /// The `russh` handle: open a channel with it.
    pub(crate) fn handle(&self) -> &Handle<Client> {
        &self.handle
    }

    /// Whether the connection has ended.
    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    /// True for the first call only: the session was just made for this user, so a failure to open a channel on it is
    /// not a stale connection and is not tried again.
    pub(crate) fn first_use(&self) -> bool {
        !self.handed_out.swap(true, Ordering::SeqCst)
    }

    /// End the connection politely, bounded in time.
    async fn close(&self) {
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, self.handle.disconnect(Disconnect::ByApplication, "", "en")).await;
    }
}

/// The last failed open, which a background opener answers from while it is young.
struct Failure {
    at: Instant,
    error: OpenError,
}

/// How much a connection is used, for the reaper.
struct Usage {
    leases: usize,
    last_used: Instant,
}

/// The connection of one saved SSH profile. See the module documentation.
pub struct Connection {
    label: String,
    target: SshTarget,
    /// Keeps the runtime alive as long as the connection (and so its session task) is.
    _runtime: Arc<OwnedRuntime>,
    handle: tokio::runtime::Handle,
    /// The session. The guard is held across a login, which is what makes a login single flight.
    slot: tokio::sync::Mutex<Option<Arc<Session>>>,
    failure: Mutex<Option<Failure>>,
    usage: Mutex<Usage>,
}

impl std::fmt::Debug for Connection {
    // The target holds a secret: only the label is printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection").field("label", &self.label).finish_non_exhaustive()
    }
}

impl Connection {
    fn new(label: String, target: SshTarget, runtime: Arc<OwnedRuntime>) -> Arc<Self> {
        let handle = runtime.handle();
        Arc::new(Self {
            label,
            target,
            _runtime: runtime,
            handle,
            slot: tokio::sync::Mutex::new(None),
            failure: Mutex::new(None),
            usage: Mutex::new(Usage { leases: 0, last_used: Instant::now() }),
        })
    }

    /// A connection with a runtime of its own (one worker thread drives keepalives and replies between calls), made
    /// without a pool. Nothing connects before the first use.
    pub fn standalone(label: impl Into<String>, target: SshTarget) -> Result<Arc<Self>> {
        Ok(Self::new(label.into(), target, Arc::new(OwnedRuntime::build(1, "hw-conn")?)))
    }

    /// The name the caller gave, for its messages.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Where this connection goes. It holds the login: never print it.
    pub fn target(&self) -> &SshTarget {
        &self.target
    }

    /// The runtime the logins and the session tasks run on.
    pub fn handle(&self) -> tokio::runtime::Handle {
        self.handle.clone()
    }

    /// Run `f` to its end on the calling thread, with the connection's runtime serving the timers and the session
    /// tasks. Blocks: never call it from an async task.
    pub fn block_on<F: Future>(&self, f: F) -> F::Output {
        self.handle.block_on(f)
    }

    /// Whether a session exists and has not ended. Says false while a login is in progress.
    pub fn is_connected(&self) -> bool {
        self.slot.try_lock().is_ok_and(|slot| slot.as_ref().is_some_and(|session| !session.is_closed()))
    }

    /// Count a use of this connection until the returned [`Lease`] is dropped: the idle reaper never closes a session
    /// that has a lease.
    pub fn lease(self: &Arc<Self>) -> Lease {
        let mut usage = self.usage.lock().unwrap_or_else(PoisonError::into_inner);
        usage.leases += 1;
        usage.last_used = usage.last_used.max(Instant::now());
        drop(usage);
        Lease { connection: self.clone() }
    }

    fn touch(&self, now: Instant) {
        let mut usage = self.usage.lock().unwrap_or_else(PoisonError::into_inner);
        usage.last_used = usage.last_used.max(now);
    }

    /// How long a background open still waits, at `now`, after the last failed open. `None` when it may try.
    pub fn cooldown_left(&self, now: Instant) -> Option<Duration> {
        let failure = self.failure.lock().unwrap_or_else(PoisonError::into_inner);
        let failure = failure.as_ref()?;
        let until = failure.at + failure.error.cooldown();
        (until > now).then(|| until - now)
    }

    /// The reason of the last failed open while a background opener must still wait.
    fn cooling(&self, now: Instant) -> Option<OpenError> {
        self.cooldown_left(now)?;
        self.failure.lock().unwrap_or_else(PoisonError::into_inner).as_ref().map(|failure| failure.error.clone())
    }

    fn record(&self, now: Instant, error: &OpenError) {
        *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(Failure { at: now, error: error.clone() });
    }

    /// The session of this connection, made when there is none (or the old one ended). Safe to call from any runtime:
    /// the login runs on the connection's own runtime and keeps running if the caller is cancelled.
    pub async fn session(self: &Arc<Self>, open: Open) -> std::result::Result<Arc<Session>, OpenError> {
        self.session_at(open, Instant::now()).await
    }

    /// [`Connection::session`] with the clock of the caller, so a test can move time without sleeping.
    pub async fn session_at(self: &Arc<Self>, open: Open, now: Instant) -> std::result::Result<Arc<Session>, OpenError> {
        let me = self.clone();
        self.handle.spawn(async move { me.session_here(open, now).await }).await.map_err(|_| OpenError::ended())?
    }

    async fn session_here(&self, open: Open, now: Instant) -> std::result::Result<Arc<Session>, OpenError> {
        let mut slot = self.slot.lock().await;
        if let Some(session) = slot.as_ref().filter(|session| !session.is_closed()) {
            self.touch(now);
            return Ok(session.clone());
        }
        *slot = None;
        // Asked after the lock: a waiter behind a login that just failed gets that failure and makes no attempt.
        if open == Open::Pinned {
            if let Some(error) = self.cooling(now) {
                return Err(error);
            }
        }
        let target = match open {
            Open::Interactive => self.target.clone(),
            Open::Pinned => pin_hosts(&self.target).map_err(OpenError::fatal)?,
        };
        let outcome = match tokio::time::timeout(OPEN_TIMEOUT, open_target(&target)).await {
            Ok(outcome) => outcome,
            Err(_) => Err(OpenError::lost("the connection timed out")),
        };
        match outcome {
            Ok(handle) => {
                *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = None;
                self.touch(now);
                let session = Arc::new(Session::new(handle));
                *slot = Some(session.clone());
                Ok(session)
            }
            Err(error) => {
                self.record(now, &error);
                Err(error)
            }
        }
    }

    /// Forget `stale` and end it, if it is still this connection's session. Compared by identity, so the session that a
    /// parallel call made in the meantime is never thrown away.
    pub async fn invalidate(&self, stale: &Arc<Session>) {
        let mut slot = self.slot.lock().await;
        if slot.as_ref().is_some_and(|current| Arc::ptr_eq(current, stale)) {
            *slot = None;
            drop(slot);
            self.close_in_background(stale.clone());
        }
    }

    /// The SFTP session on the connection's session, made on first use, with the session it lives on. A file browser is
    /// a person's own open, so the login is [`Open::Interactive`].
    pub async fn sftp(self: &Arc<Self>) -> std::result::Result<(Arc<Session>, Arc<SftpSession>), OpenError> {
        let session = self.session(Open::Interactive).await?;
        let on = session.clone();
        let sftp = self
            .handle
            .spawn(async move {
                let mut slot = on.sftp.lock().await;
                if let Some(sftp) = slot.as_ref() {
                    return Ok(sftp.clone());
                }
                let opened = tokio::time::timeout(OPEN_TIMEOUT, async {
                    let channel = on.handle.channel_open_session().await.map_err(|e| e.to_string())?;
                    channel.request_subsystem(true, "sftp").await.map_err(|e| e.to_string())?;
                    SftpSession::new(channel.into_stream()).await.map_err(|e| e.to_string())
                })
                .await;
                match opened {
                    Ok(Ok(sftp)) => {
                        let sftp = Arc::new(sftp);
                        *slot = Some(sftp.clone());
                        Ok(sftp)
                    }
                    Ok(Err(why)) => Err(OpenError::lost(why)),
                    Err(_) => Err(OpenError::lost("the connection timed out")),
                }
            })
            .await
            .map_err(|_| OpenError::ended())??;
        Ok((session, sftp))
    }

    /// Forget the SFTP session `stale` of `session`, if it is still the one it holds, and end it.
    pub async fn drop_sftp(&self, session: &Arc<Session>, stale: &Arc<SftpSession>) {
        let mut slot = session.sftp.lock().await;
        if slot.as_ref().is_some_and(|current| Arc::ptr_eq(current, stale)) {
            *slot = None;
            drop(slot);
            let _ = stale.close().await;
        }
    }

    fn close_in_background(&self, session: Arc<Session>) {
        drop(self.handle.spawn(async move { session.close().await }));
    }

    /// Close the session of this connection when it has no lease and no use for [`IDLE`] before `now`. Returns whether a
    /// session was closed.
    fn reap(&self, now: Instant) -> bool {
        {
            let usage = self.usage.lock().unwrap_or_else(PoisonError::into_inner);
            if usage.leases > 0 || now.saturating_duration_since(usage.last_used) < IDLE {
                return false;
            }
        }
        // A login or a use in progress holds the lock: that session is not idle.
        let Ok(mut slot) = self.slot.try_lock() else { return false };
        let Some(session) = slot.take() else { return false };
        drop(slot);
        self.close_in_background(session);
        true
    }

    /// Close the session now, whatever its use. For a connection the pool let go of.
    fn close_now(&self) {
        if let Ok(mut slot) = self.slot.try_lock() {
            if let Some(session) = slot.take() {
                drop(slot);
                self.close_in_background(session);
            }
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // The last user is gone: end the connection instead of waiting for the keepalive to find it unused.
        if let Some(session) = self.slot.get_mut().take() {
            self.close_in_background(session);
        }
    }
}

/// Log in to `target` on the current runtime.
async fn open_target(target: &SshTarget) -> std::result::Result<Handle<Client>, OpenError> {
    match open(target).await {
        Ok(handle) => Ok(handle),
        Err(Outcome::Fatal(why)) => Err(OpenError::fatal(why)),
        Err(Outcome::Lost(why)) => Err(OpenError::lost(why)),
        Err(Outcome::Exit(_) | Outcome::Closed) => Err(OpenError::lost("the connection ended")),
    }
}

/// A use of a connection. Dropping it releases the use that [`Connection::lease`] counted.
pub struct Lease {
    connection: Arc<Connection>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut usage = self.connection.usage.lock().unwrap_or_else(PoisonError::into_inner);
        usage.leases = usage.leases.saturating_sub(1);
        usage.last_used = usage.last_used.max(Instant::now());
    }
}

struct Entry {
    id: String,
    generation: u64,
    connection: Arc<Connection>,
}

/// The connections of the saved SSH profiles: one for each profile id and generation of the connection list.
pub struct SessionPool {
    entries: Mutex<Vec<Entry>>,
    /// The pool's runtime, built when the first connection is made, so a pool that is never used creates no thread.
    runtime: Mutex<Option<Arc<OwnedRuntime>>>,
    me: Weak<SessionPool>,
}

impl SessionPool {
    /// An empty pool. No thread exists until the first connection is made.
    pub fn new() -> Arc<Self> {
        Arc::new_cyclic(|me| Self { entries: Mutex::new(Vec::new()), runtime: Mutex::new(None), me: me.clone() })
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, Vec<Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The runtime of the pool, built (with the reaper thread) on first use.
    fn runtime(&self) -> Result<Arc<OwnedRuntime>> {
        let mut cell = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(runtime) = cell.as_ref() {
            return Ok(runtime.clone());
        }
        let runtime = Arc::new(OwnedRuntime::build(2, "hw-pool")?);
        let pool = self.me.clone();
        // A thread that cannot be made leaves the pool without a reaper: sessions then end by their keepalive instead.
        let _ = thread::Builder::new().name("hw-pool-reap".into()).spawn(move || {
            loop {
                thread::sleep(REAP_EVERY);
                let Some(pool) = pool.upgrade() else { break };
                pool.reap(Instant::now());
            }
        });
        *cell = Some(runtime.clone());
        Ok(runtime)
    }

    /// The connection of profile `id` for this `generation` of the connection list. The same id and generation give the
    /// same connection; another generation replaces it (the old one ends when its last user lets go). `build` makes the
    /// label and the target, and runs without any lock held (it reads the keychain). At most [`MAX_CONNECTIONS`] are
    /// kept: an unused one is closed to make room, and when every one is in use the call fails.
    pub fn connection(
        &self,
        id: &str,
        generation: u64,
        build: impl FnOnce() -> Result<(String, SshTarget)>,
    ) -> Result<Arc<Connection>> {
        if let Some(found) = self.find(id, generation) {
            return Ok(found);
        }
        let (label, target) = build()?;
        let runtime = self.runtime()?;
        let mut doomed = Vec::new();
        let kept = {
            let mut entries = self.entries();
            // Two callers that built the same profile at once end with one entry: the second finds the first's.
            if let Some(entry) = entries.iter().find(|e| e.id == id && e.generation == generation) {
                entry.connection.clone()
            } else {
                let (old, rest): (Vec<Entry>, Vec<Entry>) = std::mem::take(&mut *entries).into_iter().partition(|e| e.id == id);
                doomed.extend(old.into_iter().map(|e| e.connection));
                *entries = rest;
                if entries.len() >= MAX_CONNECTIONS {
                    match entries.iter().enumerate().filter_map(|(i, e)| unused_since(&e.connection).map(|at| (at, i))).min() {
                        Some((_, index)) => doomed.push(entries.remove(index).connection),
                        None => return Err(Error::Unsupported("too many connections are open".into())),
                    }
                }
                let connection = Connection::new(label, target, runtime);
                entries.push(Entry { id: id.to_owned(), generation, connection: connection.clone() });
                connection
            }
        };
        // An evicted connection that is idle ends now; one that is replaced by a new generation ends when its users go.
        for connection in doomed {
            if unused_since(&connection).is_some() {
                connection.close_now();
            }
        }
        Ok(kept)
    }

    fn find(&self, id: &str, generation: u64) -> Option<Arc<Connection>> {
        self.entries().iter().find(|e| e.id == id && e.generation == generation).map(|e| e.connection.clone())
    }

    /// How many connections are kept.
    pub fn len(&self) -> usize {
        self.entries().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Close the session of every connection with no lease and no use for [`IDLE`] before `now`.
    pub fn reap(&self, now: Instant) {
        let connections: Vec<Arc<Connection>> = self.entries().iter().map(|e| e.connection.clone()).collect();
        for connection in connections {
            connection.reap(now);
        }
    }
}

/// When a connection was last used, or `None` while it has a lease.
fn unused_since(connection: &Connection) -> Option<Instant> {
    let usage = connection.usage.lock().unwrap_or_else(PoisonError::into_inner);
    (usage.leases == 0).then_some(usage.last_used)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh::SshAuth;

    fn target(host: &str) -> SshTarget {
        SshTarget::new(host, "holz", SshAuth::Password("pw".into()))
    }

    fn pooled(pool: &SessionPool, id: &str, generation: u64) -> Result<Arc<Connection>> {
        pool.connection(id, generation, || Ok((id.to_owned(), target("127.0.0.1"))))
    }

    #[test]
    fn a_refused_login_waits_fifteen_seconds_and_a_lost_connection_three() {
        let connection = Connection::standalone("x", target("127.0.0.1")).unwrap();
        let t0 = Instant::now();
        assert_eq!(connection.cooldown_left(t0), None, "nothing failed yet");
        connection.record(t0, &OpenError::fatal("x@y refused the login"));
        assert_eq!(connection.cooldown_left(t0), Some(COOLDOWN_FATAL));
        assert_eq!(connection.cooldown_left(t0 + Duration::from_secs(14)), Some(Duration::from_secs(1)));
        assert_eq!(connection.cooldown_left(t0 + COOLDOWN_FATAL), None);
        assert_eq!(connection.cooling(t0 + Duration::from_secs(1)).unwrap().why, "x@y refused the login");
        connection.record(t0, &OpenError::lost("the connection timed out"));
        assert_eq!(connection.cooldown_left(t0 + Duration::from_secs(2)), Some(Duration::from_secs(1)));
        assert_eq!(connection.cooldown_left(t0 + COOLDOWN_LOST), None);
        assert!(connection.cooling(t0 + COOLDOWN_LOST).is_none());
    }

    #[test]
    fn a_connection_is_idle_after_two_minutes_without_a_lease_and_never_while_leased() {
        let connection = Connection::standalone("x", target("127.0.0.1")).unwrap();
        let t0 = Instant::now();
        connection.touch(t0);
        assert_eq!(unused_since(&connection), Some(t0));
        let lease = connection.lease();
        assert_eq!(unused_since(&connection), None, "a leased connection is never unused");
        assert!(!connection.reap(t0 + IDLE * 10), "a leased connection is never reaped");
        drop(lease);
        // Nothing to close here (no session was ever made), so the answer is false; the rule is in `tests/pool.rs`.
        assert!(!connection.reap(t0 + IDLE));
    }

    #[test]
    fn the_pool_keeps_sixteen_and_evicts_the_one_unused_longest() {
        let pool = SessionPool::new();
        assert!(pool.is_empty(), "a new pool is empty and has built no runtime");
        let mut first = Vec::new();
        for n in 0..MAX_CONNECTIONS {
            let connection = pooled(&pool, &format!("c{n}"), 1).unwrap();
            // A distinct, increasing last-use time (the clock moves forward by `n` seconds).
            connection.touch(Instant::now() + Duration::from_secs(n as u64));
            first.push(connection);
        }
        assert_eq!(pool.len(), MAX_CONNECTIONS);
        // c0 was used least recently: it goes for the newest.
        pooled(&pool, "new", 1).unwrap();
        assert_eq!(pool.len(), MAX_CONNECTIONS);
        let again = pooled(&pool, "c0", 1).unwrap();
        assert!(!Arc::ptr_eq(&again, &first[0]), "c0 was evicted and is made again");
        // c1 was the next-oldest and went for c0.
        assert!(Arc::ptr_eq(&pooled(&pool, "c2", 1).unwrap(), &first[2]), "c2 is still the same connection");
    }

    #[test]
    fn a_new_generation_replaces_the_old_connection_of_a_profile() {
        let pool = SessionPool::new();
        let one = pooled(&pool, "a", 1).unwrap();
        assert!(Arc::ptr_eq(&one, &pooled(&pool, "a", 1).unwrap()));
        let two = pooled(&pool, "a", 2).unwrap();
        assert!(!Arc::ptr_eq(&one, &two));
        assert_eq!(pool.len(), 1, "the old generation is gone from the pool");
    }

    #[test]
    fn the_debug_text_of_a_connection_is_its_label_only() {
        let connection = Connection::standalone("Talos", target("secret-host.example")).unwrap();
        let text = format!("{connection:?}");
        assert!(text.contains("Talos") && !text.contains("secret-host") && !text.contains("pw"), "{text}");
    }
}
