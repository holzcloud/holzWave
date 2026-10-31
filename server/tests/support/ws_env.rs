//! The environment of the WebSocket tests: a real TLS listener on a loopback port, a test application with a person who is
//! signed in, a supervisor over real in-process workers (a real `/bin/sh` in a real pseudo terminal), and a WebSocket client
//! written by hand so that a test can send what a library never would (unmasked frames, bad UTF-8, a continuation that starts
//! nothing, a length that does not fit).
//!
//! Tests pull it in with `#[path = "support/edge.rs"] mod edge;`, `#[path = "support/edge_tls.rs"] mod edge_tls;` and
//! `#[path = "support/ws_env.rs"] mod ws_env;`. Everything here needs the `in-process-workers` feature.
#![allow(dead_code)]
#![cfg(feature = "in-process-workers")]

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use holzwave_host::state::ShellSpec;
use holzwave_server::clock::{Clock, ManualClock};
use holzwave_server::http::app::Shutdown;
use holzwave_server::http::router::{self, Part};
use holzwave_server::http::ws::{self, SocketSettings, WsState};
use holzwave_server::proxy::{Admission, Mode, TrustedProxies};
use holzwave_server::store::Role;
use holzwave_server::supervisor::{AccountKey, InProcessLauncher, Supervisor, SupervisorSettings};
use holzwave_server::tls::{self, FileResolver};
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::edge::{self, HOST, ORIGIN, Signed, TestApp};
use crate::edge_tls::{Conn, Reply, Request};

#[path = "scratch.rs"]
mod scratch;
use scratch::Scratch;

/// The longest a helper waits for the server before it fails the test.
pub const PATIENCE: Duration = Duration::from_secs(15);

/// The first moment of the clock of the supervisor.
pub const SUPERVISOR_START: i64 = 1_800_000_000;

/// How a started environment differs from the default one.
#[derive(Default)]
pub struct WsOptions {
    /// Changes the settings of every socket (short times, small budgets).
    pub tune: Option<fn(&mut SocketSettings)>,
    /// Replaces the settings of the supervisor.
    pub supervisor: Option<SupervisorSettings>,
}

/// What the server holds for one person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Counts {
    /// Sockets attached to the worker.
    pub sockets: usize,
    /// Workers that run for this person.
    pub workers: usize,
    /// Lines in the audit log (all people).
    pub audit_lines: usize,
}

/// A person who exists in the test application and is signed in.
pub struct Person {
    pub name: String,
    pub signed: Signed,
    pub key: AccountKey,
}

/// A running listener with a supervisor behind it.
pub struct WsEnv {
    pub addr: SocketAddr,
    pub test: TestApp,
    pub supervisor: Supervisor,
    pub launcher: Arc<InProcessLauncher>,
    pub state: Arc<WsState>,
    pub shutdown: Shutdown,
    pub supervisor_clock: Arc<ManualClock>,
    task: Option<JoinHandle<()>>,
    der: CertificateDer<'static>,
    _scratch: Scratch,
}

impl Drop for WsEnv {
    fn drop(&mut self) {
        self.shutdown.trigger();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl WsEnv {
    /// The default environment.
    pub async fn start() -> WsEnv {
        WsEnv::start_with(WsOptions::default()).await
    }

    /// An environment with the given options.
    pub async fn start_with(options: WsOptions) -> WsEnv {
        let test = TestApp::new();
        let scratch = Scratch::new("ws");
        let shell = ShellSpec::new("/bin/sh", &[]);
        let launcher = Arc::new(InProcessLauncher::new(scratch.path().to_path_buf(), shell));
        let supervisor_clock = Arc::new(ManualClock::new(SUPERVISOR_START));
        let supervisor_clock_dyn: Arc<dyn Clock> = supervisor_clock.clone();
        let supervisor = Supervisor::new(launcher.clone(), options.supervisor.unwrap_or_default(), supervisor_clock_dyn);

        let mut settings = SocketSettings::from_limits(&test.app.limits, test.app.sessions);
        if let Some(tune) = options.tune {
            tune(&mut settings);
        }
        let state = WsState::with_settings(test.app.clone(), supervisor.clone(), settings);
        let parts: Vec<Part> = ws::parts(state.clone());
        let router = router::build(test.app.clone(), parts);

        let made = rcgen::generate_simple_self_signed(vec![HOST.to_owned()]).expect("generate a certificate");
        let cert = scratch.write_file("cert.pem", made.cert.pem().as_bytes(), 0o644);
        let key = scratch.write_file("key.pem", made.signing_key.serialize_pem().as_bytes(), 0o600);
        let resolver = Arc::new(FileResolver::new(&cert, &key).expect("load the pair"));
        let acceptor = tls::acceptor(Arc::new(tls::server_config(resolver, &tls::ALPN).expect("server configuration")));
        let admission = Admission::with_defaults(Mode::Files, TrustedProxies::none()).expect("admission");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local address");
        let shutdown = Shutdown::new();
        let edge = holzwave_server::http::app::Edge::new(router, Some(acceptor), admission, Mode::Files).expect("the edge");
        let task = tokio::spawn(holzwave_server::http::app::serve(listener, edge, shutdown.clone()));
        WsEnv {
            addr,
            test,
            supervisor,
            launcher,
            state,
            shutdown,
            supervisor_clock,
            task: Some(task),
            der: made.cert.der().clone(),
            _scratch: scratch,
        }
    }

    /// Makes the account `name` (a member) and signs it in.
    pub fn person(&self, name: &str) -> Person {
        self.test.create_account(name, Role::Member);
        let signed = self.test.sign_in(name);
        let account = self.test.store.account_by_username(name).expect("read").expect("the account");
        Person { name: name.to_owned(), key: AccountKey { id: account.id, uid: account.uid }, signed }
    }

    /// Signs the same person in once more: another session, another cookie.
    pub fn sign_in_again(&self, person: &Person) -> Signed {
        self.test.sign_in(&person.name)
    }

    /// What the server holds for `person`.
    pub fn counts(&self, person: &Person) -> Counts {
        let stats = self.supervisor.account_stats(person.key);
        let mut audit_lines = 0;
        if let Ok(entries) = std::fs::read_dir(&self.test.audit_dir) {
            for entry in entries.flatten() {
                if let Ok(text) = std::fs::read_to_string(entry.path()) {
                    audit_lines += text.lines().count();
                }
            }
        }
        Counts { sockets: stats.sockets, workers: usize::from(stats.worker_running), audit_lines }
    }

    /// The totals of the supervisor: workers and sockets over all people.
    pub fn totals(&self) -> (usize, usize) {
        let stats = self.supervisor.stats();
        (stats.workers, stats.sockets)
    }

    /// A client configuration that trusts the generated certificate and nothing else.
    fn client_config(&self) -> ClientConfig {
        let mut store = RootCertStore::empty();
        store.add(self.der.clone()).expect("trust the generated certificate");
        let mut config = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("client protocol versions")
            .with_root_certificates(store)
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        config
    }

    async fn tls(&self) -> TlsStream<TcpStream> {
        let tcp = tokio::time::timeout(PATIENCE, TcpStream::connect(self.addr)).await.expect("connect in time").expect("connect");
        let name = ServerName::try_from(HOST).expect("server name");
        let connector = TlsConnector::from(Arc::new(self.client_config()));
        tokio::time::timeout(PATIENCE, connector.connect(name, tcp)).await.expect("a handshake in time").expect("the TLS handshake")
    }

    /// One plain HTTP request on its own connection.
    pub async fn plain(&self, request: Request) -> Reply {
        let head_only = request.method == "HEAD";
        let mut conn = Conn::new(self.tls().await);
        conn.send(&request.bytes()).await;
        conn.reply(head_only).await.expect("an answer")
    }

    /// The upgrade request of a client: the right Host, the given Origin and cookie.
    pub fn upgrade_request(cookie: Option<&str>, origin: Option<&str>, host: Option<&str>) -> Request {
        let mut request = Request::new("GET", "/ws");
        if let Some(host) = host {
            request.headers.retain(|(n, _)| n != "Host");
            request = request.header("Host", host);
        }
        request = request
            .header("Upgrade", "websocket")
            .header("Connection", "Upgrade")
            .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
            .header("Sec-WebSocket-Version", "13");
        if let Some(origin) = origin {
            request = request.header("Origin", origin);
        }
        if let Some(cookie) = cookie {
            request = request.cookie(cookie);
        }
        request
    }

    /// Opens a socket with the given cookie and Origin. `Err` holds the plain answer when the server did not switch.
    pub async fn try_connect(&self, cookie: Option<&str>, origin: Option<&str>) -> Result<WsClient, Reply> {
        self.try_connect_request(WsEnv::upgrade_request(cookie, origin, None)).await
    }

    /// Opens a socket with a request of the test's making.
    pub async fn try_connect_request(&self, request: Request) -> Result<WsClient, Reply> {
        let mut conn = Conn::new(self.tls().await);
        conn.send(&request.bytes()).await;
        let mut client = WsClient::new(conn);
        let reply = client.read_head().await;
        if reply.status != 101 {
            return Err(reply);
        }
        assert_eq!(reply.header("sec-websocket-accept"), Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="), "the accept key");
        Ok(client)
    }

    /// Opens a socket as the person sees it in a browser: the session cookie and the public Origin.
    pub async fn connect_as(&self, who: &Signed) -> WsClient {
        self.connect(Some(&who.cookie), Some(ORIGIN)).await
    }

    /// Opens a socket that must be switched to the WebSocket protocol.
    pub async fn connect(&self, cookie: Option<&str>, origin: Option<&str>) -> WsClient {
        match self.try_connect(cookie, origin).await {
            Ok(client) => client,
            Err(reply) => panic!("the upgrade was answered with {}: {}", reply.status, reply.text()),
        }
    }

    /// Waits until `check` holds, at most `PATIENCE`.
    pub async fn eventually(&self, what: &str, check: impl Fn(&WsEnv) -> bool) {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while !check(self) {
            assert!(tokio::time::Instant::now() < deadline, "{what} did not happen in time");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// What the client read from the socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    /// A close message: the code (none for an empty body) and the reason.
    Close(Option<u16>, String),
}

/// What a read gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Got {
    Message(Incoming),
    /// Nothing came in the time.
    Quiet,
    /// The server closed the connection.
    Ended,
}

pub const TEXT: u8 = 1;
pub const BINARY: u8 = 2;
pub const CONTINUATION: u8 = 0;
pub const CLOSE: u8 = 8;
pub const PING: u8 = 9;
pub const PONG: u8 = 10;

/// The bytes of a client frame (masked, as a client must) with the given first byte.
pub fn masked_frame(first: u8, payload: &[u8]) -> Vec<u8> {
    let mask = [0x5a, 0xa5, 0x3c, 0xc3];
    let mut out = vec![first];
    match payload.len() {
        n if n < 126 => out.push(0x80 | n as u8),
        n if n <= 0xffff => {
            out.push(0x80 | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(0x80 | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

/// A WebSocket seen from the client side.
pub struct WsClient {
    conn: Conn<TlsStream<TcpStream>>,
    buffer: Vec<u8>,
    backlog: VecDeque<Incoming>,
    next: u64,
    /// Whether a ping of the server is answered by a pong.
    pub auto_pong: bool,
    ended: bool,
}

impl WsClient {
    fn new(conn: Conn<TlsStream<TcpStream>>) -> WsClient {
        WsClient { conn, buffer: Vec::new(), backlog: VecDeque::new(), next: 1, auto_pong: true, ended: false }
    }

    /// Reads the head of the answer to the upgrade (and its body when it is not a switch); what follows stays in the buffer.
    async fn read_head(&mut self) -> Reply {
        loop {
            if let Some(end) = self.buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&self.buffer[..end]).into_owned();
                let mut lines = head.split("\r\n");
                let status: u16 = lines.next().and_then(|l| l.split(' ').nth(1)).and_then(|s| s.parse().ok()).expect("a status line");
                let headers: Vec<(String, String)> = lines
                    .filter_map(|line| line.split_once(':').map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned())))
                    .collect();
                self.buffer.drain(..end + 4);
                let mut body = Vec::new();
                if status != 101 {
                    let length = headers
                        .iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, v)| v.parse::<usize>().ok())
                        .unwrap_or(0);
                    while self.buffer.len() < length {
                        if self.fill(PATIENCE).await != Some(true) {
                            break;
                        }
                    }
                    let take = length.min(self.buffer.len());
                    body = self.buffer.drain(..take).collect();
                }
                return Reply { status, headers, body };
            }
            assert_eq!(self.fill(PATIENCE).await, Some(true), "the server answered the upgrade");
        }
    }

    // ---- writing ----

    /// Writes bytes as they are.
    pub async fn send_raw(&mut self, bytes: &[u8]) {
        let _ = tokio::time::timeout(PATIENCE, async {
            let _ = self.conn.stream.write_all(bytes).await;
            let _ = self.conn.stream.flush().await;
        })
        .await;
    }

    /// Writes one masked frame with the given first byte (FIN and the opcode).
    pub async fn send_frame(&mut self, first: u8, payload: &[u8]) {
        self.send_raw(&masked_frame(first, payload)).await;
    }

    /// Writes a text message.
    pub async fn send_text(&mut self, text: &str) {
        self.send_frame(0x80 | TEXT, text.as_bytes()).await;
    }

    /// Writes a close message with a code.
    pub async fn send_close(&mut self, code: u16) {
        self.send_frame(0x80 | CLOSE, &code.to_be_bytes()).await;
    }

    /// Sends a command and returns the request number.
    pub async fn command(&mut self, cmd: &str, args: &str) -> u64 {
        let id = self.next;
        self.next += 1;
        self.send_text(&format!(r#"{{"id":{id},"cmd":"{cmd}","args":{args}}}"#)).await;
        id
    }

    // ---- reading ----

    async fn fill(&mut self, within: Duration) -> Option<bool> {
        let mut chunk = vec![0u8; 64 * 1024];
        match tokio::time::timeout(within, self.conn.stream.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => {
                self.buffer.extend_from_slice(&chunk[..n]);
                Some(true)
            }
            Ok(_) => Some(false),
            Err(_) => None,
        }
    }

    /// One whole frame from the buffer, if it is there: the first byte and the payload.
    fn take_frame(&mut self) -> Option<(u8, Vec<u8>)> {
        let b = &self.buffer;
        if b.len() < 2 {
            return None;
        }
        let masked = b[1] & 0x80 != 0;
        let (len, mut at) = match b[1] & 0x7f {
            126 if b.len() >= 4 => (usize::from(u16::from_be_bytes([b[2], b[3]])), 4),
            126 => return None,
            127 if b.len() >= 10 => (u64::from_be_bytes(b[2..10].try_into().ok()?) as usize, 10),
            127 => return None,
            n => (usize::from(n), 2),
        };
        if masked {
            at += 4;
        }
        if b.len() < at + len {
            return None;
        }
        let first = b[0];
        let payload = b[at..at + len].to_vec();
        self.buffer.drain(..at + len);
        Some((first, payload))
    }

    /// The next message (text and binary assembled from their fragments), or why none came.
    pub async fn read(&mut self, within: Duration) -> Got {
        if let Some(message) = self.backlog.pop_front() {
            return Got::Message(message);
        }
        let deadline = tokio::time::Instant::now() + within;
        let mut partial: Option<(u8, Vec<u8>)> = None;
        loop {
            while let Some((first, payload)) = self.take_frame() {
                let fin = first & 0x80 != 0;
                let opcode = first & 0x0f;
                let message = match opcode {
                    TEXT | BINARY if fin => Some((opcode, payload)),
                    TEXT | BINARY => {
                        partial = Some((opcode, payload));
                        None
                    }
                    CONTINUATION => {
                        let (kind, mut so_far) = partial.take().expect("a continuation of something");
                        so_far.extend_from_slice(&payload);
                        if fin {
                            Some((kind, so_far))
                        } else {
                            partial = Some((kind, so_far));
                            None
                        }
                    }
                    PING => {
                        if self.auto_pong {
                            self.send_frame(0x80 | PONG, &payload).await;
                        }
                        return Got::Message(Incoming::Ping(payload));
                    }
                    PONG => return Got::Message(Incoming::Pong(payload)),
                    CLOSE => {
                        let code = (payload.len() >= 2).then(|| u16::from_be_bytes([payload[0], payload[1]]));
                        let reason = String::from_utf8_lossy(payload.get(2..).unwrap_or_default()).into_owned();
                        return Got::Message(Incoming::Close(code, reason));
                    }
                    other => panic!("the server sent an opcode {other}"),
                };
                if let Some((kind, bytes)) = message {
                    return Got::Message(if kind == TEXT {
                        Incoming::Text(String::from_utf8(bytes).expect("the server's text is UTF-8"))
                    } else {
                        Incoming::Binary(bytes)
                    });
                }
            }
            if self.ended {
                return Got::Ended;
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return Got::Quiet;
            }
            match self.fill(left).await {
                Some(true) => {}
                Some(false) => {
                    self.ended = true;
                }
                None => return Got::Quiet,
            }
        }
    }

    /// The next text message parsed as JSON, skipping pings and pongs.
    pub async fn read_json(&mut self, within: Duration) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match self.read(left).await {
                Got::Message(Incoming::Text(text)) => {
                    return Some(serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}")));
                }
                Got::Message(Incoming::Ping(_) | Incoming::Pong(_)) => {}
                _ => return None,
            }
        }
    }

    /// The answer to request `id`; every other text message is set aside for later reads.
    pub async fn answer(&mut self, id: u64) -> Value {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        let mut aside = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match self.read(left).await {
                Got::Message(Incoming::Text(text)) => {
                    let value: Value = serde_json::from_str(&text).expect("JSON");
                    if value.get("id").and_then(Value::as_u64) == Some(id) {
                        self.backlog.extend(aside);
                        return value;
                    }
                    aside.push(Incoming::Text(text));
                }
                Got::Message(Incoming::Ping(_) | Incoming::Pong(_)) => {}
                other => panic!("no answer to request {id}: {other:?}"),
            }
        }
    }

    /// Sends a command and waits for its answer (`{"id":..,"ok":..}` or `{"id":..,"err":..}`).
    pub async fn call(&mut self, cmd: &str, args: &str) -> Value {
        let id = self.command(cmd, args).await;
        self.answer(id).await
    }

    /// Sends a command that must succeed and returns its result.
    pub async fn ok(&mut self, cmd: &str, args: &str) -> Value {
        let answer = self.call(cmd, args).await;
        assert!(answer.get("ok").is_some(), "{cmd} failed: {answer}");
        answer["ok"].clone()
    }

    /// The next event named `name` whose payload `wanted` accepts; other messages are set aside for later reads.
    pub async fn event_where(&mut self, name: &str, within: Duration, wanted: impl Fn(&Value) -> bool) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + within;
        let mut aside = Vec::new();
        let found = loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match self.read(left).await {
                Got::Message(Incoming::Text(text)) => {
                    let value: Value = serde_json::from_str(&text).expect("JSON");
                    if value.get("event").and_then(Value::as_str) == Some(name) && wanted(&value["payload"]) {
                        break Some(value);
                    }
                    aside.push(Incoming::Text(text));
                }
                Got::Message(Incoming::Ping(_) | Incoming::Pong(_)) => {}
                Got::Message(close @ Incoming::Close(..)) => {
                    aside.push(close);
                    break None;
                }
                _ => break None,
            }
        };
        for message in aside.into_iter().rev() {
            self.backlog.push_front(message);
        }
        found
    }

    /// The next event named `name`.
    pub async fn next_event(&mut self, name: &str, within: Duration) -> Option<Value> {
        self.event_where(name, within, |_| true).await
    }

    /// Reads until the close message and returns its code and reason; the code is `None` when the connection ended without one.
    pub async fn close_code(&mut self, within: Duration) -> Option<(u16, String)> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match self.read(left).await {
                Got::Message(Incoming::Close(Some(code), reason)) => return Some((code, reason)),
                Got::Message(Incoming::Close(None, _)) | Got::Ended | Got::Quiet => return None,
                Got::Message(_) => {}
            }
        }
    }

    /// Reads to the end of the connection; true when the server closed it within `within`.
    pub async fn ends_within(&mut self, within: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match self.read(left).await {
                Got::Ended => return true,
                Got::Quiet => return false,
                Got::Message(_) => {}
            }
        }
    }
}

/// The pane the layout of a view focuses.
pub fn focus(view: &Value) -> u64 {
    view["focus"].as_u64().expect("the view names the focused pane")
}

/// The server key the test application uses, for tests that build more of the stack.
pub fn key() -> holzwave_server::keyfile::ServerKey {
    edge::server_key()
}

// ---------------------------------------------------------------------------------------------------------------------
// A socket without a network
// ---------------------------------------------------------------------------------------------------------------------

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use holzwave_server::auth::session::Ended;
use holzwave_server::http::transport::{Inbound as In, Outgoing, Transport, TransportError};
use holzwave_server::http::ws::{Attached, PortError, SessionPort, Wiring, serve_socket};
use holzwave_server::limits::Limits;
use holzwave_server::config::Sessions;
use holzwave_server::store::{AccountId, Session};
use tokio::sync::{broadcast, mpsc};

/// What a [`FakeTransport`] was asked to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sent {
    Text(String),
    Binary(usize),
    Ping,
    Close(u16, String),
}

/// A transport the test drives by hand: frames go in through a channel, writes are recorded, and a write can be made to never
/// finish (a peer that does not read).
pub struct FakeTransport {
    inbound: mpsc::UnboundedReceiver<Result<In, TransportError>>,
    log: Arc<Mutex<Vec<Sent>>>,
    stall: Arc<AtomicBool>,
    stall_ping: Arc<AtomicBool>,
}

impl Transport for FakeTransport {
    async fn recv(&mut self) -> Result<In, TransportError> {
        match self.inbound.recv().await {
            Some(frame) => frame,
            None => Err(TransportError::Closed),
        }
    }

    async fn send(&mut self, message: Outgoing) -> Result<(), TransportError> {
        if self.stall.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        let sent = match message {
            Outgoing::Text(text) => Sent::Text(text),
            Outgoing::Binary(bytes) => Sent::Binary(bytes.len()),
        };
        self.log.lock().expect("lock").push(sent);
        Ok(())
    }

    async fn ping(&mut self) -> Result<(), TransportError> {
        if self.stall_ping.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        self.log.lock().expect("lock").push(Sent::Ping);
        Ok(())
    }

    async fn close(&mut self, code: u16, reason: &'static str) {
        self.log.lock().expect("lock").push(Sent::Close(code, reason.to_owned()));
    }
}

/// A [`SessionPort`] the test sets: what the checks answer and whether the store is locked.
pub struct FakePort {
    pub check_answer: Mutex<Result<(), PortError>>,
    pub touch_answer: Mutex<Result<(), PortError>>,
    pub locked: AtomicBool,
    pub checks: AtomicUsize,
    pub touches: AtomicUsize,
    pub ended: broadcast::Sender<Ended>,
}

impl FakePort {
    pub fn new() -> Arc<FakePort> {
        Arc::new(FakePort {
            check_answer: Mutex::new(Ok(())),
            touch_answer: Mutex::new(Ok(())),
            locked: AtomicBool::new(false),
            checks: AtomicUsize::new(0),
            touches: AtomicUsize::new(0),
            ended: broadcast::channel(4).0,
        })
    }
}

/// The port as the loop holds it: a clone of the test's handle.
pub struct PortHandle(pub Arc<FakePort>);

impl SessionPort for PortHandle {
    async fn check(&self, session: &Session, _now: i64) -> Result<Session, PortError> {
        self.0.checks.fetch_add(1, Ordering::SeqCst);
        (*self.0.check_answer.lock().expect("lock")).map(|()| session.clone())
    }

    async fn touch(&self, session: &Session, _now: i64) -> Result<Session, PortError> {
        self.0.touches.fetch_add(1, Ordering::SeqCst);
        (*self.0.touch_answer.lock().expect("lock")).map(|()| session.clone())
    }

    async fn locked(&self, _account: AccountId) -> bool {
        self.0.locked.load(Ordering::SeqCst)
    }

    fn ended(&self) -> broadcast::Receiver<Ended> {
        self.0.ended.subscribe()
    }
}

/// A socket loop on a fake transport, attached to a real worker of the environment.
pub struct Rig {
    pub frames: mpsc::UnboundedSender<Result<In, TransportError>>,
    pub log: Arc<Mutex<Vec<Sent>>>,
    pub stall: Arc<AtomicBool>,
    pub stall_ping: Arc<AtomicBool>,
    pub port: Arc<FakePort>,
    pub shutdown: Shutdown,
    pub task: JoinHandle<()>,
}

impl Rig {
    /// Attaches a socket of `person` and serves it over a fake transport with the settings of the limits in force, changed by
    /// `tune`.
    pub async fn start(env: &WsEnv, person: &Person, tune: fn(&mut SocketSettings)) -> Rig {
        let mut settings = SocketSettings::from_limits(&Limits::DEFAULT, Sessions::default());
        tune(&mut settings);
        let (handle, rx) = env.supervisor.attach(person.key).await.expect("attach");
        let (frames, inbound) = mpsc::unbounded_channel();
        let log = Arc::new(Mutex::new(Vec::new()));
        let stall = Arc::new(AtomicBool::new(false));
        let stall_ping = Arc::new(AtomicBool::new(false));
        let transport = FakeTransport { inbound, log: log.clone(), stall: stall.clone(), stall_ping: stall_ping.clone() };
        let port = FakePort::new();
        let shutdown = Shutdown::new();
        let clock = env.test.app.clock.clone();
        let wiring = Wiring { port: PortHandle(port.clone()), clock, settings, shutdown: shutdown.clone() };
        let session = person.signed.session.clone();
        let task = tokio::spawn(serve_socket(transport, session, Attached { handle, rx }, wiring));
        // The loop listens for the end of sessions as soon as it runs; a test that announces one waits for that.
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while port.ended.receiver_count() == 0 {
            assert!(tokio::time::Instant::now() < deadline, "the loop did not start");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Rig { frames, log, stall, stall_ping, port, shutdown, task }
    }

    /// Hands the loop a frame as if the peer had sent it.
    pub fn feed(&self, frame: Result<In, TransportError>) {
        let _ = self.frames.send(frame);
    }

    pub fn text(&self, text: &str) {
        self.feed(Ok(In::Text(text.to_owned())));
    }

    /// Everything written so far.
    pub fn sent(&self) -> Vec<Sent> {
        self.log.lock().expect("lock").clone()
    }

    /// The close message the loop wrote, if it wrote one.
    pub fn closed_with(&self) -> Option<(u16, String)> {
        self.sent().into_iter().find_map(|s| if let Sent::Close(code, reason) = s { Some((code, reason)) } else { None })
    }

    /// Waits until the loop has ended.
    pub async fn finished(&mut self, within: Duration) -> bool {
        tokio::time::timeout(within, &mut self.task).await.is_ok()
    }

    /// Waits until `check` holds for what was written.
    pub async fn wait_sent(&self, what: &str, within: Duration, check: impl Fn(&[Sent]) -> bool) {
        let deadline = tokio::time::Instant::now() + within;
        while !check(&self.sent()) {
            assert!(tokio::time::Instant::now() < deadline, "{what} was not written in time; wrote {:?}", self.sent());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// The `hw-session` states written so far.
    pub fn session_states(&self) -> Vec<String> {
        self.sent()
            .into_iter()
            .filter_map(|s| match s {
                Sent::Text(t) => serde_json::from_str::<Value>(&t).ok().filter(|v| v["event"] == "hw-session"),
                _ => None,
            })
            .map(|v| v["payload"]["state"].as_str().unwrap_or_default().to_owned())
            .collect()
    }
}
