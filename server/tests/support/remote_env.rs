//! What the tests of the real client against the real server need on top of `device_socket.rs`: a way for the core's own
//! WebSocket client (blocking, `ws://` to a loopback address) to reach the TLS listener of a `WsEnv`, and an endpoint that
//! hands the hub the address and the token of a device of the rig.
//!
//! The listener of the environment speaks TLS for the public host of the test application and nothing else, and the client
//! refuses clear text anywhere but on loopback, so a relay sits between them: it accepts a loopback connection, rewrites the
//! `Host` line of the upgrade request to the public host, opens a TLS connection to the listener that trusts its certificate,
//! and copies bytes both ways. The relay is also the network of the tests: `cut` ends every connection as a failed network
//! does (no close frame), and `block` makes it refuse to carry a new one for a while.
//!
//! Tests pull it in with `#[path = "support/remote_env.rs"] mod remote_env;` next to `device_socket`, `edge`, `edge_tls` and
//! `ws_env`. Everything here needs the `in-process-workers` feature.
#![allow(dead_code)]
#![cfg(feature = "in-process-workers")]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use holzwave_core::conn::{RemoteState, RemoteView};
use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec, RemoteTarget};
use holzwave_core::remote::hub::{Endpoint, Hub, HubOptions};
use holzwave_core::server_link::{Fault, Origin, Token};
use holzwave_core::ssh::Reconnect;
use holzwave_core::textread::TextRegion;
use rustls::pki_types::CertificateDer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{AbortHandle, JoinHandle};

use crate::device_socket::{Minted, mint};
use crate::edge::HOST;
use crate::ws_env::{Person, WsEnv, tls_to};

/// A relay from a loopback port to the TLS listener of an environment.
pub struct Relay {
    /// `http://127.0.0.1:<port>`: what an endpoint points the hub at.
    pub origin: String,
    state: Arc<RelayState>,
    task: JoinHandle<()>,
}

#[derive(Default)]
struct RelayState {
    blocked: AtomicBool,
    accepted: AtomicUsize,
    connections: Mutex<Vec<AbortHandle>>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
        self.cut();
    }
}

/// The head of an upgrade request with its `Host` line set to the public host of the test application.
fn rewrite_host(head: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(head);
    let mut out = String::new();
    for line in text.split("\r\n") {
        if line.len() >= 5 && line[..5].eq_ignore_ascii_case("host:") {
            out.push_str(&format!("Host: {HOST}"));
        } else {
            out.push_str(line);
        }
        out.push_str("\r\n");
    }
    // `split` makes one more piece than there are separators.
    out.truncate(out.len() - 2);
    out.into_bytes()
}

async fn carry(mut client: TcpStream, upstream: SocketAddr, der: CertificateDer<'static>) {
    // The head of the request, up to the empty line; whatever came after it is passed on as it is.
    let mut head = Vec::new();
    let mut buf = [0u8; 2048];
    let end = loop {
        let Ok(n) = client.read(&mut buf).await else { return };
        if n == 0 {
            return;
        }
        head.extend_from_slice(&buf[..n]);
        if let Some(at) = head.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        if head.len() > 16 * 1024 {
            return;
        }
    };
    let mut server = tls_to(upstream, &der).await;
    let mut first = rewrite_host(&head[..end]);
    first.extend_from_slice(&head[end..]);
    if server.write_all(&first).await.is_err() || server.flush().await.is_err() {
        return;
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
}

impl Relay {
    pub async fn start(env: &WsEnv) -> Relay {
        // The certificate of the listener is what a connection to it shows: read it from one and trust exactly it.
        let probe = env.connection().await;
        let der = probe
            .stream
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|chain| chain.first())
            .expect("the listener shows its certificate")
            .clone()
            .into_owned();
        drop(probe);
        let upstream = env.addr;
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("address").port();
        let state = Arc::new(RelayState::default());
        let shared = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((client, _)) = listener.accept().await else { break };
                shared.accepted.fetch_add(1, Ordering::SeqCst);
                if shared.blocked.load(Ordering::SeqCst) {
                    drop(client);
                    continue;
                }
                let handle = tokio::spawn(carry(client, upstream, der.clone()));
                shared.connections.lock().unwrap().push(handle.abort_handle());
            }
        });
        Relay { origin: format!("http://127.0.0.1:{port}"), state, task }
    }

    /// Ends every connection that is open, at once and with no close frame: the network failed.
    pub fn cut(&self) {
        for handle in self.state.connections.lock().unwrap().drain(..) {
            handle.abort();
        }
    }

    /// While on, a new connection is accepted and dropped at once.
    pub fn block(&self, on: bool) {
        self.state.blocked.store(on, Ordering::SeqCst);
    }

    /// Cuts every connection and refuses new ones for `how_long`.
    pub async fn outage(&self, how_long: Duration) {
        self.block(true);
        self.cut();
        tokio::time::sleep(how_long).await;
        self.block(false);
    }

    /// How many connections reached the relay, carried or not.
    pub fn accepted(&self) -> usize {
        self.state.accepted.load(Ordering::SeqCst)
    }
}

/// What a hub is told about a server: where it is and the token of one device. The token can be replaced (a new sign-in) and
/// taken away (a sign-out), as the host's keychain custody would.
pub struct RigEndpoint {
    origin: Origin,
    token: Mutex<Option<String>>,
    asked: AtomicUsize,
}

impl RigEndpoint {
    pub fn new(relay: &Relay, token: &str) -> Arc<RigEndpoint> {
        Arc::new(RigEndpoint {
            origin: Origin::parse(&relay.origin).expect("a loopback origin"),
            token: Mutex::new(Some(token.to_owned())),
            asked: AtomicUsize::new(0),
        })
    }

    pub fn set_token(&self, token: Option<&str>) {
        *self.token.lock().unwrap() = token.map(str::to_owned);
    }

    /// How many times the hub asked for a token: once for every connection it made.
    pub fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

impl Endpoint for RigEndpoint {
    fn origin(&self) -> Option<Origin> {
        Some(self.origin.clone())
    }

    fn token(&self) -> Result<Token, Fault> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        match self.token.lock().unwrap().as_deref() {
            Some(text) => Token::parse(text),
            None => Err(Fault::SignedOut),
        }
    }
}

/// An environment, a relay to it and a person with devices.
pub struct Remote {
    pub env: WsEnv,
    pub relay: Relay,
    pub alice: Person,
}

impl Remote {
    pub async fn start() -> Remote {
        Remote::from(crate::device_socket::env().await).await
    }

    pub async fn from(env: WsEnv) -> Remote {
        let relay = Relay::start(&env).await;
        let alice = env.person("alice");
        Remote { env, relay, alice }
    }

    /// A new device of alice and the endpoint for it.
    pub fn device(&self, name: &str) -> (Minted, Arc<RigEndpoint>) {
        let minted = mint(&self.env, &self.alice, name);
        let endpoint = RigEndpoint::new(&self.relay, &minted.token);
        (minted, endpoint)
    }

    /// A hub of a new device of alice, with the settings of a test: a short pause between tries.
    pub fn hub(&self, name: &str) -> (Hub, Minted, Arc<RigEndpoint>) {
        let (minted, endpoint) = self.device(name);
        let hub = Hub::new(endpoint.clone(), quick());
        (hub, minted, endpoint)
    }
}

/// Settings for a hub in a test: eight tries, a quarter of a second apart.
pub fn quick() -> HubOptions {
    HubOptions { reconnect: Reconnect { attempts: 8, delay: Duration::from_millis(250) }, ..HubOptions::default() }
}

/// Runs a blocking call of the client (the hub is blocking) off the async threads of the test.
pub async fn blocking<T: Send + 'static>(call: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(call).await.expect("the blocking call finished")
}

/// What a pane told its host, except frames; frames only count, and each one is stamped.
#[derive(Clone, Default)]
pub struct Probe {
    events: Arc<Mutex<Vec<PaneEvent>>>,
    frames: Arc<Mutex<Option<std::sync::mpsc::Sender<Instant>>>>,
}

impl Probe {
    fn sink(&self) -> impl FnMut(PaneEvent) + Send + 'static {
        let events = self.events.clone();
        let frames = self.frames.clone();
        move |event| {
            if matches!(event, PaneEvent::Frame(_)) {
                if let Some(tx) = frames.lock().unwrap().as_ref() {
                    let _ = tx.send(Instant::now());
                }
            } else {
                events.lock().unwrap().push(event);
            }
        }
    }

    /// From now on every frame sends the instant it was built to the returned receiver.
    pub fn stamps(&self) -> std::sync::mpsc::Receiver<Instant> {
        let (tx, rx) = std::sync::mpsc::channel();
        *self.frames.lock().unwrap() = Some(tx);
        rx
    }

    pub fn exits(&self) -> Vec<Option<u32>> {
        let events = self.events.lock().unwrap();
        events.iter().filter_map(|e| if let PaneEvent::Exited { code } = e { Some(*code) } else { None }).collect()
    }

    pub fn held(&self) -> Vec<Option<u32>> {
        let events = self.events.lock().unwrap();
        events.iter().filter_map(|e| if let PaneEvent::Held { code } = e { Some(*code) } else { None }).collect()
    }
}

/// A pane that shows `session` of the server `hub` talks to; held when its session ends, so that the end can be read.
pub fn open_pane(hub: &Hub, session: u32, size: (u16, u16)) -> (PaneHandle, Probe) {
    let mut spec = PaneSpec::new("remote", size.0, size.1);
    spec.exit_poll = None;
    spec.linger = Some(Duration::from_secs(3600));
    spec.remote = Some(RemoteTarget { session, tail: None, attach: Arc::new(hub.clone()) });
    let probe = Probe::default();
    let pane = PaneHandle::spawn_ghostty(1, spec, probe.sink()).expect("the pane starts");
    (pane, probe)
}

/// The text of the last rows and the screen of a pane.
pub fn screen(pane: &PaneHandle) -> String {
    pane.read_text(TextRegion::LastRows(2000), 4 << 20).ok().flatten().map(|r| r.text).unwrap_or_default()
}

/// Waits until `check` holds for the screen of the pane; panics with `what` after `within`.
pub async fn until_screen(pane: &PaneHandle, what: &str, within: Duration, check: impl Fn(&str) -> bool) {
    let deadline = Instant::now() + within;
    loop {
        let text = screen(pane);
        if check(&text) {
            return;
        }
        assert!(Instant::now() < deadline, "{what} did not happen in time; the screen: {:?}", &text[text.len().saturating_sub(600)..]);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits until the pane is attached and shows the prompt of a shell: not only a notice.
pub async fn until_prompt(pane: &PaneHandle) {
    until_view(pane, "the attach", Duration::from_secs(20), |v| v.state == RemoteState::None).await;
    until_screen(pane, "a prompt", Duration::from_secs(20), |t| {
        let last = t.trim_end();
        last.ends_with('$') || last.ends_with('#') || last.ends_with('%')
    })
    .await;
}

/// Waits until `check` holds for `pane.remote_view()`.
pub async fn until_view(pane: &PaneHandle, what: &str, within: Duration, check: impl Fn(&RemoteView) -> bool) {
    let deadline = Instant::now() + within;
    loop {
        if pane.remote_view().is_some_and(|v| check(&v)) {
            return;
        }
        assert!(Instant::now() < deadline, "{what} did not happen in time; the view: {:?}", pane.remote_view());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
