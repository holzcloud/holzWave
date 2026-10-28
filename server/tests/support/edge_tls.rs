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
use holzwave_server::http::app::{App, AppInit, Edge, PublicOrigin, Shutdown, serve};
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
}

impl Default for Opts {
    fn default() -> Opts {
        Opts { extra: Vec::new(), page: true, trusted_proxies: None }
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
    let admission = Admission::with_defaults(mode, proxies).expect("admission");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local address");
    let shutdown = Shutdown::new();
    let edge = Edge::new(router, acceptor, admission.clone(), mode).expect("the edge");
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
