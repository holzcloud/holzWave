//! A real listener for the tests of the page and the accept loop: a loopback port, generated certificates, the router of the
//! application with the embedded page, and clients that speak HTTP/1.1 over TLS. Later server tests reuse it with
//! `#[path = "support/edge_tls.rs"] mod edge_tls;` next to `edge.rs`.
//!
//! The raw client writes the request bytes itself and reads the answer back with a small parser, so a test can send what a
//! well-behaved library would refuse to build (a backslash in a path, a header a byte at a time).
#![allow(dead_code)]

#[path = "scratch.rs"]
mod scratch;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use holzwave_server::config::{Cidr, Sessions};
use holzwave_server::http::app::{App, AppInit, Edge, PublicOrigin, Shutdown, Timing, serve};
use holzwave_server::http::embed;
use holzwave_server::http::router::{self, Part};
use holzwave_server::limits::Limits;
use holzwave_server::proxy::{Admission, Mode, TrustedProxies};
use holzwave_server::tls::{self, FileResolver};
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use scratch::Scratch;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use super::edge::{self, ORIGIN, TestApp};

/// The longest a helper waits for the server before it fails the test.
pub const PATIENCE: Duration = Duration::from_secs(15);

/// How a started listener differs from the default one.
pub struct Opts {
    /// Routes added next to the page (probe routes of a test).
    pub extra: Vec<Part>,
    /// Whether the embedded page is mounted.
    pub page: bool,
    /// The trusted proxies, which also turns the proxy mode on (plain HTTP from them, no TLS).
    pub trusted_proxies: Option<&'static str>,
    /// Other waits than the defaults (short ones, so that a test does not take a minute).
    pub timing: Option<Timing>,
    /// Other caps than the defaults: the total and the number per address.
    pub caps: Option<(u32, u32)>,
}

impl Default for Opts {
    fn default() -> Opts {
        Opts { extra: Vec::new(), page: true, trusted_proxies: None, timing: None, caps: None }
    }
}

/// Waits that are short enough for a test and still far apart: 0.7 s, 0.5 s, 1.5 s, 1.5 s.
pub fn quick() -> Timing {
    Timing {
        handshake: Duration::from_millis(700),
        header: Duration::from_millis(500),
        idle: Duration::from_millis(1500),
        grace: Duration::from_millis(1500),
        h2_ping_interval: Duration::from_millis(400),
        h2_ping_timeout: Duration::from_millis(400),
    }
}

/// A running listener and everything a test needs to talk to it.
pub struct Started {
    pub addr: SocketAddr,
    pub shutdown: Shutdown,
    pub admission: Admission,
    pub test: TestApp,
    pub app: Arc<App>,
    pub task: Option<JoinHandle<()>>,
    der: CertificateDer<'static>,
    _scratch: Scratch,
}

impl Drop for Started {
    fn drop(&mut self) {
        self.shutdown.trigger();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// The folder the page was built into.
pub fn dist_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../app/dist")
}

/// The bytes of a built file.
pub fn dist(name: &str) -> Vec<u8> {
    std::fs::read(dist_dir().join(name)).unwrap_or_else(|e| panic!("app/dist/{name} cannot be read ({e}); build the page first"))
}

/// Starts the default listener: files mode, TLS 1.3, the embedded page, `alice` known to the test application.
pub async fn start() -> Started {
    start_with(Opts::default()).await
}

/// Starts a listener with the given options.
pub async fn start_with(opts: Opts) -> Started {
    let test = TestApp::new();
    let proxies = match opts.trusted_proxies {
        Some(list) => TrustedProxies::new(&[Cidr::parse(list).expect("a network")]).expect("proxies"),
        None => TrustedProxies::none(),
    };
    let mode = if opts.trusted_proxies.is_some() { Mode::Proxy } else { Mode::Files };
    let app = App::new(
        AppInit {
            origin: PublicOrigin::parse(ORIGIN).expect("the public url"),
            auth: test.app.auth.clone(),
            sessions: Sessions::default(),
            limits: Limits::DEFAULT,
            mode,
            proxies: proxies.clone(),
            store: test.store.clone(),
            session_manager: test.sessions.clone(),
            audit: test.app.audit.clone(),
            clock: test.app.clock.clone(),
            view: test.app.view.clone(),
        },
        &edge::server_key(),
    );
    let mut parts = if opts.page { embed::parts() } else { Vec::new() };
    parts.extend(opts.extra);
    let router = router::build(app.clone(), parts);

    let scratch = Scratch::new("edge-tls");
    let made = rcgen::generate_simple_self_signed(vec![edge::HOST.to_owned()]).expect("generate a certificate");
    let cert = scratch.write_file("cert.pem", made.cert.pem().as_bytes(), 0o644);
    let key = scratch.write_file("key.pem", made.signing_key.serialize_pem().as_bytes(), 0o600);
    let acceptor = if mode == Mode::Proxy {
        None
    } else {
        let resolver = Arc::new(FileResolver::new(&cert, &key).expect("load the pair"));
        Some(tls::acceptor(Arc::new(tls::server_config(resolver, &tls::ALPN).expect("server configuration"))))
    };
    let admission = match opts.caps {
        Some((total, per_address)) => Admission::new(mode, proxies, total, per_address).expect("admission"),
        None => Admission::with_defaults(mode, proxies).expect("admission"),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local address");
    let shutdown = Shutdown::new();
    let mut edge = Edge::new(router, acceptor, admission.clone(), mode).expect("the edge");
    if let Some(timing) = opts.timing {
        edge = edge.with_timing(timing);
    }
    let task = tokio::spawn(serve(listener, edge, shutdown.clone()));
    Started { addr, shutdown, admission, test, app, task: Some(task), der: made.cert.der().clone(), _scratch: scratch }
}

impl Started {
    /// A client configuration that trusts the generated certificate and nothing else, offering `alpn`.
    pub fn client_config(&self, alpn: &[&[u8]]) -> ClientConfig {
        let mut store = RootCertStore::empty();
        store.add(self.der.clone()).expect("trust the generated certificate");
        let mut config = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("client protocol versions")
            .with_root_certificates(store)
            .with_no_client_auth();
        config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
        config
    }

    /// A TCP connection to the listener.
    pub async fn tcp(&self) -> TcpStream {
        tokio::time::timeout(PATIENCE, TcpStream::connect(self.addr)).await.expect("connect in time").expect("connect")
    }

    /// A TLS connection with the SNI name of the public host, offering `alpn`.
    pub async fn tls(&self, alpn: &[&[u8]]) -> TlsStream<TcpStream> {
        self.try_tls(alpn).await.expect("the TLS handshake")
    }

    /// As [`Started::tls`], and the error when the handshake does not succeed.
    pub async fn try_tls(&self, alpn: &[&[u8]]) -> std::io::Result<TlsStream<TcpStream>> {
        let tcp = self.tcp().await;
        let name = ServerName::try_from(edge::HOST).expect("server name");
        let connector = TlsConnector::from(Arc::new(self.client_config(alpn)));
        tokio::time::timeout(PATIENCE, connector.connect(name, tcp))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "the handshake took too long"))?
    }

    /// A TLS connection for HTTP/1.1.
    pub async fn h1(&self) -> Conn<TlsStream<TcpStream>> {
        Conn::new(self.tls(&[b"http/1.1"]).await)
    }

    /// One request on its own connection, with `Connection: close`; the reply, read to the end.
    pub async fn get(&self, path: &str, cookie: Option<&str>) -> Reply {
        let mut request = Request::get(path).close();
        if let Some(cookie) = cookie {
            request = request.cookie(cookie);
        }
        self.send(request).await
    }

    /// Sends one request on a fresh HTTP/1.1 connection and reads the reply.
    pub async fn send(&self, request: Request) -> Reply {
        let head_only = request.method == "HEAD";
        let mut conn = self.h1().await;
        conn.send(&request.bytes()).await;
        conn.reply(head_only).await.expect("an answer")
    }
}

/// A request written by hand.
#[derive(Clone)]
pub struct Request {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// A request for `target` with the public `Host`.
    pub fn new(method: &str, target: &str) -> Request {
        Request {
            method: method.to_owned(),
            target: target.to_owned(),
            headers: vec![("Host".to_owned(), edge::HOST.to_owned())],
            body: Vec::new(),
        }
    }

    pub fn get(target: &str) -> Request {
        Request::new("GET", target)
    }

    pub fn header(mut self, name: &str, value: &str) -> Request {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// Asks the server to close after this answer.
    pub fn close(self) -> Request {
        self.header("Connection", "close")
    }

    /// Adds the session cookie.
    pub fn cookie(self, value: &str) -> Request {
        self.header("Cookie", &format!("__Host-hw_session={value}"))
    }

    /// The bytes on the wire.
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = format!("{} {} HTTP/1.1\r\n", self.method, self.target).into_bytes();
        for (name, value) in &self.headers {
            out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        if !self.body.is_empty() {
            out.extend_from_slice(format!("Content-Length: {}\r\n", self.body.len()).as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

/// An HTTP/1.1 answer.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    /// The first value of a header (the name in any case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// Every value of a header.
    pub fn all(&self, name: &str) -> Vec<&str> {
        self.headers.iter().filter(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).collect()
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("the body is not json ({e}): {}", self.text()))
    }

    /// The names of the headers, lower case, sorted, without repeats.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.headers.iter().map(|(n, _)| n.to_ascii_lowercase()).collect();
        names.sort();
        names.dedup();
        names
    }
}

/// A connection that sends bytes and reads answers with a buffer, so several answers can follow each other.
pub struct Conn<S> {
    pub stream: S,
    buffer: Vec<u8>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Conn<S> {
    pub fn new(stream: S) -> Conn<S> {
        Conn { stream, buffer: Vec::new() }
    }

    /// Writes bytes.
    pub async fn send(&mut self, bytes: &[u8]) {
        tokio::time::timeout(PATIENCE, self.stream.write_all(bytes)).await.expect("write in time").expect("write");
    }

    /// Reads one answer. `head_only` is true for the answer to a `HEAD` request. `None` when the connection ends before a
    /// status line.
    pub async fn reply(&mut self, head_only: bool) -> Option<Reply> {
        tokio::time::timeout(PATIENCE, self.read_reply(head_only)).await.expect("an answer in time")
    }

    async fn read_reply(&mut self, head_only: bool) -> Option<Reply> {
        loop {
            if let Some(end) = find(&self.buffer, b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&self.buffer[..end]).into_owned();
                let mut lines = head.split("\r\n");
                let status_line = lines.next()?;
                let status: u16 = status_line.split(' ').nth(1)?.parse().ok()?;
                let headers: Vec<(String, String)> = lines
                    .filter_map(|line| line.split_once(':').map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned())))
                    .collect();
                let length = headers
                    .iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, v)| v.parse::<usize>().ok());
                assert!(
                    !headers.iter().any(|(n, v)| n.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked")),
                    "the test client does not read chunked answers"
                );
                let no_body = head_only || status == 304 || status == 204 || (100..200).contains(&status);
                let body_len = if no_body { 0 } else { length.unwrap_or(0) };
                let total = end + 4 + body_len;
                while self.buffer.len() < total {
                    if !self.fill().await {
                        return None;
                    }
                }
                let body = self.buffer[end + 4..total].to_vec();
                self.buffer.drain(..total);
                return Some(Reply { status, headers, body });
            }
            if !self.fill().await {
                return None;
            }
        }
    }

    /// Reads once more into the buffer; false at the end of the stream or on an error.
    async fn fill(&mut self) -> bool {
        let mut chunk = [0u8; 16 * 1024];
        match self.stream.read(&mut chunk).await {
            Ok(0) | Err(_) => false,
            Ok(n) => {
                self.buffer.extend_from_slice(&chunk[..n]);
                true
            }
        }
    }

    /// Everything that is left until the peer closes (empty when it closed at once or the read failed).
    pub async fn rest(&mut self) -> Vec<u8> {
        let _ = tokio::time::timeout(PATIENCE, async {
            while self.fill().await {}
        })
        .await;
        std::mem::take(&mut self.buffer)
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

// ---------------------------------------------------------------------------------------------------------------------
// HTTP/2 by hand
// ---------------------------------------------------------------------------------------------------------------------

pub const DATA: u8 = 0;
pub const HEADERS: u8 = 1;
pub const RST_STREAM: u8 = 3;
pub const SETTINGS: u8 = 4;
pub const PING: u8 = 6;
pub const GOAWAY: u8 = 7;
pub const CONTINUATION: u8 = 9;

pub const END_STREAM: u8 = 0x1;
pub const ACK: u8 = 0x1;
pub const END_HEADERS: u8 = 0x4;

const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// One HTTP/2 frame.
#[derive(Debug, Clone)]
pub struct Frame {
    pub kind: u8,
    pub flags: u8,
    pub stream: u32,
    pub payload: Vec<u8>,
}

impl Frame {
    /// The error code of an `RST_STREAM` or `GOAWAY` frame.
    pub fn error_code(&self) -> Option<u32> {
        match self.kind {
            RST_STREAM if self.payload.len() == 4 => Some(u32::from_be_bytes(self.payload[..4].try_into().ok()?)),
            GOAWAY if self.payload.len() >= 8 => Some(u32::from_be_bytes(self.payload[4..8].try_into().ok()?)),
            _ => None,
        }
    }

    /// The `(identifier, value)` pairs of a `SETTINGS` frame.
    pub fn settings(&self) -> Vec<(u16, u32)> {
        self.payload
            .chunks_exact(6)
            .map(|c| (u16::from_be_bytes([c[0], c[1]]), u32::from_be_bytes([c[2], c[3], c[4], c[5]])))
            .collect()
    }
}

/// The bytes of a frame.
pub fn frame_bytes(kind: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
    let length = payload.len() as u32;
    let mut out = Vec::with_capacity(9 + payload.len());
    out.extend_from_slice(&length.to_be_bytes()[1..]);
    out.push(kind);
    out.push(flags);
    out.extend_from_slice(&stream.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// A prefix-coded integer of HPACK with the given number of prefix bits and the flag bits of the first byte.
fn hpack_integer(out: &mut Vec<u8>, flags: u8, prefix_bits: u8, mut value: usize) {
    let max = (1usize << prefix_bits) - 1;
    if value < max {
        out.push(flags | value as u8);
        return;
    }
    out.push(flags | max as u8);
    value -= max;
    while value >= 128 {
        out.push((value % 128) as u8 | 0x80);
        value /= 128;
    }
    out.push(value as u8);
}

fn hpack_string(out: &mut Vec<u8>, text: &[u8]) {
    hpack_integer(out, 0, 7, text.len());
    out.extend_from_slice(text);
}

/// The header block of a `GET` request for `path` to the public host, without compression: the method and the scheme from
/// the static table, the others as literals. `extra` headers follow as literals with new names.
pub fn request_block(path: &str, extra: &[(&str, &str)]) -> Vec<u8> {
    let mut block = vec![0x82, 0x87];
    hpack_integer(&mut block, 0x00, 4, 4); // :path, name from the static table, not indexed
    hpack_string(&mut block, path.as_bytes());
    hpack_integer(&mut block, 0x00, 4, 1); // :authority
    hpack_string(&mut block, edge::HOST.as_bytes());
    for (name, value) in extra {
        block.push(0x00);
        hpack_string(&mut block, name.as_bytes());
        hpack_string(&mut block, value.as_bytes());
    }
    block
}

/// What a read of the raw connection found.
#[derive(Debug)]
pub enum Read {
    /// A frame.
    Frame(Frame),
    /// Nothing arrived in the time.
    Quiet,
    /// The server closed the connection.
    Ended,
}

/// A raw HTTP/2 connection over TLS: the client side written by hand, so a test can send what a library never would.
pub struct RawH2 {
    reader: tokio::io::ReadHalf<TlsStream<TcpStream>>,
    writer: tokio::io::WriteHalf<TlsStream<TcpStream>>,
    buffer: Vec<u8>,
}

impl RawH2 {
    /// Connects with the ALPN `h2` and sends the connection preface and an empty `SETTINGS` frame.
    pub async fn connect(server: &Started) -> RawH2 {
        let tls = server.tls(&[b"h2"]).await;
        assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]), "the server speaks h2 when asked");
        let (reader, writer) = tokio::io::split(tls);
        let mut raw = RawH2 { reader, writer, buffer: Vec::new() };
        raw.write(PREFACE).await;
        raw.write(&frame_bytes(SETTINGS, 0, 0, &[])).await;
        raw
    }

    /// Reads until the server's `SETTINGS` arrive, acknowledges them and returns them.
    pub async fn handshake(&mut self) -> Vec<(u16, u32)> {
        loop {
            let frame = self.read().await.expect("the server's settings");
            if frame.kind == SETTINGS && frame.flags & ACK == 0 {
                self.write(&frame_bytes(SETTINGS, ACK, 0, &[])).await;
                return frame.settings();
            }
        }
    }

    /// Writes bytes; a failed write (the server closed) is returned as false.
    pub async fn try_write(&mut self, bytes: &[u8]) -> bool {
        matches!(tokio::time::timeout(PATIENCE, self.writer.write_all(bytes)).await, Ok(Ok(())))
    }

    pub async fn write(&mut self, bytes: &[u8]) {
        assert!(self.try_write(bytes).await, "the server closed the connection while the test was writing");
    }

    pub async fn send(&mut self, kind: u8, flags: u8, stream: u32, payload: &[u8]) {
        self.write(&frame_bytes(kind, flags, stream, payload)).await;
    }

    /// A request for `path` on `stream`, complete (headers and end of stream).
    pub async fn get(&mut self, stream: u32, path: &str) {
        self.send(HEADERS, END_HEADERS | END_STREAM, stream, &request_block(path, &[])).await;
    }

    pub async fn reset(&mut self, stream: u32) {
        self.send(RST_STREAM, 0, stream, &8u32.to_be_bytes()).await; // CANCEL
    }

    /// The next frame, or `None` when the connection ended (or nothing arrives within the patience).
    pub async fn read(&mut self) -> Option<Frame> {
        match self.read_within(PATIENCE).await {
            Read::Frame(frame) => Some(frame),
            Read::Quiet | Read::Ended => None,
        }
    }

    /// As [`RawH2::read`] with a time of the test's choosing, and with the three outcomes told apart.
    pub async fn read_within(&mut self, within: Duration) -> Read {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            if self.buffer.len() >= 9 {
                let length = u32::from_be_bytes([0, self.buffer[0], self.buffer[1], self.buffer[2]]) as usize;
                if self.buffer.len() >= 9 + length {
                    let frame = Frame {
                        kind: self.buffer[3],
                        flags: self.buffer[4],
                        stream: u32::from_be_bytes([self.buffer[5] & 0x7f, self.buffer[6], self.buffer[7], self.buffer[8]]),
                        payload: self.buffer[9..9 + length].to_vec(),
                    };
                    self.buffer.drain(..9 + length);
                    return Read::Frame(frame);
                }
            }
            let mut chunk = vec![0u8; 64 * 1024];
            match tokio::time::timeout_at(deadline, self.reader.read(&mut chunk)).await {
                Ok(Ok(n)) if n > 0 => self.buffer.extend_from_slice(&chunk[..n]),
                Ok(_) => return Read::Ended,
                Err(_) => return Read::Quiet,
            }
        }
    }

    /// Reads frames until the connection ends or `within` has passed; true when it ended (the server closed it, or sent a
    /// `GOAWAY` and then closed).
    pub async fn ends_within(&mut self, within: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            let mut chunk = vec![0u8; 64 * 1024];
            match tokio::time::timeout(left, self.reader.read(&mut chunk)).await {
                Ok(Ok(n)) if n > 0 => {}
                Ok(_) => return true,
                Err(_) => return false,
            }
        }
    }
}

/// An HTTP/2 client of hyper over TLS, for the requests a library can make.
pub async fn h2_client(
    server: &Started,
) -> hyper::client::conn::http2::SendRequest<http_body_util::Empty<axum::body::Bytes>> {
    let tls = server.tls(&[b"h2"]).await;
    let (sender, connection) =
        hyper::client::conn::http2::handshake(hyper_util::rt::TokioExecutor::new(), hyper_util::rt::TokioIo::new(tls))
            .await
            .expect("the h2 handshake");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    sender
}

/// A short, private scratch directory under `/tmp`: a Unix socket path must fit in about a hundred bytes, and the system's
/// temporary directory on some platforms is too long for that.
pub struct ShortDir {
    path: std::path::PathBuf,
}

impl ShortDir {
    pub fn new() -> ShortDir {
        use std::os::unix::fs::DirBuilderExt;
        let mut random = [0u8; 5];
        getrandom::fill(&mut random).expect("random bytes");
        let tag: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let path = std::path::PathBuf::from(format!("/tmp/hwh-{tag}"));
        std::fs::DirBuilder::new().mode(0o700).create(&path).expect("a fresh short directory");
        ShortDir { path }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Default for ShortDir {
    fn default() -> ShortDir {
        ShortDir::new()
    }
}

impl Drop for ShortDir {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
