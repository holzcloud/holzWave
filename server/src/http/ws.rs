//! The WebSocket route: the upgrade, the checks and the pump between the socket and the person's worker.
//!
//! `GET /ws` is a part of the [`Access::Socket`] kind: the router refuses nothing there and hands the handler a verdict about
//! the Origin, the Host and the session. The handler turns the verdict into the close codes the page understands. An upgrade
//! that is refused is accepted and closed at once (4403 for the origin, 4401 for the session): it reads no frame, starts no
//! worker, attaches no socket and writes no audit line.
//!
//! A text frame is one command, `{"id":<u64>,"cmd":"<name>","args":{...}}`, with no other member. The name is looked up in the
//! command table and the length of the frame is checked against the cap of that command before the person's worker is asked;
//! the answer and the events come back as the supervisor queued them. This file never parses a result or a payload.
//!
//! Every size, rate and time of a socket is bounded: a bucket of commands, a budget of violations, a ping, a silence time, a
//! stall time for a write and a periodic look at the session behind the socket. The loop is generic over [`Transport`] and over
//! the [`SessionPort`], so that the timing cases run on a paused clock with no real socket.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::{FromRequestParts, Request};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Extension;
use holzwave_host::commands::registry::lookup;
use holzwave_host::sessions::SessionFault;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{Notify, broadcast};
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep_until, timeout};

use super::app::{App, ConnectionPermit, Shutdown, StoreState};
use super::features::{Feature, feature_of_command};
use super::router::{Access, Authed, Part, SocketVerdict, seconds_left};
use super::routes_auth::Object;
use super::transport::{Inbound, Outgoing, SocketLimits, Transport, TransportError, Upgrade};
use crate::auth::session::{Ended, SessionError};
use crate::clock::Clock;
use crate::config::Sessions;
use crate::limits::Limits;
use crate::store::{AccountId, Session};
use crate::supervisor::{AccountKey, AttachError, Outbound, SocketHandle, SocketRx, SubmitError, Supervisor};

// ---------------------------------------------------------------------------------------------------------------------
// Words and codes
// ---------------------------------------------------------------------------------------------------------------------

/// The close code for an upgrade whose Origin or Host is not the public one.
pub const CLOSE_ORIGIN: u16 = 4403;
/// The close code for an upgrade without a live session, and for a session that ended.
pub const CLOSE_SESSION: u16 = 4401;

/// The close reasons: fixed words, never built from input.
pub mod reason {
    /// The Origin or the Host is not the public one.
    pub const ORIGIN: &str = "origin";
    /// There is no live session.
    pub const SESSION: &str = "session";
    /// A message is too large.
    pub const SIZE: &str = "size";
    /// A binary message.
    pub const TYPE: &str = "type";
    /// The peer broke the WebSocket protocol.
    pub const PROTOCOL: &str = "protocol";
    /// Text that is not UTF-8.
    pub const ENCODING: &str = "encoding";
    /// A violation budget or a refusal ceiling was passed.
    pub const POLICY: &str = "policy";
    /// Too many sockets, or a socket that does not keep up.
    pub const BUSY: &str = "busy";
    /// The server is stopping.
    pub const GOING_AWAY: &str = "going away";
    /// The peer was silent for too long.
    pub const IDLE: &str = "idle";
}

/// The answer to a command that is not in the table. It does not echo the name.
pub const ANSWER_UNKNOWN: &str = "Unknown command.";
/// The answer to a message over the cap of its command.
pub const ANSWER_TOO_LARGE: &str = "The message is too large.";
/// The answer to a command over the rate of the socket.
pub const ANSWER_TOO_MANY: &str = "Too many commands. Wait a moment.";
/// The answer to a message that is not a command.
pub const ANSWER_INVALID: &str = "The message is not valid.";
/// The answer to a command the worker cannot take now.
pub const ANSWER_BUSY: &str = "The server is busy. Try again.";

/// The answer to a command that makes or reaches a session of the server while the operator has remote sessions off (the
/// commands are named in [`super::features::REMOTE_PAGE_COMMANDS`]): the host's own sentence, so the page reads the same
/// words as on the desktop.
pub fn answer_remote_off() -> &'static str {
    SessionFault::Off.sentence()
}

/// The commands that carry what the person types or does with the pointer: only these keep a session from going idle.
pub const TOUCHING: [&str; 6] = ["key", "text", "paste", "mouse", "wheel", "send_edit"];

/// The longest a close message may take to write.
const CLOSE_BOUND: Duration = Duration::from_secs(3);

/// How long a worker may take to start for a socket that has been accepted.
const ATTACH_BOUND: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------------------------------------------------
// The settings of a socket
// ---------------------------------------------------------------------------------------------------------------------

/// Every size, rate and time of one socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketSettings {
    /// The largest message the transport accepts (the largest command cap).
    pub message_bytes: usize,
    /// The commands per second over a longer time.
    pub per_second: u32,
    /// The commands in a burst.
    pub burst: u32,
    /// How often a ping goes out.
    pub ping: Duration,
    /// How often the session is looked at.
    pub recheck: Duration,
    /// How long no frame of any kind may arrive.
    pub silence: Duration,
    /// How long a write may make no progress.
    pub stall: Duration,
    /// The violations allowed in the window.
    pub violations: u32,
    /// The span violations are counted in.
    pub violation_window: Duration,
    /// The refusals for the rate in a row that close the socket.
    pub refusals: u32,
    /// The seconds left of a session at which the page is told that it is about to end.
    pub expiring: i64,
    /// The session times.
    pub lifetimes: Sessions,
}

impl SocketSettings {
    /// The settings of the limits in force and the session times.
    pub fn from_limits(limits: &Limits, lifetimes: Sessions) -> SocketSettings {
        SocketSettings {
            message_bytes: usize::try_from(limits.ws_message_bytes).unwrap_or(usize::MAX),
            per_second: limits.commands_per_second,
            burst: limits.command_burst,
            ping: limits.ping_interval,
            recheck: limits.session_recheck,
            silence: limits.ws_silence,
            stall: limits.ws_write_stall,
            violations: limits.ws_violations,
            violation_window: limits.ws_violation_window,
            refusals: limits.ws_refusals_in_a_row,
            expiring: i64::try_from(limits.session_expiring.as_secs()).unwrap_or(i64::MAX),
            lifetimes,
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The pure counters
// ---------------------------------------------------------------------------------------------------------------------

/// A token bucket: `burst` commands at once, `per_second` more every second. It takes the instant as an argument.
#[derive(Debug, Clone)]
pub struct Bucket {
    /// Millionths of a token.
    tokens: u64,
    capacity: u64,
    per_second: u64,
    at: Instant,
}

const MICRO: u64 = 1_000_000;

impl Bucket {
    /// A full bucket at `now`.
    pub fn new(per_second: u32, burst: u32, now: Instant) -> Bucket {
        let capacity = u64::from(burst).saturating_mul(MICRO);
        Bucket { tokens: capacity, capacity, per_second: u64::from(per_second), at: now }
    }

    /// Takes one token at `now`; `false` when the bucket is empty.
    pub fn take(&mut self, now: Instant) -> bool {
        let elapsed = u64::try_from(now.saturating_duration_since(self.at).as_micros()).unwrap_or(u64::MAX);
        if now > self.at {
            self.at = now;
        }
        self.tokens = self.tokens.saturating_add(elapsed.saturating_mul(self.per_second)).min(self.capacity);
        if self.tokens >= MICRO {
            self.tokens -= MICRO;
            true
        } else {
            false
        }
    }
}

/// The violations of one socket in a sliding window.
#[derive(Debug, Clone)]
pub struct Budget {
    most: usize,
    window: Duration,
    events: VecDeque<Instant>,
}

impl Budget {
    /// A budget of `most` violations per `window`.
    pub fn new(most: u32, window: Duration) -> Budget {
        Budget { most: usize::try_from(most).unwrap_or(usize::MAX), window, events: VecDeque::new() }
    }

    /// Records a violation at `now`; `true` when it is one too many (the socket is to be closed).
    pub fn record(&mut self, now: Instant) -> bool {
        while let Some(first) = self.events.front() {
            if now.saturating_duration_since(*first) >= self.window {
                self.events.pop_front();
            } else {
                break;
            }
        }
        self.events.push_back(now);
        // The queue never holds more than the budget plus the one that broke it.
        while self.events.len() > self.most.saturating_add(1) {
            self.events.pop_front();
        }
        self.events.len() > self.most
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The session, as the socket asks about it
// ---------------------------------------------------------------------------------------------------------------------

/// Why the session behind a socket could not be confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortError {
    /// The session is over: signed out, past its time, or the account cannot sign in.
    Gone,
    /// The store could not be asked. The socket decides from the times it already knows.
    Failed,
}

/// What the loop of a socket asks of the sessions and of the secret store. The product's implementation is [`ManagerPort`].
pub trait SessionPort: Send + Sync + 'static {
    /// The session as it is stored now, if it is alive at `now` (unix seconds). Never extends the session.
    fn check(&self, session: &Session, now: i64) -> impl Future<Output = Result<Session, PortError>> + Send;
    /// Records the activity of a person at `now` and returns the session as it is stored then.
    fn touch(&self, session: &Session, now: i64) -> impl Future<Output = Result<Session, PortError>> + Send;
    /// Whether the secret store of the account is locked.
    fn locked(&self, account: AccountId) -> impl Future<Output = bool> + Send;
    /// A listener for the sessions that are ended on purpose.
    fn ended(&self) -> broadcast::Receiver<Ended>;
    /// Whether the operator has `feature` on now: read for every command of the page that belongs to it.
    fn feature_on(&self, feature: Feature) -> bool;
}

/// The sessions and the secret store of the application.
#[derive(Clone)]
pub struct ManagerPort {
    app: Arc<App>,
}

impl ManagerPort {
    /// The port over the application state.
    pub fn new(app: Arc<App>) -> ManagerPort {
        ManagerPort { app }
    }
}

fn port_error(error: &SessionError) -> PortError {
    match error {
        SessionError::Unknown | SessionError::Expired | SessionError::AccountDisabled => PortError::Gone,
        SessionError::Random | SessionError::Store(_) => PortError::Failed,
    }
}

impl SessionPort for ManagerPort {
    async fn check(&self, session: &Session, now: i64) -> Result<Session, PortError> {
        let app = self.app.clone();
        let session = session.clone();
        tokio::task::spawn_blocking(move || app.session_manager.check(&session, now).map_err(|e| port_error(&e)))
            .await
            .unwrap_or(Err(PortError::Failed))
    }

    async fn touch(&self, session: &Session, now: i64) -> Result<Session, PortError> {
        let app = self.app.clone();
        let mut session = session.clone();
        tokio::task::spawn_blocking(move || {
            app.session_manager.touch(&mut session, now).map_err(|e| port_error(&e))?;
            Ok(session)
        })
        .await
        .unwrap_or(Err(PortError::Failed))
    }

    async fn locked(&self, account: AccountId) -> bool {
        let app = self.app.clone();
        tokio::task::spawn_blocking(move || app.view.state(account) == StoreState::Locked).await.unwrap_or(true)
    }

    fn ended(&self) -> broadcast::Receiver<Ended> {
        self.app.session_manager.subscribe_ended()
    }

    fn feature_on(&self, feature: Feature) -> bool {
        self.app.features.is_on(feature)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The sockets that are open
// ---------------------------------------------------------------------------------------------------------------------

/// Counts the open sockets, so that the shutdown can wait for them to end.
#[derive(Default)]
pub struct Tracker {
    open: AtomicUsize,
    changed: Notify,
}

/// Held by a socket for as long as it is open.
pub struct Open(Arc<Tracker>);

impl Drop for Open {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
        self.0.changed.notify_waiters();
    }
}

impl Tracker {
    /// Counts one more socket until the returned value drops.
    pub fn enter(self: &Arc<Self>) -> Open {
        self.open.fetch_add(1, Ordering::SeqCst);
        Open(self.clone())
    }

    /// The sockets open now.
    pub fn open(&self) -> usize {
        self.open.load(Ordering::SeqCst)
    }

    /// Waits until no socket is open, at most `within`; says whether that happened.
    pub async fn wait_empty(&self, within: Duration) -> bool {
        let wait = async {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self.open() == 0 {
                    return;
                }
                changed.await;
            }
        };
        timeout(within, wait).await.is_ok()
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The state of the endpoint
// ---------------------------------------------------------------------------------------------------------------------

/// What the handler needs: the application, the supervisor, the settings and the count of open sockets.
pub struct WsState {
    /// The application state.
    pub app: Arc<App>,
    /// The supervisor that holds the workers.
    pub supervisor: Supervisor,
    /// The sizes, rates and times of a socket.
    pub settings: SocketSettings,
    /// The open sockets.
    pub tracker: Arc<Tracker>,
}

impl WsState {
    /// The state over the application and the supervisor, with the settings of the limits in force.
    pub fn new(app: Arc<App>, supervisor: Supervisor) -> Arc<WsState> {
        let settings = SocketSettings::from_limits(&app.limits, app.sessions);
        WsState::with_settings(app, supervisor, settings)
    }

    /// The same with other settings.
    pub fn with_settings(app: Arc<App>, supervisor: Supervisor, settings: SocketSettings) -> Arc<WsState> {
        Arc::new(WsState { app, supervisor, settings, tracker: Arc::new(Tracker::default()) })
    }
}

/// The part of the application that serves `GET /ws`.
pub fn parts(state: Arc<WsState>) -> Vec<Part> {
    let router = Router::new().route("/ws", get(handler)).layer(Extension(state));
    vec![Part { access: Access::Socket, router }]
}

/// Whether a socket may be served, decided from the verdict of the router.
pub enum Admission {
    /// The Origin and the Host are right and the session is alive.
    Admitted {
        /// The session row, with its identity (never the cookie value).
        session: Box<Session>,
        /// The person, by the account and the user id of the worker.
        account: AccountKey,
    },
    /// The socket is accepted and closed at once with this code and word.
    Refused {
        /// The close code.
        code: u16,
        /// The close reason.
        reason: &'static str,
    },
}

/// The admission of a request, from the verdict and the session the router found.
pub fn admit(verdict: Option<SocketVerdict>, who: Option<Authed>) -> Admission {
    match verdict {
        Some(v) if v.origin_ok && v.host_ok => match who {
            Some(authed) if v.session => Admission::Admitted {
                account: AccountKey { id: authed.account.id, uid: authed.account.uid },
                session: Box::new(authed.session),
            },
            _ => Admission::Refused { code: CLOSE_SESSION, reason: reason::SESSION },
        },
        _ => Admission::Refused { code: CLOSE_ORIGIN, reason: reason::ORIGIN },
    }
}

async fn handler(request: Request) -> Response {
    let (mut parts, _body) = request.into_parts();
    let upgrade = match Upgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => upgrade,
        Err(error) => return error.into_response(),
    };
    let Some(state) = parts.extensions.get::<Arc<WsState>>().cloned() else {
        return super::router::ApiError::Busy.into_response();
    };
    let verdict = parts.extensions.get::<SocketVerdict>().copied();
    let who = parts.extensions.remove::<Authed>();
    let shutdown = parts.extensions.get::<Shutdown>().cloned().unwrap_or_default();
    let permit = parts.extensions.get::<ConnectionPermit>().cloned();
    let admission = admit(verdict, who);
    let limits = SocketLimits::for_message_bytes(state.settings.message_bytes);
    upgrade.accept(limits, move |transport| async move {
        // The admission slot of the connection lives as long as the socket does.
        let _permit = permit;
        let _open = state.tracker.enter();
        let wiring = Wiring {
            port: ManagerPort::new(state.app.clone()),
            clock: state.app.clock.clone(),
            settings: state.settings,
            shutdown,
        };
        run_socket(transport, admission, state.supervisor.clone(), wiring).await;
    })
}

// ---------------------------------------------------------------------------------------------------------------------
// The socket loop
// ---------------------------------------------------------------------------------------------------------------------

/// What a socket loop needs besides the transport.
pub struct Wiring<P> {
    /// The sessions and the secret store.
    pub port: P,
    /// The clock of the sessions (unix seconds).
    pub clock: Arc<dyn Clock>,
    /// The sizes, rates and times.
    pub settings: SocketSettings,
    /// Ends the socket when the server stops.
    pub shutdown: Shutdown,
}

/// The two halves of an attached socket.
pub struct Attached {
    /// Submits commands to the worker.
    pub handle: SocketHandle,
    /// Yields what the worker says to this socket.
    pub rx: SocketRx,
}

/// Runs one accepted socket: a refused one is closed at once, an admitted one is attached to the person's worker (which this
/// starts when none runs) and served until it ends. The socket is detached whatever the end.
pub async fn run_socket<T: Transport, P: SessionPort>(
    mut transport: T,
    admission: Admission,
    supervisor: Supervisor,
    wiring: Wiring<P>,
) {
    match admission {
        Admission::Refused { code, reason } => close_bounded(&mut transport, code, reason).await,
        Admission::Admitted { session, account } => {
            let attached = tokio::select! {
                () = wiring.shutdown.wait() => Err((1001, reason::GOING_AWAY)),
                attached = timeout(ATTACH_BOUND, supervisor.attach(account)) => match attached {
                    Ok(Ok((handle, rx))) => Ok(Attached { handle, rx }),
                    Ok(Err(AttachError::ShuttingDown)) => Err((1001, reason::GOING_AWAY)),
                    Ok(Err(AttachError::TooManySockets | AttachError::Launch(_))) | Err(_) => Err((1013, reason::BUSY)),
                },
            };
            match attached {
                Ok(attached) => serve_socket(transport, *session, attached, wiring).await,
                Err((code, reason)) => close_bounded(&mut transport, code, reason).await,
            }
        }
    }
}

async fn close_bounded<T: Transport>(transport: &mut T, code: u16, reason: &'static str) {
    let _ = timeout(CLOSE_BOUND, transport.close(code, reason)).await;
}

/// Why a socket loop ended.
enum Exit {
    /// Close with this code and word.
    Close(u16, &'static str),
    /// The transport is gone; nothing more can be written.
    Gone,
}

/// The close word for a code the supervisor closed a socket with. The supervisor's own texts are not passed on.
fn word_for(code: u16) -> &'static str {
    match code {
        4401 => reason::SESSION,
        1013 => reason::BUSY,
        _ => reason::GOING_AWAY,
    }
}

fn session_event(state: &str, seconds: i64) -> String {
    format!(r#"{{"event":"hw-session","payload":{{"state":"{state}","seconds":{seconds}}}}}"#)
}

fn answer_err(id: u64, text: &str) -> String {
    format!(r#"{{"id":{id},"err":"{text}"}}"#)
}

/// An error answer whose text is a sentence of the host, written as JSON whatever it holds.
fn answer_sentence(id: u64, text: &str) -> String {
    serde_json::json!({ "id": id, "err": text }).to_string()
}

/// A command as the page sends it. No other member is allowed: the window is always `main` and the client never names one.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    id: u64,
    cmd: String,
    #[serde(default, deserialize_with = "present")]
    args: Option<Value>,
}

fn present<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// The id of a frame that is not a command, if it has a readable one: for the answer to the sender.
#[derive(Deserialize)]
struct IdOnly {
    id: u64,
}

fn lenient_id(text: &str) -> Option<u64> {
    serde_json::from_str::<Object<IdOnly>>(text).ok().map(|o| o.0.id)
}

/// The state of one socket.
struct Machine<'a, P> {
    port: &'a P,
    clock: &'a dyn Clock,
    settings: &'a SocketSettings,
    handle: &'a SocketHandle,
    shutdown: &'a Shutdown,
    ended: broadcast::Receiver<Ended>,
    session: Session,
    identity: Session,
    bucket: Bucket,
    budget: Budget,
    refusals: u32,
    locked: bool,
}

impl<P: SessionPort> Machine<'_, P> {
    /// Resolves when the server stops or when the session of this socket is ended on purpose.
    async fn interrupt(&mut self) -> Exit {
        loop {
            tokio::select! {
                () = self.shutdown.wait() => return Exit::Close(1001, reason::GOING_AWAY),
                told = self.ended.recv() => match told {
                    Ok(ended) if ended.ends(&self.identity) => return Exit::Close(CLOSE_SESSION, reason::SESSION),
                    Ok(_) => {}
                    // Some announcements were missed: the session is looked at again, and the answer decides.
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if matches!(self.port.check(&self.identity, self.clock.unix()).await, Err(PortError::Gone)) {
                            return Exit::Close(CLOSE_SESSION, reason::SESSION);
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => std::future::pending::<()>().await,
                },
            }
        }
    }

    /// Writes one message; a write that makes no progress for the stall time closes the socket, and the server stopping or the
    /// session ending interrupts it.
    async fn write<T: Transport>(&mut self, transport: &mut T, message: Outgoing) -> Result<(), Exit> {
        let stall = self.settings.stall;
        tokio::select! {
            biased;
            exit = self.interrupt() => Err(exit),
            sent = timeout(stall, transport.send(message)) => match sent {
                Ok(Ok(())) => Ok(()),
                Ok(Err(_)) => Err(Exit::Gone),
                Err(_) => Err(Exit::Close(1013, reason::BUSY)),
            },
        }
    }

    async fn text<T: Transport>(&mut self, transport: &mut T, text: String) -> Result<(), Exit> {
        self.write(transport, Outgoing::Text(text)).await
    }

    fn violation(&mut self, now: Instant) -> Result<(), Exit> {
        if self.budget.record(now) { Err(Exit::Close(1008, reason::POLICY)) } else { Ok(()) }
    }

    async fn ended_event<T: Transport>(&mut self, transport: &mut T) -> Exit {
        // The page is told before the close; if the write fails or stalls the close still goes out. The announcement of the end
        // reaches this very socket, so the write is not interrupted by it.
        let _ = timeout(self.settings.stall, transport.send(Outgoing::Text(session_event("ended", 0)))).await;
        Exit::Close(CLOSE_SESSION, reason::SESSION)
    }

    /// One text frame.
    async fn command<T: Transport>(&mut self, transport: &mut T, text: String) -> Result<(), Exit> {
        let now = Instant::now();
        if !self.bucket.take(now) {
            self.refusals = self.refusals.saturating_add(1);
            if let Some(id) = lenient_id(&text) {
                self.text(transport, answer_err(id, ANSWER_TOO_MANY)).await?;
            }
            return if self.refusals >= self.settings.refusals { Err(Exit::Close(1008, reason::POLICY)) } else { Ok(()) };
        }
        self.refusals = 0;
        let envelope = match serde_json::from_str::<Object<Envelope>>(&text) {
            Ok(Object(envelope)) if matches!(envelope.args, None | Some(Value::Object(_))) => envelope,
            _ => {
                self.violation(now)?;
                if let Some(id) = lenient_id(&text) {
                    self.text(transport, answer_err(id, ANSWER_INVALID)).await?;
                }
                return Ok(());
            }
        };
        let Some(command) = lookup(&envelope.cmd) else {
            self.violation(now)?;
            return self.text(transport, answer_err(envelope.id, ANSWER_UNKNOWN)).await;
        };
        if !command.admits(text.len()) {
            self.violation(now)?;
            return self.text(transport, answer_err(envelope.id, ANSWER_TOO_LARGE)).await;
        }
        // The switch is read for every such command, so turning it off or on takes effect with the next one. Nothing of the
        // device surface (no token, no header) is read here: the page's own sign-in is the credential.
        if let Some(feature) = feature_of_command(command.name)
            && !self.port.feature_on(feature)
        {
            return self.text(transport, answer_sentence(envelope.id, answer_remote_off())).await;
        }
        if TOUCHING.contains(&command.name) {
            match self.port.touch(&self.session, self.clock.unix()).await {
                Ok(session) => self.session = session,
                Err(PortError::Gone) => return Err(self.ended_event(transport).await),
                Err(PortError::Failed) => {}
            }
        }
        let args = match &envelope.args {
            Some(value) => value.to_string(),
            None => String::from("{}"),
        };
        match self.handle.submit(envelope.id, command.name, &args) {
            Ok(()) => Ok(()),
            Err(SubmitError::Busy) => self.text(transport, answer_err(envelope.id, ANSWER_BUSY)).await,
            Err(SubmitError::TooLarge) => self.text(transport, answer_err(envelope.id, ANSWER_TOO_LARGE)).await,
            Err(SubmitError::BadCommand | SubmitError::BadRequest) => {
                self.violation(now)?;
                self.text(transport, answer_err(envelope.id, ANSWER_INVALID)).await
            }
            // The worker or the socket is gone: the queue delivers the close.
            Err(SubmitError::Closed) => Ok(()),
        }
    }

    /// The periodic look at the session and at the lock of the secret store.
    async fn recheck<T: Transport>(&mut self, transport: &mut T) -> Result<(), Exit> {
        let now = self.clock.unix();
        match self.port.check(&self.session, now).await {
            Ok(fresh) => self.session = fresh,
            Err(PortError::Gone) => return Err(self.ended_event(transport).await),
            Err(PortError::Failed) => {
                // The store cannot be asked: what is already known still decides.
                let idle = i64::try_from(self.settings.lifetimes.idle.as_secs()).unwrap_or(i64::MAX);
                if now > self.session.last_seen.saturating_add(idle) || now > self.session.absolute_expires {
                    return Err(self.ended_event(transport).await);
                }
            }
        }
        let seconds = seconds_left(&self.session, &self.settings.lifetimes, now);
        let state = if seconds <= self.settings.expiring { "expiring" } else { "ok" };
        self.text(transport, session_event(state, seconds)).await?;
        let locked = self.port.locked(self.session.account_id).await;
        if locked {
            self.text(transport, session_event("locked", seconds)).await?;
        } else if self.locked {
            self.text(transport, session_event("unlocked", seconds)).await?;
        }
        self.locked = locked;
        Ok(())
    }
}

/// Serves an attached socket until it ends, then closes the transport with the code of the end and detaches the socket.
pub async fn serve_socket<T: Transport, P: SessionPort>(
    mut transport: T,
    session: Session,
    attached: Attached,
    wiring: Wiring<P>,
) {
    let Attached { handle, mut rx } = attached;
    let started = Instant::now();
    let settings = wiring.settings;
    let mut machine = Machine {
        port: &wiring.port,
        clock: &*wiring.clock,
        settings: &settings,
        handle: &handle,
        shutdown: &wiring.shutdown,
        ended: wiring.port.ended(),
        identity: session.clone(),
        session,
        bucket: Bucket::new(settings.per_second, settings.burst, started),
        budget: Budget::new(settings.violations, settings.violation_window),
        refusals: 0,
        locked: false,
    };
    let exit = drive(&mut machine, &mut transport, &mut rx, started).await;
    if let Exit::Close(code, word) = exit {
        close_bounded(&mut transport, code, word).await;
    }
    // The receiver and the handle drop here: the socket leaves its worker and its queue is freed.
}

async fn drive<T: Transport, P: SessionPort>(
    m: &mut Machine<'_, P>,
    transport: &mut T,
    rx: &mut SocketRx,
    started: Instant,
) -> Exit {
    let settings = *m.settings;
    let mut ping = interval_at(started + settings.ping, settings.ping);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut recheck = interval_at(started + settings.recheck, settings.recheck);
    recheck.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_inbound = started;

    // The first message is always `ok`; a store that is locked is told right after it.
    let now = m.clock.unix();
    let seconds = seconds_left(&m.session, &settings.lifetimes, now);
    if let Err(exit) = m.text(transport, session_event("ok", seconds)).await {
        return exit;
    }
    m.locked = m.port.locked(m.session.account_id).await;
    if m.locked
        && let Err(exit) = m.text(transport, session_event("locked", seconds)).await
    {
        return exit;
    }

    loop {
        let silent_at = last_inbound + settings.silence;
        let step: Result<(), Exit> = tokio::select! {
            biased;
            exit = m.interrupt() => Err(exit),
            () = sleep_until(silent_at) => Err(Exit::Close(1001, reason::IDLE)),
            frame = transport.recv() => {
                last_inbound = Instant::now();
                match frame {
                    Ok(Inbound::Text(text)) => m.command(transport, text).await,
                    Ok(Inbound::Binary(_) | Inbound::Bytes(_)) => Err(Exit::Close(1003, reason::TYPE)),
                    Ok(Inbound::Pong) => Ok(()),
                    Err(TransportError::TooLarge) => Err(Exit::Close(1009, reason::SIZE)),
                    Err(TransportError::Encoding) => Err(Exit::Close(1007, reason::ENCODING)),
                    Err(TransportError::Protocol) => Err(Exit::Close(1002, reason::PROTOCOL)),
                    Err(TransportError::Closed) => Err(Exit::Gone),
                }
            },
            out = rx.recv() => match out {
                Some(Outbound::Text(text)) => m.text(transport, text).await,
                Some(Outbound::Binary(bytes)) => m.write(transport, Outgoing::Binary(bytes)).await,
                Some(Outbound::Close { code, .. }) => Err(Exit::Close(code, word_for(code))),
                // A page's socket is never given a frame about a session: the supervisor delivers those to device sockets only.
                Some(Outbound::Ses(_)) => Err(Exit::Close(1001, reason::GOING_AWAY)),
                None => Err(Exit::Close(1001, reason::GOING_AWAY)),
            },
            _ = ping.tick() => {
                let stall = settings.stall;
                match timeout(stall, transport.ping()).await {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(_)) => Err(Exit::Gone),
                    Err(_) => Err(Exit::Close(1013, reason::BUSY)),
                }
            },
            _ = recheck.tick() => m.recheck(transport).await,
        };
        if let Err(exit) = step {
            return exit;
        }
    }
}
