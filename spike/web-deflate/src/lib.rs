//! Measurement of `permessage-deflate` on the traffic a terminal page receives.
//!
//! What is here: the corpus reader, a stream wrapper that counts the bytes a socket really sends, a server on the
//! WebSocket stack of the web server without compression (the baseline every result is compared with), and the
//! results file the verdict is computed from. The compression candidate and the bomb tests build on the same parts.
//! Every listener binds 127.0.0.1 only.
//!
//! The candidate is `yawc` with `permessage-deflate` on, behind the same counting listener and the same message cap.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// The largest message either server accepts, in bytes. The same cap on both sides keeps the comparison fair.
pub const MESSAGE_CAP: usize = 8 * 1024 * 1024;

/// The recorded frame traffic.
pub mod corpus {
    use std::path::{Path, PathBuf};

    /// The four workloads, in the order of the results table.
    pub const WORKLOADS: [&str; 4] = ["typing", "listing", "cat-large", "redraw"];

    /// The folder the corpus files are in. It is not committed.
    pub fn dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus")
    }

    /// The path of the corpus file of one workload.
    pub fn path(workload: &str) -> PathBuf {
        dir().join(format!("{workload}.ndjson"))
    }

    /// The command (run from the repository root) that creates the file of one workload.
    pub fn command(workload: &str) -> String {
        format!(
            "cargo run -p holzwave-spike --bin record_frames -- {workload} spike/web-deflate/corpus/{workload}.ndjson"
        )
    }

    /// The lines of a corpus file: one JSON text per line. A missing or empty file is an error that names the
    /// command that creates it.
    pub fn read(path: &Path) -> Result<Vec<String>, String> {
        let workload = path.file_stem().and_then(|s| s.to_str()).unwrap_or("<workload>");
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("{}: {e}; create it from the repository root with: {}", path.display(), command(workload)))?;
        let lines: Vec<String> = text.lines().map(str::to_owned).collect();
        if lines.is_empty() {
            return Err(format!("{} is empty; recreate it with: {}", path.display(), command(workload)));
        }
        Ok(lines)
    }
}

/// Byte counters shared between a listener and the test that reads them.
#[derive(Debug, Default)]
pub struct Counters {
    written: AtomicU64,
    read: AtomicU64,
    mark: AtomicU64,
    panics: AtomicU64,
}

impl Counters {
    /// Bytes the server's sockets sent, the handshake included.
    pub fn bytes_written(&self) -> u64 {
        self.written.load(Ordering::SeqCst)
    }

    /// Bytes the server's sockets received.
    pub fn bytes_read(&self) -> u64 {
        self.read.load(Ordering::SeqCst)
    }

    /// How many connection tasks of the server ended in a panic.
    pub fn panics(&self) -> u64 {
        self.panics.load(Ordering::SeqCst)
    }

    /// Remember how much has been sent so far: the handshake is over and the messages start.
    pub fn mark(&self) {
        self.mark.store(self.bytes_written(), Ordering::SeqCst);
    }

    /// Bytes sent up to the mark: the handshake response.
    pub fn handshake_bytes(&self) -> u64 {
        self.mark.load(Ordering::SeqCst)
    }

    /// Bytes sent since the mark: what the messages cost on the wire.
    pub fn message_bytes(&self) -> u64 {
        self.bytes_written().saturating_sub(self.handshake_bytes())
    }
}

/// An async stream that counts the bytes written and read through it.
pub struct CountingStream<S> {
    inner: S,
    counters: Arc<Counters>,
}

impl<S> CountingStream<S> {
    pub fn new(inner: S, counters: Arc<Counters>) -> Self {
        Self { inner, counters }
    }

    pub fn counters(&self) -> &Arc<Counters> {
        &self.counters
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for CountingStream<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &polled {
            self.counters.read.fetch_add((buf.filled().len() - before) as u64, Ordering::SeqCst);
        }
        polled
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for CountingStream<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let polled = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &polled {
            self.counters.written.fetch_add(*n as u64, Ordering::SeqCst);
        }
        polled
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let polled = Pin::new(&mut self.inner).poll_write_vectored(cx, bufs);
        if let Poll::Ready(Ok(n)) = &polled {
            self.counters.written.fetch_add(*n as u64, Ordering::SeqCst);
        }
        polled
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// A TCP listener whose accepted sockets count their bytes into one [`Counters`].
pub struct CountingListener {
    inner: TcpListener,
    counters: Arc<Counters>,
}

impl CountingListener {
    /// Bind `addr`, which must be a loopback address: nothing in this project listens on a public one.
    pub async fn bind(addr: SocketAddr, counters: Arc<Counters>) -> io::Result<Self> {
        if !addr.ip().is_loopback() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "only a loopback address may be bound"));
        }
        Ok(Self { inner: TcpListener::bind(addr).await?, counters })
    }
}

impl axum::serve::Listener for CountingListener {
    type Io = CountingStream<tokio::net::TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.inner.accept().await {
                Ok((stream, addr)) => return (CountingStream::new(stream, self.counters.clone()), addr),
                // A failed accept (descriptor limit, aborted connection) is not the end of the listener.
                Err(_) => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

/// A running server on a loopback port. Dropping it stops it.
pub struct Server {
    pub addr: SocketAddr,
    pub counters: Arc<Counters>,
    task: JoinHandle<()>,
}

impl Server {
    /// Serve `router` on `addr` (loopback only) behind a counting listener.
    pub async fn start(addr: SocketAddr, counters: Arc<Counters>, router: Router) -> io::Result<Server> {
        let listener = CountingListener::bind(addr, counters.clone()).await?;
        let addr = axum::serve::Listener::local_addr(&listener)?;
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Server { addr, counters, task })
    }

    /// The `ws://` URL of the WebSocket endpoint.
    pub fn url(&self) -> String {
        format!("ws://{}/ws", self.addr)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Shared state of the corpus servers.
#[derive(Clone)]
struct Corpus {
    lines: Arc<Vec<String>>,
    counters: Arc<Counters>,
}

/// Send every line as one text message when the client says `go`, then close. The first message the client sends is
/// the signal that the handshake is over: the counter is marked there, so the handshake is not counted as traffic.
async fn send_corpus(mut socket: WebSocket, corpus: Corpus) {
    match socket.recv().await {
        Some(Ok(Message::Text(_))) => {}
        _ => return,
    }
    corpus.counters.mark();
    for line in corpus.lines.iter() {
        if socket.send(Message::Text(line.clone().into())).await.is_err() {
            return;
        }
    }
    if socket.send(Message::Close(None)).await.is_err() {
        return;
    }
    // Wait for the client's close, so that the socket is not dropped under a client that still reads.
    while let Some(Ok(_)) = socket.recv().await {}
}

async fn baseline_ws(State(corpus): State<Corpus>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(MESSAGE_CAP)
        .max_frame_size(MESSAGE_CAP)
        .on_upgrade(move |socket| send_corpus(socket, corpus))
}

/// The baseline: the WebSocket stack of the web server without compression, serving `lines` on connect.
pub async fn serve_baseline(addr: SocketAddr, lines: Vec<String>) -> io::Result<Server> {
    let counters = Arc::new(Counters::default());
    let corpus = Corpus { lines: Arc::new(lines), counters: counters.clone() };
    let router = Router::new().route("/ws", get(baseline_ws)).with_state(corpus);
    Server::start(addr, counters, router).await
}

/// The candidate: `yawc` with `permessage-deflate`.
pub mod candidate {
    use super::*;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use futures_util::SinkExt;
    use yawc::close::CloseCode;
    use yawc::{Frame, HttpWebSocket, IncomingUpgrade, OpCode, Options};

    /// The options of the candidate: balanced compression (context takeover on, as negotiated by default), UTF-8
    /// validation of text, and the message cap for one frame and for one reassembled message.
    pub fn options() -> Options {
        Options::default().with_balanced_compression().with_limits(MESSAGE_CAP, MESSAGE_CAP).with_utf8()
    }

    #[derive(Clone)]
    struct State {
        corpus: Corpus,
        options: Options,
    }

    /// Run a connection task and count it when it panics, so that a hostile peer that makes the library panic is seen.
    fn spawn_counted(counters: Arc<Counters>, task: impl std::future::Future<Output = ()> + Send + 'static) {
        let handle = tokio::spawn(task);
        tokio::spawn(async move {
            if let Err(e) = handle.await {
                if e.is_panic() {
                    counters.panics.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
    }

    async fn corpus_session(mut ws: HttpWebSocket, corpus: Corpus) {
        match ws.next_frame().await {
            Ok(frame) if frame.opcode() == OpCode::Text => {}
            _ => return,
        }
        corpus.counters.mark();
        for line in corpus.lines.iter() {
            if ws.send(Frame::text(line.clone())).await.is_err() {
                return;
            }
        }
        if ws.send(Frame::close(CloseCode::Normal, b"")).await.is_err() {
            return;
        }
        while ws.next_frame().await.is_ok() {}
        let _ = ws.next_frame().await;
    }

    /// Send every message back to the peer as it came (text as text, binary as binary).
    ///
    /// The library queues what it owes the peer (the pong for a ping, the reply to a close, the close frame that
    /// names a protocol error or an oversize message) and writes it on the next read poll. A loop that stops at the
    /// first error, or at the peer's close, therefore leaves those frames unsent: after an error or a close the
    /// connection is polled once more, which writes them.
    async fn echo_session(mut ws: HttpWebSocket) {
        loop {
            match ws.next_frame().await {
                Ok(frame) => match frame.opcode() {
                    OpCode::Text => {
                        if ws.send(Frame::text(frame.into_payload())).await.is_err() {
                            return;
                        }
                    }
                    OpCode::Binary => {
                        if ws.send(Frame::binary(frame.into_payload())).await.is_err() {
                            return;
                        }
                    }
                    _ => {}
                },
                Err(e) => return fail_connection(ws, e).await,
            }
        }
    }

    /// End a connection after a read error the way RFC 6455 section 7.1.7 asks: the close frame that names the error
    /// goes out, then the connection is dropped. The library queues that frame itself for most errors (oversize
    /// message, bad opcode, bad reserved bits, bad fragmentation) and writes it on the next poll, but not for invalid
    /// UTF-8, which it reports without a close frame; the application sends status 1007 for that one. The wait for
    /// the write is bounded so that a peer that stalls cannot hold the task.
    async fn fail_connection(mut ws: HttpWebSocket, error: yawc::WebSocketError) {
        if matches!(error, yawc::WebSocketError::InvalidUTF8) {
            let _ = ws.send(Frame::close(CloseCode::Invalid, b"")).await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), ws.next_frame()).await;
    }

    async fn corpus_ws(axum::extract::State(state): axum::extract::State<State>, ws: IncomingUpgrade) -> axum::response::Response {
        let Ok((response, upgrade)) = ws.upgrade(state.options.clone()) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let corpus = state.corpus.clone();
        spawn_counted(state.corpus.counters.clone(), async move {
            if let Ok(socket) = upgrade.await {
                corpus_session(socket, corpus).await;
            }
        });
        response.into_response()
    }

    async fn echo_ws(axum::extract::State(state): axum::extract::State<State>, ws: IncomingUpgrade) -> axum::response::Response {
        let Ok((response, upgrade)) = ws.upgrade(state.options.clone()) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        spawn_counted(state.corpus.counters.clone(), async move {
            if let Ok(socket) = upgrade.await {
                echo_session(socket).await;
            }
        });
        response.into_response()
    }

    /// The candidate: `/ws` sends `lines` when the client says `go`; `/` and `/echo` echo every message; `/blank` is an empty page.
    pub async fn serve_candidate(addr: SocketAddr, lines: Vec<String>, options: Options) -> io::Result<Server> {
        let counters = Arc::new(Counters::default());
        let corpus = Corpus { lines: Arc::new(lines), counters: counters.clone() };
        let router = Router::new()
            .route("/ws", get(corpus_ws))
            .route("/", get(echo_ws))
            .route("/echo", get(echo_ws))
            // A blank page for the browser check: Chromium blocks a WebSocket to a local address from about:blank
            // and from a page that the test script fulfils itself, and allows it from a page that loopback served.
            .route("/blank", get(|| async { axum::response::Html("<!doctype html><title>blank</title>") }))
            .with_state(State { corpus, options });
        Server::start(addr, counters, router).await
    }
}

/// A WebSocket client written by hand over a TCP stream, for the tests that need frames no library client sends: a
/// hostile compressed message, fragments of chosen sizes, and a look at the handshake response.
pub mod raw {
    use std::io;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    /// What the server answered to the handshake.
    #[derive(Debug)]
    pub struct Handshake {
        pub status_line: String,
        /// The value of `Sec-WebSocket-Extensions`, if the server sent one.
        pub extensions: Option<String>,
    }

    /// A connected raw client.
    pub struct Client {
        stream: TcpStream,
        buf: Vec<u8>,
    }

    /// What the client saw after it sent something.
    #[derive(Debug, PartialEq, Eq)]
    pub enum Outcome {
        /// A data message of this many bytes came back (text or binary).
        Message(usize),
        /// A close frame with this status code (`None` for a close without a code).
        Close(Option<u16>),
        /// The server dropped the connection without a close frame.
        Dropped,
    }

    impl Client {
        /// Connect and perform the opening handshake, offering `permessage-deflate` when `offer` is set.
        pub async fn connect(addr: std::net::SocketAddr, path: &str, offer: Option<&str>) -> io::Result<(Client, Handshake)> {
            let mut stream = TcpStream::connect(addr).await?;
            let mut request = format!(
                "GET {path} HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
                 Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n"
            );
            if let Some(offer) = offer {
                request.push_str(&format!("Sec-WebSocket-Extensions: {offer}\r\n"));
            }
            request.push_str("\r\n");
            stream.write_all(request.as_bytes()).await?;
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).await? == 0 {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the server closed during the handshake"));
                }
                head.push(byte[0]);
                if head.len() > 16 * 1024 {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "the handshake response is too long"));
                }
            }
            let text = String::from_utf8_lossy(&head).into_owned();
            let mut lines = text.lines();
            let status_line = lines.next().unwrap_or_default().to_owned();
            let extensions = lines
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| k.eq_ignore_ascii_case("sec-websocket-extensions"))
                .map(|(_, v)| v.trim().to_owned());
            Ok((Client { stream, buf: Vec::new() }, Handshake { status_line, extensions }))
        }

        /// Send one frame. `rsv1` marks a compressed message (its first frame). The payload is masked with a fixed key.
        pub async fn send_frame(&mut self, opcode: u8, fin: bool, rsv1: bool, payload: &[u8]) -> io::Result<()> {
            let key = [0x12u8, 0x34, 0x56, 0x78];
            let mut frame = Vec::with_capacity(payload.len() + 14);
            frame.push(u8::from(fin) << 7 | u8::from(rsv1) << 6 | opcode);
            match payload.len() {
                n if n < 126 => frame.push(0x80 | n as u8),
                n if n <= 0xffff => {
                    frame.push(0x80 | 126);
                    frame.extend_from_slice(&(n as u16).to_be_bytes());
                }
                n => {
                    frame.push(0x80 | 127);
                    frame.extend_from_slice(&(n as u64).to_be_bytes());
                }
            }
            frame.extend_from_slice(&key);
            frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
            self.stream.write_all(&frame).await
        }

        async fn fill(&mut self, wanted: usize) -> io::Result<bool> {
            let mut chunk = [0u8; 64 * 1024];
            while self.buf.len() < wanted {
                let n = match self.stream.read(&mut chunk).await {
                    Ok(0) => return Ok(false),
                    Ok(n) => n,
                    // A reset is the server dropping the connection.
                    Err(e) if e.kind() == io::ErrorKind::ConnectionReset => return Ok(false),
                    Err(e) => return Err(e),
                };
                self.buf.extend_from_slice(&chunk[..n]);
            }
            Ok(true)
        }

        /// Read the next frame from the server: `(opcode, fin, rsv1, payload)`; `None` when the connection ended.
        pub async fn read_frame(&mut self) -> io::Result<Option<(u8, bool, bool, Vec<u8>)>> {
            if !self.fill(2).await? {
                return Ok(None);
            }
            let (b0, b1) = (self.buf[0], self.buf[1]);
            let (len, header) = match b1 & 0x7f {
                126 => {
                    if !self.fill(4).await? {
                        return Ok(None);
                    }
                    (usize::from(u16::from_be_bytes([self.buf[2], self.buf[3]])), 4)
                }
                127 => {
                    if !self.fill(10).await? {
                        return Ok(None);
                    }
                    let mut eight = [0u8; 8];
                    eight.copy_from_slice(&self.buf[2..10]);
                    (u64::from_be_bytes(eight) as usize, 10)
                }
                n => (usize::from(n), 2),
            };
            if !self.fill(header + len).await? {
                return Ok(None);
            }
            let payload = self.buf[header..header + len].to_vec();
            self.buf.drain(..header + len);
            Ok(Some((b0 & 0x0f, b0 & 0x80 != 0, b0 & 0x40 != 0, payload)))
        }

        /// Read until the server answers with a data message, a close, or drops the connection.
        pub async fn outcome(&mut self) -> io::Result<Outcome> {
            loop {
                match self.read_frame().await? {
                    None => return Ok(Outcome::Dropped),
                    Some((0x1 | 0x2, _, _, payload)) => return Ok(Outcome::Message(payload.len())),
                    Some((0x8, _, _, payload)) => {
                        let code = (payload.len() >= 2).then(|| u16::from_be_bytes([payload[0], payload[1]]));
                        return Ok(Outcome::Close(code));
                    }
                    Some(_) => {}
                }
            }
        }
    }
}

/// The results file the verdict is computed from.
pub mod results {
    use super::PathBuf;
    use serde_json::{Map, Value};
    use std::sync::Mutex;

    static LOCK: Mutex<()> = Mutex::new(());

    /// `results.json` next to the manifest. It is not committed.
    pub fn path() -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("results.json")
    }

    /// The whole file; an object, empty when the file does not exist yet.
    pub fn load() -> Value {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| Value::Object(Map::new()))
    }

    /// Set `section.key` to `value` and write the file again, through a temporary file so that a reader never sees a
    /// half-written one.
    pub fn set(section: &str, key: &str, value: Value) {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = load();
        let root = all.as_object_mut().expect("an object");
        let entry = root.entry(section).or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        entry.as_object_mut().expect("an object").insert(key.to_owned(), value);
        let tmp = path().with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&all).expect("json")).expect("write results");
        std::fs::rename(&tmp, path()).expect("replace results");
    }
}

/// The decision, as a pure function of the results file.
pub mod verdict {
    use serde_json::Value;

    /// Fewest percent saved on the frame workloads.
    pub const MIN_SAVED_FRAMES: f64 = 30.0;
    /// Fewest percent saved on the large-text workload.
    pub const MIN_SAVED_LARGE_TEXT: f64 = 50.0;
    /// The workloads that count as frame traffic; the large-text workload is `cat-large`.
    pub const FRAME_WORKLOADS: [&str; 3] = ["typing", "listing", "redraw"];

    /// The state of one criterion.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum State {
        Met,
        NotMet,
        NotRun,
    }

    #[derive(Debug, Clone)]
    pub struct Criterion {
        pub name: &'static str,
        pub state: State,
        pub detail: String,
    }

    #[derive(Debug, Clone)]
    pub struct Verdict {
        pub criteria: Vec<Criterion>,
    }

    impl Verdict {
        /// `pass` only when every criterion was measured and held.
        pub fn pass(&self) -> bool {
            self.criteria.iter().all(|c| c.state == State::Met)
        }

        /// The text of the `VERDICT` file: `pass` or `fail`, then one line per criterion.
        pub fn text(&self) -> String {
            let mut out = String::from(if self.pass() { "pass\n" } else { "fail\n" });
            for c in &self.criteria {
                let state = match c.state {
                    State::Met => "met",
                    State::NotMet => "NOT MET",
                    State::NotRun => "NOT RUN",
                };
                out.push_str(&format!("{}: {}: {}\n", c.name, state, c.detail));
            }
            out
        }
    }

    fn saved(results: &Value, workload: &str) -> Option<f64> {
        results["candidate"][workload]["saved_percent"].as_f64()
    }

    fn savings(results: &Value, name: &'static str, workloads: &[&str], least: f64) -> Criterion {
        let mut parts = Vec::new();
        let mut state = State::Met;
        for w in workloads {
            match saved(results, w) {
                None => {
                    state = State::NotRun;
                    parts.push(format!("{w} not measured"));
                }
                Some(p) => {
                    if p < least && state == State::Met {
                        state = State::NotMet;
                    }
                    parts.push(format!("{w} {p:.1}%"));
                }
            }
        }
        Criterion { name, state, detail: format!("{} (needs at least {least:.0}% each)", parts.join(", ")) }
    }

    fn bomb(results: &Value) -> Criterion {
        let name = "bomb refused within the memory bound";
        let mut parts = Vec::new();
        let mut state = State::Met;
        for case in ["single", "fragments"] {
            let b = &results["bomb"][case];
            if !b.is_object() {
                state = State::NotRun;
                parts.push(format!("{case} not run"));
                continue;
            }
            let refused = b["refused"].as_bool().unwrap_or(false);
            let within = b["within_bound"].as_bool().unwrap_or(false);
            let panics = b["panics"].as_u64().unwrap_or(1);
            if (!refused || !within || panics != 0) && state == State::Met {
                state = State::NotMet;
            }
            parts.push(format!(
                "{case}: {} , peak {} bytes of {} allowed, {panics} panics",
                b["outcome"].as_str().unwrap_or("unknown"),
                b["peak_bytes"],
                b["bound_bytes"]
            ));
        }
        Criterion { name, state, detail: parts.join("; ") }
    }

    fn autobahn(results: &Value) -> Criterion {
        let name = "no failed Autobahn case";
        let a = &results["autobahn"];
        if a["ran"].as_bool() != Some(true) {
            let why = a["reason"].as_str().unwrap_or("it was not run");
            return Criterion { name, state: State::NotRun, detail: format!("NOT RUN: {why}") };
        }
        let failed = a["failed"].as_u64().unwrap_or(u64::MAX);
        let cases = a["cases"].as_u64().unwrap_or(0);
        let state = if failed == 0 && cases > 0 { State::Met } else { State::NotMet };
        let listed: Vec<&str> = a["failed_cases"].as_array().map(|v| v.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        let shown = listed.iter().take(3).copied().collect::<Vec<_>>().join(", ");
        let those = match listed.len() {
            0 => String::new(),
            n if n <= 3 => format!("; those cases: {shown}"),
            n => format!("; those cases: {shown} and {} more", n - 3),
        };
        Criterion {
            name,
            state,
            detail: format!(
                "{cases} cases, {} with behaviour FAILED, {failed} outside OK, NON-STRICT and INFORMATIONAL; behaviours {}{those}",
                a["counts"]["FAILED"].as_u64().unwrap_or(0),
                a["counts"]
            ),
        }
    }

    fn browsers(results: &Value) -> Criterion {
        let name = "both browsers negotiate and round-trip";
        let mut parts = Vec::new();
        let mut state = State::Met;
        for engine in ["chromium", "webkit"] {
            let b = &results["browsers"][engine];
            if b["ran"].as_bool() != Some(true) {
                state = State::NotRun;
                parts.push(format!("{engine} NOT RUN: {}", b["reason"].as_str().unwrap_or("it was not run")));
                continue;
            }
            let ext = b["extensions"].as_str().unwrap_or("");
            let ok = ext.contains("permessage-deflate") && b["identical"].as_bool() == Some(true);
            if !ok && state == State::Met {
                state = State::NotMet;
            }
            parts.push(format!("{engine} extensions \"{ext}\", identical {}", b["identical"]));
        }
        Criterion { name, state, detail: parts.join("; ") }
    }

    /// Decide from the results file. A criterion that has no data counts as not run, and not run is not met.
    pub fn verdict(results: &Value) -> Verdict {
        Verdict {
            criteria: vec![
                savings(results, "frame traffic saved", &FRAME_WORKLOADS, MIN_SAVED_FRAMES),
                savings(results, "large text saved", &["cat-large"], MIN_SAVED_LARGE_TEXT),
                bomb(results),
                autobahn(results),
                browsers(results),
            ],
        }
    }
}
