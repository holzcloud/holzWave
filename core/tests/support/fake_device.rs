//! A scripted server for the tests of the client of the device socket: a listener on a loopback port that upgrades what
//! connects (with tungstenite's server side and the vocabulary of `proto`, so it can say exactly the bytes a test wants),
//! records what the upgrade request carried, and hands the test the connection. The test thread is the server: it reads the
//! requests of the client, answers or does not, and sends events and chunks, or hostile bytes, in the order it chooses.
//!
//! Tests pull it in with `#[path = "support/fake_device.rs"] mod fake_device;`.
#![allow(dead_code, clippy::result_large_err, clippy::too_many_arguments)]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use holzwave_core::remote::hub::Endpoint;
use holzwave_core::remote::proto::{self, Answer, ErrorWord, Event, Input, Op, Reply, Request, Stream, WriterView};
use holzwave_core::server_link::{Fault, Origin, Token};
use tungstenite::handshake::server::{ErrorResponse, Request as HttpRequest, Response};
use tungstenite::{Message, WebSocket, http};

/// How long a helper waits for the client before it fails the test.
pub const PATIENCE: Duration = Duration::from_secs(15);

/// What the peer saw of one upgrade request.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    pub authorization: Option<String>,
    pub has_origin: bool,
    pub path: String,
}

/// An endpoint that points at a [`FakeDevice`] and hands out a different token every time it is asked.
pub struct FakeEndpoint {
    origin: Mutex<Option<Origin>>,
    counter: AtomicU64,
    handed: Mutex<Vec<String>>,
    /// When set, the endpoint says that the person is signed out.
    signed_out: Mutex<bool>,
    /// How often the hub said that the server refused the token.
    refusals: AtomicU64,
}

impl FakeEndpoint {
    /// The tokens handed out so far, in the form they were written.
    pub fn handed(&self) -> Vec<String> {
        self.handed.lock().unwrap().clone()
    }

    pub fn sign_out(&self, out: bool) {
        *self.signed_out.lock().unwrap() = out;
    }

    /// How often the hub said that the server refused the token.
    pub fn refusals(&self) -> u64 {
        self.refusals.load(Ordering::SeqCst)
    }
}

impl Endpoint for FakeEndpoint {
    fn origin(&self) -> Option<Origin> {
        if *self.signed_out.lock().unwrap() { None } else { self.origin.lock().unwrap().clone() }
    }

    fn token(&self) -> Result<Token, Fault> {
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let text = format!("hwd_{n:0>43}");
        self.handed.lock().unwrap().push(text.clone());
        Token::parse(&text)
    }

    fn refused(&self) {
        self.refusals.fetch_add(1, Ordering::SeqCst);
    }
}

/// A listener that plays a server.
pub struct FakeDevice {
    listener: TcpListener,
    pub addr: SocketAddr,
    pub endpoint: Arc<FakeEndpoint>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl FakeDevice {
    pub fn start() -> FakeDevice {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        listener.set_nonblocking(true).expect("non-blocking accept");
        let addr = listener.local_addr().expect("local address");
        let origin = Origin::parse(&format!("http://{addr}")).expect("a loopback origin");
        let endpoint = Arc::new(FakeEndpoint {
            origin: Mutex::new(Some(origin)),
            counter: AtomicU64::new(0),
            handed: Mutex::new(Vec::new()),
            signed_out: Mutex::new(false),
            refusals: AtomicU64::new(0),
        });
        FakeDevice { listener, addr, endpoint, seen: Arc::new(Mutex::new(Vec::new())) }
    }

    /// What every upgrade request carried, in the order they came.
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn accept_stream(&self, wait: Duration) -> Option<TcpStream> {
        let deadline = Instant::now() + wait;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).expect("blocking stream");
                    return Some(stream);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => panic!("accept: {e}"),
            }
        }
    }

    /// Accepts the next connection and upgrades it. `None` when nothing connected in time.
    pub fn accept_within(&self, wait: Duration) -> Option<Conn> {
        let stream = self.accept_stream(wait)?;
        let seen = self.seen.clone();
        let ws = tungstenite::accept_hdr(stream, |request: &HttpRequest, response: Response| {
            let headers = request.headers();
            seen.lock().unwrap().push(Seen {
                authorization: headers.get("authorization").and_then(|v| v.to_str().ok()).map(str::to_owned),
                has_origin: headers.contains_key("origin"),
                path: request.uri().path().to_owned(),
            });
            Ok(response)
        })
        .ok()?;
        Some(Conn { ws })
    }

    /// Accepts the next connection and upgrades it, or fails the test.
    pub fn accept(&self) -> Conn {
        self.accept_within(PATIENCE).expect("the client connected and the upgrade succeeded")
    }

    /// Accepts the next connection and answers its upgrade with `status`.
    pub fn refuse(&self, status: u16) {
        let stream = self.accept_stream(PATIENCE).expect("the client connected");
        let seen = self.seen.clone();
        let refused = tungstenite::accept_hdr(stream, |request: &HttpRequest, _: Response| {
            seen.lock().unwrap().push(Seen {
                authorization: request.headers().get("authorization").and_then(|v| v.to_str().ok()).map(str::to_owned),
                has_origin: request.headers().contains_key("origin"),
                path: request.uri().path().to_owned(),
            });
            let response: ErrorResponse = http::Response::builder().status(status).body(Some("refused".to_owned())).expect("a response");
            Err(response)
        });
        assert!(refused.is_err(), "the peer refused the upgrade");
    }

    /// Accepts connections and drops each at once, for `wait`: a server that is there and does not speak.
    pub fn drop_connections_for(&self, wait: Duration) {
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            if let Some(stream) = self.accept_stream(Duration::from_millis(20)) {
                drop(stream);
            }
        }
    }
}

/// What the client sent.
#[derive(Debug, PartialEq, Eq)]
pub enum Got {
    Request(Request),
    Input { session: u32, bytes: Vec<u8> },
    /// A message the vocabulary does not have (a test of the client never makes one).
    Strange,
    Closed(Option<u16>),
    Quiet,
}

/// One upgraded connection, seen from the server's side.
pub struct Conn {
    ws: WebSocket<TcpStream>,
}

impl Conn {
    /// The next thing the client sent, waiting at most `wait`.
    pub fn next(&mut self, wait: Duration) -> Got {
        // A connection the client has closed refuses the setting; the read then says so.
        let _ = self.ws.get_ref().set_read_timeout(Some(wait.max(Duration::from_millis(1))));
        match self.ws.read() {
            Ok(Message::Text(text)) => match Request::parse(text.as_str()) {
                Ok(request) => Got::Request(request),
                Err(_) => Got::Strange,
            },
            Ok(Message::Binary(bytes)) => match Input::parse(&bytes) {
                Ok(input) => Got::Input { session: input.session, bytes: input.bytes.to_vec() },
                Err(_) => Got::Strange,
            },
            Ok(Message::Close(frame)) => Got::Closed(frame.map(|f| u16::from(f.code))),
            Ok(_) => Got::Quiet,
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => Got::Quiet,
            Err(_) => Got::Closed(None),
        }
    }

    /// The next request of the client, skipping beats; fails the test when anything else comes.
    pub fn request(&mut self) -> Request {
        let deadline = Instant::now() + PATIENCE;
        loop {
            match self.next(Duration::from_millis(100)) {
                Got::Request(Request { op: Op::Beat { .. }, .. }) => {}
                Got::Request(request) => return request,
                Got::Quiet => assert!(Instant::now() < deadline, "the client sent no request in time"),
                other => panic!("a request was expected, got {other:?}"),
            }
        }
    }

    /// The next request that is not a beat, or `None` when none came within `wait`.
    pub fn request_within(&mut self, wait: Duration) -> Option<Request> {
        let deadline = Instant::now() + wait;
        loop {
            match self.next(Duration::from_millis(20)) {
                Got::Request(Request { op: Op::Beat { .. }, .. }) => {}
                Got::Request(request) => return Some(request),
                Got::Quiet => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                }
                _ => return None,
            }
        }
    }

    /// Reads for `wait` and returns everything the client sent in it.
    pub fn drain(&mut self, wait: Duration) -> Vec<Got> {
        let deadline = Instant::now() + wait;
        let mut out = Vec::new();
        while Instant::now() < deadline {
            match self.next(Duration::from_millis(10)) {
                Got::Quiet => {}
                other => out.push(other),
            }
        }
        out
    }

    fn send(&mut self, message: Message) {
        self.ws.send(message).expect("the client is reading");
    }

    pub fn answer(&mut self, id: u64, answer: Answer) {
        self.send(Message::text(Reply::Answer { id, result: Ok(answer) }.encode()));
    }

    pub fn error(&mut self, id: u64, word: ErrorWord) {
        self.send(Message::text(Reply::Answer { id, result: Err(word) }.encode()));
    }

    /// The answer to an attach of `session`.
    pub fn attached(&mut self, id: u64, session: u32, cols: u16, rows: u16, writer: WriterView, more: bool, snapshot: u32) {
        self.answer(id, Answer::Attached { session, cols, rows, writer, more, snapshot });
    }

    pub fn event(&mut self, event: Event) {
        self.send(Message::text(Reply::Event(event).encode()));
    }

    pub fn text(&mut self, text: &str) {
        self.send(Message::text(text));
    }

    /// Text that the client may refuse before it has read all of it: a failed write is not an error here.
    pub fn text_lossy(&mut self, text: &str) {
        let _ = self.ws.send(Message::text(text));
    }

    pub fn chunk(&mut self, stream: Stream, session: u32, seq: u32, bytes: &[u8]) {
        let frame = proto::encode_chunk(stream, session, seq, bytes).expect("a chunk of the vocabulary");
        self.send(Message::binary(frame));
    }

    pub fn snapshot(&mut self, session: u32, seq: u32, bytes: &[u8]) {
        self.chunk(Stream::Snapshot, session, seq, bytes);
    }

    pub fn live(&mut self, session: u32, seq: u32, bytes: &[u8]) {
        self.chunk(Stream::Live, session, seq, bytes);
    }

    /// A binary frame of exact bytes, whatever they are.
    pub fn raw_binary(&mut self, bytes: &[u8]) {
        self.send(Message::binary(bytes.to_vec()));
    }

    /// Ends the connection with `code`.
    pub fn close(mut self, code: u16) {
        let frame = tungstenite::protocol::CloseFrame { code: code.into(), reason: "".into() };
        let _ = self.ws.close(Some(frame));
        let _ = self.ws.flush();
    }

    /// Drops the connection without a close frame, as a network that failed does.
    pub fn cut(self) {
        let _ = self.ws.get_ref().shutdown(std::net::Shutdown::Both);
    }
}

/// The text of a line of numbered output, for tests that need bytes with a shape.
pub fn numbered(n: u32) -> Vec<u8> {
    format!("n {n}\r\n").into_bytes()
}
