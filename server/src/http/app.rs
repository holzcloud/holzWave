use std::convert::Infallible;
use std::fmt;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::request::Parts;
use axum::http::{HeaderValue, Response, StatusCode, header};
use hyper::Request;
use hyper::body::{Body as HttpBody, Frame, Incoming, SizeHint};
use hyper::server::conn::{http1, http2};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, watch};
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt as _;
use zeroize::Zeroizing;

use crate::audit::Audit;
use crate::auth::session::SessionManager;
use crate::clock::Clock;
use crate::config::{self, Auth, Origin, Sessions};
use crate::keyfile::{KEY_BYTES, Purpose, ServerKey};
use crate::limits::{self, Limits};
use crate::proxy::{Admission, Mode, Permit, TrustedProxies};
use crate::store::{AccountId, Session, Store};
use crate::tls;

use super::csrf;
use super::router::PeerAddr;

/// The address people type in the browser, as the HTTP side compares it: the exact `Origin` text, the authority a `Host`
/// header must name, the host name and the port. The default port is never part of the authority text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicOrigin {
    origin: Origin,
}

impl PublicOrigin {
    /// Reads `public_url` with the rules of the configuration (`https`, a host, an optional port, no path).
    pub fn parse(url: &str) -> Result<PublicOrigin, config::Refusal> {
        config::public_origin(url).map(PublicOrigin::from_origin)
    }

    /// The origin of an already checked configuration.
    pub fn from_origin(origin: Origin) -> PublicOrigin {
        PublicOrigin { origin }
    }

    /// The exact value of an `Origin` header for this server, such as `https://hw.example.org:8443`.
    pub fn text(&self) -> &str {
        self.origin.as_str()
    }

    /// The host, with the port unless it is 443: what a `Host` header names and what `connect-src` allows.
    pub fn authority(&self) -> &str {
        self.origin.authority()
    }

    /// The host name in lower case, without brackets and without the port.
    pub fn host(&self) -> &str {
        self.origin.host()
    }

    /// The port; 443 when the address names none.
    pub fn port(&self) -> u16 {
        self.origin.port()
    }
}

/// Whether the secret store of an account can be used right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreState {
    /// The store is open.
    Open,
    /// The store is locked: the person has to enter the passphrase or the recovery code.
    Locked,
}

/// The state of the secret store of one account. The worker side implements this; the HTTP side only reads it.
pub trait StoreView: Send + Sync {
    /// Whether the store of `account` is open or locked.
    fn state(&self, account: AccountId) -> StoreState;
}

/// A [`StoreView`] that reports every store as open: for tests and for a server whose worker side is not running yet.
#[derive(Debug, Clone, Copy, Default)]
pub struct AlwaysOpen;

impl StoreView for AlwaysOpen {
    fn state(&self, _account: AccountId) -> StoreState {
        StoreState::Open
    }
}

/// The already built services an [`App`] is made from.
pub struct AppInit {
    /// The public address.
    pub origin: PublicOrigin,
    /// The sign-in methods of the configuration.
    pub auth: Auth,
    /// The session times of the configuration.
    pub sessions: Sessions,
    /// The limits in force.
    pub limits: Limits,
    /// How the listener is secured.
    pub mode: Mode,
    /// The proxies whose word about the client counts (none unless the mode is the proxy mode).
    pub proxies: TrustedProxies,
    /// The database.
    pub store: Arc<Store>,
    /// The sessions.
    pub session_manager: Arc<SessionManager>,
    /// The audit log.
    pub audit: Arc<Audit>,
    /// The clock.
    pub clock: Arc<dyn Clock>,
    /// The state of the secret stores.
    pub view: Arc<dyn StoreView>,
}

/// The application state of the HTTP side.
pub struct App {
    /// The public address.
    pub origin: PublicOrigin,
    /// The sign-in methods of the configuration.
    pub auth: Auth,
    /// The session times of the configuration.
    pub sessions: Sessions,
    /// The limits in force.
    pub limits: Limits,
    /// How the listener is secured.
    pub mode: Mode,
    /// The proxies whose word about the client counts.
    pub proxies: TrustedProxies,
    /// The database.
    pub store: Arc<Store>,
    /// The sessions.
    pub session_manager: Arc<SessionManager>,
    /// The audit log.
    pub audit: Arc<Audit>,
    /// The clock.
    pub clock: Arc<dyn Clock>,
    /// The state of the secret stores.
    pub view: Arc<dyn StoreView>,
    csrf_key: Zeroizing<[u8; KEY_BYTES]>,
}

impl fmt::Debug for App {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("App(..)")
    }
}

impl App {
    /// Builds the application state. The anti-forgery key is the `csrf` purpose key of the server key.
    pub fn new(init: AppInit, key: &ServerKey) -> Arc<App> {
        Arc::new(App {
            origin: init.origin,
            auth: init.auth,
            sessions: init.sessions,
            limits: init.limits,
            mode: init.mode,
            proxies: init.proxies,
            store: init.store,
            session_manager: init.session_manager,
            audit: init.audit,
            clock: init.clock,
            view: init.view,
            csrf_key: key.derive(Purpose::Csrf),
        })
    }

    /// The anti-forgery token of a session: 43 characters, stateless, different for every session.
    pub fn csrf_token(&self, session: &Session) -> String {
        csrf::token_for(&self.csrf_key, &session.id_hash)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The accept loop
// ---------------------------------------------------------------------------------------------------------------------

/// The signal that ends the accept loop. Every clone sees the same state; triggering twice changes nothing.
#[derive(Clone)]
pub struct Shutdown {
    sender: Arc<watch::Sender<bool>>,
    receiver: watch::Receiver<bool>,
}

impl fmt::Debug for Shutdown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Shutdown(..)")
    }
}

impl Default for Shutdown {
    fn default() -> Shutdown {
        Shutdown::new()
    }
}

impl Shutdown {
    /// A signal that has not been triggered.
    pub fn new() -> Shutdown {
        let (sender, receiver) = watch::channel(false);
        Shutdown { sender: Arc::new(sender), receiver }
    }

    /// Triggers the signal.
    pub fn trigger(&self) {
        self.sender.send_replace(true);
    }

    /// Whether the signal was triggered.
    pub fn is_triggered(&self) -> bool {
        *self.receiver.borrow()
    }

    /// Resolves when the signal is triggered (at once if it was).
    pub async fn wait(&self) {
        let mut receiver = self.receiver.clone();
        let _ = receiver.wait_for(|triggered| *triggered).await;
    }
}

/// Why an [`Edge`] cannot be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeError {
    /// The modes that serve TLS themselves need an acceptor.
    NeedsTls,
    /// The proxy mode serves plain HTTP and must not have an acceptor.
    NoTlsBehindProxy,
}

impl fmt::Display for EdgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EdgeError::NeedsTls => "this mode serves TLS itself and needs a TLS acceptor",
            EdgeError::NoTlsBehindProxy => "the proxy mode serves plain HTTP to trusted proxies and takes no TLS acceptor",
        })
    }
}

impl std::error::Error for EdgeError {}

/// How the bytes of a request reached the server, put into every request as an extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// Over TLS that this server terminated.
    Tls,
    /// Plain HTTP from a trusted proxy.
    Proxy,
}

/// The admission slot of the connection a request came in on, put into every request as an extension. A handler that keeps
/// the connection after the request (the WebSocket upgrade) keeps this too, so the slot is given back when the connection
/// really ends.
#[derive(Clone)]
pub struct ConnectionPermit(pub Arc<Permit>);

impl fmt::Debug for ConnectionPermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConnectionPermit(..)")
    }
}

/// Every wait of the accept loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// The whole TLS handshake.
    pub handshake: Duration,
    /// The head of a request, from its first byte to its last, and the first request of a new connection.
    pub header: Duration,
    /// A connection with no request in progress, between two requests.
    pub idle: Duration,
    /// How long a peer may leave the server unable to write a single byte, because it does not read.
    pub stall: Duration,
    /// How long requests in flight may finish after the shutdown signal, before the rest is dropped.
    pub grace: Duration,
    /// How often an HTTP/2 connection is asked whether the peer is still there.
    pub h2_ping_interval: Duration,
    /// How long the answer to that question may take.
    pub h2_ping_timeout: Duration,
}

impl Timing {
    /// The waits of the contract: 10 s for the handshake and for a request head, 60 s idle, 10 s of grace.
    pub const DEFAULT: Timing = Timing {
        handshake: limits::HANDSHAKE_TIMEOUT,
        header: limits::HEADER_TIMEOUT,
        idle: Duration::from_secs(60),
        stall: Duration::from_secs(10),
        grace: Duration::from_secs(10),
        h2_ping_interval: Duration::from_secs(20),
        h2_ping_timeout: Duration::from_secs(10),
    };

    /// The defaults with the handshake and header times of the resolved limits.
    pub fn from_limits(limits: &Limits) -> Timing {
        Timing { handshake: limits.handshake_timeout, header: limits.header_timeout, ..Timing::DEFAULT }
    }
}

/// What the accept loop needs, all of it handed in: it reads no configuration itself.
pub struct Edge {
    router: Router,
    acceptor: Option<TlsAcceptor>,
    admission: Admission,
    mode: Mode,
    timing: Timing,
}

impl Edge {
    /// The listener of a router. `acceptor` is `Some` in the `files` and `acme` modes and `None` in the proxy mode.
    pub fn new(router: Router, acceptor: Option<TlsAcceptor>, admission: Admission, mode: Mode) -> Result<Edge, EdgeError> {
        match (mode, &acceptor) {
            (Mode::Proxy, Some(_)) => Err(EdgeError::NoTlsBehindProxy),
            (Mode::Files | Mode::Acme, None) => Err(EdgeError::NeedsTls),
            _ => Ok(Edge { router, acceptor, admission, mode, timing: Timing::DEFAULT }),
        }
    }

    /// The same edge with other waits.
    pub fn with_timing(mut self, timing: Timing) -> Edge {
        self.timing = timing;
        self
    }
}

/// How long the loop waits after the system refuses to accept a connection (for example when it is out of descriptors).
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

/// The most streams an HTTP/2 connection may have open at once.
pub const MAX_H2_STREAMS: u32 = 128;

/// The most bytes of decoded header list an HTTP/2 request may have.
pub const MAX_H2_HEADER_LIST: u32 = 64 * 1024;

/// The most streams a peer may reset before the server has looked at them, before the connection is ended.
const H2_PENDING_ACCEPT_RESETS: usize = 20;

/// The most streams the server may reset because of a peer's error before the connection is ended.
const H2_LOCAL_ERROR_RESETS: usize = 128;

/// The most requests one connection may start and abandon within [`WINDOW`] before it is ended. A peer that opens a stream
/// and resets it at once, over and over, is the rapid-reset attack; a page abandons a handful of requests in a moment.
pub const MAX_ABANDONED_REQUESTS: u32 = 200;

/// The most bytes a connection may send within [`WINDOW`] that are not the body of a request. Request heads, pings, settings
/// and window updates are such bytes; a flood of the small frames that cost the server work and carry nothing ends here.
pub const MAX_NON_BODY_BYTES: u64 = 1024 * 1024;

/// The span the two caps above are counted in, in milliseconds.
const WINDOW: u64 = 10_000;

/// The most request headers HTTP/1.1 accepts.
pub const MAX_H1_HEADERS: usize = 100;

/// The most bytes HTTP/1.1 buffers for a request head (the request line and the headers together).
pub const MAX_H1_BUFFER: usize = 64 * 1024;

/// What the connection has done so far, shared by the byte counter, the request wrappers and the watchdog. Times are
/// milliseconds since the connection became ready; a head time is stored plus one so that zero can mean "not set".
struct Gauge {
    start: Instant,
    /// Whether the start of a request head can be told from other bytes (HTTP/1.1 only).
    track_head: bool,
    served: AtomicBool,
    inflight: AtomicU32,
    body_open: AtomicU32,
    head_since: AtomicU64,
    idle_since: AtomicU64,
    last_read: AtomicU64,
    write_stalled: AtomicU64,
    window_start: AtomicU64,
    abandoned: AtomicU32,
    non_body: AtomicU64,
    flooded: AtomicBool,
    wake: Notify,
}

/// What the watchdog does next.
enum Verdict {
    /// End the connection now.
    Cut,
    /// Look again at this time.
    Until(u64),
    /// No limit is running; look again when something changes.
    Nothing,
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl Gauge {
    fn new(track_head: bool) -> Gauge {
        Gauge {
            start: Instant::now(),
            track_head,
            served: AtomicBool::new(false),
            inflight: AtomicU32::new(0),
            body_open: AtomicU32::new(0),
            head_since: AtomicU64::new(0),
            idle_since: AtomicU64::new(0),
            last_read: AtomicU64::new(0),
            write_stalled: AtomicU64::new(0),
            window_start: AtomicU64::new(0),
            abandoned: AtomicU32::new(0),
            non_body: AtomicU64::new(0),
            flooded: AtomicBool::new(false),
            wake: Notify::new(),
        }
    }

    fn now(&self) -> u64 {
        millis(self.start.elapsed())
    }

    /// Starts a new counting window when the last one is over.
    fn roll(&self, now: u64) {
        if now.saturating_sub(self.window_start.load(Ordering::Relaxed)) >= WINDOW {
            self.window_start.store(now, Ordering::Relaxed);
            self.abandoned.store(0, Ordering::Relaxed);
            self.non_body.store(0, Ordering::Relaxed);
        }
    }

    /// `count` bytes arrived from the peer. With no request in progress they are the start of a request head.
    fn read(&self, count: usize) {
        let now = self.now();
        self.last_read.store(now, Ordering::Relaxed);
        if self.body_open.load(Ordering::Acquire) == 0 {
            self.roll(now);
            let total = self.non_body.fetch_add(count as u64, Ordering::Relaxed).saturating_add(count as u64);
            if total > MAX_NON_BODY_BYTES {
                self.flooded.store(true, Ordering::Release);
                self.wake.notify_one();
            }
        }
        if self.track_head
            && self.inflight.load(Ordering::Acquire) == 0
            && self.body_open.load(Ordering::Acquire) == 0
            && self.head_since.load(Ordering::Acquire) == 0
        {
            self.head_since.store(now + 1, Ordering::Release);
            self.wake.notify_one();
        }
    }

    /// Bytes went out: the peer is reading.
    fn wrote(&self) {
        if self.write_stalled.load(Ordering::Relaxed) != 0 {
            self.write_stalled.store(0, Ordering::Relaxed);
        }
    }

    /// A write cannot go on until the peer reads.
    fn write_blocked(&self) {
        if self.write_stalled.load(Ordering::Relaxed) == 0 {
            self.write_stalled.store(self.now() + 1, Ordering::Relaxed);
            self.wake.notify_one();
        }
    }

    fn request_started(&self) {
        self.served.store(true, Ordering::Release);
        self.inflight.fetch_add(1, Ordering::AcqRel);
        self.head_since.store(0, Ordering::Release);
    }

    fn request_ended(&self, abandoned: bool) {
        if abandoned {
            let now = self.now();
            self.roll(now);
            if self.abandoned.fetch_add(1, Ordering::Relaxed) + 1 >= MAX_ABANDONED_REQUESTS {
                self.flooded.store(true, Ordering::Release);
            }
        }
        if self.inflight.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.idle_since.store(self.now(), Ordering::Release);
        }
        self.wake.notify_one();
    }

    fn body_opened(&self) {
        self.body_open.fetch_add(1, Ordering::AcqRel);
    }

    fn body_closed(&self) {
        self.body_open.fetch_sub(1, Ordering::AcqRel);
        self.wake.notify_one();
    }

    /// The deadline that applies now. A request head must be complete within the header time. A connection with no request in
    /// progress has the header time for its first request and the idle time between requests. A request body that has stopped
    /// arriving has the idle time, and an answer the peer does not read has the stall time.
    fn verdict(&self, timing: &Timing) -> Verdict {
        if self.flooded.load(Ordering::Acquire) {
            return Verdict::Cut;
        }
        let now = self.now();
        let mut deadline: Option<u64> = None;
        let mut limit = |at: u64| deadline = Some(deadline.map_or(at, |current| current.min(at)));
        let head = self.head_since.load(Ordering::Acquire);
        if head != 0 {
            limit((head - 1).saturating_add(millis(timing.header)));
        } else if self.inflight.load(Ordering::Acquire) == 0 {
            let wait = if self.served.load(Ordering::Acquire) { timing.idle } else { timing.header };
            limit(self.idle_since.load(Ordering::Acquire).saturating_add(millis(wait)));
        }
        if self.body_open.load(Ordering::Acquire) > 0 {
            limit(self.last_read.load(Ordering::Relaxed).saturating_add(millis(timing.idle)));
        }
        let stalled = self.write_stalled.load(Ordering::Relaxed);
        if stalled != 0 {
            limit((stalled - 1).saturating_add(millis(timing.stall)));
        }
        match deadline {
            None => Verdict::Nothing,
            Some(at) if at <= now => Verdict::Cut,
            Some(at) => Verdict::Until(at),
        }
    }
}

/// Resolves when the connection has outstayed one of its limits.
async fn watch(gauge: Arc<Gauge>, timing: Timing) {
    loop {
        match gauge.verdict(&timing) {
            Verdict::Cut => return,
            Verdict::Until(at) => {
                let wait = Duration::from_millis(at.saturating_sub(gauge.now()));
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    () = gauge.wake.notified() => {}
                }
            }
            Verdict::Nothing => gauge.wake.notified().await,
        }
    }
}

/// The byte stream of a connection, reporting to its [`Gauge`] what moves through it.
struct Watched<I> {
    inner: I,
    gauge: Arc<Gauge>,
}

impl<I: AsyncRead + Unpin> AsyncRead for Watched<I> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let poll = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(poll, Poll::Ready(Ok(()))) {
            let count = buf.filled().len() - before;
            if count > 0 {
                self.gauge.read(count);
            }
        }
        poll
    }
}

impl<I: AsyncWrite + Unpin> AsyncWrite for Watched<I> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        let poll = Pin::new(&mut self.inner).poll_write(cx, buf);
        match &poll {
            Poll::Ready(Ok(n)) if *n > 0 => self.gauge.wrote(),
            Poll::Pending => self.gauge.write_blocked(),
            _ => {}
        }
        poll
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        let poll = Pin::new(&mut self.inner).poll_flush(cx);
        match &poll {
            Poll::Ready(Ok(())) => self.gauge.wrote(),
            Poll::Pending => self.gauge.write_blocked(),
            _ => {}
        }
        poll
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let poll = Pin::new(&mut self.inner).poll_write_vectored(cx, bufs);
        match &poll {
            Poll::Ready(Ok(n)) if *n > 0 => self.gauge.wrote(),
            Poll::Pending => self.gauge.write_blocked(),
            _ => {}
        }
        poll
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

/// A request being handled. Dropping it before the answer exists counts the request as abandoned.
struct Inflight {
    gauge: Arc<Gauge>,
    answered: bool,
}

impl Inflight {
    fn begin(gauge: &Arc<Gauge>) -> Inflight {
        gauge.request_started();
        Inflight { gauge: gauge.clone(), answered: false }
    }
}

impl Drop for Inflight {
    fn drop(&mut self) {
        self.gauge.request_ended(!self.answered);
    }
}

/// The body of an answer. It holds the request open until the connection is done with the body.
struct Held {
    body: Body,
    _request: Inflight,
}

impl HttpBody for Held {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        Pin::new(&mut self.body).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

/// The body of a request. While it is not finished, a peer that stops sending is cut after the idle time.
struct BodyWatch {
    body: Body,
    gauge: Arc<Gauge>,
    open: bool,
}

impl BodyWatch {
    fn close(&mut self) {
        if self.open {
            self.open = false;
            self.gauge.body_closed();
        }
    }
}

impl HttpBody for BodyWatch {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let poll = Pin::new(&mut self.body).poll_frame(cx);
        if matches!(poll, Poll::Ready(None | Some(Err(_)))) {
            self.close();
        }
        poll
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

impl Drop for BodyWatch {
    fn drop(&mut self) {
        self.close();
    }
}

/// What every request of a connection shares.
struct Link {
    router: Router,
    gauge: Arc<Gauge>,
    peer: IpAddr,
    kind: TransportKind,
    shutdown: Shutdown,
    permit: ConnectionPermit,
}

/// The bytes of a request head: the target and every header name and value.
fn head_size(parts: &Parts) -> usize {
    let target = parts.uri.path_and_query().map_or(0, |target| target.as_str().len());
    parts.headers.iter().fold(target, |sum, (name, value)| sum + name.as_str().len() + value.len())
}

/// One request: the extensions the handlers read, the watch over its body, and the router.
async fn handle(link: Arc<Link>, mut inflight: Inflight, request: Request<Incoming>) -> Result<Response<Body>, Infallible> {
    let (mut parts, incoming) = request.into_parts();
    if head_size(&parts) > MAX_H1_BUFFER {
        // Both protocols are held to the same size of request head, whatever their own parsers let through.
        let mut refused = Response::new(Body::empty());
        *refused.status_mut() = StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE;
        refused.headers_mut().insert(header::CONNECTION, HeaderValue::from_static("close"));
        refused.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        inflight.answered = true;
        return Ok(refused.map(|body| Body::new(Held { body, _request: inflight })));
    }
    parts.extensions.insert(PeerAddr(link.peer));
    parts.extensions.insert(link.kind);
    parts.extensions.insert(link.shutdown.clone());
    parts.extensions.insert(link.permit.clone());
    let body = Body::new(incoming);
    let body = if body.is_end_stream() {
        body
    } else {
        link.gauge.body_opened();
        Body::new(BodyWatch { body, gauge: link.gauge.clone(), open: true })
    };
    let response = match link.router.clone().oneshot(Request::from_parts(parts, body)).await {
        Ok(response) => response,
        Err(never) => match never {},
    };
    inflight.answered = true;
    Ok(response.map(|body| Body::new(Held { body, _request: inflight })))
}

/// Collects how a connection task ended. A panic in a handler ends only that task: the join set is the boundary, the permit
/// and the connection go with the task, and the loop goes on. This file writes no log line, so nothing a request carried can
/// reach one from here; the panic itself is reported by the panic hook.
fn note(ended: Result<(), tokio::task::JoinError>) {
    drop(ended);
}

/// Accepts connections on `listener` until `shutdown` is triggered. This is the only place the server accepts connections: a
/// connection is admitted before one byte is read, completes the TLS handshake within the handshake time, and is then served
/// as HTTP/1.1 or, when the peer chose it, HTTP/2.
///
/// After the signal the loop stops accepting and closes the listener, gives the connections the grace time to finish what they
/// are doing (idle ones close at once), and drops the rest.
pub async fn serve(listener: TcpListener, edge: Edge, shutdown: Shutdown) {
    let edge = Arc::new(edge);
    let mut tasks: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            () = shutdown.wait() => break,
            Some(ended) = tasks.join_next(), if !tasks.is_empty() => note(ended),
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else {
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                    continue;
                };
                // A connection over the limit is closed at once: the stream is dropped before a byte is read.
                let Ok(permit) = edge.admission.try_admit(peer.ip().to_canonical()) else { continue };
                tasks.spawn(connection(stream, peer.ip().to_canonical(), permit, edge.clone(), shutdown.clone()));
            }
        }
    }
    drop(listener);
    let grace = tokio::time::sleep(edge.timing.grace);
    tokio::pin!(grace);
    loop {
        tokio::select! {
            ended = tasks.join_next() => match ended {
                Some(ended) => note(ended),
                None => return,
            },
            () = &mut grace => break,
        }
    }
    tasks.abort_all();
    while let Some(ended) = tasks.join_next().await {
        note(ended);
    }
}

/// One connection, from the handshake to its close. The admission permit is held until it ends, and longer if a handler keeps
/// the connection (an upgrade) and with it the permit.
async fn connection(stream: TcpStream, peer: IpAddr, permit: Permit, edge: Arc<Edge>, shutdown: Shutdown) {
    let permit = ConnectionPermit(Arc::new(permit));
    let _ = stream.set_nodelay(true);
    match &edge.acceptor {
        Some(acceptor) => {
            let Ok(tls) = tls::accept_tls(acceptor, stream, edge.timing.handshake).await else { return };
            let http2 = tls.get_ref().1.alpn_protocol() == Some(b"h2".as_slice());
            run(tls, http2, TransportKind::Tls, peer, permit, &edge, shutdown).await;
        }
        None => {
            let kind = if edge.mode == Mode::Proxy { TransportKind::Proxy } else { TransportKind::Tls };
            run(stream, false, kind, peer, permit, &edge, shutdown).await;
        }
    }
}

/// Serves HTTP over `io` until the peer is done, a limit is passed, or the shutdown signal has had its way.
async fn run<I>(io: I, http2: bool, kind: TransportKind, peer: IpAddr, permit: ConnectionPermit, edge: &Edge, shutdown: Shutdown)
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let timing = edge.timing;
    let gauge = Arc::new(Gauge::new(!http2));
    let link = Arc::new(Link { router: edge.router.clone(), gauge: gauge.clone(), peer, kind, shutdown: shutdown.clone(), permit });
    let service = service_fn(move |request: Request<Incoming>| {
        let inflight = Inflight::begin(&link.gauge);
        handle(link.clone(), inflight, request)
    });
    let io = TokioIo::new(Watched { inner: io, gauge: gauge.clone() });
    let watchdog = watch(gauge, timing);
    tokio::pin!(watchdog);
    let mut stopping = false;
    if http2 {
        let mut builder = http2::Builder::new(TokioExecutor::new());
        builder
            .timer(TokioTimer::new())
            .max_concurrent_streams(MAX_H2_STREAMS)
            .max_header_list_size(MAX_H2_HEADER_LIST)
            .max_pending_accept_reset_streams(H2_PENDING_ACCEPT_RESETS)
            .max_local_error_reset_streams(H2_LOCAL_ERROR_RESETS)
            .keep_alive_interval(timing.h2_ping_interval)
            .keep_alive_timeout(timing.h2_ping_timeout);
        let connection = builder.serve_connection(io, service);
        tokio::pin!(connection);
        loop {
            tokio::select! {
                _ = connection.as_mut() => break,
                () = watchdog.as_mut() => break,
                () = shutdown.wait(), if !stopping => {
                    stopping = true;
                    connection.as_mut().graceful_shutdown();
                }
            }
        }
    } else {
        let mut builder = http1::Builder::new();
        builder
            .timer(TokioTimer::new())
            .header_read_timeout(timing.idle + timing.header)
            .max_headers(MAX_H1_HEADERS)
            .max_buf_size(MAX_H1_BUFFER)
            .keep_alive(true);
        let connection = builder.serve_connection(io, service).with_upgrades();
        tokio::pin!(connection);
        loop {
            tokio::select! {
                _ = connection.as_mut() => break,
                () = watchdog.as_mut() => break,
                () = shutdown.wait(), if !stopping => {
                    stopping = true;
                    connection.as_mut().graceful_shutdown();
                }
            }
        }
    }
}
