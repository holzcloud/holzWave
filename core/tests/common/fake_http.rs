//! An in-process HTTP/1.1 server for the AI tests.
//!
//! It uses the standard library only (no dependency) and runs one thread per connection with `connection: close`, so a
//! test sees exactly one request per connection. It binds `127.0.0.1` on a free port, records what each request
//! carried and when each connection was accepted, completed and closed by the peer, and answers with a reply the test
//! chose: a chunked event stream cut at chosen offsets, a stall, a dropped connection, an error status with headers
//! and a body, or silence. Every wait in the server is bounded, and dropping the server stops its threads.
//!
//! Include it through `mod common;` and `use common::fake_http::{FakeHttp, Reply, Ending};`.

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// The longest request head the server reads.
const HEAD_LIMIT: usize = 64 * 1024;
/// The largest request body the server reads.
const BODY_LIMIT: usize = 32 * 1024 * 1024;
/// How long the server waits for a request to arrive.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a held socket is looked at for the peer closing.
const WATCH_INTERVAL: Duration = Duration::from_millis(5);

/// One request as the server read it.
#[derive(Clone, Debug)]
pub struct Seen {
    /// The method, for example `POST`.
    pub method: String,
    /// The request target, for example `/v1/lines`.
    pub target: String,
    /// The headers, names in lower case, in the order they arrived.
    pub headers: Vec<(String, String)>,
    /// The body bytes.
    pub body: Vec<u8>,
}

impl Seen {
    /// The value of the first header called `name` (lower case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }

    /// The body as text (lossy).
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// What the server knows about one connection.
#[derive(Clone, Copy, Debug)]
pub struct ConnInfo {
    /// When the connection was accepted.
    pub accepted: Instant,
    /// When the whole request had been read.
    pub request_complete: Option<Instant>,
    /// When the server saw the client close its end. Only recorded while the server holds the socket open
    /// ([`Ending::Hold`], [`Reply::Hang`]).
    pub peer_closed: Option<Instant>,
}

/// How a streamed reply ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// Write the terminating chunk and close.
    Finish,
    /// Write nothing more, keep the socket open and watch for the client closing it.
    Hold,
    /// Close at once with no terminating chunk: the client sees the end in the middle of a chunked body.
    Drop,
}

/// A streamed reply: the status line, then every element of `chunks` as one HTTP chunk.
#[derive(Clone, Debug)]
pub struct SseReply {
    status: u16,
    chunks: Vec<Vec<u8>>,
    gap: Duration,
    ending: Ending,
    content_type: String,
    headers: Vec<(String, String)>,
}

/// A plain reply with a status, headers and a body of known length.
#[derive(Clone, Debug)]
pub struct StatusReply {
    code: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// What the server answers to one request.
#[derive(Clone, Debug)]
pub enum Reply {
    /// A chunked event stream (see [`Reply::sse`]).
    Sse(SseReply),
    /// A status with a body (see [`Reply::status`]).
    Status(StatusReply),
    /// Read the request and never answer; the socket stays open until the client closes it.
    Hang,
    /// Read the request and close the socket.
    Close,
}

impl Reply {
    /// A 200 event stream whose chunks are the given texts, in order.
    pub fn sse(chunks: &[&str]) -> Reply {
        Reply::sse_bytes(chunks.iter().map(|chunk| chunk.as_bytes().to_vec()).collect())
    }

    /// A 200 event stream whose chunks are the given byte strings.
    pub fn sse_bytes(chunks: Vec<Vec<u8>>) -> Reply {
        Reply::Sse(SseReply {
            status: 200,
            chunks,
            gap: Duration::ZERO,
            ending: Ending::Finish,
            content_type: "text/event-stream".to_owned(),
            headers: Vec::new(),
        })
    }

    /// A 200 event stream whose body is `body` cut at the byte `offsets` (ascending, each inside the body).
    pub fn sse_split(body: &[u8], offsets: &[usize]) -> Reply {
        let mut chunks = Vec::new();
        let mut start = 0;
        for &offset in offsets {
            chunks.push(body[start..offset].to_vec());
            start = offset;
        }
        chunks.push(body[start..].to_vec());
        Reply::sse_bytes(chunks)
    }

    /// An answer with this status code, no header and no body.
    pub fn status(code: u16) -> Reply {
        Reply::Status(StatusReply { code, headers: Vec::new(), body: Vec::new() })
    }

    /// The time to wait between two chunks of a streamed reply.
    pub fn gap(mut self, gap: Duration) -> Reply {
        if let Reply::Sse(reply) = &mut self {
            reply.gap = gap;
        }
        self
    }

    /// How a streamed reply ends.
    pub fn ending(mut self, ending: Ending) -> Reply {
        if let Reply::Sse(reply) = &mut self {
            reply.ending = ending;
        }
        self
    }

    /// The content type of a streamed reply.
    pub fn content_type(mut self, content_type: &str) -> Reply {
        if let Reply::Sse(reply) = &mut self {
            reply.content_type = content_type.to_owned();
        }
        self
    }

    /// An extra response header.
    pub fn header(mut self, name: &str, value: &str) -> Reply {
        match &mut self {
            Reply::Sse(reply) => reply.headers.push((name.to_owned(), value.to_owned())),
            Reply::Status(reply) => reply.headers.push((name.to_owned(), value.to_owned())),
            Reply::Hang | Reply::Close => {}
        }
        self
    }

    /// The body of a status reply.
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Reply {
        if let Reply::Status(reply) = &mut self {
            reply.body = body.into();
        }
        self
    }
}

type Handler = dyn Fn(&Seen, usize) -> Reply + Send + Sync;

struct Shared {
    stop: AtomicBool,
    connections: Mutex<Vec<ConnInfo>>,
    seen: Mutex<Vec<Seen>>,
    handler: Box<Handler>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An HTTP/1.1 server on a free loopback port. Its threads end when it is dropped.
pub struct FakeHttp {
    addr: SocketAddr,
    shared: Arc<Shared>,
}

impl FakeHttp {
    /// Starts a server that answers request `i` with `replies[i]` and every later request with the last reply. With no
    /// reply at all it closes every connection.
    pub fn start(replies: Vec<Reply>) -> FakeHttp {
        FakeHttp::start_with(move |_, index| match replies.len() {
            0 => Reply::Close,
            len => replies[index.min(len - 1)].clone(),
        })
    }

    /// Starts a server whose reply to each request is decided by `handler(request, request_number)`.
    pub fn start_with(handler: impl Fn(&Seen, usize) -> Reply + Send + Sync + 'static) -> FakeHttp {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let addr = listener.local_addr().expect("local address");
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            connections: Mutex::new(Vec::new()),
            seen: Mutex::new(Vec::new()),
            handler: Box::new(handler),
        });
        let acceptor = shared.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                if acceptor.stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let conn = {
                    let mut connections = lock(&acceptor.connections);
                    connections.push(ConnInfo { accepted: Instant::now(), request_complete: None, peer_closed: None });
                    connections.len() - 1
                };
                let shared = acceptor.clone();
                thread::spawn(move || serve(stream, conn, &shared));
            }
        });
        FakeHttp { addr, shared }
    }

    /// The address the server listens on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The port the server listens on.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The text of the base address: scheme, address and port, with no path.
    pub fn base(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// How many connections were accepted.
    pub fn accepted(&self) -> usize {
        lock(&self.shared.connections).len()
    }

    /// Every request read so far, in order.
    pub fn seen(&self) -> Vec<Seen> {
        lock(&self.shared.seen).clone()
    }

    /// What is known about connection `i` (0 is the first accepted).
    pub fn connection(&self, i: usize) -> Option<ConnInfo> {
        lock(&self.shared.connections).get(i).copied()
    }

    /// Waits until at least `n` connections were accepted; false when the time ran out.
    pub fn wait_accepted(&self, n: usize, timeout: Duration) -> bool {
        super::wait_until(timeout, || self.accepted() >= n)
    }

    /// Waits until the server saw the client close connection `i` and returns that moment; `None` when the time ran out.
    pub fn wait_closed(&self, i: usize, timeout: Duration) -> Option<Instant> {
        let mut closed = None;
        super::wait_until(timeout, || {
            closed = self.connection(i).and_then(|info| info.peer_closed);
            closed.is_some()
        });
        closed
    }
}

impl Drop for FakeHttp {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // Wake the accept call so the thread sees the flag.
        let _ = TcpStream::connect(self.addr);
    }
}

/// Reads one request: the head up to the blank line and as many body bytes as `content-length` says.
fn read_request(stream: &TcpStream) -> Option<Seen> {
    let mut reader = stream;
    let mut buffer: Vec<u8> = Vec::new();
    let mut piece = [0u8; 8192];
    let head_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        if buffer.len() > HEAD_LIMIT {
            return None;
        }
        match reader.read(&mut piece) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buffer.extend_from_slice(&piece[..n]),
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split_whitespace();
    let method = request_line.next()?.to_owned();
    let target = request_line.next()?.to_owned();
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line.split_once(':')?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    if length > BODY_LIMIT {
        return None;
    }
    let mut body: Vec<u8> = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        match reader.read(&mut piece) {
            Ok(0) | Err(_) => return None,
            Ok(n) => body.extend_from_slice(&piece[..n]),
        }
    }
    body.truncate(length);
    Some(Seen { method, target, headers, body })
}

fn serve(stream: TcpStream, conn: usize, shared: &Shared) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
    let Some(seen) = read_request(&stream) else { return };
    let index = {
        let mut all = lock(&shared.seen);
        all.push(seen.clone());
        all.len() - 1
    };
    if let Some(info) = lock(&shared.connections).get_mut(conn) {
        info.request_complete = Some(Instant::now());
    }
    match (shared.handler)(&seen, index) {
        Reply::Sse(reply) => write_stream(&stream, &reply, conn, shared),
        Reply::Status(reply) => write_status(&stream, &reply),
        Reply::Hang => hold(&stream, conn, shared),
        Reply::Close => {}
    }
}

/// The reason phrase of the status codes the tests use.
fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        529 => "Overloaded",
        _ => "Status",
    }
}

fn write_status(stream: &TcpStream, reply: &StatusReply) {
    let mut head = format!("HTTP/1.1 {} {}\r\n", reply.code, reason(reply.code));
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("content-length: {}\r\nconnection: close\r\n\r\n", reply.body.len()));
    let mut writer = stream;
    let mut out = head.into_bytes();
    out.extend_from_slice(&reply.body);
    let _ = writer.write_all(&out);
    let _ = writer.flush();
    let _ = stream.shutdown(Shutdown::Write);
}

fn write_stream(stream: &TcpStream, reply: &SseReply, conn: usize, shared: &Shared) {
    let mut writer = stream;
    let mut head = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncache-control: no-cache\r\ntransfer-encoding: chunked\r\nconnection: close\r\n",
        reply.status,
        reason(reply.status),
        reply.content_type
    );
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    if writer.write_all(head.as_bytes()).and_then(|()| writer.flush()).is_err() {
        return;
    }
    for (index, chunk) in reply.chunks.iter().enumerate() {
        if index > 0 && !reply.gap.is_zero() {
            thread::sleep(reply.gap);
        }
        // One write per chunk, so the framing is never split by the server.
        let mut frame = format!("{:x}\r\n", chunk.len()).into_bytes();
        frame.extend_from_slice(chunk);
        frame.extend_from_slice(b"\r\n");
        if writer.write_all(&frame).and_then(|()| writer.flush()).is_err() {
            return;
        }
    }
    match reply.ending {
        Ending::Finish => {
            let _ = writer.write_all(b"0\r\n\r\n");
            let _ = writer.flush();
            let _ = stream.shutdown(Shutdown::Write);
        }
        Ending::Hold => hold(stream, conn, shared),
        Ending::Drop => {}
    }
}

/// Keeps the socket open and records the moment the client closes its end. Ends when the server is dropped.
fn hold(stream: &TcpStream, conn: usize, shared: &Shared) {
    let _ = stream.set_read_timeout(Some(WATCH_INTERVAL));
    let mut reader = stream;
    let mut sink = [0u8; 256];
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        match reader.read(&mut sink) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
            Err(_) => break,
        }
    }
    if let Some(info) = lock(&shared.connections).get_mut(conn) {
        info.peer_closed = Some(Instant::now());
    }
}
