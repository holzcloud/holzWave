//! What only the whole-phase scenario needs on top of the rigs of the device socket and the real client: a relay that carries
//! every request of a kept-alive connection (the sign-in, the sync and the session socket of a real app all go through one
//! address), a person's device as the app holds it (a data folder, a keychain stand-in, the link, the sync and a core that shows
//! panes), the requests a person's page and an administrator's page send, and the readers the sweep needs.
//!
//! The listener of the environment speaks TLS for the public host of the test application only, and the client refuses clear
//! text anywhere but on loopback. The relay of `remote_env.rs` rewrites the first request of a connection, which is enough for a
//! socket; the app's HTTP agent keeps a connection for several requests, so this relay reads every request head of a connection,
//! sets its `Host` to the public host, passes its body on by its length, and after an upgrade carries the bytes as they are. It
//! keeps a copy of what the server sent back, so that the test can learn the codes and tokens the real client received without
//! asking the client for a secret it keeps to itself.
//!
//! Tests pull it in with `#[path = "support/phase_env.rs"] mod phase_env;` next to `device_socket`, `edge`, `edge_tls`, `sweep`
//! and `ws_env`. Everything here needs the `in-process-workers` feature.
#![allow(dead_code)]
#![cfg(feature = "in-process-workers")]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use holzwave_core::connections::ConnView;
use holzwave_core::engine::DomKey;
use holzwave_core::layout::{MAIN_WINDOW, PaneId};
use holzwave_core::remote::hub::HubOptions;
use holzwave_core::secrets::{MemoryStore, SecretStore};
use holzwave_core::server_link::token_load;
use holzwave_core::ssh::Reconnect;
use holzwave_host::seams::{Host, HostLimits, NeutralHost, Platform};
use holzwave_host::server::{ServerRuntime, SignInPoll, SignInView};
use holzwave_host::sessions::{ServerParts, SessionRow};
use holzwave_host::state::{AppCore, Out, ShellSpec, Sink, Target, View};
use holzwave_host::sync::SyncRuntime;
use holzwave_host::windows::{CursorSpot, Place};
use rustls::pki_types::CertificateDer;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{AbortHandle, JoinHandle};

use crate::edge::{HOST, ORIGIN, Signed};
use crate::edge_tls::{Reply, Request};
use crate::ws_env::{WsEnv, tls_to};

/// The longest wait for a shell, a prompt or an attach.
pub const SHELL: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------------------------------------------------
// The relay
// ---------------------------------------------------------------------------------------------------------------------

/// A relay from a loopback port to the TLS listener of an environment that rewrites the `Host` of every request.
pub struct HttpRelay {
    /// `http://127.0.0.1:<port>`: the address the apps sign in at.
    pub origin: String,
    task: JoinHandle<()>,
    connections: Arc<Mutex<Vec<AbortHandle>>>,
    /// Every byte the server sent back through the relay, in the order it came on each connection.
    capture: Arc<Mutex<Vec<u8>>>,
}

impl Drop for HttpRelay {
    fn drop(&mut self) {
        self.task.abort();
        for handle in self.connections.lock().unwrap().drain(..) {
            handle.abort();
        }
    }
}

/// Where `needle` starts in `haystack`.
pub fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// What a request head says about the bytes that follow it.
struct Head {
    bytes: Vec<u8>,
    length: usize,
    upgrade: bool,
}

/// The head with its `Host` line set to the public host, and the length of its body. A body without a stated length is not
/// carried: the app's agent states it, and a test that sees the panic learns that this changed.
fn rewrite(head: &[u8]) -> Head {
    let text = String::from_utf8_lossy(head);
    let mut out = String::new();
    let (mut length, mut upgrade) = (0usize, false);
    for line in text.split("\r\n") {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("host:") {
            out.push_str(&format!("Host: {HOST}"));
        } else {
            if let Some(value) = lower.strip_prefix("content-length:") {
                length = value.trim().parse().expect("a length the relay can read");
            }
            assert!(!lower.starts_with("transfer-encoding:"), "the relay carries bodies of a stated length only");
            if lower.starts_with("upgrade:") {
                upgrade = true;
            }
            out.push_str(line);
        }
        out.push_str("\r\n");
    }
    out.truncate(out.len() - 2);
    Head { bytes: out.into_bytes(), length, upgrade }
}

async fn carry(client: TcpStream, upstream: SocketAddr, der: CertificateDer<'static>, capture: Arc<Mutex<Vec<u8>>>) {
    let server = tls_to(upstream, &der).await;
    let (mut from_client, mut to_client) = client.into_split();
    let (mut from_server, mut to_server) = tokio::io::split(server);
    // The answers go back as they come, and a copy is kept.
    let down = tokio::spawn(async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match from_server.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    capture.lock().unwrap().extend_from_slice(&buf[..n]);
                    if to_client.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = to_client.shutdown().await;
    });
    let mut pending: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 16 * 1024];
    'requests: loop {
        let end = loop {
            if let Some(at) = find(&pending, b"\r\n\r\n") {
                break at + 4;
            }
            if pending.len() > 64 * 1024 {
                break 'requests;
            }
            match from_client.read(&mut buf).await {
                Ok(0) | Err(_) => break 'requests,
                Ok(n) => pending.extend_from_slice(&buf[..n]),
            }
        };
        let head = rewrite(&pending[..end]);
        pending.drain(..end);
        if to_server.write_all(&head.bytes).await.is_err() {
            break;
        }
        if head.upgrade {
            // From here on the connection is a socket: the bytes pass as they are.
            if to_server.write_all(&pending).await.is_err() {
                break;
            }
            let _ = tokio::io::copy(&mut from_client, &mut to_server).await;
            break;
        }
        let mut left = head.length;
        while left > 0 {
            if pending.is_empty() {
                match from_client.read(&mut buf).await {
                    Ok(0) | Err(_) => break 'requests,
                    Ok(n) => pending.extend_from_slice(&buf[..n]),
                }
            }
            let take = left.min(pending.len());
            if to_server.write_all(&pending[..take]).await.is_err() {
                break 'requests;
            }
            pending.drain(..take);
            left -= take;
        }
        if to_server.flush().await.is_err() {
            break;
        }
    }
    let _ = to_server.shutdown().await;
    let _ = down.await;
}

impl HttpRelay {
    pub async fn start(env: &WsEnv) -> HttpRelay {
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
        let connections = Arc::new(Mutex::new(Vec::new()));
        let capture = Arc::new(Mutex::new(Vec::new()));
        let (kept, copied) = (connections.clone(), capture.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((client, _)) = listener.accept().await else { break };
                let handle = tokio::spawn(carry(client, upstream, der.clone(), copied.clone()));
                kept.lock().unwrap().push(handle.abort_handle());
            }
        });
        HttpRelay { origin: format!("http://127.0.0.1:{port}"), task, connections, capture }
    }

    /// Every distinct secret of the shape `prefix` plus 43 base64url characters that the server sent to an app.
    pub fn secrets_sent(&self, prefix: &str) -> Vec<String> {
        let capture = self.capture.lock().unwrap();
        let mut found: Vec<String> = Vec::new();
        let mut at = 0;
        while let Some(start) = find(&capture[at..], prefix.as_bytes()) {
            let from = at + start;
            let body = &capture[from + prefix.len()..];
            let len = body.iter().take_while(|b| b.is_ascii_alphanumeric() || **b == b'-' || **b == b'_').count();
            if len == 43 {
                let text = String::from_utf8_lossy(&capture[from..from + prefix.len() + 43]).into_owned();
                if !found.contains(&text) {
                    found.push(text);
                }
            }
            at = from + prefix.len();
        }
        found
    }

    /// How many connections the relay carries or carried.
    pub fn connections(&self) -> usize {
        self.connections.lock().unwrap().len()
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// A device as the app holds it
// ---------------------------------------------------------------------------------------------------------------------

/// What the page of a core would show: the rows of every pane, built from its frames, and the exits it was told.
#[derive(Default)]
pub struct Screens {
    rows: Mutex<HashMap<PaneId, Vec<String>>>,
}

impl Sink for Screens {
    fn send(&self, _target: Target, out: Out) {
        if let Out::Frame(frame) = out {
            let mut all = self.rows.lock().unwrap();
            let rows = all.entry(frame.pane).or_default();
            if frame.full {
                rows.clear();
            }
            rows.resize(usize::from(frame.rows), String::new());
            for row in &frame.changed {
                if let Some(slot) = rows.get_mut(usize::from(row.y)) {
                    // One code point per cell; the second half of a wide character is a NUL.
                    *slot = row.runs.iter().flat_map(|run| run.0.chars()).filter(|c| *c != '\0').collect();
                }
            }
        }
    }

    fn close_window(&self, _label: &str) {}

    fn open_window(&self, _label: &str, _place: &Place) -> Result<(), String> {
        Ok(())
    }

    fn cursor_spot(&self, _source: &str) -> CursorSpot {
        CursorSpot::Unknown
    }
}

impl Screens {
    /// The screen of `pane` as text, one line per row, the empty rows at the end left out.
    pub fn text(&self, pane: PaneId) -> String {
        let all = self.rows.lock().unwrap();
        let rows = all.get(&pane).cloned().unwrap_or_default();
        let used = rows.iter().rposition(|row| !row.trim().is_empty()).map_or(0, |at| at + 1);
        rows[..used].iter().map(|row| row.trim_end()).collect::<Vec<&str>>().join("\n")
    }
}

/// The neutral host with the keychain stand-in of the device.
struct DeskHost {
    base: NeutralHost,
    store: Arc<MemoryStore>,
}

impl Host for DeskHost {
    fn platform(&self) -> Platform {
        self.base.platform()
    }

    fn secrets(&self) -> Arc<dyn SecretStore> {
        self.store.clone()
    }

    fn local_network(&self) -> Arc<dyn holzwave_host::local_network::Source> {
        self.base.local_network()
    }

    fn text_scale(&self) -> u32 {
        self.base.text_scale()
    }

    fn limits(&self) -> HostLimits {
        self.base.limits()
    }
}

/// Settings for the hub of a device in this scenario: tries a quarter of a second apart.
pub fn quick() -> HubOptions {
    HubOptions { reconnect: Reconnect { attempts: 8, delay: Duration::from_millis(250) }, ..HubOptions::default() }
}

/// Runs a blocking call of the client (the runtimes block on the network) off the async threads of the test.
pub async fn blocking<T: Send + 'static>(call: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(call).await.expect("the blocking call finished")
}

/// One device of a person: its data folder, its keychain stand-in, its link to the server, its sync and, once started, the core
/// of the app that shows its panes. The link and the sync share one runtime, as they do in the app.
pub struct Home {
    pub dir: PathBuf,
    pub store: Arc<MemoryStore>,
    pub server: Arc<ServerRuntime>,
    pub sync: Arc<SyncRuntime>,
}

impl Home {
    pub fn new(tag: &str) -> Home {
        let dir = std::env::temp_dir().join(format!("hw-phase-flow-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the data folder");
        let store = Arc::new(MemoryStore::default());
        let server = Arc::new(ServerRuntime::new_for_loopback(dir.clone(), store.clone()).with_hub_options(quick()));
        let sync = Arc::new(SyncRuntime::new(dir.clone(), store.clone(), server.clone()));
        Home { dir, store, server, sync }
    }

    /// Starts a sign-in at `origin` with the app's own client.
    pub async fn sign_in_start(&self, origin: &str) -> Result<SignInView, String> {
        let (server, origin) = (self.server.clone(), origin.to_owned());
        blocking(move || server.sign_in_start(&origin)).await
    }

    /// One poll of the sign-in in progress.
    pub async fn sign_in_poll(&self) -> Result<SignInPoll, String> {
        let server = self.server.clone();
        blocking(move || server.sign_in_poll()).await
    }

    /// The token this device keeps, as the keychain stand-in holds it; `None` once it was forgotten.
    pub fn token(&self) -> Option<String> {
        let kept = token_load(&*self.store).expect("the keychain stand-in reads");
        kept.map(|token| token.bearer().trim_start_matches("Bearer ").to_owned())
    }

    /// The id of this device on the server, as the link file holds it.
    pub fn device_id(&self) -> i64 {
        let text = std::fs::read_to_string(self.dir.join("server_link.json")).expect("the link file");
        let link: Value = serde_json::from_str(&text).expect("the link file is JSON");
        link["device_id"].as_str().expect("a device id").parse().expect("a number")
    }

    /// The app starts on this folder, with the runtime its sync uses.
    pub async fn start(&self) -> Desk {
        let (dir, store, server) = (self.dir.clone(), self.store.clone(), self.server.clone());
        let screens = Arc::new(Screens::default());
        let sink = screens.clone();
        let core = blocking(move || {
            let host = Arc::new(DeskHost { base: NeutralHost::new(), store });
            let parts = ServerParts { runtime: Some(server), sessions: None };
            AppCore::new_with_server(dir, sink, ShellSpec::new("/bin/sh", &[]), None, None, host, parts)
        })
        .await;
        Desk { core, screens }
    }

    /// The files of the sync that the app wrote here (the synced files and the sync's own state), by name.
    pub fn synced(&self) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        for name in ["settings.json", "layout.json", "connections.json", "widget_notes.json"] {
            if let Ok(bytes) = std::fs::read(self.dir.join(name)) {
                out.push((name.to_owned(), bytes));
            }
        }
        out
    }

    pub fn remove(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A running app: its core and what its page would show.
pub struct Desk {
    pub core: Arc<AppCore>,
    pub screens: Arc<Screens>,
}

impl Desk {
    /// A call of the core that may block on the network.
    pub async fn call<T: Send + 'static>(&self, call: impl FnOnce(&AppCore) -> T + Send + 'static) -> T {
        let core = self.core.clone();
        blocking(move || call(&core)).await
    }

    pub async fn sessions(&self) -> Result<Vec<SessionRow>, String> {
        self.call(|core| core.server_sessions()).await
    }

    pub async fn new_session(&self) -> Result<View, String> {
        self.call(|core| core.server_session_new_in(MAIN_WINDOW)).await
    }

    pub async fn open(&self, session: u32) -> Result<View, String> {
        self.call(move |core| core.server_session_open_in(MAIN_WINDOW, session)).await
    }

    pub async fn end(&self, session: u32) -> Result<(), String> {
        self.call(move |core| core.server_session_end(session)).await
    }

    pub async fn takeover(&self, pane: PaneId) -> Result<(), String> {
        self.call(move |core| core.pane_takeover(pane)).await
    }

    /// The page starts the pane, as it does when it sees it in a view.
    pub fn start(&self, pane: PaneId) {
        self.core.start_pane(pane, 80, 24, 8, 16).expect("the pane starts");
    }

    pub fn typed(&self, pane: PaneId, text: &str) {
        self.core.text(pane, text.to_owned()).expect("typed");
    }

    pub fn enter(&self, pane: PaneId) {
        let key = DomKey { code: "Enter".into(), key: "Enter".into(), ..DomKey::default() };
        self.core.key(pane, key).expect("the key");
    }

    pub fn screen(&self, pane: PaneId) -> String {
        self.screens.text(pane)
    }

    /// The line of `pane` in the answer of `pane_connections`.
    pub fn conn(&self, pane: PaneId) -> Option<ConnView> {
        self.core.pane_connections().into_iter().find(|view| view.pane == pane)
    }

    /// The word of who writes in `pane`, empty when the pane has no remote view.
    pub fn writer(&self, pane: PaneId) -> &'static str {
        self.conn(pane).and_then(|view| view.remote).map_or("", |remote| remote.writer)
    }

    pub async fn until_screen(&self, pane: PaneId, what: &str, check: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + SHELL;
        loop {
            let text = self.screen(pane);
            if check(&text) {
                return;
            }
            let line = self.conn(pane);
            assert!(Instant::now() < deadline, "{what} did not happen in time; the screen: {text:?}; the line: {line:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Waits until the pane is attached and shows the prompt of the shell.
    pub async fn until_prompt(&self, pane: PaneId) {
        until("the attach", SHELL, || {
            self.conn(pane).is_some_and(|v| v.state == "connected" && v.remote.is_some_and(|r| r.phase.is_empty()))
        })
        .await;
        self.until_screen(pane, "a prompt", |text| {
            let last = text.trim_end();
            last.ends_with('$') || last.ends_with('#') || last.ends_with('%')
        })
        .await;
    }

    /// A line of the screen that is exactly `line`: the output of a command, never the echo of what was typed.
    pub async fn until_line(&self, pane: PaneId, line: &str) {
        self.until_screen(pane, line, |text| has_line(text, line)).await;
    }

    pub async fn until_writer(&self, pane: PaneId, word: &str) {
        until(&format!("the label {word}"), SHELL, || self.writer(pane) == word).await;
    }

    /// The app ends: the layout is written and every pane is closed. The data folder stays.
    pub async fn quit(self) {
        blocking(move || {
            self.core.shutdown();
            drop(self);
        })
        .await;
    }
}

pub fn has_line(text: &str, line: &str) -> bool {
    text.lines().any(|l| l.trim() == line)
}

pub async fn until(what: &str, within: Duration, check: impl Fn() -> bool) {
    let deadline = Instant::now() + within;
    while !check() {
        assert!(Instant::now() < deadline, "{what} did not happen in time");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The one pane a view of a new tab shows.
pub fn pane_of(view: &View) -> PaneId {
    assert_eq!(view.panes.len(), 1, "a new tab has one pane");
    view.focus
}

// ---------------------------------------------------------------------------------------------------------------------
// What a page and a native client send
// ---------------------------------------------------------------------------------------------------------------------

/// A change as a signed-in page sends it: the public `Origin`, JSON, the session cookie and the request token.
pub fn page_change(method: &str, path: &str, who: &Signed, body: &Value) -> Request {
    let mut request = Request::new(method, path)
        .header("Origin", ORIGIN)
        .header("Content-Type", "application/json")
        .cookie(&who.cookie)
        .header("X-CSRF-Token", &who.csrf);
    request.body = body.to_string().into_bytes();
    request
}

/// A read as a signed-in page sends it.
pub fn page_read(path: &str, who: &Signed) -> Request {
    Request::new("GET", path).cookie(&who.cookie)
}

/// A request of a native client: the public host, no cookie, no `Origin`, and the bearer header when given.
pub fn native(method: &str, path: &str, bearer: Option<&str>, body: Option<&[u8]>) -> Request {
    let mut request = Request::new(method, path);
    if let Some(token) = bearer {
        request = request.header("Authorization", &format!("Bearer {token}"));
    }
    if let Some(bytes) = body {
        request = request.header("Content-Type", "application/json");
        request.body = bytes.to_vec();
    }
    request
}

/// The administrator turns the two switches with a recent sign-in; the answer must be 200.
pub async fn switches(env: &WsEnv, admin: &Signed, body: Value) -> Value {
    let reply = env.plain(page_change("POST", "/api/admin/device-access", admin, &body)).await;
    assert_eq!(reply.status, 200, "the switch change: {}", reply.text());
    reply.json()
}

/// The person approves the code their app shows, on the page of the server.
pub async fn approve(env: &WsEnv, who: &Signed, code: &str) -> Reply {
    env.plain(page_change("POST", "/api/device/approve", who, &serde_json::json!({ "code": code }))).await
}

/// The code of a refusal (`{"code": ...}`), empty when there is none.
pub fn code_of(reply: &Reply) -> String {
    serde_json::from_slice::<Value>(&reply.body).ok().and_then(|v| v["code"].as_str().map(str::to_owned)).unwrap_or_default()
}

/// The names and entity tags a device is told for the folder `dir` of its own sync store.
pub async fn listing(env: &WsEnv, token: &str, dir: &str) -> Reply {
    env.plain(native("GET", &format!("/sync/v1/list/{dir}"), Some(token), None)).await
}

/// The `(name, etag)` pairs of a listing that answered 200.
pub fn entries(reply: &Reply) -> Vec<(String, String)> {
    assert_eq!(reply.status, 200, "the listing: {}", reply.text());
    let mut out: Vec<(String, String)> = reply
        .json()
        .as_array()
        .expect("a list")
        .iter()
        .map(|entry| {
            let name = entry["name"].as_str().unwrap_or_default().to_owned();
            let tag = entry["etag"].as_str().or_else(|| entry["checksum"].as_str()).unwrap_or_default().to_owned();
            (name, tag)
        })
        .collect();
    out.sort();
    out
}

// ---------------------------------------------------------------------------------------------------------------------
// The sweep
// ---------------------------------------------------------------------------------------------------------------------

/// The key the sync derives from `passphrase` for the folder whose `meta.json` is `meta`, computed here with the same
/// parameters, so the sweep can look for it.
pub fn derived_key(passphrase: &str, meta: &[u8]) -> [u8; 32] {
    use argon2::{Algorithm, Argon2, Params, Version};
    let meta: Value = serde_json::from_slice(meta).expect("meta.json is JSON");
    let number = |name: &str| u32::try_from(meta[name].as_u64().expect("a parameter")).expect("in range");
    let salt_hex = meta["salt"].as_str().expect("a salt");
    let byte = |i: usize| u8::from_str_radix(&salt_hex[i..i + 2], 16).expect("hex");
    let salt: Vec<u8> = (0..salt_hex.len()).step_by(2).map(byte).collect();
    let params = Params::new(number("m_kib"), number("t"), number("p"), Some(32)).expect("the parameters");
    let mut out = [0u8; 32];
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    argon.hash_password_into(passphrase.as_bytes(), &salt, &mut out).expect("derive");
    out
}

/// The files of a device that hold its link and its state: the link, the sync's settings and state, the record of the remote
/// panes. Each one that exists, with its bytes.
pub fn link_and_state_files(dir: &Path) -> Vec<PathBuf> {
    ["server_link.json", "sync.json", "sync_state.json", "remote_panes.json"]
        .iter()
        .map(|name| dir.join(name))
        .filter(|path| path.exists())
        .collect()
}

/// Lower-case hexadecimal.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}
