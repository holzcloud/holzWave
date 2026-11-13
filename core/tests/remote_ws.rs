//! The WebSocket client of the core (`holzwave_core::remote::ws`), against peers in this process.
//!
//! No server of holzWave is involved: the peer is tungstenite's own server side on a loopback socket (and a rustls server for
//! the TLS cases), so what is proved is the client alone. The peer records what it was sent; a test says what the client must
//! do and what the peer must have seen:
//!
//! - the handshake carries `Authorization: Bearer <token>` and no `Origin`, text and binary round trip, and `close` ends with
//!   the code given;
//! - a message over the limit, a frame that announces a huge length, a message that is assembled over the limit, text that is
//!   not UTF-8 and a masked frame from the server each end the connection with a typed error, without a crash or an
//!   allocation of the announced size;
//! - a read timeout leaves the connection usable;
//! - a refused upgrade is one of three faults and carries no text of the server;
//! - the TLS configuration of the platform is in the path: a certificate that is not trusted is refused, and the same
//!   certificate is accepted when (and only when) the caller's configuration trusts it.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use holzwave_core::remote::ws::{Recv, WsClient, WsError, WsLimits, WsUrl, platform_tls};
use holzwave_core::server_link::{Fault, Token};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http;
use tungstenite::{Message, WebSocket};

/// A token with the shape of a real one and text that is easy to find in any output.
const TOKEN: &str = "hwd_canaryCANARY-0123456789_abcdefghijklmnopqrs";

/// A word the peer puts into its refusals: it must never come back in an error of the client.
const SERVER_WORDS: &str = "server-words-that-must-not-be-echoed";

fn token() -> Token {
    Token::parse(TOKEN).expect("the test token has the shape of a token")
}

/// What the peer saw of the upgrade request.
#[derive(Clone, Debug, Default)]
struct Seen {
    authorization: Option<String>,
    has_origin: bool,
    path: String,
}

type SeenSlot = Arc<Mutex<Option<Seen>>>;

fn record(request: &Request, slot: &SeenSlot) {
    let headers = request.headers();
    let seen = Seen {
        authorization: headers.get("authorization").and_then(|v| v.to_str().ok()).map(str::to_owned),
        has_origin: headers.contains_key("origin"),
        path: request.uri().path().to_owned(),
    };
    *slot.lock().unwrap() = Some(seen);
}

struct Peer {
    addr: SocketAddr,
    seen: SeenSlot,
    thread: JoinHandle<()>,
}

impl Peer {
    fn url(&self) -> WsUrl {
        WsUrl::parse(&format!("ws://{}/ws/device", self.addr)).expect("a loopback address")
    }

    fn seen(&self) -> Seen {
        self.seen.lock().unwrap().clone().expect("the peer saw an upgrade request")
    }

    fn finish(self) {
        self.thread.join().expect("the peer thread ended without a panic");
    }
}

/// A peer that accepts one connection, upgrades it, and hands the socket to `script`.
fn plain_peer(script: impl FnOnce(WebSocket<TcpStream>) + Send + 'static) -> Peer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let addr = listener.local_addr().expect("local address");
    let seen: SeenSlot = Arc::default();
    let slot = seen.clone();
    let thread = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("a connection");
        let ws = tungstenite::accept_hdr(stream, |request: &Request, response: Response| {
            record(request, &slot);
            Ok(response)
        })
        .expect("the upgrade succeeds");
        script(ws);
    });
    Peer { addr, seen, thread }
}

/// A peer that answers the upgrade with `status`, a body of server words and, for a redirect, a `Location`.
fn refusing_peer(status: u16) -> Peer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let addr = listener.local_addr().expect("local address");
    let seen: SeenSlot = Arc::default();
    let slot = seen.clone();
    let thread = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("a connection");
        let refused = tungstenite::accept_hdr(stream, |request: &Request, _: Response| {
            record(request, &slot);
            let response: ErrorResponse = http::Response::builder()
                .status(status)
                .header("Location", "https://elsewhere.invalid/ws")
                .body(Some(SERVER_WORDS.to_owned()))
                .expect("a response");
            Err(response)
        });
        assert!(refused.is_err(), "the peer refused the upgrade");
    });
    Peer { addr, seen, thread }
}

/// An unmasked frame as a server writes it.
fn frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![(u8::from(fin) << 7) | opcode];
    match payload.len() {
        n if n < 126 => out.push(n as u8),
        n if n <= 0xffff => {
            out.push(126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
    out
}

fn write_raw(ws: &mut WebSocket<TcpStream>, bytes: &[u8]) {
    ws.get_mut().write_all(bytes).expect("raw bytes");
    ws.get_mut().flush().expect("flush");
}

/// Blocks until the other end is gone, so the socket stays open while the client is judged.
fn wait_for_the_client(ws: &mut WebSocket<TcpStream>) {
    while ws.read().is_ok() {}
}

const WAIT: Duration = Duration::from_secs(10);

fn expect_error(client: &mut WsClient) -> WsError {
    match client.recv(WAIT) {
        Err(e) => e,
        Ok(other) => panic!("expected an error, got {other:?}"),
    }
}

// ---- handshake, header, round trip, close ----

#[test]
fn the_handshake_carries_the_bearer_and_no_origin_and_messages_round_trip_and_close_ends_with_the_code() {
    let closed_with = Arc::new(Mutex::new(None));
    let record_close = closed_with.clone();
    let peer = plain_peer(move |mut ws| {
        let text = ws.read().expect("text").into_text().expect("a text message");
        ws.send(Message::text(format!("echo:{}", text.as_str()))).expect("echo text");
        let bytes = ws.read().expect("binary").into_data();
        let mut reversed = bytes.to_vec();
        reversed.reverse();
        ws.send(Message::binary(reversed)).expect("echo binary");
        // The close of the client, with its code; the library answers it on this read.
        loop {
            match ws.read() {
                Ok(Message::Close(frame)) => *record_close.lock().unwrap() = frame.map(|f| u16::from(f.code)),
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    let mut client = WsClient::connect(&peer.url(), &token(), None, &WsLimits::default()).expect("the connection opens");
    client.send_text("hello").expect("send text");
    assert_eq!(client.recv(WAIT), Ok(Recv::Text("echo:hello".into())));
    client.send_binary(&[1, 2, 3]).expect("send binary");
    assert_eq!(client.recv(WAIT), Ok(Recv::Binary(vec![3, 2, 1])));

    let shown = format!("{client:?}");
    assert!(!shown.contains("canary"), "{shown}");
    client.close(4001).expect("close");
    assert!(client.is_closed());
    assert_eq!(client.recv(Duration::from_millis(10)), Ok(Recv::Closed(4001)), "every later call says the same");
    assert_eq!(client.send_text("late"), Err(WsError::Closed));
    drop(client);

    let seen = peer.seen();
    assert_eq!(seen.authorization.as_deref(), Some(format!("Bearer {TOKEN}").as_str()));
    assert!(!seen.has_origin, "no Origin is sent");
    assert_eq!(seen.path, "/ws/device");
    peer.finish();
    assert_eq!(*closed_with.lock().unwrap(), Some(4001), "the peer read the close with the code given");
}

#[test]
fn a_close_from_the_peer_is_delivered_with_its_code() {
    let peer = plain_peer(|mut ws| {
        ws.close(Some(tungstenite::protocol::CloseFrame { code: 1013u16.into(), reason: "".into() })).expect("close");
        wait_for_the_client(&mut ws);
    });
    let mut client = WsClient::connect(&peer.url(), &token(), None, &WsLimits::default()).expect("the connection opens");
    assert_eq!(client.recv(WAIT), Ok(Recv::Closed(1013)));
    assert_eq!(client.recv(WAIT), Ok(Recv::Closed(1013)));
    drop(client);
    peer.finish();
}

// ---- the bounds ----

fn limited() -> WsLimits {
    WsLimits::new(8192, 4096)
}

#[test]
fn a_frame_over_the_limit_is_too_large() {
    let peer = plain_peer(|mut ws| {
        write_raw(&mut ws, &frame(true, 2, &vec![7u8; 10_000]));
        wait_for_the_client(&mut ws);
    });
    let mut client = WsClient::connect(&peer.url(), &token(), None, &limited()).expect("the connection opens");
    assert_eq!(expect_error(&mut client), WsError::TooLarge);
    drop(client);
    peer.finish();
}

#[test]
fn a_frame_that_announces_a_huge_length_is_refused_before_any_payload_is_read() {
    // Only the header of a frame of 1 GiB is sent, and no payload ever follows: a client that waited for the payload, or
    // reserved it, would not answer within the time.
    let peer = plain_peer(|mut ws| {
        let mut header = vec![0x82, 127];
        header.extend_from_slice(&(1u64 << 30).to_be_bytes());
        write_raw(&mut ws, &header);
        wait_for_the_client(&mut ws);
    });
    let mut client = WsClient::connect(&peer.url(), &token(), None, &limited()).expect("the connection opens");
    let started = Instant::now();
    assert_eq!(expect_error(&mut client), WsError::TooLarge);
    assert!(started.elapsed() < Duration::from_secs(5), "the refusal did not wait for the payload");
    drop(client);
    peer.finish();
}

#[test]
fn a_message_assembled_over_the_limit_from_small_frames_is_too_large() {
    let peer = plain_peer(|mut ws| {
        // Three fragments of 3000 bytes: every frame is within the frame limit, the message is over the message limit.
        write_raw(&mut ws, &frame(false, 2, &[1u8; 3000]));
        write_raw(&mut ws, &frame(false, 0, &[2u8; 3000]));
        write_raw(&mut ws, &frame(true, 0, &[3u8; 3000]));
        wait_for_the_client(&mut ws);
    });
    let mut client = WsClient::connect(&peer.url(), &token(), None, &limited()).expect("the connection opens");
    assert_eq!(expect_error(&mut client), WsError::TooLarge);
    drop(client);
    peer.finish();
}

#[test]
fn a_message_within_the_limit_that_is_sent_in_fragments_is_delivered_whole() {
    let peer = plain_peer(|mut ws| {
        write_raw(&mut ws, &frame(false, 2, &[1u8; 3000]));
        write_raw(&mut ws, &frame(true, 0, &[2u8; 3000]));
        wait_for_the_client(&mut ws);
    });
    let mut client = WsClient::connect(&peer.url(), &token(), None, &limited()).expect("the connection opens");
    match client.recv(WAIT) {
        Ok(Recv::Binary(bytes)) => {
            assert_eq!(bytes.len(), 6000);
            assert!(bytes[..3000].iter().all(|b| *b == 1) && bytes[3000..].iter().all(|b| *b == 2));
        }
        other => panic!("expected the whole message, got {other:?}"),
    }
    drop(client);
    peer.finish();
}

#[test]
fn text_that_is_not_utf8_is_a_protocol_error() {
    let peer = plain_peer(|mut ws| {
        write_raw(&mut ws, &frame(true, 1, &[0xff, 0xfe, 0xfd]));
        wait_for_the_client(&mut ws);
    });
    let mut client = WsClient::connect(&peer.url(), &token(), None, &WsLimits::default()).expect("the connection opens");
    assert_eq!(expect_error(&mut client), WsError::Fault(Fault::Protocol));
    drop(client);
    peer.finish();
}

#[test]
fn a_masked_frame_from_the_server_is_a_protocol_error() {
    let peer = plain_peer(|mut ws| {
        // The mask bit set on a frame that goes from a server to a client: RFC 6455 forbids it.
        write_raw(&mut ws, &[0x81, 0x82, 1, 2, 3, 4, b'h' ^ 1, b'i' ^ 2]);
        wait_for_the_client(&mut ws);
    });
    let mut client = WsClient::connect(&peer.url(), &token(), None, &WsLimits::default()).expect("the connection opens");
    assert_eq!(expect_error(&mut client), WsError::Fault(Fault::Protocol));
    drop(client);
    peer.finish();
}

#[test]
fn a_message_over_the_limit_is_not_sent() {
    let peer = plain_peer(|mut ws| wait_for_the_client(&mut ws));
    let mut client = WsClient::connect(&peer.url(), &token(), None, &limited()).expect("the connection opens");
    assert_eq!(client.send_text(&"x".repeat(8193)), Err(WsError::TooLarge));
    assert_eq!(client.send_binary(&vec![0u8; 8193]), Err(WsError::TooLarge));
    client.send_binary(&vec![0u8; 8192]).expect("a message at the limit is sent");
    drop(client);
    peer.finish();
}

// ---- the read timeout ----

#[test]
fn a_read_timeout_returns_and_the_connection_is_usable_afterwards() {
    let (go, wait_for_go) = mpsc::channel::<()>();
    let peer = plain_peer(move |mut ws| {
        wait_for_go.recv().expect("the test says go");
        ws.send(Message::text("late")).expect("send late");
        let text = ws.read().expect("a message after the timeouts").into_text().expect("text");
        ws.send(Message::text(format!("echo:{}", text.as_str()))).expect("echo");
        wait_for_the_client(&mut ws);
    });
    let mut client = WsClient::connect(&peer.url(), &token(), None, &WsLimits::default()).expect("the connection opens");
    for _ in 0..2 {
        let started = Instant::now();
        assert_eq!(client.recv(Duration::from_millis(100)), Ok(Recv::Timeout));
        assert!(started.elapsed() >= Duration::from_millis(90), "the call waited for the time given");
    }
    assert!(!client.is_closed());
    go.send(()).expect("go");
    assert_eq!(client.recv(WAIT), Ok(Recv::Text("late".into())));
    client.send_text("after").expect("send after the timeouts");
    assert_eq!(client.recv(WAIT), Ok(Recv::Text("echo:after".into())));
    drop(client);
    peer.finish();
}

#[test]
fn a_timeout_in_the_middle_of_a_frame_keeps_what_was_read_and_the_rest_completes_the_message() {
    let (go, wait_for_go) = mpsc::channel::<()>();
    let peer = plain_peer(move |mut ws| {
        let whole = frame(true, 2, &[9u8; 3000]);
        // The header and a part of the payload now, the rest only when the test says so.
        write_raw(&mut ws, &whole[..1000]);
        wait_for_go.recv().expect("the test says go");
        write_raw(&mut ws, &whole[1000..]);
        wait_for_the_client(&mut ws);
    });
    let mut client = WsClient::connect(&peer.url(), &token(), None, &WsLimits::default()).expect("the connection opens");
    assert_eq!(client.recv(Duration::from_millis(150)), Ok(Recv::Timeout), "a part of a frame is not a message");
    assert_eq!(client.recv(Duration::from_millis(50)), Ok(Recv::Timeout));
    assert!(!client.is_closed());
    go.send(()).expect("go");
    assert_eq!(client.recv(WAIT), Ok(Recv::Binary(vec![9u8; 3000])), "the part that was read before the timeout is not lost");
    drop(client);
    peer.finish();
}

#[test]
fn a_ping_is_answered_and_does_not_end_a_wait_early() {
    let peer = plain_peer(|mut ws| {
        ws.send(Message::Ping(tungstenite::Bytes::new())).expect("ping");
        // The pong comes back on the client's next write or read.
        let mut got_pong = false;
        while let Ok(message) = ws.read() {
            if matches!(message, Message::Pong(_)) {
                got_pong = true;
                break;
            }
        }
        assert!(got_pong, "the client answered the ping");
        ws.send(Message::text("after-ping")).expect("text");
        wait_for_the_client(&mut ws);
    });
    let mut client = WsClient::connect(&peer.url(), &token(), None, &WsLimits::default()).expect("the connection opens");
    // The ping is read and answered inside the wait; the text that follows is the delivery.
    assert_eq!(client.recv(WAIT), Ok(Recv::Text("after-ping".into())));
    drop(client);
    peer.finish();
}

// ---- refusals ----

fn refusal_of(status: u16) -> WsError {
    let peer = refusing_peer(status);
    let url = peer.url();
    let Err(e) = WsClient::connect(&url, &token(), None, &WsLimits::default()) else {
        panic!("status {status} must not open a connection");
    };
    peer.finish();
    // The error carries nothing the server wrote, and never the token.
    let shown = format!("{e} {e:?}");
    assert!(!shown.contains(SERVER_WORDS) && !shown.contains("elsewhere") && !shown.contains("canary"), "{shown}");
    e
}

#[test]
fn a_refused_upgrade_is_one_of_three_typed_faults_without_a_server_text() {
    assert_eq!(refusal_of(401), WsError::Fault(Fault::SignedOut));
    assert_eq!(refusal_of(404), WsError::Fault(Fault::Off));
    assert_eq!(refusal_of(500), WsError::Fault(Fault::Refused));
    assert_eq!(refusal_of(403), WsError::Fault(Fault::Refused));
    assert_eq!(refusal_of(302), WsError::Fault(Fault::Refused), "a redirect is refused, not followed");
    assert_eq!(refusal_of(401).to_string(), Fault::SignedOut.sentence());
}

#[test]
fn nothing_listening_is_unreachable() {
    let port = TcpListener::bind("127.0.0.1:0").map(|l| l.local_addr().map_or(0, |a| a.port())).unwrap_or(0);
    let url = WsUrl::parse(&format!("ws://127.0.0.1:{port}/ws/device")).expect("an address");
    let Err(e) = WsClient::connect(&url, &token(), None, &WsLimits::default()) else { panic!("nothing listens on the port") };
    assert_eq!(e, WsError::Fault(Fault::Unreachable));
}

#[test]
fn a_peer_that_does_not_upgrade_ends_the_handshake_in_time() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let addr = listener.local_addr().expect("address");
    let thread = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("a connection");
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        // Never answers; stays open until the client has given up.
        let _ = stream.read(&mut buf);
    });
    let url = WsUrl::parse(&format!("ws://{addr}/ws/device")).expect("an address");
    let mut limits = WsLimits::default();
    limits.handshake_timeout = Duration::from_millis(300);
    let started = Instant::now();
    let Err(e) = WsClient::connect(&url, &token(), None, &limits) else { panic!("a silent peer is no connection") };
    assert_eq!(e, WsError::Fault(Fault::Unreachable));
    assert!(started.elapsed() < Duration::from_secs(5), "the handshake is bounded");
    thread.join().expect("the peer ended");
}

#[test]
fn a_peer_that_answers_the_upgrade_one_byte_at_a_time_cannot_hold_the_handshake_beyond_its_bound() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let addr = listener.local_addr().expect("address");
    let thread = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("a connection");
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        // An answer that never ends, a byte every 40 ms: never silent for long, so a bound on each read alone never trips.
        let answer = format!("HTTP/1.1 101 Switching Protocols\r\nX-Slow: {}", "a".repeat(400));
        for byte in answer.bytes() {
            if stream.write_all(&[byte]).is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(40));
        }
    });
    let url = WsUrl::parse(&format!("ws://{addr}/ws/device")).expect("an address");
    let mut limits = WsLimits::default();
    limits.handshake_timeout = Duration::from_millis(300);
    let started = Instant::now();
    let Err(e) = WsClient::connect(&url, &token(), None, &limits) else { panic!("an answer that never ends is no connection") };
    let took = started.elapsed();
    assert_eq!(e, WsError::Fault(Fault::Unreachable));
    assert!(took < Duration::from_secs(2), "the whole handshake is bounded, it took {took:?}");
    thread.join().expect("the peer ended");
}

// ---- TLS ----

/// A self-signed certificate for `127.0.0.1` and `localhost` (valid for a century) and its key, made once with `openssl req
/// -x509` for these tests and never used for anything else. The bytes are DER in hex.
const CERT_HEX: &str = concat!(
        "3082017e30820123a00302010202145bc0a4311ced4d61ff5df05004736c5e24b5a1b6300a06082a8648ce3d04030230",
        "143112301006035504030c096c6f63616c686f73743020170d3236313131303034343633385a180f3231323631303137",
        "3034343633385a30143112301006035504030c096c6f63616c686f73743059301306072a8648ce3d020106082a8648ce",
        "3d03010703420004dc6dd4f441304ea3f7acb7bda608edb3b3a1a28dcdb92ddf0d5e1b2d87bf9626c54b07a76b2aee63",
        "06b3563a15ef1d640e79591ec14f40f22675ed7068f4cbd5a351304f300c0603551d130101ff04023000300e0603551d",
        "0f0101ff04040302078030130603551d25040c300a06082b06010505070301301a0603551d110413301187047f000001",
        "82096c6f63616c686f7374300a06082a8648ce3d0403020349003046022100cbb490256be562383d4180853d907edb4e",
        "16233d9e62c97e03b58cc7c2770d5e022100fe7f85c5668e5c5418d07a1df1ea9e27c4c664294801f0326d669671082d",
        "820d"
);
const KEY_HEX: &str = concat!(
        "308187020100301306072a8648ce3d020106082a8648ce3d030107046d306b0201010420d3cf70e623894ef47e9c6de6",
        "079b01bfcbe6fbc3f4f6b833fc2423c94668c6e4a14403420004dc6dd4f441304ea3f7acb7bda608edb3b3a1a28dcdb9",
        "2ddf0d5e1b2d87bf9626c54b07a76b2aee6306b3563a15ef1d640e79591ec14f40f22675ed7068f4cbd5"
);

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex")).collect()
}

fn ring() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn server_tls() -> Arc<rustls::ServerConfig> {
    let cert = rustls::pki_types::CertificateDer::from(unhex(CERT_HEX));
    let key = rustls::pki_types::PrivateKeyDer::from(rustls::pki_types::PrivatePkcs8KeyDer::from(unhex(KEY_HEX)));
    Arc::new(
        rustls::ServerConfig::builder_with_provider(ring())
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .expect("the certificate and key fit"),
    )
}

/// A client configuration that trusts exactly the test certificate.
fn trusting_the_test_certificate() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(rustls::pki_types::CertificateDer::from(unhex(CERT_HEX))).expect("a trust anchor");
    Arc::new(
        rustls::ClientConfig::builder_with_provider(ring())
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// What the TLS peer's handshake ended with: `None` when it worked.
type TlsOutcome = Arc<Mutex<Option<String>>>;

/// A TLS peer on a loopback port. `script` gets the upgraded socket when the handshakes work.
fn tls_peer(
    script: impl FnOnce(WebSocket<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>) + Send + 'static,
) -> (Peer, TlsOutcome) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let addr = listener.local_addr().expect("local address");
    let seen: SeenSlot = Arc::default();
    let slot = seen.clone();
    let outcome: TlsOutcome = Arc::default();
    let result = outcome.clone();
    let config = server_tls();
    let thread = thread::spawn(move || {
        let (tcp, _) = listener.accept().expect("a connection");
        let connection = rustls::ServerConnection::new(config).expect("a server connection");
        let stream = rustls::StreamOwned::new(connection, tcp);
        match tungstenite::accept_hdr(stream, |request: &Request, response: Response| {
            record(request, &slot);
            Ok(response)
        }) {
            Ok(ws) => script(ws),
            Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Io(e))) => {
                let alert = e.get_ref().and_then(|inner| inner.downcast_ref::<rustls::Error>());
                *result.lock().unwrap() = Some(match alert {
                    Some(rustls::Error::AlertReceived(alert)) => format!("alert:{alert:?}"),
                    other => format!("other:{other:?} {e}"),
                });
            }
            Err(_) => *result.lock().unwrap() = Some("failure".into()),
        }
    });
    (Peer { addr, seen, thread }, outcome)
}

fn wss_url(peer: &Peer) -> WsUrl {
    WsUrl::parse(&format!("wss://{}/ws/device", peer.addr)).expect("an address")
}

#[test]
fn the_platform_tls_configuration_builds() {
    platform_tls().expect("the ring provider and the platform verifier make a configuration");
}

/// A client configuration with a trust store that holds nothing: every certificate is from an unknown issuer.
fn trusting_nothing() -> Arc<rustls::ClientConfig> {
    Arc::new(
        rustls::ClientConfig::builder_with_provider(ring())
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth(),
    )
}

/// The certificate check is in the path of the client's stream: a certificate the trust store does not hold is refused, the peer
/// sees the client's alert, and no upgrade request, so no token, is ever written.
fn assert_refused_by_the_certificate_check(config: Option<Arc<rustls::ClientConfig>>) -> Duration {
    let (peer, outcome) = tls_peer(|_| panic!("a connection with an untrusted certificate must not be upgraded"));
    let started = Instant::now();
    let Err(e) = WsClient::connect(&wss_url(&peer), &token(), config, &WsLimits::default()) else {
        panic!("a certificate that is not trusted must not be accepted");
    };
    let took = started.elapsed();
    assert_eq!(e, WsError::Tls, "{e}");
    peer.thread.join().expect("the peer ended");
    let outcome = outcome.lock().unwrap().clone().expect("the peer's handshake failed");
    // The client got as far as the certificate, looked at it and said so.
    assert!(outcome.starts_with("alert:"), "{outcome}");
    assert!(peer.seen.lock().unwrap().is_none(), "no upgrade request, so no token, reached the peer");
    took
}

#[test]
fn a_certificate_the_trust_store_does_not_hold_is_refused_and_the_peer_sees_the_alert() {
    let took = assert_refused_by_the_certificate_check(Some(trusting_nothing()));
    assert!(took < Duration::from_secs(8), "{took:?}");
}

/// The same refusal through the configuration of the platform (`None`), which is what the app uses: it proves the platform
/// verifier is the one in the path and that it has no way to accept a self-signed certificate.
///
/// Ignored in the ordinary run because of time, not result: the macOS verifier took between 40 and 80 seconds to answer on the
/// machine this was written on, apparently waiting on the system's trust service (`security verify-cert` on the same
/// certificate answers in 15 ms). Run it with `--ignored`.
#[test]
#[ignore = "the macOS platform verifier needs 40 to 80 seconds to refuse a self-signed certificate on the development machine"]
fn a_self_signed_certificate_is_refused_by_the_platform_verifier() {
    let took = assert_refused_by_the_certificate_check(None);
    eprintln!("the platform verifier refused the certificate after {took:?}");
}

#[test]
fn the_same_certificate_works_when_the_callers_configuration_trusts_it_and_the_header_crosses_tls() {
    let (peer, outcome) = tls_peer(|mut ws| {
        let text = ws.read().expect("text").into_text().expect("a text message");
        ws.send(Message::text(format!("echo:{}", text.as_str()))).expect("echo");
        wait_for_the_client_tls(&mut ws);
    });
    let mut client = WsClient::connect(&wss_url(&peer), &token(), Some(trusting_the_test_certificate()), &WsLimits::default())
        .expect("a trusted certificate opens the connection");
    client.send_text("over tls").expect("send");
    assert_eq!(client.recv(WAIT), Ok(Recv::Text("echo:over tls".into())));
    drop(client);
    let seen = peer.seen();
    assert_eq!(seen.authorization.as_deref(), Some(format!("Bearer {TOKEN}").as_str()));
    assert!(!seen.has_origin);
    peer.finish();
    assert_eq!(*outcome.lock().unwrap(), None);
}

fn wait_for_the_client_tls(ws: &mut WebSocket<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>) {
    while ws.read().is_ok() {}
}

#[test]
fn a_peer_that_does_not_speak_tls_is_a_tls_fault_and_its_words_are_not_echoed() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let addr = listener.local_addr().expect("address");
    let thread = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("a connection");
        let mut hello = [0u8; 4096];
        let _ = stream.read(&mut hello);
        let _ = stream.write_all(format!("HTTP/1.1 400 Bad Request\r\n\r\n{SERVER_WORDS}").as_bytes());
        // Stays open until the client has given up.
        let _ = stream.read(&mut hello);
    });
    let url = WsUrl::parse(&format!("wss://{addr}/ws/device")).expect("an address");
    let Err(e) = WsClient::connect(&url, &token(), None, &WsLimits::default()) else { panic!("plain text is no TLS") };
    assert_eq!(e, WsError::Tls);
    assert!(!format!("{e} {e:?}").contains(SERVER_WORDS));
    thread.join().expect("the peer ended");
}

#[test]
fn the_test_token_has_the_shape_the_client_requires() {
    assert_eq!(TOKEN.len(), 47);
    assert!(Token::parse(TOKEN).is_ok());
}
