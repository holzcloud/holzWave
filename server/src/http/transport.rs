//! The transport seam under the WebSocket, so that the framing library can change without touching the route.
//!
//! The endpoint talks to a socket only through [`Transport`]: a frame is text, binary or a pong (the sign that the peer is
//! there), and there are four things to do with a socket: read a frame, write a message, ping and close. The one
//! implementation, [`AxumTransport`], is axum's WebSocket without compression. This is the only file that names the
//! WebSocket library; [`select`] is the one place that decides which implementation a configuration gets.
//!
//! Every size is fixed when the socket is accepted ([`SocketLimits`]); a frame or a message over it ends the socket with a
//! [`TransportError::TooLarge`] and no byte of it is handed on. No error holds a byte of what the peer sent.

use std::future::Future;
use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};

use super::router::ApiError;
use crate::config::Compression;

/// What the peer sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    /// A text message: valid UTF-8, already assembled from its fragments.
    Text(String),
    /// A binary message; only its size is kept, because the endpoint takes none.
    Binary(usize),
    /// A pong, or a ping of the peer's own (the implementation answers it): the sign that the peer is there.
    Pong,
}

/// What the endpoint writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outgoing {
    /// A text message.
    Text(String),
    /// A binary message.
    Binary(Vec<u8>),
}

/// Why a read or a write did not give a message. The variants are fixed words; none holds a byte of the peer's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    /// The peer closed the connection, or it broke.
    Closed,
    /// A message or a frame is larger than the limit fixed at accept time.
    TooLarge,
    /// A text message is not valid UTF-8.
    Encoding,
    /// The peer broke the WebSocket protocol.
    Protocol,
}

/// A WebSocket as the endpoint sees it. The futures borrow the transport, so a caller that wants to read while a write is
/// pending drops the write (the transport is closed or dropped after that).
pub trait Transport: Send {
    /// The next message from the peer, waiting for it. Cancel-safe.
    fn recv(&mut self) -> impl Future<Output = Result<Inbound, TransportError>> + Send;
    /// Writes one message and flushes it.
    fn send(&mut self, message: Outgoing) -> impl Future<Output = Result<(), TransportError>> + Send;
    /// Writes a ping.
    fn ping(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send;
    /// Writes a close message with `code` and `reason` and gives the peer a short time to answer it.
    fn close(&mut self, code: u16, reason: &'static str) -> impl Future<Output = ()> + Send;
}

/// The sizes of a socket, fixed when it is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketLimits {
    /// The largest message, and the largest frame, the peer may send.
    pub message_bytes: usize,
    /// The bytes written before a flush is forced.
    pub write_buffer_bytes: usize,
    /// The bytes that may wait to be written; the largest message the endpoint writes must fit.
    pub max_write_buffer_bytes: usize,
}

impl SocketLimits {
    /// The limits of a message cap: the write buffer holds the largest answer the supervisor passes on.
    pub fn for_message_bytes(message_bytes: usize) -> SocketLimits {
        SocketLimits { message_bytes, write_buffer_bytes: 128 * 1024, max_write_buffer_bytes: 32 * 1024 * 1024 }
    }
}

/// How long the peer gets to answer a close message before the connection is dropped.
const CLOSE_DRAIN: Duration = Duration::from_millis(1000);

/// The upgrade of a request, with the library's type kept inside this file. A request that is no WebSocket upgrade is 400
/// `bad_request`.
pub struct Upgrade(WebSocketUpgrade);

impl<S: Send + Sync> FromRequestParts<S> for Upgrade {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Upgrade, ApiError> {
        WebSocketUpgrade::from_request_parts(parts, state)
            .await
            .map(Upgrade)
            .map_err(|_| ApiError::BadRequest { field: None })
    }
}

impl Upgrade {
    /// Answers the upgrade (101) and, once the connection is the person's, runs `serve` on the transport. The sizes are those
    /// of `limits`; the transport is the one [`select`] chose for the configuration.
    pub fn accept<F, Fut>(self, limits: SocketLimits, serve: F) -> Response
    where
        F: FnOnce(AxumTransport) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.0
            .max_message_size(limits.message_bytes)
            .max_frame_size(limits.message_bytes)
            .write_buffer_size(limits.write_buffer_bytes)
            .max_write_buffer_size(limits.max_write_buffer_bytes)
            .on_upgrade(move |socket| serve(AxumTransport { socket }))
            .into_response()
    }
}

/// axum's WebSocket, without compression.
#[derive(Debug)]
pub struct AxumTransport {
    socket: WebSocket,
}

/// The kind of an error of the library, from the start of its text and nothing else: the rest of the text may hold bytes of the
/// peer and is dropped here.
fn classify(error: &axum::Error) -> TransportError {
    let text = error.to_string();
    if text.starts_with("Space limit exceeded") {
        TransportError::TooLarge
    } else if text.starts_with("UTF-8 encoding error") {
        TransportError::Encoding
    } else if text.starts_with("WebSocket protocol error") {
        TransportError::Protocol
    } else {
        TransportError::Closed
    }
}

impl Transport for AxumTransport {
    async fn recv(&mut self) -> Result<Inbound, TransportError> {
        match self.socket.recv().await {
            None => Err(TransportError::Closed),
            Some(Ok(Message::Close(_))) => {
                // The library answers the close of the peer at the next read; reading once more sends the answer out.
                let _ = tokio::time::timeout(CLOSE_DRAIN, self.socket.recv()).await;
                Err(TransportError::Closed)
            }
            Some(Err(error)) => Err(classify(&error)),
            Some(Ok(Message::Text(text))) => Ok(Inbound::Text(text.as_str().to_owned())),
            Some(Ok(Message::Binary(bytes))) => Ok(Inbound::Binary(bytes.len())),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => Ok(Inbound::Pong),
        }
    }

    async fn send(&mut self, message: Outgoing) -> Result<(), TransportError> {
        let message = match message {
            Outgoing::Text(text) => Message::Text(Utf8Bytes::from(text)),
            Outgoing::Binary(bytes) => Message::Binary(bytes.into()),
        };
        self.socket.send(message).await.map_err(|_| TransportError::Closed)
    }

    async fn ping(&mut self) -> Result<(), TransportError> {
        self.socket.send(Message::Ping(Vec::new().into())).await.map_err(|_| TransportError::Closed)
    }

    async fn close(&mut self, code: u16, reason: &'static str) {
        let frame = CloseFrame { code, reason: Utf8Bytes::from_static(reason) };
        if self.socket.send(Message::Close(Some(frame))).await.is_err() {
            return;
        }
        // The peer's answer is read so that the connection is not reset before the close message arrives.
        let _ = tokio::time::timeout(CLOSE_DRAIN, async {
            while let Some(Ok(_)) = self.socket.recv().await {}
        })
        .await;
    }
}

/// Whether this build has a compressing WebSocket. It has none: the measurement of `permessage-deflate` did not pass its
/// rule, so the extension is never offered and a configuration that asks for it is refused with [`check`]'s sentence.
pub const COMPRESSED_AVAILABLE: bool = false;

/// The one check of the configured `transport.compression`: `Ok` when this build can honour it, and otherwise the sentence
/// that says what to change. [`select`] and the configuration check (`serve` and `check-config`) both call it, so the two
/// never disagree and the words exist once.
pub fn check(compression: Compression) -> Result<(), &'static str> {
    match compression {
        Compression::Off => Ok(()),
        Compression::Deflate if COMPRESSED_AVAILABLE => Ok(()),
        Compression::Deflate => {
            Err("transport.compression is deflate, but compression is not available in this build; it must be off")
        }
    }
}

/// The transport a configuration asks for: the one implementation for `off`, and the sentence that stops the server from
/// starting for `deflate` (no compressing transport is built; running uncompressed in silence would be a lie).
pub fn select(compression: Compression) -> Result<Kind, &'static str> {
    check(compression)?;
    Ok(Kind::Plain)
}

/// Which transport [`select`] chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// [`AxumTransport`]: no compression.
    Plain,
}
