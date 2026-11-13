//! The WebSocket client of the core: one blocking connection to a holzWave server, with the device token in an
//! `Authorization` header and nothing else the server could use to tell who is asking.
//!
//! What this file decides, and the library does not:
//!
//! * **The transport.** `wss` runs over the core's own rustls stream with the ring provider and the operating system's trust
//!   store (the configuration of the AI client, [`crate::ai::http::tls_config`]); the library's TLS features are not used and
//!   there is no way to skip the certificate check. `ws` is accepted for a loopback address only, so a token never crosses a
//!   network in clear text. A redirect is an answer like any other: the upgrade is not followed anywhere.
//! * **The bounds.** The server and the network are untrusted. A message is at most [`WsLimits::max_message`] bytes and a frame
//!   at most [`WsLimits::max_frame`] (the library checks a frame's announced length before it reads or allocates the payload),
//!   a masked frame from the server is a protocol error, a text message that is not UTF-8 is one too, and every wait has a
//!   bound: the connect, the handshake, a write and a read.
//! * **What an error says.** A refused upgrade is one of the three faults of [`crate::server_link::Fault`] by its status alone.
//!   No error holds a text the server wrote, and the token is never in one: it is read once, into the header value of the
//!   upgrade request, and not kept by [`WsClient`].
//!
//! The connection runs on the person's request after sign-in and nowhere else. It carries opaque text and binary messages;
//! the vocabulary of the sessions on top of it is not in this file.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use tungstenite::client::IntoClientRequest;
use tungstenite::error::CapacityError;
use tungstenite::http::header::AUTHORIZATION;
use tungstenite::http::{HeaderValue, Uri};
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tungstenite::{Bytes, HandshakeError, Message, WebSocket};

use crate::server_link::{Fault, Token};

/// The longest address that is accepted, in bytes.
pub const MAX_URL: usize = 512;
/// The longest host name, as in DNS.
pub const MAX_HOST: usize = 253;
/// The longest path of an address.
pub const MAX_PATH: usize = 256;

/// The smallest and the largest message the limits can be set to.
const MESSAGE_RANGE: (usize, usize) = (4 * 1024, 16 * 1024 * 1024);
/// The smallest and the largest frame the limits can be set to (a frame is never larger than a message).
const FRAME_RANGE: (usize, usize) = (1024, 4 * 1024 * 1024);
/// The most addresses of a host name that are tried.
const MAX_ADDRESSES: usize = 8;
/// How long [`WsClient::close`] waits for the answer of the peer.
const CLOSE_WAIT: Duration = Duration::from_secs(1);

/// Why an address is not accepted. Every case has a fixed sentence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UrlError {
    /// Not `wss://` or `ws://`.
    Scheme,
    /// `ws://` for a host that is not a loopback address: a token would cross a network in clear text.
    Insecure,
    /// A user name or password in the address.
    Userinfo,
    /// A query (`?`).
    Query,
    /// A fragment (`#`).
    Fragment,
    /// An empty host, a host that is not a name or an address, or one over the bound.
    Host,
    /// A port that is not a number from 1 to 65535.
    Port,
    /// A path over the bound or with a character that is not plain.
    Path,
    /// The whole address is over the bound.
    TooLong,
}

impl UrlError {
    pub fn sentence(self) -> &'static str {
        match self {
            UrlError::Scheme => "The server address must be a secure web address.",
            UrlError::Insecure => "A server address without TLS is only accepted for this computer.",
            UrlError::Userinfo => "The server address must not hold a user name or a password.",
            UrlError::Query => "The server address must not hold a query.",
            UrlError::Fragment => "The server address must not hold a fragment.",
            UrlError::Host => "The host of the server address is not valid.",
            UrlError::Port => "The port of the server address is not valid.",
            UrlError::Path => "The path of the server address is not valid.",
            UrlError::TooLong => "The server address is too long.",
        }
    }
}

impl fmt::Display for UrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.sentence())
    }
}

impl std::error::Error for UrlError {}

impl From<UrlError> for crate::Error {
    /// The refusal as the crate's error, with exactly its fixed sentence as the text.
    fn from(e: UrlError) -> Self {
        crate::Error::Io(io::Error::other(e.sentence()))
    }
}

/// The address of a WebSocket endpoint: `wss://host[:port][/path]`, or `ws://` for a loopback host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WsUrl {
    secure: bool,
    /// The host without brackets.
    host: String,
    port: u16,
    path: String,
}

fn is_loopback_name(host: &str) -> bool {
    host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

impl WsUrl {
    /// Reads an address. Refused: any other scheme (including `https://`), `ws://` for a host that is not loopback, user
    /// information, a query, a fragment, an empty or malformed host, a bad port, a path with anything but letters, digits and
    /// `-._~/`, and anything over [`MAX_URL`], [`MAX_HOST`] or [`MAX_PATH`].
    pub fn parse(text: &str) -> Result<WsUrl, UrlError> {
        if text.len() > MAX_URL {
            return Err(UrlError::TooLong);
        }
        let (scheme, rest) = text.split_once("://").ok_or(UrlError::Scheme)?;
        let secure = match scheme {
            "wss" => true,
            "ws" => false,
            _ => return Err(UrlError::Scheme),
        };
        if rest.contains('#') {
            return Err(UrlError::Fragment);
        }
        if rest.contains('?') {
            return Err(UrlError::Query);
        }
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        if authority.contains('@') {
            return Err(UrlError::Userinfo);
        }
        let default_port = if secure { 443 } else { 80 };
        let (host, port_text) = if let Some(inner) = authority.strip_prefix('[') {
            let (host, after) = inner.split_once(']').ok_or(UrlError::Host)?;
            if host.parse::<Ipv6Addr>().is_err() {
                return Err(UrlError::Host);
            }
            let port = match after {
                "" => None,
                _ => Some(after.strip_prefix(':').ok_or(UrlError::Host)?),
            };
            (host, port)
        } else {
            let (host, port) = match authority.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            };
            let name_ok = !host.is_empty()
                && host.len() <= MAX_HOST
                && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
                && !host.starts_with(['-', '.'])
                && !host.ends_with('-')
                && !host.contains("..");
            if !name_ok {
                return Err(UrlError::Host);
            }
            (host, port)
        };
        let port = match port_text {
            None => default_port,
            Some(p) if !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()) => {
                p.parse::<u16>().ok().filter(|p| *p != 0).ok_or(UrlError::Port)?
            }
            Some(_) => return Err(UrlError::Port),
        };
        if path.len() > MAX_PATH || !path.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~/".contains(&b)) {
            return Err(UrlError::Path);
        }
        if !secure && !is_loopback_name(host) {
            return Err(UrlError::Insecure);
        }
        Ok(WsUrl { secure, host: host.to_owned(), port, path: path.to_owned() })
    }

    /// `wss://` (TLS) or `ws://` (loopback only).
    pub fn secure(&self) -> bool {
        self.secure
    }

    /// The host, an IPv6 address without brackets.
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The path, always starting with `/`.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The address as written into the request: the port only when it is not the default of the scheme.
    fn to_uri_text(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        let default = if self.secure { 443 } else { 80 };
        let port = if self.port == default { String::new() } else { format!(":{}", self.port) };
        format!("{}://{host}{port}{}", if self.secure { "wss" } else { "ws" }, self.path)
    }
}

/// The bounds of one connection. Both sizes are clamped (see [`WsLimits::new`]) and the waits are never unbounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WsLimits {
    /// The most bytes of one message, assembled from its frames.
    pub max_message: usize,
    /// The most bytes of the payload of one frame.
    pub max_frame: usize,
    /// How long a connection attempt to one address may take.
    pub connect_timeout: Duration,
    /// How long the TLS and upgrade handshakes may wait on the network, in all: the time every read and write of the
    /// handshakes blocks is added up, so a peer that sends one byte at a time cannot hold the connect beyond it. The time the
    /// certificate check takes is not counted (it waits on no byte of the peer).
    pub handshake_timeout: Duration,
    /// How long one write may block.
    pub write_timeout: Duration,
}

impl Default for WsLimits {
    /// A message of 8 MiB and a frame of 1 MiB, ten seconds for each wait.
    fn default() -> Self {
        WsLimits::new(8 * 1024 * 1024, 1024 * 1024)
    }
}

impl WsLimits {
    /// The limits for a message and a frame size: the message is clamped to 4 KiB..=16 MiB, the frame to 1 KiB..=4 MiB and to
    /// no more than the message.
    pub fn new(max_message: usize, max_frame: usize) -> Self {
        let max_message = max_message.clamp(MESSAGE_RANGE.0, MESSAGE_RANGE.1);
        let max_frame = max_frame.clamp(FRAME_RANGE.0, FRAME_RANGE.1).min(max_message);
        WsLimits {
            max_message,
            max_frame,
            connect_timeout: Duration::from_secs(10),
            handshake_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(10),
        }
    }

    /// The same limits with the sizes clamped again and every wait between 1 ms and 60 s: a caller that built the struct by
    /// hand cannot turn a bound off.
    fn clamped(&self) -> WsLimits {
        let wait = |d: Duration| d.clamp(Duration::from_millis(1), Duration::from_secs(60));
        WsLimits {
            connect_timeout: wait(self.connect_timeout),
            handshake_timeout: wait(self.handshake_timeout),
            write_timeout: wait(self.write_timeout),
            ..WsLimits::new(self.max_message, self.max_frame)
        }
    }
}

/// What a connection can fail with. None of the texts is one the server wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WsError {
    /// One of the faults of the link (the status of a refused upgrade, a connection that broke, a rule of the protocol).
    Fault(Fault),
    /// The TLS handshake failed: the certificate is not trusted or does not fit the name, or the peer does not speak TLS.
    Tls,
    /// The platform did not allow a connection to the local network. The sentence is the core's own.
    Blocked(String),
    /// A message is larger than the limits allow, coming in or going out.
    TooLarge,
    /// The connection is closed.
    Closed,
}

impl From<Fault> for WsError {
    fn from(fault: Fault) -> Self {
        WsError::Fault(fault)
    }
}

impl fmt::Display for WsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WsError::Fault(fault) => f.write_str(fault.sentence()),
            WsError::Tls => f.write_str("The secure connection to the server could not be verified."),
            WsError::Blocked(sentence) => f.write_str(sentence),
            WsError::TooLarge => f.write_str("A message is larger than holzWave accepts."),
            WsError::Closed => f.write_str("The connection to the server is closed."),
        }
    }
}

impl std::error::Error for WsError {}

impl From<WsError> for crate::Error {
    fn from(e: WsError) -> Self {
        crate::Error::Io(io::Error::other(e.to_string()))
    }
}

/// One thing a connection delivered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recv {
    Text(String),
    Binary(Vec<u8>),
    /// Nothing arrived within the wait. The connection is still usable.
    Timeout,
    /// The peer closed the connection with this code (1005 when it gave none, 1006 when the connection ended without a close
    /// frame). Every later call says the same.
    Closed(u16),
}

/// The TLS configuration of the platform: the ring provider and the operating system's trust store. The same function as the
/// AI client's; there is no way to skip the certificate check.
pub fn platform_tls() -> Result<Arc<rustls::ClientConfig>, WsError> {
    crate::ai::http::tls_config().map(Arc::new).map_err(|_| WsError::Tls)
}

/// The TCP connection under both kinds of stream. While the handshakes run it has a budget of time: every read and write is
/// given only what is left of it as its timeout, the time it blocked is taken off, and once nothing is left every read and
/// write fails as timed out. After the handshakes there is no budget and the caller's timeouts apply.
struct Sock {
    tcp: TcpStream,
    budget: Option<Duration>,
    write_timeout: Duration,
}

impl Sock {
    /// One read or write under the budget, when there is one. `set` gives the call its timeout.
    fn timed<T>(
        &mut self,
        set: fn(&TcpStream, Option<Duration>) -> io::Result<()>,
        cap: Duration,
        call: impl FnOnce(&mut TcpStream) -> io::Result<T>,
    ) -> io::Result<T> {
        let Some(left) = self.budget else { return call(&mut self.tcp) };
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        set(&self.tcp, Some(left.min(cap)))?;
        let started = Instant::now();
        let result = call(&mut self.tcp);
        self.budget = Some(left.saturating_sub(started.elapsed()));
        result
    }

    /// The handshakes are done: the read timeout is the caller's again (set before the next read) and the write timeout is
    /// the one of the limits.
    fn end_budget(&mut self) -> io::Result<()> {
        self.budget = None;
        self.tcp.set_write_timeout(Some(self.write_timeout))
    }
}

impl Read for Sock {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.timed(TcpStream::set_read_timeout, Duration::MAX, |tcp| tcp.read(buf))
    }
}

impl Write for Sock {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let cap = self.write_timeout;
        self.timed(TcpStream::set_write_timeout, cap, |tcp| tcp.write(buf))
    }

    /// All the buffers in one write, as the socket does it: rustls hands its last records (a refused certificate's alert after
    /// the change of cipher) over in one call and does not call again, so a write of the first buffer alone would lose the alert.
    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        let cap = self.write_timeout;
        self.timed(TcpStream::set_write_timeout, cap, |tcp| tcp.write_vectored(bufs))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}

/// The two kinds of stream the client runs over.
enum Stream {
    Plain(Sock),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, Sock>>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(sock) => &sock.tcp,
            Stream::Tls(tls) => &tls.sock.tcp,
        }
    }

    fn sock_mut(&mut self) -> &mut Sock {
        match self {
            Stream::Plain(sock) => sock,
            Stream::Tls(tls) => &mut tls.sock,
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(tcp) => tcp.read(buf),
            Stream::Tls(tls) => tls.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(tcp) => tcp.write(buf),
            Stream::Tls(tls) => tls.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(tcp) => tcp.flush(),
            Stream::Tls(tls) => tls.flush(),
        }
    }
}

/// Whether an I/O error is a read or write that ran out of time (`WouldBlock` on Unix, `TimedOut` on Windows).
fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

/// The kind of an error of the library, without any text it carries.
fn map_error(e: tungstenite::Error) -> WsError {
    use tungstenite::Error as E;
    match e {
        E::ConnectionClosed | E::AlreadyClosed => WsError::Closed,
        E::Protocol(tungstenite::error::ProtocolError::ResetWithoutClosingHandshake) => WsError::Closed,
        // The connection ended before the answer to the upgrade was complete: nothing was answered, so the server is not there.
        E::Protocol(tungstenite::error::ProtocolError::HandshakeIncomplete) => WsError::Fault(Fault::Unreachable),
        E::Io(e) => {
            if e.get_ref().is_some_and(|inner| inner.is::<rustls::Error>()) {
                WsError::Tls
            } else {
                WsError::Fault(Fault::Unreachable)
            }
        }
        E::Tls(_) => WsError::Tls,
        E::Capacity(CapacityError::MessageTooLong { .. }) => WsError::TooLarge,
        E::Http(response) => WsError::Fault(Fault::of_status(response.status().as_u16())),
        E::WriteBufferFull(_) => WsError::Fault(Fault::Unreachable),
        // A rule of the protocol, a text that is not UTF-8, too many headers, an attack, a bad address or header.
        _ => WsError::Fault(Fault::Protocol),
    }
}

/// One connection to a server. Not `Clone`; closing it, or dropping it, ends the connection.
pub struct WsClient {
    ws: WebSocket<Stream>,
    limits: WsLimits,
    /// The read timeout that is set on the socket now, so that a call with the same wait does not set it again.
    read_timeout: Option<Duration>,
    /// The code the connection ended with, once it has ended.
    closed: Option<u16>,
}

impl fmt::Debug for WsClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WsClient").field("closed", &self.closed).finish_non_exhaustive()
    }
}

/// A connection to the first address of the host that answers, within the bound.
fn open(url: &WsUrl, limits: &WsLimits) -> Result<TcpStream, WsError> {
    let mut addrs: Vec<SocketAddr> =
        (url.host(), url.port()).to_socket_addrs().map_err(|_| Fault::Unreachable)?.take(MAX_ADDRESSES).collect();
    if !url.secure() {
        // `ws` is for this computer only: a name that resolves anywhere else is not used.
        addrs.retain(|a| a.ip().is_loopback());
        if addrs.is_empty() {
            return Err(Fault::Refused.into());
        }
    }
    for addr in addrs {
        if let Ok(tcp) = TcpStream::connect_timeout(&addr, limits.connect_timeout) {
            return Ok(tcp);
        }
    }
    Err(Fault::Unreachable.into())
}

impl WsClient {
    /// Opens a connection and runs the handshakes.
    ///
    /// The platform's local-network gate is asked first (it returns at once where there is none). `tls` is the configuration
    /// of a `wss` address; `None` is the platform's ([`platform_tls`]), and a caller passes another configuration only to
    /// trust a particular certificate, never to skip the check. The upgrade request carries the token in `Authorization` and
    /// no `Origin`. A refused upgrade is [`Fault::SignedOut`] (401), [`Fault::Off`] (404) or [`Fault::Refused`] (any other
    /// status, a redirect included).
    pub fn connect(
        url: &WsUrl,
        token: &Token,
        tls: Option<Arc<rustls::ClientConfig>>,
        limits: &WsLimits,
    ) -> Result<WsClient, WsError> {
        let limits = limits.clamped();
        crate::reach::ensure_blocking(url.host(), url.port()).map_err(WsError::Blocked)?;
        // The configuration first: a platform without a verifier is known before a socket is opened.
        let tls = if url.secure() { Some(tls.map_or_else(platform_tls, Ok)?) } else { None };
        let tcp = open(url, &limits)?;
        tcp.set_nodelay(true).map_err(|_| Fault::Unreachable)?;
        // Both handshakes together may wait on the network for the handshake bound, however the peer spreads its bytes.
        let sock = Sock { tcp, budget: Some(limits.handshake_timeout), write_timeout: limits.write_timeout };
        let stream = match tls {
            Some(config) => {
                let name = ServerName::try_from(url.host().to_owned()).map_err(|_| Fault::Protocol)?;
                let connection = rustls::ClientConnection::new(config, name).map_err(|_| WsError::Tls)?;
                Stream::Tls(Box::new(rustls::StreamOwned::new(connection, sock)))
            }
            None => Stream::Plain(sock),
        };

        let uri: Uri = url.to_uri_text().parse().map_err(|_| Fault::Protocol)?;
        let mut request = uri.into_client_request().map_err(|_| Fault::Protocol)?;
        // The one place the token's text is used. The value is marked sensitive so that no debug print of the request shows
        // it; the buffer the library writes the request into is dropped without being wiped, which this library offers no
        // way to change.
        let mut bearer = HeaderValue::from_str(&token.bearer()).map_err(|_| Fault::Protocol)?;
        bearer.set_sensitive(true);
        request.headers_mut().insert(AUTHORIZATION, bearer);

        let config = WebSocketConfig::default()
            .read_buffer_size(64 * 1024)
            .write_buffer_size(0)
            .max_write_buffer_size(limits.max_message + 64 * 1024)
            .max_message_size(Some(limits.max_message))
            .max_frame_size(Some(limits.max_frame))
            .accept_unmasked_frames(false);
        match tungstenite::client::client_with_config(request, stream, Some(config)) {
            Ok((mut ws, _response)) => {
                ws.get_mut().sock_mut().end_budget().map_err(|_| Fault::Unreachable)?;
                // The read timeout on the socket is whatever the budget left: the first wait sets its own.
                Ok(WsClient { ws, limits, read_timeout: None, closed: None })
            }
            Err(HandshakeError::Failure(e)) => Err(map_error(e)),
            // A blocking stream that ran out of time in the handshake.
            Err(HandshakeError::Interrupted(_)) => Err(Fault::Unreachable.into()),
        }
    }

    /// Whether the connection has ended.
    pub fn is_closed(&self) -> bool {
        self.closed.is_some()
    }

    fn set_read_timeout(&mut self, wait: Duration) -> Result<(), WsError> {
        let wait = wait.max(Duration::from_millis(1));
        if self.read_timeout != Some(wait) {
            self.ws.get_ref().tcp().set_read_timeout(Some(wait)).map_err(|_| Fault::Unreachable)?;
            self.read_timeout = Some(wait);
        }
        Ok(())
    }

    /// The next message, waiting at most `wait`. Pings and pongs are answered by the library and not delivered. A message
    /// over the limits ends the connection with [`WsError::TooLarge`], a rule broken by the peer with
    /// [`Fault::Protocol`]. After a [`Recv::Timeout`] the connection is as usable as before.
    pub fn recv(&mut self, wait: Duration) -> Result<Recv, WsError> {
        if let Some(code) = self.closed {
            return Ok(Recv::Closed(code));
        }
        let deadline = Instant::now() + wait;
        loop {
            self.set_read_timeout(deadline.saturating_duration_since(Instant::now()))?;
            match self.ws.read() {
                Ok(Message::Text(text)) => return Ok(Recv::Text(text.as_str().to_owned())),
                Ok(Message::Binary(bytes)) => return Ok(Recv::Binary(bytes.to_vec())),
                Ok(Message::Close(frame)) => {
                    let code = frame.map_or(1005, |f| u16::from(f.code));
                    self.closed = Some(code);
                    // The library has queued its answer to the close; send it.
                    let _ = self.ws.flush();
                    return Ok(Recv::Closed(code));
                }
                Ok(_) => {
                    // A ping, a pong: the library answers it on the next write. Bounded by the deadline like any wait.
                    let _ = self.ws.flush();
                    if Instant::now() >= deadline {
                        return Ok(Recv::Timeout);
                    }
                }
                Err(tungstenite::Error::Io(e)) if is_timeout(&e) => return Ok(Recv::Timeout),
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    self.closed = Some(1006);
                    return Ok(Recv::Closed(1006));
                }
                // The peer's side of the connection ended with no close frame: a network that failed, not a rule that was broken.
                Err(tungstenite::Error::Protocol(tungstenite::error::ProtocolError::ResetWithoutClosingHandshake)) => {
                    self.closed = Some(1006);
                    return Ok(Recv::Closed(1006));
                }
                Err(e) => return Err(map_error(e)),
            }
        }
    }

    fn send(&mut self, message: Message) -> Result<(), WsError> {
        if self.closed.is_some() {
            return Err(WsError::Closed);
        }
        self.ws.send(message).map_err(map_error)
    }

    /// Sends a text message of at most [`WsLimits::max_message`] bytes.
    pub fn send_text(&mut self, text: &str) -> Result<(), WsError> {
        if text.len() > self.limits.max_message {
            return Err(WsError::TooLarge);
        }
        self.send(Message::text(text))
    }

    /// Sends a binary message of at most [`WsLimits::max_message`] bytes.
    pub fn send_binary(&mut self, bytes: &[u8]) -> Result<(), WsError> {
        if bytes.len() > self.limits.max_message {
            return Err(WsError::TooLarge);
        }
        self.send(Message::binary(Bytes::copy_from_slice(bytes)))
    }

    /// Sends a ping with no payload.
    pub fn ping(&mut self) -> Result<(), WsError> {
        self.send(Message::Ping(Bytes::new()))
    }

    /// Ends the connection with `code` and no reason, and waits up to a second for the answer of the peer so that the close
    /// handshake finishes. Closing a closed connection is not an error.
    pub fn close(&mut self, code: u16) -> Result<(), WsError> {
        if self.closed.is_some() {
            return Ok(());
        }
        let frame = CloseFrame { code: CloseCode::from(code), reason: "".into() };
        self.ws.close(Some(frame)).map_err(map_error)?;
        self.closed = Some(code);
        let deadline = Instant::now() + CLOSE_WAIT;
        while Instant::now() < deadline {
            self.set_read_timeout(deadline.saturating_duration_since(Instant::now()))?;
            match self.ws.read() {
                Ok(_) => {}
                Err(_) => break,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<WsUrl, UrlError> {
        WsUrl::parse(text)
    }

    #[test]
    fn a_secure_address_is_read_with_its_defaults() {
        let url = parse("wss://host.example").unwrap();
        assert_eq!((url.secure(), url.host(), url.port(), url.path()), (true, "host.example", 443, "/"));
        let url = parse("wss://host.example:8443/ws/device").unwrap();
        assert_eq!((url.secure(), url.host(), url.port(), url.path()), (true, "host.example", 8443, "/ws/device"));
        let url = parse("wss://[2001:db8::1]:9000/x").unwrap();
        assert_eq!((url.host(), url.port()), ("2001:db8::1", 9000));
        assert_eq!(url.to_uri_text(), "wss://[2001:db8::1]:9000/x");
        assert_eq!(parse("wss://a.example/").unwrap().to_uri_text(), "wss://a.example/");
        assert_eq!(parse("wss://a.example:443/p").unwrap().to_uri_text(), "wss://a.example/p");
    }

    #[test]
    fn a_plain_address_is_for_a_loopback_host_only() {
        for ok in ["ws://127.0.0.1:8080/x", "ws://localhost/x", "ws://[::1]:1/x", "ws://127.9.9.9"] {
            let url = parse(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
            assert!(!url.secure(), "{ok}");
        }
        for bad in ["ws://example.com", "ws://10.0.0.1", "ws://[2001:db8::1]", "ws://localhost.example.com", "ws://127.0.0.1.example"] {
            assert_eq!(parse(bad), Err(UrlError::Insecure), "{bad}");
        }
    }

    #[test]
    fn what_cannot_carry_a_token_is_refused() {
        assert_eq!(parse("https://host.example/ws"), Err(UrlError::Scheme));
        assert_eq!(parse("http://127.0.0.1/ws"), Err(UrlError::Scheme));
        assert_eq!(parse("host.example"), Err(UrlError::Scheme));
        assert_eq!(parse("WSS://host.example"), Err(UrlError::Scheme));
        assert_eq!(parse("wss://user:pw@host.example/ws"), Err(UrlError::Userinfo));
        assert_eq!(parse("wss://user@host.example"), Err(UrlError::Userinfo));
        assert_eq!(parse("wss://host.example/ws?x=1"), Err(UrlError::Query));
        assert_eq!(parse("wss://host.example?x=1"), Err(UrlError::Query));
        assert_eq!(parse("wss://host.example/ws#frag"), Err(UrlError::Fragment));
        assert_eq!(parse("wss://"), Err(UrlError::Host));
        assert_eq!(parse("wss:///ws"), Err(UrlError::Host));
        assert_eq!(parse("wss://:443/ws"), Err(UrlError::Host));
        assert_eq!(parse("wss://bad host/ws"), Err(UrlError::Host));
        assert_eq!(parse("wss://-bad.example"), Err(UrlError::Host));
        assert_eq!(parse("wss://a..b"), Err(UrlError::Host));
        assert_eq!(parse("wss://[not-an-address]/ws"), Err(UrlError::Host));
        assert_eq!(parse("wss://[::1/ws"), Err(UrlError::Host));
        assert_eq!(parse("wss://host.example:0/ws"), Err(UrlError::Port));
        assert_eq!(parse("wss://host.example:65536/ws"), Err(UrlError::Port));
        assert_eq!(parse("wss://host.example:/ws"), Err(UrlError::Port));
        assert_eq!(parse("wss://host.example:80a/ws"), Err(UrlError::Port));
        assert_eq!(parse("wss://host.example/a b"), Err(UrlError::Path));
        assert_eq!(parse("wss://host.example/a%20b"), Err(UrlError::Path));
    }

    #[test]
    fn the_bounds_of_an_address_hold() {
        let long_host = format!("wss://{}.example/ws", "a".repeat(MAX_HOST));
        assert_eq!(parse(&long_host), Err(UrlError::Host));
        let ok_host = format!("wss://{}/ws", "a".repeat(MAX_HOST));
        assert!(parse(&ok_host).is_ok());
        assert_eq!(parse(&format!("wss://host.example/{}", "p".repeat(MAX_PATH))), Err(UrlError::Path));
        assert!(parse(&format!("wss://host.example/{}", "p".repeat(MAX_PATH - 1))).is_ok());
        assert_eq!(parse(&format!("wss://host.example/{}", "p".repeat(MAX_URL))), Err(UrlError::TooLong));
    }

    #[test]
    fn no_refusal_echoes_the_address() {
        for bad in ["wss://user:secret-pw@host.example/ws", "https://secret-host.example/ws", "wss://host.example/ws?token=secret"] {
            let e = parse(bad).unwrap_err();
            assert!(!e.to_string().contains("secret"), "{e}");
        }
    }

    #[test]
    fn the_limits_are_clamped_and_the_frame_never_exceeds_the_message() {
        let d = WsLimits::default();
        assert_eq!((d.max_message, d.max_frame), (8 * 1024 * 1024, 1024 * 1024));
        let tiny = WsLimits::new(0, 0);
        assert_eq!((tiny.max_message, tiny.max_frame), (4096, 1024));
        let huge = WsLimits::new(usize::MAX, usize::MAX);
        assert_eq!((huge.max_message, huge.max_frame), (16 * 1024 * 1024, 4 * 1024 * 1024));
        let small = WsLimits::new(8192, 1024 * 1024);
        assert_eq!(small.max_frame, 8192);
        // A struct built by hand is clamped again at connect.
        let by_hand = WsLimits { max_message: usize::MAX, max_frame: usize::MAX, connect_timeout: Duration::ZERO, handshake_timeout: Duration::MAX, write_timeout: Duration::ZERO };
        let c = by_hand.clamped();
        assert_eq!((c.max_message, c.max_frame), (16 * 1024 * 1024, 4 * 1024 * 1024));
        assert_eq!((c.connect_timeout, c.handshake_timeout), (Duration::from_millis(1), Duration::from_secs(60)));
    }
}
