//! Measurement of `permessage-deflate` on the traffic a terminal page receives.
//!
//! What is here: the corpus reader, a stream wrapper that counts the bytes a socket really sends, a server on the
//! WebSocket stack of the web server without compression (the baseline every result is compared with), and the
//! results file the verdict is computed from. The compression candidate and the bomb tests build on the same parts.
//! Every listener binds 127.0.0.1 only.

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
