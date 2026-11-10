//! A device as the tests of the device socket see it: a token minted through the store, a bearer upgrade request, and a client on
//! top of the hand-written WebSocket client of `ws_env.rs` that files what the server says (answers, events, the two kinds of
//! binary chunk) so that a test can ask for the one thing it wants.
//!
//! Tests pull it in with `#[path = "support/device_socket.rs"] mod device_socket;` next to `edge`, `edge_tls` and `ws_env`.
#![allow(dead_code)]
#![cfg(feature = "in-process-workers")]

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use holzwave_core::remote::proto::{Answer, Chunk, ErrorWord, Event, Input, Op, Reply, Request as Call, Stream};
use holzwave_server::clock::Clock as _;
use holzwave_server::device::DeviceToken;
use holzwave_server::http::features::Feature;
use holzwave_server::store::NewDevice;
use holzwave_server::supervisor::SupervisorSettings;
use holzwave_server::worker::sessions::SessionLimits;

use crate::edge::ORIGIN;
use crate::edge_tls::{Reply as Http, Request};
use crate::ws_env::{BINARY, Got, Incoming, PATIENCE, Person, WsClient, WsEnv, WsOptions};

/// A device the store holds, with the token it signs in with.
pub struct Minted {
    /// The token in its written form.
    pub token: String,
    /// The id of the device row.
    pub id: i64,
}

/// An environment with the real gate (the routes of the devices are mounted) and the remote switch on.
pub async fn env_with(options: WsOptions) -> WsEnv {
    let env = WsEnv::start_with(WsOptions { real_gate: true, ..options }).await;
    remote(&env, true);
    env
}

/// The default environment of these tests.
pub async fn env() -> WsEnv {
    env_with(WsOptions::default()).await
}

/// Settings of the supervisor with a writer timeout of `ms` milliseconds, so that a test never waits for the real one.
pub fn writer_timeout(ms: u32) -> SupervisorSettings {
    SupervisorSettings {
        sessions: SessionLimits { writer_timeout_ms: ms, ..SessionLimits::DEFAULT },
        ..SupervisorSettings::default()
    }
}

/// Turns the remote switch of the application the router serves.
pub fn remote(env: &WsEnv, on: bool) {
    env.state.app.features.set(Feature::Remote, on).expect("the switch is written");
}

/// Turns the sync switch.
pub fn sync(env: &WsEnv, on: bool) {
    env.state.app.features.set(Feature::Sync, on).expect("the switch is written");
}

/// Makes a device of `person` through the store; its token lives `lifetime` seconds from now on the clock of the application.
pub fn mint_for(env: &WsEnv, person: &Person, name: &str, lifetime: i64) -> Minted {
    let token = DeviceToken::generate().expect("a token");
    let now = env.test.clock.unix();
    let device = env
        .test
        .store
        .write(|tx| {
            tx.device_create(&NewDevice {
                token_hash: token.hash(),
                account_id: person.key.id,
                name,
                platform: "linux",
                now,
                expires_at: now + lifetime,
                addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            })
        })
        .expect("a device");
    Minted { token: token.expose().to_owned(), id: device.id }
}

/// A device that lives for a day.
pub fn mint(env: &WsEnv, person: &Person, name: &str) -> Minted {
    mint_for(env, person, name, 86_400)
}

/// The upgrade request of a device: the right Host, no Origin (a native client sends none), and the bearer header when given.
pub fn upgrade(bearer: Option<&str>) -> Request {
    let mut request = Request::new("GET", "/ws/device")
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("Sec-WebSocket-Version", "13");
    if let Some(token) = bearer {
        request = request.header("Authorization", &format!("Bearer {token}"));
    }
    request
}

/// The plain answer to an upgrade that was not switched.
pub async fn refused(env: &WsEnv, request: Request) -> Http {
    match env.try_connect_request(request).await {
        Ok(_) => panic!("the upgrade was switched, and it must not be"),
        Err(reply) => reply,
    }
}

/// The request of a person who revokes one of their own devices, as the page sends it.
pub fn revoke_request(person: &Person, id: i64) -> Request {
    Request::new("DELETE", &format!("/api/me/devices/{id}"))
        .header("Origin", ORIGIN)
        .header("Content-Type", "application/json")
        .cookie(&person.signed.cookie)
        .header("X-CSRF-Token", &person.signed.csrf)
}

/// Revokes a device through the route of the person; the answer must be 204.
pub async fn revoke(env: &WsEnv, person: &Person, id: i64) {
    let reply = env.plain(revoke_request(person, id)).await;
    assert_eq!(reply.status, 204, "the revocation: {}", reply.text());
}

/// A connected device socket and what it has been told so far.
pub struct Device {
    pub ws: WsClient,
    next: u64,
    answers: HashMap<u64, Reply>,
    /// Every event, in the order they came.
    pub events: Vec<Event>,
    /// The bytes of the snapshot chunks, by session.
    pub snapshot: HashMap<u32, Vec<u8>>,
    /// The bytes of the live chunks, by session.
    pub live: HashMap<u32, Vec<u8>>,
    /// The sequence numbers of every chunk, by session, in the order they came.
    pub seqs: HashMap<u32, Vec<u32>>,
    /// The close message, once the server sent one.
    pub closed: Option<(Option<u16>, String)>,
    /// Whether the connection ended.
    pub ended: bool,
    /// The messages that were not understood (there must be none).
    pub strange: Vec<String>,
}

impl Device {
    /// Opens a socket with the token; the upgrade must be switched.
    pub async fn connect(env: &WsEnv, token: &str) -> Device {
        match env.try_connect_request(upgrade(Some(token))).await {
            Ok(ws) => Device::wrap(ws),
            Err(reply) => panic!("the upgrade was answered with {}: {}", reply.status, reply.text()),
        }
    }

    pub fn wrap(ws: WsClient) -> Device {
        Device {
            ws,
            next: 1,
            answers: HashMap::new(),
            events: Vec::new(),
            snapshot: HashMap::new(),
            live: HashMap::new(),
            seqs: HashMap::new(),
            closed: None,
            ended: false,
            strange: Vec::new(),
        }
    }

    /// Reads one message within `within` and files it. False when nothing came (or the connection ended).
    pub async fn pump(&mut self, within: Duration) -> bool {
        match self.ws.read(within).await {
            Got::Message(Incoming::Text(text)) => {
                match Reply::parse(&text) {
                    Ok(Reply::Answer { id, result }) => {
                        let reply = Reply::Answer { id, result };
                        self.answers.insert(id, reply);
                    }
                    Ok(Reply::Event(event)) => self.events.push(event),
                    Err(_) => self.strange.push(text),
                }
                true
            }
            Got::Message(Incoming::Binary(bytes)) => {
                match Chunk::parse(&bytes) {
                    Ok(chunk) => {
                        let table = if chunk.stream == Stream::Snapshot { &mut self.snapshot } else { &mut self.live };
                        table.entry(chunk.session).or_default().extend_from_slice(chunk.bytes);
                        self.seqs.entry(chunk.session).or_default().push(chunk.seq);
                    }
                    Err(_) => self.strange.push(format!("a binary frame of {} bytes", bytes.len())),
                }
                true
            }
            Got::Message(Incoming::Ping(_) | Incoming::Pong(_)) => true,
            Got::Message(Incoming::Close(code, reason)) => {
                self.closed = Some((code, reason));
                true
            }
            Got::Quiet => false,
            Got::Ended => {
                self.ended = true;
                false
            }
        }
    }

    /// Reads until `check` holds, at most `within`; panics with `what` when it never did.
    pub async fn until(&mut self, what: &str, within: Duration, check: impl Fn(&Device) -> bool) {
        let deadline = tokio::time::Instant::now() + within;
        while !check(self) {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(!left.is_zero(), "{what} did not happen in time");
            if !self.pump(left.min(Duration::from_millis(200))).await && self.ended {
                assert!(check(self), "{what} did not happen before the connection ended");
                return;
            }
        }
    }

    /// Whether `check` holds within `within`.
    pub async fn eventually(&mut self, within: Duration, check: impl Fn(&Device) -> bool) -> bool {
        let deadline = tokio::time::Instant::now() + within;
        while !check(self) {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() || self.closed.is_some() || self.ended {
                return check(self);
            }
            self.pump(left.min(Duration::from_millis(100))).await;
        }
        true
    }

    /// Sends a request and returns its number.
    pub async fn send(&mut self, op: Op) -> u64 {
        let id = self.next;
        self.next += 1;
        self.ws.send_text(&Call { id, op }.encode()).await;
        id
    }

    /// The answer to request `id`.
    pub async fn answer(&mut self, id: u64) -> Result<Answer, ErrorWord> {
        self.until("an answer", PATIENCE, |d| d.answers.contains_key(&id)).await;
        match self.answers.remove(&id) {
            Some(Reply::Answer { result, .. }) => result,
            _ => unreachable!("an answer was filed"),
        }
    }

    /// Sends a request and waits for its answer.
    pub async fn ask(&mut self, op: Op) -> Result<Answer, ErrorWord> {
        let id = self.send(op).await;
        self.answer(id).await
    }

    /// Sends a request that must succeed.
    pub async fn ok(&mut self, op: Op) -> Answer {
        match self.ask(op).await {
            Ok(answer) => answer,
            Err(error) => panic!("{} failed: {error}", op.name()),
        }
    }

    /// Sends a request that must fail, and returns the word.
    pub async fn err(&mut self, op: Op) -> ErrorWord {
        match self.ask(op).await {
            Ok(answer) => panic!("{} succeeded: {answer:?}", op.name()),
            Err(error) => error,
        }
    }

    /// Starts a session of 80 by 24 and returns its number.
    pub async fn new_session(&mut self) -> u32 {
        match self.ok(Op::New { cols: 80, rows: 24 }).await {
            Answer::Created { session, .. } => session,
            other => panic!("not a created session: {other:?}"),
        }
    }

    /// Attaches and returns the size of the snapshot the server announced.
    pub async fn attach(&mut self, session: u32, tail: Option<u32>) -> Answer {
        self.ok(Op::Attach { session, tail }).await
    }

    /// Sends input for a session as a binary frame.
    pub async fn type_into(&mut self, session: u32, bytes: &[u8]) {
        let frame = holzwave_core::remote::proto::encode_input(session, bytes).expect("an input frame");
        self.ws.send_frame(0x80 | BINARY, &frame).await;
    }

    /// Sends a binary frame as it is.
    pub async fn send_binary(&mut self, bytes: &[u8]) {
        self.ws.send_frame(0x80 | BINARY, bytes).await;
    }

    /// Sends a text message as it is.
    pub async fn send_text(&mut self, text: &str) {
        self.ws.send_text(text).await;
    }

    /// The live bytes of a session so far, as text.
    pub fn live_text(&self, session: u32) -> String {
        String::from_utf8_lossy(self.live.get(&session).map_or(&[][..], Vec::as_slice)).into_owned()
    }

    /// The snapshot bytes of a session so far, as text.
    pub fn snapshot_text(&self, session: u32) -> String {
        String::from_utf8_lossy(self.snapshot.get(&session).map_or(&[][..], Vec::as_slice)).into_owned()
    }

    /// Waits until the live bytes of a session hold `needle`.
    pub async fn until_live(&mut self, session: u32, needle: &str, within: Duration) {
        let what = format!("{needle} in the live bytes");
        self.until(&what, within, |d| d.live_text(session).contains(needle)).await;
    }

    /// The writer events of a session so far, as words.
    pub fn writer_words(&self, session: u32) -> Vec<&'static str> {
        self.events
            .iter()
            .filter_map(|e| match e {
                Event::Writer { session: s, state } if *s == session => Some(state.word()),
                _ => None,
            })
            .collect()
    }

    /// Waits for the close message; its code and reason.
    pub async fn close_code(&mut self, within: Duration) -> Option<(u16, String)> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            if let Some((Some(code), reason)) = &self.closed {
                return Some((*code, reason.clone()));
            }
            if self.ended {
                return None;
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            self.pump(left).await;
        }
    }

    /// Reads to the end of the connection; true when the server closed it within `within`.
    pub async fn ends_within(&mut self, within: Duration) -> bool {
        self.ws.ends_within(within).await
    }
}

/// A frame the vocabulary does not know, for tests of the drop rule.
pub fn input_frame(session: u32, bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![holzwave_core::remote::proto::TAG_INPUT];
    out.extend_from_slice(&session.to_be_bytes());
    out.extend_from_slice(bytes);
    assert!(Input::parse(&out).is_ok());
    out
}
